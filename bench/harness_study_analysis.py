"""Offline, version-stratified pilot inventory; never executes agents or checks.

Usage: python3 -m bench.harness_study_analysis --store EVIDENCE --output NEW_DIRECTORY
       [--correction target/harness-study/pilot-revision-v3.json]

The output directory preserves this analyzer and its grading dependencies. A
scheduled cell is not another attempt: replacements remain separate attempts.
"""
import argparse
from collections import Counter, defaultdict
import copy
import hashlib
import json
import math
from pathlib import Path

from .harness_study import artifacts, grading


FIELDS = ("study_id", "config_sha256", "model", "backend", "condition", "scenario_id")
METRICS = ("elapsed_seconds", "input_tokens", "output_tokens", "cache_read_tokens", "helper_tokens")


def _key(value):
    return tuple(value.get(field) for field in FIELDS)


def _sealed_bytes(run, seal, relative):
    content = (run / relative).read_bytes()
    if hashlib.sha256(content).hexdigest() != seal["files"].get(relative, {}).get("sha256"):
        raise ValueError(f"sealed artifact changed: {run.name}/{relative}")
    return content


def _resources(episodes):
    result = {}
    for field in METRICS:
        values = [(episode.get("metrics") or {}).get(field) for episode in episodes]
        observed = [value for value in values if type(value) in (int, float)
                    and math.isfinite(value) and value >= 0]
        result[field] = {"observed_episodes": len(observed),
                         "unavailable_episodes": len(values) - len(observed),
                         "observed_sum": sum(observed) if observed else None,
                         "observed_mean": sum(observed) / len(observed) if observed else None}
    return result


def _correct(receipt, runs, root, seals):
    """Accept only the explicit retained v3 interruption-accounting correction."""
    change = receipt.get("classification_correction", {})
    run_id = change.get("run_id")
    target = next((run for run in runs if run["run_id"] == run_id), None)
    if target is None or target["integrity"] != "sealed":
        raise ValueError("correction target must be a verified sealed run in this store")
    original = target["recorded_outcome"]
    if (receipt.get("replaced_attempt") != run_id
            or receipt.get("predecessor") != target["manifest"].get("study_id")
            or change.get("sealed_outcome_unchanged") is not True
            or change.get("recorded_status") != "unattempted"
            or change.get("correct_interpretation") != "attempted, interrupted infrastructure failure; no behavioral grade"
            or original["status"] != "infrastructure_failure"
            or not original.get("error", "").startswith("KeyboardInterrupt:")):
        raise ValueError("correction receipt does not match the interrupted run classification")
    episodes = original["episodes"]
    selected = next((episode for episode in episodes if episode["id"] == change.get("episode")), None)
    others = [episode for episode in episodes if episode is not selected]
    if (selected is None or selected["status"] != "unattempted"
            or change.get("unattempted_episodes") != [episode["id"] for episode in others]
            or any(episode["status"] != "unattempted" for episode in others)):
        raise ValueError("correction episode statuses do not match sealed evidence")
    seal = seals[run_id]
    events = _sealed_bytes(root / "runs" / run_id, seal, "events.jsonl")
    observed = any(event.get("channel") in {"native", "model"}
                   and event.get("payload", {}).get("episode") == selected["id"]
                   for event in map(json.loads, events.splitlines()))
    if not observed or not any(path.startswith("interrupted-workspace/") for path in seal["files"]):
        raise ValueError("correction lacks recorded episode activity or an interrupted workspace")
    interpreted = copy.deepcopy(original)
    corrected = next(episode for episode in interpreted["episodes"] if episode["id"] == selected["id"])
    corrected.update(status="infrastructure_failure", checks=[], metrics={},
                     classification_source="explicit retained correction receipt; original outcome unchanged")
    target["interpreted_outcome"] = interpreted
    target["correction_applied"] = True
    return {"run_id": run_id, "episode_id": selected["id"],
            "target_evidence_sha256": seal["evidence_sha256"], "receipt": receipt}


