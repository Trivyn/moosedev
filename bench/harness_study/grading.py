"""Offline outcome summaries and evidence-bound human judgments.

This module never executes submissions, tests, model calls, or network requests.
It summarizes the checks recorded by the runner; semantic truth comes exclusively
from retained reviewer judgments, never from graph shape or matching wording.
"""

from collections import Counter, defaultdict
import hashlib
import math
import os
from pathlib import Path
import re
import uuid

from .artifacts import (ArtifactStore, _append, _directory, _now, _publish,
                        _read_json, _read_lines, canonical_json, sha256_file)


STATUSES = {"success", "agent_failure", "infrastructure_failure", "preflight_failure"}
VERDICTS = {"supported", "missing", "unsupported", "duplicate", "stale"}
GROUP_FIELDS = ("model", "backend", "condition", "scenario_id")


def _hash(value):
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def _scenario_facts(run, seal):
    if "scenario.json" not in seal["files"]:
        return None
    scenario = _read_json(run / "scenario.json")
    episodes = scenario.get("episodes", [])
    if not episodes or any("expected_fact_ids" not in episode for episode in episodes):
        return None
    expected = {episode["id"]: set(episode["expected_fact_ids"]) for episode in episodes}
    inherited = {fact["id"] for fact in scenario.get("initial_facts", [])}
    return expected, inherited


def _knowledge_metrics(claims, facts):
    if facts is None:
        return None
    expected, inherited = facts
    metrics = {}
    for episode_id, fact_ids in expected.items():
        judgments = [claim for claim in claims if claim.get("episode_id") == episode_id]
        counts = Counter(claim["verdict"] for claim in judgments)
        addressed = {claim["fact_id"] for claim in judgments if claim.get("fact_id") in fact_ids
                     and claim["verdict"] in {"supported", "missing"}}
        supported = {claim["fact_id"] for claim in judgments if claim.get("fact_id") in fact_ids
                     and claim["verdict"] == "supported"}
        complete = bool(fact_ids) and addressed == fact_ids
        asserted = sum(counts[verdict] for verdict in ("supported", "unsupported", "stale"))

        def recall(subset):
            return len(supported & subset) / len(subset) if subset and subset <= addressed else None

        metrics[episode_id] = {
            "expected_fact_count": len(fact_ids), "addressed_fact_count": len(addressed),
            "supported_fact_count": len(supported), "complete": complete,
            "expected_fact_coverage": len(addressed) / len(fact_ids) if fact_ids else None,
            "recall": recall(fact_ids),
            "precision": counts["supported"] / asserted if complete and asserted else None,
            "inherited_recall": recall(fact_ids & inherited), "new_recall": recall(fact_ids - inherited),
            "asserted_claim_count": asserted, "duplicate_count": counts["duplicate"],
            "stale_count": counts["stale"],
        }
    return metrics


def _validate_review(review, run, seal, manifest):
    if not isinstance(review, dict):
        raise ValueError("review must be an object")
    if not isinstance(review.get("reviewer_id"), str) or not review["reviewer_id"].strip():
        raise ValueError("reviewer_id is required")
    gold = review.get("scenario_gold_sha256")
    if not _hash(gold) or gold != manifest.get("scenario_gold_sha256"):
        raise ValueError("review must match the frozen scenario gold hash")
    if review.get("evidence_sha256", seal["evidence_sha256"]) != seal["evidence_sha256"]:
        raise ValueError("review evidence hash does not match sealed run")
    claims = review.get("claims")
    if not isinstance(claims, list) or not claims:
        raise ValueError("review must contain claim judgments")
    identifiers, assessments = set(), {}
    facts = _scenario_facts(run, seal)
    known_facts = set().union(*facts[0].values(), facts[1]) if facts is not None else None
    for claim in claims:
        if not isinstance(claim, dict):
            raise ValueError("claim judgment must be an object")
        claim_id = claim.get("claim_id")
        if not isinstance(claim_id, str) or not claim_id or claim_id in identifiers:
            raise ValueError("claim IDs must be unique nonempty strings within a review")
        identifiers.add(claim_id)
        if claim.get("verdict") not in VERDICTS:
            raise ValueError("unknown claim verdict")
        for key in ("episode_id", "fact_id"):
            if key in claim and (not isinstance(claim[key], str) or not claim[key]):
                raise ValueError(f"optional {key} must be a nonempty string")
        episode_id, fact_id = claim.get("episode_id"), claim.get("fact_id")
        if facts is not None:
            if episode_id is not None and episode_id not in facts[0]:
                raise ValueError("claim episode_id is absent from the frozen scenario")
            if fact_id is not None and fact_id not in known_facts:
                raise ValueError("claim fact_id is absent from the frozen scenario")
        if episode_id and fact_id and claim["verdict"] in {"supported", "missing"}:
            key = (episode_id, fact_id)
            if key in assessments and assessments[key] != claim["verdict"]:
                raise ValueError("a fact cannot be both supported and missing in one episode review")
            assessments[key] = claim["verdict"]
        spans = claim.get("evidence")
        if not isinstance(spans, list) or (not spans and claim["verdict"] != "missing"):
            raise ValueError("non-missing judgments require supporting evidence spans")
        if claim["verdict"] == "missing" and not claim.get("rationale"):
            raise ValueError("a missing judgment must explain what evidence was inspected")
        for span in spans:
            if not isinstance(span, dict) or span.get("path") not in seal["files"]:
                raise ValueError("evidence span must name a sealed run artifact")
            start, end = span.get("start_line"), span.get("end_line")
            if (type(start) is not int or type(end) is not int or not 1 <= start <= end):
                raise ValueError("evidence spans use inclusive 1-based line numbers")
            try:
                lines = (run / span["path"]).read_text(encoding="utf-8").splitlines()
            except UnicodeError as error:
                raise ValueError("line evidence must reference UTF-8 text") from error
            if end > len(lines):
                raise ValueError("evidence span exceeds artifact length")


