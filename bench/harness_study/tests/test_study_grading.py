import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study.artifacts import ArtifactStore, canonical_json
from bench.harness_study.grading import record_review, report


class GradingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.store = ArtifactStore(Path(self.temporary.name).resolve() / "study")
        self.manifest = {"model": "local", "backend": "harness", "condition": "with_memory",
                         "scenario_id": "cache", "scenario_gold_sha256": "a" * 64}

    def make_run(self, status="success", metrics=None, sealed=True, scenario=None):
        run = self.store.create_run(self.manifest)
        self.store.put_bytes(run, "notes.txt", b"Cache identity includes the selected rules.\nRetries preserve identity.\n")
        if scenario is not None:
            self.store.put_bytes(run, "scenario.json", canonical_json(scenario))
        self.store.put_bytes(run, "outcome.json", canonical_json({
            "status": status,
            "episodes": [{"id": "first", "status": status, "checks": {"constraint": True}, "metrics": metrics}],
        }))
        if sealed:
            self.store.seal_run(run)
        return run

    def review(self, verdict="supported", claim_id="cache-key"):
        return {"reviewer_id": "blinded-reviewer-1", "scenario_gold_sha256": "a" * 64,
                "claims": [{"claim_id": claim_id, "verdict": verdict,
                            "evidence": [{"path": "notes.txt", "start_line": 1, "end_line": 1}],
                            "rationale": "Equivalent meaning despite different wording."}]}

    def test_all_attempts_remain_in_denominator_and_missing_is_not_zero(self):
        self.make_run(metrics={"seconds": 12, "tokens": None})
        self.make_run("agent_failure", metrics={"seconds": None})
        self.make_run("infrastructure_failure")
        self.make_run("preflight_failure")
        self.make_run(sealed=False)
        damaged = self.make_run()
        (damaged / "notes.txt").write_text("tampered")
        result = report(self.store.root)
        self.assertEqual(result["attempt_count"], 6)
        group = result["groups"][0]
        self.assertEqual(group["outcomes"], {"success": 1, "agent_failure": 1,
                          "infrastructure_failure": 1, "preflight_failure": 1,
                          "unfinished": 1, "invalid": 1})
        self.assertAlmostEqual(group["success_fraction"], 1 / 6)
        self.assertEqual(group["episode_metrics"]["seconds"], {"observed": 1, "missing": 3, "sum": 12, "mean": 12})
        self.assertEqual(group["episode_metrics"]["tokens"], {"observed": 0, "missing": 4, "sum": None, "mean": None})
        self.assertTrue(all(run["semantic"]["status"] == "pending" for run in result["runs"]))

    def test_offline_report_credits_reviewed_paraphrases_and_retains_corrections(self):
        run = self.make_run()
        first = record_review(self.store.root, run.name, self.review())
        first_bytes = first.read_bytes()
        review = self.review("unsupported")
        review["supersedes"] = json.loads(first_bytes)["review_id"]
        second = record_review(self.store.root, run.name, review)
        self.assertEqual(first.read_bytes(), first_bytes)
        self.assertNotEqual(first, second)
        with patch("socket.socket", side_effect=AssertionError("network prohibited")), \
                patch("subprocess.Popen", side_effect=AssertionError("execution prohibited")):
            result = report(self.store.root)
            self.assertEqual(result, report(self.store.root))
        semantic = result["runs"][0]["semantic"]
        self.assertEqual(semantic["status"], "reviewed")
        self.assertEqual(len(semantic["judgments"]), 2)
        active = [item for item in semantic["judgments"] if item["active"]]
        self.assertEqual(len(active), 1)
        self.assertEqual(active[0]["counts"]["unsupported"], 1)
        self.assertIsNone(active[0]["knowledge_metrics"])
        self.assertEqual(next(item for item in semantic["judgments"] if not item["active"])["counts"]["supported"], 1)

    def test_separate_omissions_inventions_duplicates_and_stale_claims(self):
        run = self.make_run()
        review = self.review()
        review["claims"] = [self.review(verdict, verdict)["claims"][0]
                            for verdict in ("supported", "missing", "unsupported", "duplicate", "stale")]
        record_review(self.store.root, run.name, review)
        counts = report(self.store.root)["runs"][0]["semantic"]["judgments"][0]["counts"]
        self.assertEqual(counts, {"supported": 1, "missing": 1, "unsupported": 1, "duplicate": 1, "stale": 1})

    def test_review_rejects_wrong_gold_evidence_unsealed_run_and_invalid_spans(self):
        run = self.make_run()
        for mutation in (
            lambda r: r.update(scenario_gold_sha256="b" * 64),
            lambda r: r.update(evidence_sha256="b" * 64),
            lambda r: r["claims"][0].update(evidence=[]),
            lambda r: r["claims"][0]["evidence"][0].update(path="../gold.json"),
            lambda r: r["claims"][0]["evidence"][0].update(start_line=0),
            lambda r: r["claims"][0]["evidence"][0].update(end_line=999),
        ):
            review = self.review()
            mutation(review)
            with self.assertRaises(ValueError):
                record_review(self.store.root, run.name, review)
        unsealed = self.make_run(sealed=False)
        with self.assertRaises(FileNotFoundError):
            record_review(self.store.root, unsealed.name, self.review())

    def test_tampered_judgment_is_not_counted(self):
        run = self.make_run()
        path = record_review(self.store.root, run.name, self.review())
        value = json.loads(path.read_bytes())
        value["claims"][0]["verdict"] = "unsupported"
        path.write_bytes(canonical_json(value))
        semantic = report(self.store.root)["runs"][0]["semantic"]
        self.assertEqual(semantic["status"], "pending")
        self.assertEqual(len(semantic["invalid"]), 1)

    def test_malformed_success_cannot_earn_credit(self):
        run = self.store.create_run(self.manifest)
        self.store.put_bytes(run, "outcome.json", canonical_json({"status": "success", "episodes": []}))
        self.store.seal_run(run)
        result = report(self.store.root)
        self.assertEqual(result["runs"][0]["status"], "invalid")
        self.assertEqual(result["groups"][0]["success_fraction"], 0)

    def semantic_fixture(self):
        return {"episodes": [{"id": "first", "expected_fact_ids": ["seeded", "learned"]},
                             {"id": "later", "expected_fact_ids": ["learned"]}],
                "initial_facts": [{"id": "seeded"}]}

    def claim(self, claim_id, verdict, fact_id=None, episode_id="first"):
        claim = self.review(verdict, claim_id)["claims"][0]
        claim["episode_id"] = episode_id
        if fact_id is not None:
            claim["fact_id"] = fact_id
        return claim

    def knowledge_metrics(self, run, claims):
        review = self.review()
        review["claims"] = claims
        record_review(self.store.root, run.name, review)
        return report(self.store.root)["runs"][0]["semantic"]["judgments"][0]["knowledge_metrics"]

    def test_complete_review_separates_inherited_and_new_recall_and_claim_precision(self):
        run = self.make_run(scenario=self.semantic_fixture())
        metrics = self.knowledge_metrics(run, [
            self.claim("paraphrased-seed", "supported", "seeded"),
            self.claim("omitted-new", "missing", "learned"),
            self.claim("invented-rationale", "unsupported"),
            self.claim("duplicate-seed", "duplicate", "seeded"),
            self.claim("obsolete-assertion", "stale", "seeded"),
        ])
        first = metrics["first"]
        self.assertTrue(first["complete"])
        self.assertEqual(first["expected_fact_coverage"], 1)
        self.assertEqual(first["recall"], 0.5)
        self.assertEqual(first["inherited_recall"], 1)
        self.assertEqual(first["new_recall"], 0)
        self.assertAlmostEqual(first["precision"], 1 / 3)
        self.assertEqual(first["duplicate_count"], 1)
        self.assertEqual(first["stale_count"], 1)
        self.assertIsNone(metrics["later"]["recall"])

    def test_partial_review_leaves_unassessed_knowledge_unknown(self):
        run = self.make_run(scenario=self.semantic_fixture())
        metrics = self.knowledge_metrics(run, [self.claim("seed", "supported", "seeded")])["first"]
        self.assertFalse(metrics["complete"])
        self.assertEqual(metrics["expected_fact_coverage"], 0.5)
        self.assertIsNone(metrics["recall"])
        self.assertIsNone(metrics["precision"])
        self.assertIsNone(metrics["new_recall"])
        self.assertEqual(metrics["inherited_recall"], 1)

    def test_free_claims_cannot_earn_fact_recall_and_repeated_fact_is_counted_once(self):
        run = self.make_run(scenario=self.semantic_fixture())
        metrics = self.knowledge_metrics(run, [
            self.claim("free-prose", "supported"), self.claim("seed-one", "supported", "seeded"),
            self.claim("seed-two", "supported", "seeded"),
        ])["first"]
        self.assertEqual(metrics["supported_fact_count"], 1)
        self.assertEqual(metrics["addressed_fact_count"], 1)
        self.assertEqual(metrics["asserted_claim_count"], 3)
        self.assertIsNone(metrics["recall"])

    def test_unknown_ids_and_contradictory_fact_assessments_are_rejected(self):
        run = self.make_run(scenario=self.semantic_fixture())
        for claims in (
            [self.claim("invented-fact", "supported", "nonexistent")],
            [self.claim("invented-episode", "supported", "seeded", "nonexistent")],
            [self.claim("present", "supported", "seeded"), self.claim("absent", "missing", "seeded")],
        ):
            review = self.review()
            review["claims"] = claims
            with self.assertRaises(ValueError):
                record_review(self.store.root, run.name, review)


if __name__ == "__main__":
    unittest.main()