def analyze(store_root, *, correction=None):
    root = Path(store_root).resolve()
    inventory = grading.report(root)  # Validates seals/indexes; does not execute submissions.
    runs, seals, schedules = [], {}, {}
    for item in inventory["runs"]:
        run = {"run_id": item["run_id"], "integrity": item["integrity"], "status": item["status"],
               "manifest": item["manifest"] or {}, "recorded_outcome": item["outcome"],
               "interpreted_outcome": item["outcome"], "correction_applied": False,
               "semantic": item["semantic"]}
        if "error" in item:
            run["integrity_error"] = item["error"]
        runs.append(run)
        if item["integrity"] != "sealed":
            continue
        path = root / "runs" / run["run_id"]
        seal = json.loads((path / "seal.json").read_bytes())
        seals[run["run_id"]] = seal
        run["evidence_sha256"] = seal["evidence_sha256"]
        if "preflight.json" not in seal["files"]:
            continue
        frozen = json.loads(_sealed_bytes(path, seal, "preflight.json"))
        version = (run["manifest"].get("study_id"), run["manifest"].get("config_sha256"))
        config = frozen["config"]
        if (config.get("study_id") != version[0] or frozen.get("config_sha256") != version[1]
                or hashlib.sha256(artifacts.canonical_json(config)).hexdigest() != version[1]):
            raise ValueError("sealed preflight and run configuration identity disagree")
        cells = frozen["schedule"]
        indexes = [cell.get("schedule_index") for cell in cells]
        if any(type(index) is not int or index < 0 for index in indexes) or len(set(indexes)) != len(indexes):
            raise ValueError("sealed schedule has invalid or duplicate cell indexes")
        if version in schedules and schedules[version] != cells:
            raise ValueError("one configuration identity has inconsistent frozen schedules")
        schedules[version] = cells

    corrections = []
    if correction is not None:
        path = Path(correction)
        content = path.read_bytes()
        applied = _correct(json.loads(content), runs, root, seals)
        applied.update(receipt_sha256=hashlib.sha256(content).hexdigest())
        corrections.append(applied)

    grouped, scheduled = defaultdict(list), defaultdict(set)
    for version, cells in schedules.items():
        for cell in cells:
            scheduled[_key(dict(cell, study_id=version[0], config_sha256=version[1]))].add(cell["schedule_index"])
    for run in runs:
        grouped[_key(run["manifest"])].append(run)
    groups = []
    for key in sorted(grouped.keys() | scheduled.keys(), key=lambda value: json.dumps(value)):
        members = grouped[key]
        has_schedule = key[:2] in schedules
        expected = scheduled[key] if has_schedule else None
        covered = {run["manifest"].get("schedule_index") for run in members
                   if run["integrity"] == "sealed" and expected is not None
                   and run["manifest"].get("schedule_index") in expected}
        episodes = [episode for run in members for episode in (run["interpreted_outcome"] or {}).get("episodes", [])]
        attempted = [episode for episode in episodes if episode["status"] != "unattempted"]
        counts = dict(Counter(run["status"] for run in members))
        groups.append({"configuration": dict(zip(FIELDS, key)), "attempt_count": len(members),
                       "run_ids": [run["run_id"] for run in members], "outcomes": counts,
                       "all_attempt_success_fraction": counts.get("success", 0) / len(members) if members else None,
                       "scheduled_cell_count": len(expected) if expected is not None else None,
                       "covered_scheduled_cell_count": len(covered) if expected is not None else None,
                       "uncovered_schedule_indexes": sorted(expected - covered) if expected is not None else None,
                       "attempts_outside_verified_schedule": [run["run_id"] for run in members
                           if run["integrity"] != "sealed" or expected is None
                           or run["manifest"].get("schedule_index") not in expected],
                       "episode_statuses": dict(Counter(episode["status"] for episode in episodes)),
                       "attempted_episode_count": len(attempted),
                       "unattempted_episode_count": len(episodes) - len(attempted),
                       "attempts_without_episode_outcomes": sum(run["interpreted_outcome"] is None for run in members),
                       "resources_all_recorded_episodes": _resources(episodes),
                       "resources_attempted_episodes": _resources(attempted)})
    return {"schema_version": 1, "attempt_count": len(runs), "integrity_counts": inventory["integrity_counts"],
            "groups": groups, "runs": runs, "corrections": corrections, "warnings": inventory["warnings"],
            "interpretation": [
                "Groups separate study and configuration identities; replacements remain distinct attempts.",
                "Schedule coverage counts distinct verified cells with any attempt, not successful cells.",
                "Missing resource fields remain unavailable; observed sums are not complete resource totals.",
                "Original outcomes are retained. Only explicit validated receipts alter interpreted episode counts.",
                "Semantic judgments are reproduced without new grading or aggregation; reviewed need not mean complete.",
                "No inferred tool-failure, malformed-action or capture-omission rates are computed."]}


def write_report(store_root, output, *, correction=None):
    result = analyze(store_root, correction=correction)
    output = Path(output)
    root = Path(store_root).resolve()
    if output.resolve().is_relative_to(root):
        raise ValueError("analysis output must be outside the evidence store")
    output.mkdir(parents=True, exist_ok=False)
    sources = {"sources/bench/harness_study_analysis.py": Path(__file__),
               "sources/bench/harness_study/grading.py": Path(grading.__file__),
               "sources/bench/harness_study/artifacts.py": Path(artifacts.__file__)}
    provenance = {"command": "python3 -m bench.harness_study_analysis", "source_files": {},
                  "store": str(root), "replay_working_directory": "sources",
                  "replay_arguments": ["--store", str(root), "--output", "NEW_OUTPUT_DIRECTORY"]}
    for name, path in sources.items():
        content = path.read_bytes()
        (output / name).parent.mkdir(parents=True, exist_ok=True)
        with (output / name).open("xb") as stream:
            stream.write(content)
        provenance["source_files"][name] = {"original_path": str(path.resolve()),
                                            "sha256": hashlib.sha256(content).hexdigest()}
    if correction is not None:
        content = Path(correction).read_bytes()
        if hashlib.sha256(content).hexdigest() != result["corrections"][0]["receipt_sha256"]:
            raise ValueError("correction receipt changed during reporting")
        with (output / "correction-receipt.json").open("xb") as stream:
            stream.write(content)
        provenance["correction_source_path"] = str(Path(correction).resolve())
        provenance["replay_arguments"] += ["--correction", "../correction-receipt.json"]
    with (output / "comparison.json").open("xb") as stream:
        stream.write(artifacts.canonical_json(result))
    provenance["comparison_sha256"] = artifacts.sha256_file(output / "comparison.json")
    with (output / "provenance.json").open("xb") as stream:
        stream.write(artifacts.canonical_json(provenance))
    return output


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--store", type=Path, default=Path("target/harness-study/evidence"))
    parser.add_argument("--output", type=Path, required=True, help="new exclusive output directory")
    parser.add_argument("--correction", type=Path)
    args = parser.parse_args(argv)
    print(write_report(args.store, args.output, correction=args.correction))


if __name__ == "__main__":
    main()
