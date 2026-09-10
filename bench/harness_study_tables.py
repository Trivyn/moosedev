"""Offline pilot tables with explicit v3 retention and v4 continuation selection."""
import argparse
from collections import Counter
import hashlib
import json
import math
from pathlib import Path


SCORED = {"success", "agent_failure"}
SETUP = ("model", "backend", "condition", "scenario_id", "schedule_index")


def encoded(value):
    return (json.dumps(value, sort_keys=True, ensure_ascii=False, allow_nan=False,
                       separators=(",", ":")) + "\n").encode()


def frozen_schedule(value, study_id):
    config = value["config"]
    digest = hashlib.sha256(encoded(config)).hexdigest()
    if not value.get("ready") or config["study_id"] != study_id or value["config_sha256"] != digest:
        raise ValueError("preflight configuration identity is invalid")
    cells = value["schedule"]
    indexes = [cell["schedule_index"] for cell in cells]
    if any(type(index) is not int for index in indexes) or sorted(indexes) != list(range(16)):
        raise ValueError("preflight must contain exactly the 16 distinct scheduled cells")
    return digest, {cell["schedule_index"]: cell for cell in cells}


def semantic_summary(run, attempted_ids):
    semantic = run.get("semantic") or {}
    summaries = []
    for review in semantic.get("judgments", []):
        if not review.get("active"):
            continue
        metrics = review.get("knowledge_metrics") or {}
        selected = [metrics.get(episode) for episode in attempted_ids]
        available = bool(selected) and all(metric is not None for metric in selected)
        claims = [claim for claim in review.get("claims", []) if claim.get("episode_id") in attempted_ids]
        counts = Counter(claim["verdict"] for claim in claims)
        summaries.append({"review_id": review["review_id"], "reviewer_id": review["reviewer_id"],
                          "supported": sum(metric["supported_fact_count"] for metric in selected) if available else None,
                          "expected": sum(metric["expected_fact_count"] for metric in selected) if available else None,
                          "addressed": sum(metric["addressed_fact_count"] for metric in selected) if available else None,
                          "complete": all(metric["complete"] for metric in selected) if available else False,
                          "unsupported": counts["unsupported"], "duplicate": counts["duplicate"], "stale": counts["stale"],
                          "unmapped_or_unattempted_claims": len(review.get("claims", [])) - len(claims),
                          "per_episode": {episode: metrics.get(episode) for episode in attempted_ids}})
    return {"active_reviews": summaries, "invalid_reviews": semantic.get("invalid", []),
            "status": "reviewed" if summaries else "pending"}


def run_summary(run):
    outcome = run["interpreted_outcome"]
    episodes = outcome["episodes"]
    attempted = [episode for episode in episodes if episode["status"] != "unattempted"]
    checks = [check for episode in attempted for check in episode.get("checks", [])]
    checks_observed = [check for check in checks if type(check.get("passed")) is bool]
    passing = [check for check in checks_observed if check["passed"]]
    elapsed = [(episode.get("metrics") or {}).get("elapsed_seconds") for episode in attempted]
    observed = [value for value in elapsed if type(value) in (int, float) and math.isfinite(value) and value >= 0]
    test_counts = [check.get("tests_run") for check in passing]
    return {"run_id": run["run_id"], "status": run["status"], "episodes": [
                {"id": episode["id"], "status": episode["status"], "checks": [
                    {key: check.get(key) for key in ("passed", "status", "tests_run")}
                    for check in episode.get("checks", [])]} for episode in episodes],
            "hidden_suites_passed": len(passing), "hidden_suites_observed": len(checks_observed),
            "tests_in_passing_suites": sum(test_counts) if passing and all(type(n) is int for n in test_counts) else None,
            "elapsed_observed_seconds": sum(observed) if observed else None,
            "elapsed_observed_episodes": len(observed), "attempted_episodes": len(attempted),
            "semantic": semantic_summary(run, [episode["id"] for episode in attempted])}


