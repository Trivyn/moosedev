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


    def build_run(self, build_id, episodes):
        manifest = dict(self.manifest)
        if build_id is not None:
            manifest["build_id"] = build_id
        run = self.store.create_run(manifest)
        attempted = [episode["status"] for episode in episodes if episode["status"] != "unattempted"]
        status = "success" if attempted and all(value == "success" for value in attempted) else "agent_failure"
        self.store.put_bytes(run, "outcome.json", canonical_json({"status": status, "episodes": episodes}))
        self.store.seal_run(run)
        return run

    def test_report_refuses_to_pool_runs_from_multiple_builds(self):
        episode = {"id": "first", "status": "agent_failure", "checks": [], "metrics": {}}
        self.build_run("build-a", [episode])
        self.build_run("build-b", [episode])
        with self.assertRaisesRegex(ValueError, "multiple build_ids: build-a, build-b"):
            report(self.store.root)

    def test_missing_build_id_counts_as_a_distinct_build(self):
        episode = {"id": "first", "status": "agent_failure", "checks": [], "metrics": {}}
        self.build_run("build-a", [episode])
        self.build_run(None, [episode])
        with self.assertRaisesRegex(ValueError, "multiple build_ids"):
            report(self.store.root)

    def test_single_build_reports_typed_terminal_causes_with_unknown_for_legacy_outcomes(self):
        self.build_run("build-a", [
            {"id": "first", "status": "success", "checks": [{"passed": True}], "metrics": {},
             "terminal_cause": "success", "first_edit_reached": True, "first_edit_seconds": 2.5},
            {"id": "second", "status": "agent_failure", "checks": [], "metrics": {},
             "terminal_cause": "reviewer_idle_deadline", "first_edit_reached": False, "first_edit_seconds": None},
            {"id": "third", "status": "unattempted", "reason": "episode_limit", "checks": [], "metrics": {}}])
        self.build_run("build-a", [{"id": "first", "status": "agent_failure", "checks": [], "metrics": {}}])
        result = report(self.store.root)
        group = result["groups"][0]
        self.assertEqual(group["configuration"]["build_id"], "build-a")
        self.assertEqual(group["episode_terminals"], {
            "terminal_causes": {"success": 1, "reviewer_idle_deadline": 1, "unknown": 1},
            "first_edit_reached": 1, "first_edit_seconds": [2.5]})
        self.assertEqual(group["episode_statuses"], {"success": 1, "agent_failure": 2, "unattempted": 1})


class BaselineReviewTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.store = ArtifactStore(Path(self.temporary.name).resolve() / "study")

    def sealed_run(self, backend, condition, intent_policy):
        from bench.harness_study import evolution
        run = self.store.create_run({"model": "local", "backend": backend, "condition": condition,
                                     "intent_policy": intent_policy, "scenario_id": "ruleset_cache",
                                     "build_id": "build-a", "evaluation_mode": evolution.STAGE2_BASELINE_MODE,
                                     "scenario_gold_sha256": "a" * 64})
        self.store.put_bytes(run, "evidence.txt", b"sealed evidence\n")
        self.store.put_bytes(run, "outcome.json", canonical_json({"status": "success", "episodes": [
            {"id": "e1", "status": "success", "checks": [{"passed": True}], "metrics": {}}]}))
        self.store.seal_run(run)
        return run

    def review(self, episode):
        span = {"path": "evidence.txt", "start_line": 1, "end_line": 1}
        return {"reviewer_id": "independent", "scenario_gold_sha256": "a" * 64,
                "claims": [{"claim_id": "c", "verdict": "supported", "evidence": [span]}],
                "evolution_assessment": {"episodes": [dict(episode, episode_id="e1", rationale="Inspected.",
                                                           evidence=[span])]}}

    def test_native_review_accepts_null_reconciliation_and_requires_empty_reuse_list(self):
        run = self.sealed_run("opencode", "without", None)
        base = {"task_requirements_complete": True, "tests_complete": True,
                "capture_reconciliation_correct": None, "associations_relevant": None, "reuse_assessments": []}
        record_review(self.store.root, run.name, self.review(base))
        for bad, message in ((dict(base, reuse_assessments=[{"operation_id": "op"}]), "must be empty"),
                             (dict(base, capture_reconciliation_correct=False, reuse_assessments=[{"operation_id": "op"}]), "must be empty"),
                             (dict(base, capture_reconciliation_correct="n/a"), "capture_reconciliation_correct or null"),
                             (dict(base, tests_complete=None), "Boolean tests_complete")):
            with self.subTest(message=message), self.assertRaisesRegex(ValueError, message):
                record_review(self.store.root, run.name, self.review(bad))
        result = report(self.store.root)
        constituents = result["runs"][0]["evolution_constituents"]["e1"]
        self.assertTrue(constituents["task_requirements_complete"])
        self.assertIsNone(constituents["capture_reconciliation_correct"])
        self.assertNotIn("intent_primary", result["runs"][0])

    def test_harness_review_still_requires_boolean_reconciliation(self):
        run = self.sealed_run("harness", "harness", "current")
        base = {"task_requirements_complete": True, "tests_complete": True,
                "capture_reconciliation_correct": True, "associations_relevant": None, "reuse_assessments": []}
        record_review(self.store.root, run.name, self.review(base))
        with self.assertRaisesRegex(ValueError, "Boolean capture_reconciliation_correct"):
            record_review(self.store.root, run.name, self.review(dict(base, capture_reconciliation_correct=None)))

    def test_none_intent_policy_groups_native_cells_apart_from_harness_arms(self):
        self.sealed_run("opencode", "without", None)
        self.sealed_run("opencode", "without", None)
        self.sealed_run("harness", "harness", "current")
        self.sealed_run("harness", "harness", "change-level-v2")
        groups = report(self.store.root)["groups"]
        configurations = [(g["configuration"]["backend"], g["configuration"]["intent_policy"], g["attempts"])
                          for g in groups]
        self.assertEqual(sorted(configurations, key=str), sorted(
            [("harness", "change-level-v2", 1), ("harness", "current", 1), ("opencode", None, 2)], key=str))


