import copy
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

from bench.harness_study_tables import encoded, select_matrix, write_tables


class TableTests(unittest.TestCase):
    def setUp(self):
        self.schedule = [{"schedule_index": index, "model": "model", "backend": "harness", "condition": "harness",
                          "scenario_id": "cache" if index % 2 else "retry"} for index in range(16)]
        self.preflights = {}
        for version in ("v3", "v4"):
            config = {"study_id": version}
            self.preflights[version] = {"ready": True, "config": config,
                "config_sha256": hashlib.sha256(encoded(config)).hexdigest(), "schedule": self.schedule}
        self.policy = {"study_id": "v4", "predecessor": "v3", "retained_valid_v3_cells": [
            {"cell": index, "run_id": f"run-{index}", "status": "success"} for index in range(3)]}
        self.comparison = {"runs": [self.make_run(index) for index in range(16)]}

    def make_run(self, index, *, name=None, version=None, status="success"):
        version = version or ("v3" if index < 3 else "v4")
        return {"run_id": name or f"run-{index}", "integrity": "sealed", "status": status,
                "manifest": {**self.schedule[index], "study_id": version,
                             "config_sha256": self.preflights[version]["config_sha256"]},
                "interpreted_outcome": {"status": status, "episodes": [
                    {"id": "e1", "status": status, "metrics": {"elapsed_seconds": None},
                     "checks": [{"passed": True, "tests_run": 3, "status": "success"}]}]},
                "semantic": {"judgments": [], "invalid": []}}

    def select(self):
        return select_matrix(self.comparison, self.policy, self.preflights["v3"], self.preflights["v4"])

    def test_exact_retention_and_continuation_select_once_with_honest_nulls(self):
        self.comparison["runs"].append(self.make_run(3, name="infrastructure-attempt", status="infrastructure_failure"))
        result = self.select()
        self.assertTrue(result["complete"])
        self.assertEqual(result["selected_cell_count"], 16)
        self.assertEqual(result["all_attempt_count"], 17)
        self.assertEqual(result["all_attempt_outcomes"], {"success": 16, "infrastructure_failure": 1})
        self.assertEqual(result["cells"][0]["result"]["run_id"], "run-0")
        first = result["cells"][0]["result"]
        self.assertIsNone(first["elapsed_observed_seconds"])
        self.assertEqual(first["elapsed_observed_episodes"], 0)
        self.assertEqual(first["semantic"]["status"], "pending")
        self.assertEqual(first["hidden_suites_passed"], 1)
        self.assertEqual(first["tests_in_passing_suites"], 3)

    def test_missing_or_unsealed_cell_is_pending(self):
        self.comparison["runs"] = self.comparison["runs"][:-1]
        result = self.select()
        self.assertFalse(result["complete"])
        self.assertEqual(result["cells"][15]["selection"], "pending")
        self.comparison["runs"].append(self.make_run(15))
        self.comparison["runs"][-1]["integrity"] = "unsealed"
        self.assertEqual(self.select()["cells"][15]["selection"], "pending")

    def test_additional_scored_attempt_creates_conflict_instead_of_choosing_best(self):
        self.comparison["runs"].append(self.make_run(3, name="other-outcome", status="agent_failure"))
        result = self.select()
        self.assertEqual(result["cells"][3]["selection"], "conflict")
        self.assertIsNone(result["cells"][3]["result"])
        self.assertEqual(result["all_attempt_count"], 17)
        self.comparison["runs"].pop()
        self.comparison["runs"].append(self.make_run(0, name="unapproved-repeat", version="v4"))
        result = self.select()
        self.assertEqual(result["cells"][0]["selection"], "conflict")
        self.assertEqual(result["cells"][0]["unexpected_scored_attempt_ids"], ["unapproved-repeat"])

    def test_wrong_config_or_setup_cannot_be_selected(self):
        for key, value in (("config_sha256", "0" * 64), ("model", "substitute")):
            with self.subTest(key=key):
                original = self.comparison["runs"][5]["manifest"][key]
                self.comparison["runs"][5]["manifest"][key] = value
                result = self.select()
                self.assertEqual(result["identity_errors"], ["run-5"])
                self.assertEqual(result["cells"][5]["selection"], "pending")
                self.comparison["runs"][5]["manifest"][key] = original
        altered = copy.deepcopy(self.preflights["v4"])
        altered["schedule"][0]["scenario_id"] = "other"
        with self.assertRaisesRegex(ValueError, "unchanged"):
            select_matrix(self.comparison, self.policy, self.preflights["v3"], altered)

    def test_active_semantic_reviews_remain_separate_and_omit_unattempted_facts(self):
        run = self.comparison["runs"][0]
        review = {"review_id": "a", "reviewer_id": "reviewer-a", "active": True,
                  "knowledge_metrics": {"e1": {"supported_fact_count": 2, "expected_fact_count": 3,
                    "addressed_fact_count": 3, "complete": True}, "e2": {"supported_fact_count": 100}},
                  "claims": [{"episode_id": "e1", "verdict": "unsupported"},
                             {"episode_id": "e2", "verdict": "stale"}]}
        second = copy.deepcopy(review)
        second.update(review_id="b", reviewer_id="reviewer-b")
        second["knowledge_metrics"]["e1"]["supported_fact_count"] = 1
        run["semantic"]["judgments"] = [review, second]
        summaries = self.select()["cells"][0]["result"]["semantic"]["active_reviews"]
        self.assertEqual([item["supported"] for item in summaries], [2, 1])
        self.assertEqual(summaries[0]["expected"], 3)
        self.assertEqual(summaries[0]["unsupported"], 1)
        self.assertEqual(summaries[0]["stale"], 0)
        self.assertEqual(summaries[0]["unmapped_or_unattempted_claims"], 1)

    def test_exclusive_bundle_records_hashes_and_replays_standalone(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory).resolve()
            inputs = {"comparison": self.comparison, "policy": self.policy,
                      "v3": self.preflights["v3"], "v4": self.preflights["v4"]}
            for name, value in inputs.items():
                (root / f"{name}.json").write_bytes(encoded(value))
            paths = [root / f"{name}.json" for name in inputs]
            output = root / "tables"
            write_tables(*paths, output)
            provenance = json.loads((output / "provenance.json").read_bytes())
            for name, digest in provenance["files_sha256"].items():
                self.assertEqual(hashlib.sha256((output / name).read_bytes()).hexdigest(), digest)
            subprocess.run([sys.executable, "make_pilot_tables.py", "--comparison", "comparison.json", "--policy",
                "retention-policy.json", "--v3", "preflight-v3.json", "--v4", "preflight-v4.json", "--output", "../replay"],
                cwd=output, check=True, capture_output=True)
            self.assertEqual((output / "selected-matrix.json").read_bytes(), (root / "replay/selected-matrix.json").read_bytes())
            with self.assertRaises(FileExistsError):
                write_tables(*paths, output)


if __name__ == "__main__":
    unittest.main()