def select_matrix(comparison, policy, v3_preflight, v4_preflight):
    previous, current = policy["predecessor"], policy["study_id"]
    previous_hash, previous_cells = frozen_schedule(v3_preflight, previous)
    current_hash, current_cells = frozen_schedule(v4_preflight, current)
    if previous_cells != current_cells:
        raise ValueError("v4 retention policy requires an unchanged randomized schedule")
    retained = policy["retained_valid_v3_cells"]
    if (any(type(item["cell"]) is not int or item["status"] not in SCORED for item in retained)
            or sorted(item["cell"] for item in retained) != [0, 1, 2]
            or len({item["run_id"] for item in retained}) != 3):
        raise ValueError("retention policy must name exactly three distinct v3 runs for cells 0–2")
    retention = {item["cell"]: item for item in retained}
    runs = comparison["runs"]
    if len({run["run_id"] for run in runs}) != len(runs):
        raise ValueError("comparison contains duplicate run identities")
    identity_errors = []
    candidates = {index: [] for index in range(16)}
    for run in runs:
        manifest = run.get("manifest") or {}
        version = manifest.get("study_id")
        if version not in {previous, current}:
            continue
        digest, cells = (previous_hash, previous_cells) if version == previous else (current_hash, current_cells)
        index = manifest.get("schedule_index")
        if (manifest.get("config_sha256") != digest or type(index) is not int or index not in cells
                or any(manifest.get(key) != cells[index].get(key) for key in SETUP)):
            identity_errors.append(run["run_id"])
            continue
        if run["integrity"] == "sealed" and run["status"] in SCORED:
            outcome = run.get("interpreted_outcome")
            if not outcome or outcome["status"] != run["status"]:
                identity_errors.append(run["run_id"])
            else:
                candidates[index].append(run)
    rows = []
    for index, cell in sorted(current_cells.items()):
        required_version = previous if index < 3 else current
        scored = candidates[index]
        if index < 3:
            eligible = [run for run in scored if run["run_id"] == retention[index]["run_id"]
                        and run["manifest"]["study_id"] == previous
                        and run["status"] == retention[index]["status"]]
        else:
            eligible = [run for run in scored if run["manifest"]["study_id"] == current]
        unexpected = [run["run_id"] for run in scored if run not in eligible]
        conflict = bool(unexpected) or len(eligible) > 1
        selected = eligible[0] if len(eligible) == 1 and not conflict else None
        rows.append({**cell, "required_version": required_version,
                     "required_config_sha256": previous_hash if index < 3 else current_hash,
                     "selection": "conflict" if conflict else "selected" if selected else "pending",
                     "required_run_id": retention[index]["run_id"] if index < 3 else None,
                     "scored_candidate_ids": [run["run_id"] for run in scored],
                     "unexpected_scored_attempt_ids": unexpected,
                     "result": run_summary(selected) if selected else None})
    strata = {}
    for run in runs:
        manifest = run.get("manifest") or {}
        key = (manifest.get("study_id"), manifest.get("config_sha256"))
        strata.setdefault(key, []).append(run)
    all_attempts = [{"study_id": key[0], "config_sha256": key[1], "attempt_count": len(members),
                     "outcomes": dict(Counter(run["status"] for run in members)),
                     "run_ids": [run["run_id"] for run in members]}
                    for key, members in sorted(strata.items(), key=lambda item: json.dumps(item[0]))]
    selected_count = sum(row["selection"] == "selected" for row in rows)
    return {"schema_version": 1, "selected_cell_count": selected_count, "required_cell_count": 16,
            "complete": selected_count == 16 and not identity_errors,
            "identity_errors": identity_errors, "cells": rows,
            "all_attempt_count": len(runs), "all_attempt_outcomes": dict(Counter(run["status"] for run in runs)),
            "all_attempt_version_strata": all_attempts,
            "notes": ["Selection follows the explicit retention policy; additional scored attempts create conflicts.",
                      "Semantic assessments remain separate per active reviewer; missing review is unavailable, not zero.",
                      "Hidden suites passed is not an inferred count of individual tests passed in failing suites.",
                      "Observed elapsed sums exclude unavailable episode durations; no token totals are imputed."]}