def record_review(store_root, run_id, review):
    """Append a judgment; corrections explicitly supersede an earlier judgment."""
    store = ArtifactStore(store_root)
    with store.locked():
        run = store.run_path(store.root / "runs" / run_id)
        seal = store._verify(run)
        manifest = _read_json(run / "manifest.json")
        _validate_review(review, run, seal, manifest)
        for generated in ("review_id", "run_id", "created_at", "schema_version"):
            if generated in review:
                raise ValueError(f"{generated} is generated by the review store")
        reviews = _directory(store.root / "reviews" / run_id, create=True)
        previous = review.get("supersedes")
        if previous is not None:
            if not isinstance(previous, str) or str(uuid.UUID(previous)) != previous:
                raise ValueError("supersedes must be a review UUID")
            old = _read_json(reviews / f"{previous}.json")
            if old["reviewer_id"] != review["reviewer_id"]:
                raise ValueError("reviewers may only correct their own judgments")
            if any(_read_json(path).get("supersedes") == previous for path in reviews.glob("*.json")):
                raise ValueError("review was already superseded; correct its successor")
        saved = dict(review, schema_version=1, review_id=str(uuid.uuid4()), run_id=run_id,
                     created_at=_now(), evidence_sha256=seal["evidence_sha256"])
        data = canonical_json(saved)
        destination = reviews / f"{saved['review_id']}.json"
        _publish(destination, data)
        _append(store.root / "review_index.jsonl", {
            "run_id": run_id, "review_id": saved["review_id"], "created_at": saved["created_at"],
            "sha256": hashlib.sha256(data).hexdigest(), "supersedes": previous,
        })
        return destination


def _reviews(store, run, seal, manifest, index):
    directory = store.root / "reviews" / run.name
    if not directory.exists():
        return {"status": "pending", "judgments": [], "invalid": []}
    _directory(directory)
    judgments, invalid = [], []
    for path in sorted(directory.iterdir()):
        try:
            if path.suffix != ".json":
                raise ValueError("unfinished or unexpected review artifact")
            value = _read_json(path)
            review_id = value.get("review_id")
            if value.get("run_id") != run.name or path.name != f"{review_id}.json":
                raise ValueError("review identity mismatch")
            entries = [item for item in index if item.get("review_id") == review_id]
            if len(entries) != 1 or entries[0].get("sha256") != sha256_file(path):
                raise ValueError("review index hash missing, duplicated, or mismatched")
            _validate_review(value, run, seal, manifest)
            counts = dict.fromkeys(sorted(VERDICTS), 0)
            counts.update(Counter(claim["verdict"] for claim in value["claims"]))
            judgments.append({"review_id": review_id, "reviewer_id": value["reviewer_id"],
                              "supersedes": value.get("supersedes"), "counts": counts,
                              "claims": value["claims"],
                              "knowledge_metrics": _knowledge_metrics(value["claims"], _scenario_facts(run, seal))})
        except (OSError, ValueError, KeyError, TypeError) as error:
            invalid.append({"path": path.name, "error": str(error)})
    superseded = {item["supersedes"] for item in judgments if item["supersedes"]}
    for judgment in judgments:
        judgment["active"] = judgment["review_id"] not in superseded
    return {"status": "reviewed" if any(item["active"] for item in judgments) else "pending",
            "judgments": judgments, "invalid": invalid}


