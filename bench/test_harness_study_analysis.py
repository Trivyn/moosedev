import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study.artifacts import ArtifactStore, canonical_json
from bench.harness_study_analysis import analyze, write_report


class AnalysisTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.store = ArtifactStore(self.root / "evidence")

    def make_run(self, *, study="v1", setting=0, status="success", metrics=None,
                 replacement_for=None, interrupted=False):
        config = {"study_id": study, "setting": setting}
        digest = hashlib.sha256(canonical_json(config)).hexdigest()
        cell = {"model": "local", "backend": "harness", "condition": "with", "scenario_id": "cache", "schedule_index": 0}
        run = self.store.create_run({**cell, "study_id": study, "config_sha256": digest,
                                     "replacement_for": replacement_for})
        self.store.put_bytes(run, "preflight.json", canonical_json({"config": config, "config_sha256": digest,
            "schedule": [cell, dict(cell, schedule_index=1, scenario_id="retry")]}))
        episodes = [{"id": f"e{number}", "status": "unattempted" if interrupted or number > 1 and status != "success" else status,
                     "metrics": metrics if number == 1 and not interrupted else {}, "checks": []} for number in (1, 2, 3)]
        outcome = {"status": status, "episodes": episodes}
        if interrupted:
            outcome["error"] = "KeyboardInterrupt: "
            self.store.append_event(run, "model", {"episode": "e1", "event": "request"})
            self.store.put_bytes(run, "interrupted-workspace/service.py", b"# retained edits\n")
        self.store.put_bytes(run, "outcome.json", canonical_json(outcome))
        self.store.seal_run(run)
        return run

    def correction(self, run):
        receipt = {"study_id": "v3", "predecessor": "v2", "replaced_attempt": run.name,
                   "classification_correction": {"run_id": run.name, "episode": "e1",
                       "recorded_status": "unattempted", "sealed_outcome_unchanged": True,
                       "correct_interpretation": "attempted, interrupted infrastructure failure; no behavioral grade",
                       "unattempted_episodes": ["e2", "e3"]}}
        path = self.root / "correction.json"
        path.write_bytes(canonical_json(receipt))
        return path

    def test_versions_and_replacements_are_separate_from_scheduled_cells(self):
        first = self.make_run(status="infrastructure_failure")
        self.make_run(replacement_for=first.name)
        self.make_run(study="v2")
        self.make_run(study="v2", setting=1)
        with patch("socket.socket", side_effect=AssertionError("network prohibited")), \
                patch("subprocess.Popen", side_effect=AssertionError("execution prohibited")):
            result = analyze(self.store.root)
        self.assertEqual(result["attempt_count"], 4)
        self.assertEqual(len(result["groups"]), 6)  # Two scheduled scenarios in each of three versions.
        cache = [group for group in result["groups"] if group["configuration"]["scenario_id"] == "cache"]
        self.assertEqual(sorted(group["attempt_count"] for group in cache), [1, 1, 2])
        retried = next(group for group in cache if group["attempt_count"] == 2)
        self.assertEqual(retried["scheduled_cell_count"], 1)
        self.assertEqual(retried["covered_scheduled_cell_count"], 1)
        self.assertEqual(retried["all_attempt_success_fraction"], 0.5)
        retry = [group for group in result["groups"] if group["configuration"]["scenario_id"] == "retry"]
        self.assertTrue(all(group["uncovered_schedule_indexes"] == [1] for group in retry))

    def test_unavailable_resources_are_never_zero(self):
        self.make_run(status="agent_failure", metrics={"elapsed_seconds": 12, "input_tokens": None})
        result = analyze(self.store.root)
        group = next(group for group in result["groups"] if group["attempt_count"])
        self.assertEqual(group["attempted_episode_count"], 1)
        self.assertEqual(group["unattempted_episode_count"], 2)
        all_metrics = group["resources_all_recorded_episodes"]
        self.assertEqual(all_metrics["elapsed_seconds"], {"observed_episodes": 1, "unavailable_episodes": 2,
                                                          "observed_sum": 12, "observed_mean": 12})
        self.assertEqual(all_metrics["helper_tokens"]["unavailable_episodes"], 3)
        self.assertIsNone(all_metrics["helper_tokens"]["observed_sum"])
        self.assertIsNone(group["resources_attempted_episodes"]["input_tokens"]["observed_mean"])

    def test_correction_is_explicit_bound_to_evidence_and_preserves_original(self):
        run = self.make_run(study="v2", status="infrastructure_failure", interrupted=True)
        before = (run / "outcome.json").read_bytes()
        receipt = self.correction(run)
        original = analyze(self.store.root)
        corrected = analyze(self.store.root, correction=receipt)
        self.assertFalse(original["runs"][0]["correction_applied"])
        self.assertTrue(corrected["runs"][0]["correction_applied"])
        result = corrected["runs"][0]
        self.assertEqual(result["recorded_outcome"]["episodes"][0]["status"], "unattempted")
        self.assertEqual(result["interpreted_outcome"]["episodes"][0]["status"], "infrastructure_failure")
        self.assertEqual((run / "outcome.json").read_bytes(), before)
        self.store.verify_run(run)
        self.assertEqual(len(corrected["corrections"][0]["target_evidence_sha256"]), 64)
        receipt_data = json.loads(receipt.read_bytes())
        receipt_data["classification_correction"]["recorded_status"] = "success"
        receipt.write_bytes(canonical_json(receipt_data))
        with self.assertRaisesRegex(ValueError, "classification"):
            analyze(self.store.root, correction=receipt)

    def test_correction_refuses_wrong_target_or_missing_activity(self):
        run = self.make_run(study="v2", status="infrastructure_failure")
        receipt = self.correction(run)
        with self.assertRaises(ValueError):
            analyze(self.store.root, correction=receipt)
        value = json.loads(receipt.read_bytes())
        value["classification_correction"]["run_id"] = "absent"
        receipt.write_bytes(canonical_json(value))
        with self.assertRaisesRegex(ValueError, "target"):
            analyze(self.store.root, correction=receipt)

    def test_output_archives_sources_and_refuses_overwrite(self):
        self.make_run()
        output = self.root / "analysis-v1"
        write_report(self.store.root, output)
        provenance = json.loads((output / "provenance.json").read_bytes())
        self.assertEqual(provenance["comparison_sha256"], hashlib.sha256((output / "comparison.json").read_bytes()).hexdigest())
        for name, identity in provenance["source_files"].items():
            self.assertEqual(identity["sha256"], hashlib.sha256((output / name).read_bytes()).hexdigest())
        replay = self.root / "replayed"
        subprocess.run([sys.executable, "-m", "bench.harness_study_analysis", "--store", str(self.store.root),
                        "--output", str(replay)], cwd=output / "sources", check=True, capture_output=True)
        self.assertEqual((replay / "comparison.json").read_bytes(), (output / "comparison.json").read_bytes())
        with self.assertRaises(FileExistsError):
            write_report(self.store.root, output)
        with self.assertRaisesRegex(ValueError, "outside"):
            write_report(self.store.root, self.store.root / "analysis")

    def test_corrected_report_replays_identically_from_archived_receipt(self):
        run = self.make_run(study="v2", status="infrastructure_failure", interrupted=True)
        receipt = self.correction(run)
        output = self.root / "corrected"
        write_report(self.store.root, output, correction=receipt)
        provenance = json.loads((output / "provenance.json").read_bytes())
        self.assertEqual(provenance["correction_source_path"], str(receipt))
        result = json.loads((output / "comparison.json").read_bytes())
        self.assertNotIn("receipt_path", result["corrections"][0])
        replay = self.root / "corrected-replay"
        subprocess.run([sys.executable, "-m", "bench.harness_study_analysis", "--store", str(self.store.root),
                        "--correction", "../correction-receipt.json", "--output", str(replay)],
                       cwd=output / "sources", check=True, capture_output=True)
        self.assertEqual((replay / "comparison.json").read_bytes(), (output / "comparison.json").read_bytes())


if __name__ == "__main__":
    unittest.main()