def markdown(result):
    def safe(value):
        return str(value).replace("|", "\\|").replace("\n", " ")

    lines = [f"Selected cells: {result['selected_cell_count']}/16; complete: {result['complete']}.", "",
             "| Cell | Version | Model | Backend | Scenario | Status | Episodes | Hidden suites passed / observed | Elapsed seconds (episodes observed) | Semantic supported / expected; addressed | Unsupported / duplicate / stale |",
             "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"]
    for row in result["cells"]:
        run = row["result"]
        if run:
            episodes = "; ".join(f"{episode['id']}:{episode['status']}" for episode in run["episodes"])
            hidden = f"{run['hidden_suites_passed']} / {run['hidden_suites_observed']}"
            elapsed = "unavailable" if run["elapsed_observed_seconds"] is None else f"{run['elapsed_observed_seconds']:.1f} ({run['elapsed_observed_episodes']}/{run['attempted_episodes']})"
            reviews = run["semantic"]["active_reviews"]
            knowledge = "; ".join(
                f"{review['reviewer_id']}: unavailable" if review["expected"] is None else
                f"{review['reviewer_id']}: {review['supported']}/{review['expected']}; {review['addressed']} addressed"
                for review in reviews) or "pending"
            errors = "; ".join(f"{review['reviewer_id']}: {review['unsupported']}/{review['duplicate']}/{review['stale']}" for review in reviews) or "unavailable"
            status = run["status"]
        else:
            status = row["selection"]
            episodes = hidden = elapsed = knowledge = errors = "unavailable"
        values = [row["schedule_index"], row["required_version"], row["model"], row["backend"], row["scenario_id"], status,
                  episodes, hidden, elapsed, knowledge, errors]
        lines.append("| " + " | ".join(map(safe, values)) + " |")
    lines.extend(["", f"All retained attempts: {result['all_attempt_count']} (separate from selected cells).", "",
                  "| Study | Configuration SHA-256 | Attempts | Outcomes |", "| --- | --- | --- | --- |"])
    for stratum in result["all_attempt_version_strata"]:
        values = [stratum["study_id"], stratum["config_sha256"], stratum["attempt_count"], json.dumps(stratum["outcomes"], sort_keys=True)]
        lines.append("| " + " | ".join(map(safe, values)) + " |")
    lines += ["", *result["notes"], ""]
    return "\n".join(lines)


def write_tables(comparison_path, policy_path, v3_path, v4_path, output):
    paths = {"comparison.json": Path(comparison_path), "retention-policy.json": Path(policy_path),
             "preflight-v3.json": Path(v3_path), "preflight-v4.json": Path(v4_path)}
    contents = {name: path.read_bytes() for name, path in paths.items()}
    values = {name: json.loads(content) for name, content in contents.items()}
    result = select_matrix(values["comparison.json"], values["retention-policy.json"], values["preflight-v3.json"], values["preflight-v4.json"])
    output = Path(output)
    output.mkdir(parents=True, exist_ok=False)
    contents.update({"selected-matrix.json": encoded(result), "rows.md": markdown(result).encode(),
                     "make_pilot_tables.py": Path(__file__).read_bytes()})
    for name, content in contents.items():
        with (output / name).open("xb") as stream:
            stream.write(content)
    provenance = {"inputs": {name: {"path": str(path.resolve()), "sha256": hashlib.sha256(contents[name]).hexdigest()}
                              for name, path in paths.items()},
                  "files_sha256": {name: hashlib.sha256(content).hexdigest() for name, content in contents.items()},
                  "replay_command": "python3 make_pilot_tables.py --comparison comparison.json --policy retention-policy.json --v3 preflight-v3.json --v4 preflight-v4.json --output NEW_DIRECTORY"}
    with (output / "provenance.json").open("xb") as stream:
        stream.write(encoded(provenance))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("comparison", "policy", "v3", "v4", "output"):
        parser.add_argument("--" + name, type=Path, required=True)
    args = parser.parse_args()
    result = write_tables(args.comparison, args.policy, args.v3, args.v4, args.output)
    print(json.dumps({"selected": result["selected_cell_count"], "complete": result["complete"], "output": str(args.output)}))


if __name__ == "__main__":
    main()