if __name__ == "__main__":
    unittest.main()


class FieldCheckGradingTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.store = ArtifactStore(Path(self.temporary.name).resolve() / "study")

    def sealed_run(self, mode, study_id):
        run = self.store.create_run({"model": "local", "backend": "harness", "condition": "harness",
                                     "intent_policy": "symbolic", "scenario_id": "retry_ledger",
                                     "build_id": "build-a", "evaluation_mode": mode, "study_id": study_id,
                                     "scenario_gold_sha256": "a" * 64})
        self.store.put_bytes(run, "evidence.txt", b"sealed evidence\n")
        self.store.put_bytes(run, "outcome.json", canonical_json({"status": "success", "episodes": [
            {"id": "e1", "status": "success", "checks": [{"passed": True}], "metrics": {}}]}))
        self.store.seal_run(run)
        return run

    def test_field_check_runs_are_never_reviewed(self):
        from bench.harness_study import field_check
        run = self.sealed_run(field_check.MODE, "field-check-g31b")
        review = {"reviewer_id": "independent", "scenario_gold_sha256": "a" * 64,
                  "claims": [{"claim_id": "c", "verdict": "supported",
                              "evidence": [{"path": "evidence.txt", "start_line": 1, "end_line": 1}]}]}
        with self.assertRaisesRegex(ValueError, "never reviewed"):
            record_review(self.store.root, run.name, review)

    def test_report_never_pools_field_check_runs_with_another_study(self):
        from bench.harness_study import evolution, field_check
        self.sealed_run(field_check.MODE, "field-check-g31b")
        self.sealed_run(field_check.MODE, "field-check-g31b")
        self.assertEqual(report(self.store.root)["attempt_count"], 2)
        self.sealed_run(evolution.SYMBOLIC_BASELINE_MODE, "symbolic-v3")
        with self.assertRaisesRegex(ValueError, "never pooled"):
            report(self.store.root)


class EpisodeLimitOutcomeTests(unittest.TestCase):
    def test_success_with_episode_limit_padding_is_valid(self):
        from bench.harness_study.grading import _outcome
        value = {"status": "success", "episodes": [
            {"id": "e1", "status": "success", "checks": [], "metrics": {}},
            {"id": "e2", "status": "unattempted", "reason": "episode_limit", "checks": [], "metrics": {}}]}
        self.assertEqual(_outcome(value)["status"], "success")

    def test_success_with_dependent_unattempted_is_still_invalid(self):
        from bench.harness_study.grading import _outcome
        value = {"status": "success", "episodes": [
            {"id": "e1", "status": "success", "checks": [], "metrics": {}},
            {"id": "e2", "status": "unattempted", "checks": [], "metrics": {}}]}
        with self.assertRaises(ValueError):
            _outcome(value)