def _outcome(value):
    if not isinstance(value, dict) or value.get("status") not in STATUSES:
        raise ValueError("invalid recorded outcome status")
    episodes = value.get("episodes", [])
    if not isinstance(episodes, list):
        raise ValueError("outcome episodes must be a list")
    identities = set()
    for episode in episodes:
        if (not isinstance(episode, dict) or not isinstance(episode.get("id"), str)
                or episode["id"] in identities):
            raise ValueError("episode IDs must be unique strings")
        identities.add(episode["id"])
        if episode.get("status") not in STATUSES | {"unattempted"}:
            raise ValueError("invalid episode status")
        if episode.get("metrics") is not None and not isinstance(episode["metrics"], dict):
            raise ValueError("episode metrics must be an object or null")
    if value["status"] == "success" and (not episodes or any(e["status"] != "success" for e in episodes)):
        raise ValueError("successful run must have successful episodes")
    return dict(value, episodes=episodes)


def _metric_summary(outcomes):
    episodes = [episode for result in outcomes for episode in result.get("episodes", [])]
    keys = sorted({key for episode in episodes for key in (episode.get("metrics") or {})})
    metrics = {}
    for key in keys:
        values = [(episode.get("metrics") or {}).get(key) for episode in episodes]
        numbers = [v for v in values if type(v) in (int, float) and math.isfinite(v)]
        metrics[key] = {"observed": len(numbers), "missing": len(values) - len(numbers),
                        "sum": sum(numbers) if numbers else None,
                        "mean": sum(numbers) / len(numbers) if numbers else None}
    return metrics


def report(store_root):
    """Inventory all attempts. Invalid/unsealed evidence never earns a success."""
    # Refuse nonexistent roots: a typo must not manufacture an empty study.
    root = _directory(store_root)
    store = ArtifactStore(root)
    attempts, warnings = [], []
    with store.locked():
        try:
            index = _read_lines(root / "review_index.jsonl")
        except (OSError, ValueError) as error:
            index = []
            warnings.append(str(error))
        for run in sorted((root / "runs").iterdir()):
            item = {"run_id": run.name, "integrity": "invalid", "status": "invalid",
                    "manifest": None, "outcome": None, "semantic": {"status": "pending", "judgments": [], "invalid": []}}
            try:
                store.run_path(run)
                manifest = _read_json(run / "manifest.json")
                if not isinstance(manifest, dict) or manifest.get("run_id") != run.name:
                    raise ValueError("manifest identity mismatch")
                item["manifest"] = manifest
                if not os.path.lexists(run / "seal.json"):
                    item.update(integrity="unsealed", status="unfinished")
                else:
                    seal = store._verify(run)
                    item["integrity"] = "sealed"
                    outcome = _outcome(_read_json(run / "outcome.json"))
                    item.update(status=outcome["status"], outcome=outcome)
                    item["semantic"] = _reviews(store, run, seal, manifest, index)
            except (OSError, ValueError, KeyError, TypeError) as error:
                item["error"] = str(error)
            attempts.append(item)
    grouped = defaultdict(list)
    for item in attempts:
        manifest = item["manifest"] or {}
        key = tuple(canonical_json(manifest.get(field)).decode().strip() for field in GROUP_FIELDS)
        grouped[key].append(item)
    groups = []
    for key, members in sorted(grouped.items()):
        manifest = members[0]["manifest"] or {}
        counts = dict.fromkeys(sorted(STATUSES | {"unfinished", "invalid"}), 0)
        counts.update(Counter(item["status"] for item in members))
        outcomes = [item["outcome"] for item in members if item["outcome"] is not None]
        groups.append({"configuration": {field: manifest.get(field) for field in GROUP_FIELDS},
                       "attempts": len(members), "outcomes": counts,
                       "success_fraction": counts["success"] / len(members),
                       "episode_statuses": dict(Counter(e["status"] for o in outcomes for e in o["episodes"])),
                       "episode_metrics": _metric_summary(outcomes),
                       "semantic_reviewed_runs": sum(m["semantic"]["status"] == "reviewed" for m in members)})
    return {"schema_version": 1, "attempt_count": len(attempts),
            "integrity_counts": dict(Counter(item["integrity"] for item in attempts)),
            "groups": groups, "runs": attempts, "warnings": warnings}
