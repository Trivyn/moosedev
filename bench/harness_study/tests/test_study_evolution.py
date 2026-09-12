from collections import Counter
import copy
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from bench.harness_study import config, evolution, intent
from bench.harness_study.artifacts import ArtifactStore, canonical_json
from bench.harness_study.grading import record_review, report
from bench.harness_study.reviewer import review_input
from bench.harness_study.run import _only_deadline_downstream_disconnects, _proxy_failure_evidence


def specification(stage):
    return {"evaluation_mode": stage, "scenario_ids": list(intent.SCENARIOS),
            "local_models": [{"id": model} for model in intent.MODELS],
            "intent_policies": list(evolution.POLICIES[stage]), "seed": 20260910}


class EvolutionDesignTests(unittest.TestCase):
    def test_stage_schedules_are_exact_distinct_and_deterministic(self):
        for stage, size in ((evolution.STAGE1_MODE, 6), (evolution.STAGE2_MODE, 12)):
            cells = config.schedule(specification(stage))
            self.assertEqual(len(cells), size)
            self.assertEqual(len({(c["model"], c["intent_policy"], c["scenario_id"]) for c in cells}), size)
            self.assertEqual(cells, config.schedule(specification(stage)))
            self.assertEqual(config.required_clients(specification(stage)), ("lms",))
        stage2 = config.schedule(specification(evolution.STAGE2_MODE))
        for model in intent.MODELS:
            for scenario in intent.SCENARIOS:
                pair = [cell for cell in stage2 if cell["model"] == model and cell["scenario_id"] == scenario]
                self.assertEqual({cell["intent_policy"] for cell in pair}, {"current", "change-level-v2"})

    def test_schedule_rejects_substitution_reordering_and_policy_bypass(self):
        base = specification(evolution.STAGE2_MODE)
        mutations = [
            {"scenario_ids": list(reversed(intent.SCENARIOS))},
            {"local_models": list(reversed(base["local_models"]))},
            {"intent_policies": ["current"]},
            {"intent_policies": ["change-level-v2", "current"]},
        ]
        for update in mutations:
            with self.subTest(update=update), self.assertRaises(ValueError):
                config.schedule(dict(base, **update))

    def test_new_design_identities_bind_governing_records_and_preserve_v1(self):
        historical = copy.deepcopy(intent.design_identity())
        stage1 = evolution.design_identity(evolution.STAGE1_MODE)
        stage2 = evolution.design_identity(evolution.STAGE2_MODE)
        self.assertNotEqual(stage1["sha256"], stage2["sha256"])
        self.assertEqual(intent.design_identity(), historical)
        for identity in (stage1, stage2):
            payload = identity["payload"]
            self.assertEqual(payload["requirement"], evolution.REQUIREMENT)
            self.assertEqual(payload["decision"], evolution.DECISION)
            self.assertEqual(payload["historical_baseline"], historical)

    def test_postedit_contract_is_stage2_only_for_both_arms(self):
        for mode in (None, intent.MODE, evolution.STAGE1_MODE, "local-harness-development"):
            self.assertFalse(evolution.postedit_association_contract(mode))
        for cell in config.schedule(specification(evolution.STAGE2_MODE)):
            self.assertTrue(evolution.postedit_association_contract(evolution.STAGE2_MODE))
            self.assertIn(cell["intent_policy"], {"current", "change-level-v2"})

    def test_deadline_disconnect_classification_is_narrow(self):
        detail = {"error_type": "BrokenPipeError", "failure_origin": "downstream",
                  "response_started": True,
                  "response_body_complete": False, "occurred_at_monotonic": 11.0}
        proxy = SimpleNamespace(failures=["broken pipe"], failure_details=[detail])
        timed_out = {"timed_out": True, "_deadline_shutdown_monotonic": 10.0}
        self.assertTrue(_only_deadline_downstream_disconnects([proxy], timed_out))
        for changed in (
                {"error_type": "ValueError"}, {"response_started": False},
                {"response_body_complete": True}, {"occurred_at_monotonic": 9.0},
                {"failure_origin": "upstream"}):
            with self.subTest(changed=changed):
                bad = SimpleNamespace(failures=["failure"], failure_details=[dict(detail, **changed)])
                self.assertFalse(_only_deadline_downstream_disconnects([bad], timed_out))
        earlier = SimpleNamespace(failures=["provider failed"], failure_details=[dict(
            detail, error_type="ValueError", occurred_at_monotonic=9.0)])
        self.assertFalse(_only_deadline_downstream_disconnects([earlier, proxy], timed_out))
        self.assertFalse(_only_deadline_downstream_disconnects([proxy], {}))
        retained = _proxy_failure_evidence([proxy], timed_out)
        self.assertEqual(retained[0]["seconds_from_episode_deadline"], 1.0)
        self.assertEqual(retained[0]["failure_origin"], "downstream")
        self.assertNotIn("occurred_at_monotonic", retained[0])

    def test_upstream_reset_after_headers_and_deadline_is_not_client_cancellation(self):
        upstream_reset = SimpleNamespace(failures=["connection reset"], failure_details=[{
            "error_type": "ConnectionResetError", "failure_origin": "upstream",
            "response_started": True, "response_body_complete": False,
            "occurred_at_monotonic": 11.0}])
        result = {"timed_out": True, "_deadline_shutdown_monotonic": 10.0}
        self.assertFalse(_only_deadline_downstream_disconnects([upstream_reset], result))
        evidence = _proxy_failure_evidence([upstream_reset], result)
        self.assertEqual(evidence[0]["seconds_from_episode_deadline"], 1.0)
        self.assertEqual(evidence[0]["failure_origin"], "upstream")

    def test_metrics_separate_candidates_dispositions_interactions_and_gates(self):
        kinds = ("reuse_candidate", "association_candidate", "record_review", "link_review",
                 "reuse_review", "plan_approval_attempt")
        events = [{"id": str(index), "kind": kind, "cycle": "cycle",
                   "interaction": "review-1" if "review" in kind else None, "detail": "fixture"}
                  for index, kind in enumerate(kinds)]
        measured = evolution.review_metrics(events + events)
        self.assertEqual(measured["candidate_events"], {"reuse_candidate": 1, "association_candidate": 1})
        self.assertEqual(measured["individual_dispositions"], 3)
        self.assertEqual(measured["gate_decisions"], 4)
        self.assertEqual(measured["review_interactions"], 1)
        self.assertEqual(measured["approval_cycles"], 1)
        with self.assertRaisesRegex(ValueError, "conflicting"):
            evolution.review_metrics(events + [dict(events[0], kind="reuse_review")])

    def test_review_interaction_ids_override_legacy_event_count(self):
        identified = [
            {"id": "a", "kind": "review_interaction", "interaction": "batch-1"},
            {"id": "b", "kind": "record_review", "interaction": "batch-1"},
            {"id": "c", "kind": "link_review", "interaction": "batch-1"},
            {"id": "d", "kind": "review_interaction"},
        ]
        self.assertEqual(evolution.review_metrics(identified)["review_interactions"], 1)
        legacy = [{"id": "old-1", "kind": "review_interaction"},
                  {"id": "old-2", "kind": "review_interaction"}]
        self.assertEqual(evolution.review_metrics(legacy)["review_interactions"], 2)

    def test_execution_constituents_do_not_invent_semantic_completeness(self):
        measured = evolution.outcome_constituents({"status": "success", "checks": [{"passed": True}]})
        self.assertTrue(measured["within_budget_completion"])
        self.assertTrue(measured["source_correctness"])
        self.assertIsNone(measured["task_requirements_complete"])
        self.assertIsNone(measured["tests_complete"])
        self.assertIsNone(measured["capture_reconciliation_correct"])

    def test_online_reuse_review_checks_structure_and_scope_without_semantic_gold(self):
        candidate = {"iri": "urn:existing", "title": "Existing", "kind": "Requirement",
                     "status": "accepted", "assertion_digest": "d" * 64,
                     "owned_by_requester": False, "literals": [], "relations": []}
        reuse = {"operation_id": "operation", "candidate_iri": "urn:existing",
                 "candidate_title": "Existing", "original_claim": "new words",
                 "existing_claim": "old words", "rationale": "model judgment",
                 "reuse_unchanged": True, "recommendation_source": "model",
                 "original_proposal": {"kind": "Requirement",
                     "title": "Proposed", "description": "Claim", "evidence": ["event-1"],
                     "files": ["service.py"]}, "evidence_page": [{"id": "event-1"}],
                 "candidate_pages": [{"revision": "r" * 64, "candidates": [candidate]}]}
        outer = {"capture_resolution": reuse, "request": {"operation_id": "operation", "proposals": []}}
        state = {"task": {"phase": "AwaitingReview", "reviews": [outer]}}
        episode = {"allowed_paths": ["*.py"]}
        decision = review_input(state, episode)
        self.assertEqual(decision["input"], "/accept operation")
        self.assertIn("unassessed", decision["reason"])
        reuse["original_proposal"]["files"] = ["../gold.py"]
        self.assertEqual(review_input(state, episode)["input"], "/reject operation")
        reuse["original_proposal"]["files"] = ["service.py"]
        reuse["candidate_pages"][0]["candidates"] = []
        self.assertEqual(review_input(state, episode)["input"], "/reject operation")
        reuse["candidate_pages"][0]["candidates"] = [candidate]
        reuse["recommendation_source"] = "human_required"
        self.assertEqual(review_input(state, episode)["input"], "/reject operation")

    def test_offline_evolution_assessment_preserves_reuse_proposal_and_acceptance(self):
        with tempfile.TemporaryDirectory() as temporary:
            store = ArtifactStore(Path(temporary).resolve() / "store")
            run = store.create_run({"model": intent.MODELS[0], "backend": "harness", "condition": "harness",
                "intent_policy": "current", "scenario_id": "ruleset_cache",
                "evaluation_mode": evolution.STAGE1_MODE, "scenario_gold_sha256": "a" * 64})
            store.put_bytes(run, "evidence.txt", b"Operation proposed reuse.\nHuman accepted reuse.\nTask result.\n")
            store.put_bytes(run, "scenario.json", canonical_json({"episodes": [
                {"id": "e1", "expected_fact_ids": ["fact"]}], "initial_facts": [{"id": "fact"}]}))
            native = {"events": [
                {"id": "candidate", "kind": "reuse_candidate", "detail": "claims match"},
                {"id": "review", "kind": "reuse_review", "detail": "accepted reuse-1"}]}
            store.put_bytes(run, "outcome.json", canonical_json({"status": "success", "episodes": [
                {"id": "e1", "status": "success", "checks": [{"passed": True}],
                 "evolution_reviews": native}]}))
            store.seal_run(run)
            span = {"path": "evidence.txt", "start_line": 1, "end_line": 3}
            assessment = {"episodes": [{"episode_id": "e1", "task_requirements_complete": True,
                "tests_complete": True, "capture_reconciliation_correct": True,
                "associations_relevant": None, "rationale": "Evidence supports each constituent.",
                "evidence": [span], "reuse_assessments": [{"operation_id": "reuse-1",
                    "proposed": True, "accepted": True, "verdict": "supported",
                    "rationale": "Existing claim is equivalent.", "evidence": [span]}]}]}
            review = {"reviewer_id": "independent", "scenario_gold_sha256": "a" * 64,
                "claims": [{"claim_id": "fact", "episode_id": "e1", "fact_id": "fact",
                            "verdict": "supported", "evidence": [span]}],
                "evolution_assessment": assessment}
            record_review(store.root, run.name, review)
            result = report(store.root)["runs"][0]
            self.assertTrue(result["evolution_constituents"]["e1"]["capture_reconciliation_correct"])
            saved = [j for j in result["semantic"]["judgments"] if j["active"]][0]
            reuse = saved["evolution_assessment"]["episodes"][0]["reuse_assessments"][0]
            self.assertTrue(reuse["proposed"])
            self.assertTrue(reuse["accepted"])

    def test_offline_evolution_rejects_unattempted_episode_and_fake_reuse_operation(self):
        with tempfile.TemporaryDirectory() as temporary:
            store = ArtifactStore(Path(temporary).resolve() / "store")
            run = store.create_run({"model": intent.MODELS[0], "backend": "harness", "condition": "harness",
                "intent_policy": "current", "scenario_id": "ruleset_cache",
                "evaluation_mode": evolution.STAGE1_MODE, "scenario_gold_sha256": "a" * 64})
            store.put_bytes(run, "evidence.txt", b"sealed evidence\n")
            store.put_bytes(run, "scenario.json", canonical_json({"episodes": [
                {"id": "e1", "expected_fact_ids": ["fact"]},
                {"id": "e2", "expected_fact_ids": ["fact"]}], "initial_facts": [{"id": "fact"}]}))
            store.put_bytes(run, "outcome.json", canonical_json({"status": "agent_failure", "episodes": [
                {"id": "e1", "status": "agent_failure", "checks": [], "evolution_reviews": {"events": [
                    {"id": "candidate", "kind": "reuse_candidate", "detail": "candidate"},
                    {"id": "review", "kind": "reuse_review", "detail": "rejected real-op"}]}},
                {"id": "e2", "status": "unattempted", "checks": []}]}))
            store.seal_run(run)
            span = {"path": "evidence.txt", "start_line": 1, "end_line": 1}
            base = {"task_requirements_complete": False, "tests_complete": False,
                    "capture_reconciliation_correct": False, "associations_relevant": None,
                    "rationale": "Evidence inspected.", "evidence": [span],
                    "reuse_assessments": [{"operation_id": "fake-op", "proposed": True,
                        "accepted": False, "verdict": "unsupported", "rationale": "Not equivalent.",
                        "evidence": [span]}]}
            review = {"reviewer_id": "independent", "scenario_gold_sha256": "a" * 64,
                "claims": [{"claim_id": "fact", "episode_id": "e1", "fact_id": "fact",
                            "verdict": "missing", "rationale": "Absent", "evidence": []}],
                "evolution_assessment": {"episodes": [dict(base, episode_id="e1")]}}
            with self.assertRaisesRegex(ValueError, "absent from the sealed episode receipts"):
                record_review(store.root, run.name, review)
            review["evolution_assessment"]["episodes"] = [dict(base, episode_id="e2")]
            with self.assertRaisesRegex(ValueError, "attempted episodes"):
                record_review(store.root, run.name, review)


    def test_recovery_identity_binds_primary_outcome_and_sealed_predecessors(self):
        from bench.harness_study.cause import HARNESS_TERMINAL_CAUSES, RECOVERY_CONTROLLER_CLASS
        stage2 = evolution.design_identity(evolution.STAGE2_MODE)
        recovery = evolution.design_identity(evolution.STAGE2_RECOVERY_MODE)
        self.assertNotEqual(stage2["sha256"], recovery["sha256"])
        # The closed recovery campaign's identity: the baseline derives from its preflight.
        self.assertEqual(recovery["sha256"], "763ccc521e31595001d67dc15c9bddcd0c288443ae70184d0a20195914f340db")
        for key in ("primary_outcome", "terminal_cause_classes", "episode_limit", "shared_fixes", "sealed_predecessors"):
            self.assertNotIn(key, stage2["payload"])
        payload = recovery["payload"]
        self.assertEqual(payload["stage"], evolution.STAGE2_RECOVERY_MODE)
        self.assertEqual(payload["policies"], ["current", "change-level-v2"])
        # The frozen four-class list; the later reject-loop guard class never joins it.
        self.assertEqual(payload["primary_outcome"]["controller_class"], sorted(RECOVERY_CONTROLLER_CLASS))
        self.assertNotIn("reviewer_reject_loop", payload["primary_outcome"]["controller_class"])
        for key in ("evidence_byte_limit", "reject_loop_limit"):
            self.assertNotIn(key, payload)
        for cause in ("evidence_limit", "reviewer_reject_loop"):
            self.assertNotIn(cause, payload["terminal_cause_classes"])
        self.assertIn("first_edit_reached AND terminal_cause not in controller class",
                      payload["primary_outcome"]["definition"])
        self.assertEqual(payload["terminal_cause_classes"], sorted(HARNESS_TERMINAL_CAUSES))
        self.assertNotIn("native_no_completion", payload["terminal_cause_classes"])
        self.assertEqual(payload["episode_limit"], 1)
        self.assertEqual(payload["shared_fixes"], list(evolution.SHARED_FIXES))
        self.assertEqual(len(payload["shared_fixes"]), 5)
        self.assertEqual(payload["sealed_predecessors"], [
            "2f108e95e5bc6588bcbe3052e55ad4788385661d87528ce539bf8df7364f49c2",
            "364140e4a4f0d64573b3f7ca2c86d85486e3edc2c4aa02a4a89cd09995b01a73"])
        self.assertTrue(evolution.postedit_association_contract(evolution.STAGE2_RECOVERY_MODE))
        self.assertEqual(payload["historical_baseline"], intent.design_identity())

    def test_recovery_schedule_has_twelve_cells_across_both_policies(self):
        cells = config.schedule(specification(evolution.STAGE2_RECOVERY_MODE))
        self.assertEqual(len(cells), 12)
        self.assertEqual(len({(c["model"], c["intent_policy"], c["scenario_id"]) for c in cells}), 12)
        self.assertEqual({c["intent_policy"] for c in cells}, {"current", "change-level-v2"})
        self.assertEqual(cells, config.schedule(specification(evolution.STAGE2_RECOVERY_MODE)))
        self.assertIn(evolution.STAGE2_RECOVERY_MODE, evolution.MODES)

    def test_episode_limit_truncates_attempts_and_labels_excluded_episodes(self):
        from bench.harness_study.run import pad_unattempted, planned_episodes
        scenario = {"episodes": [{"id": "e1"}, {"id": "e2"}, {"id": "e3"}]}
        self.assertEqual(planned_episodes(scenario, None), (scenario["episodes"], []))
        planned, excluded = planned_episodes(scenario, 1)
        self.assertEqual([e["id"] for e in planned], ["e1"])
        self.assertEqual([e["id"] for e in excluded], ["e2", "e3"])
        for bad in (0, -1, "1", 1.0, True):
            with self.subTest(limit=bad), self.assertRaisesRegex(ValueError, "episode_limit"):
                planned_episodes(scenario, bad)
        outcome = {"episodes": [{"id": "e1", "status": "success", "checks": [], "metrics": {}}]}
        pad_unattempted(outcome, scenario, 1)
        self.assertEqual(outcome["episodes"][1:], [
            {"id": "e2", "status": "unattempted", "reason": "episode_limit", "checks": [], "metrics": {}},
            {"id": "e3", "status": "unattempted", "reason": "episode_limit", "checks": [], "metrics": {}}])
        unlimited = {"episodes": [{"id": "e1", "status": "agent_failure", "checks": [], "metrics": {}}]}
        pad_unattempted(unlimited, scenario)
        self.assertEqual(unlimited["episodes"][1], {"id": "e2", "status": "unattempted", "checks": [], "metrics": {}})

    def test_recovery_config_descends_from_pilot_or_stage2_and_refuses_sealed_builds(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pilot = {"study_id": "pilot", "evaluation_mode": intent.MODE, "seed": 7,
                     "gold_approval": str(root / "approval.json"),
                     "scenario_ids": list(intent.SCENARIOS), "local_models": [{"id": m} for m in intent.MODELS],
                     "generation_policy": {"local_temperature": 0.0}}
            (root / "approval.json").write_text(canonical_json(config.approval_payload("fixture", config=pilot)).decode())
            stage2 = dict(pilot, study_id="stage2", evaluation_mode=evolution.STAGE2_MODE,
                          intent_policies=list(evolution.POLICIES[evolution.STAGE2_MODE]),
                          evolution_design=evolution.design_identity(evolution.STAGE2_MODE))

            def parent(original, build_id="parent-build"):
                return {"ready": True, "config": original, "config_sha256": config.configuration_hash(original),
                        "binaries": {"build_id": build_id}}

            manifest = root / "manifest.json"
            with patch.object(config, "verify_binaries", return_value={"build_id": "fresh-build"}):
                for original in (pilot, stage2):
                    derived = config.evolution_config(parent(original), manifest, "recovery-v1",
                                                      evolution.STAGE2_RECOVERY_MODE)
                    self.assertEqual(derived["evaluation_mode"], evolution.STAGE2_RECOVERY_MODE)
                    self.assertEqual(derived["episode_limit"], 1)
                    self.assertEqual(derived["evolution_design"], evolution.design_identity(evolution.STAGE2_RECOVERY_MODE))
                    self.assertEqual(len(config.schedule(derived)), 12)
                stage2_derived = config.evolution_config(parent(pilot), manifest, "stage2-v1", evolution.STAGE2_MODE)
                self.assertNotIn("episode_limit", stage2_derived)
                with self.assertRaisesRegex(ValueError, "descend"):
                    config.evolution_config(parent(stage2), manifest, "stage2-v2", evolution.STAGE2_MODE)
            for sealed in evolution.SEALED_PREDECESSORS:
                with patch.object(config, "verify_binaries", return_value={"build_id": sealed}):
                    with self.assertRaisesRegex(ValueError, "sealed"):
                        config.evolution_config(parent(stage2), manifest, "recovery-v1", evolution.STAGE2_RECOVERY_MODE)
            with patch.object(config, "verify_binaries", return_value={"build_id": "parent-build"}):
                with self.assertRaisesRegex(ValueError, "newly frozen"):
                    config.evolution_config(parent(stage2), manifest, "recovery-v1", evolution.STAGE2_RECOVERY_MODE)

    def test_baseline_identity_binds_arms_native_causes_and_preregistration(self):
        import hashlib
        from bench.harness_study.cause import CONTROLLER_CLASS, TERMINAL_CAUSES
        stage = evolution.STAGE2_BASELINE_MODE
        self.assertIn(stage, evolution.MODES)
        self.assertEqual(evolution.POLICIES[stage], ("current", "change-level-v2"))
        self.assertTrue(evolution.postedit_association_contract(stage))
        baseline = evolution.design_identity(stage)
        recovery = evolution.design_identity(evolution.STAGE2_RECOVERY_MODE)
        self.assertNotEqual(baseline["sha256"], recovery["sha256"])
        payload = baseline["payload"]
        self.assertEqual(payload["stage"], stage)
        self.assertEqual(payload["arms"], [
            {"backend": "harness", "condition": "harness", "intent_policy": "current"},
            {"backend": "harness", "condition": "harness", "intent_policy": "change-level-v2"},
            {"backend": "opencode", "condition": "without", "intent_policy": None}])
        self.assertIn("harness-current vs opencode-without", payload["primary_outcome"]["definition"])
        self.assertEqual(payload["primary_outcome"]["baseline_knowledge_outcomes"], "not applicable")
        self.assertIn("identical across arms", payload["primary_outcome"]["prompt_rule"])
        self.assertEqual(payload["terminal_cause_classes"], sorted(TERMINAL_CAUSES))
        for cause in ("evidence_limit", "reviewer_reject_loop"):
            self.assertIn(cause, payload["terminal_cause_classes"])
        self.assertEqual(payload["controller_class"], sorted(CONTROLLER_CLASS))
        self.assertIn("reviewer_reject_loop", payload["controller_class"])
        self.assertEqual(payload["native_terminal_causes"],
                         ["success", "native_no_completion", "deadline_native", "infrastructure", "unknown"])
        self.assertFalse({"native_no_completion", "deadline_native"} & CONTROLLER_CLASS)
        self.assertEqual(payload["evidence_byte_limit"], 8 * 1024 ** 3)
        self.assertEqual(payload["evidence_byte_limit"], evolution.BASELINE_EVIDENCE_BYTE_LIMIT)
        self.assertEqual(payload["reject_loop_limit"], 5)
        self.assertEqual(payload["reject_loop_limit"], evolution.BASELINE_REJECT_LOOP_LIMIT)
        self.assertEqual(payload["episode_limit"], 1)
        self.assertEqual(payload["shared_fixes"], list(evolution.SHARED_FIXES))
        self.assertEqual(payload["sealed_predecessors"], list(evolution.SEALED_PREDECESSORS))
        self.assertEqual(payload["sealed_predecessors"][2],
                         "86dcd22618223c7eb686388ecea7607bd04dea4669115089fd671f823b109e69")
        self.assertEqual(payload["sealed_predecessors"][3],
                         "5cdf8bd6c2f85532d3d675e309824f3f5d9730ca7031016edb77ca39e52ea75a")
        self.assertEqual(len(payload["sealed_predecessors"]), 4)
        self.assertEqual(recovery["payload"]["sealed_predecessors"], list(evolution.SEALED_STAGE2))
        self.assertEqual(len(recovery["payload"]["sealed_predecessors"]), 2)
        expected = hashlib.sha256((Path(evolution.__file__).with_name("BASELINE.md")).read_bytes()).hexdigest()
        self.assertEqual(payload["preregistration_sha256"], expected)
        self.assertNotEqual(payload["preregistration_sha256"], recovery["payload"]["preregistration_sha256"])
        self.assertEqual(payload["historical_baseline"], intent.design_identity())

    def test_baseline_schedule_has_eighteen_cells_across_three_arms(self):
        stage = evolution.STAGE2_BASELINE_MODE
        cells = config.schedule(specification(stage))
        self.assertEqual(len(cells), 18)
        self.assertEqual(cells, config.schedule(specification(stage)))
        self.assertEqual([c["schedule_index"] for c in cells], list(range(18)))
        arms = Counter((c["backend"], c["condition"], c["intent_policy"]) for c in cells)
        self.assertEqual(arms, {("harness", "harness", "current"): 6,
                                ("harness", "harness", "change-level-v2"): 6,
                                ("opencode", "without", None): 6})
        for model in intent.MODELS:
            for scenario in intent.SCENARIOS:
                triple = [c for c in cells if c["model"] == model and c["scenario_id"] == scenario]
                self.assertEqual(len(triple), 3)
                self.assertEqual({(c["backend"], c["intent_policy"]) for c in triple},
                                 {("harness", "current"), ("harness", "change-level-v2"), ("opencode", None)})
        self.assertEqual(config.required_clients(specification(stage)), ("opencode", "lms"))
        with self.assertRaises(ValueError):
            config.schedule(dict(specification(stage), intent_policies=["current"]))

    def test_baseline_config_descends_from_recovery_parent_and_refuses_sealed_builds(self):
        from unittest.mock import patch
        stage = evolution.STAGE2_BASELINE_MODE
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pilot = {"study_id": "pilot", "evaluation_mode": intent.MODE, "seed": 7,
                     "gold_approval": str(root / "approval.json"), "opencode": str(root / "opencode"),
                     "scenario_ids": list(intent.SCENARIOS), "local_models": [{"id": m} for m in intent.MODELS],
                     "generation_policy": {"local_temperature": 0.0}}
            (root / "approval.json").write_text(canonical_json(config.approval_payload("fixture", config=pilot)).decode())
            recovery = dict(pilot, study_id="recovery-v2", evaluation_mode=evolution.STAGE2_RECOVERY_MODE,
                            intent_policies=list(evolution.POLICIES[evolution.STAGE2_RECOVERY_MODE]),
                            episode_limit=1, evolution_design=evolution.design_identity(evolution.STAGE2_RECOVERY_MODE))

            def parent(original, build_id="86dcd22618223c7eb686388ecea7607bd04dea4669115089fd671f823b109e69"):
                return {"ready": True, "config": original, "config_sha256": config.configuration_hash(original),
                        "binaries": {"build_id": build_id}}

            manifest = root / "manifest.json"
            with patch.object(config, "verify_binaries", return_value={"build_id": "fresh-build"}):
                derived = config.evolution_config(parent(recovery), manifest, "baseline-v1", stage)
                self.assertEqual(derived["evaluation_mode"], stage)
                self.assertEqual(derived["episode_limit"], 1)
                self.assertEqual(derived["evidence_byte_limit"], evolution.BASELINE_EVIDENCE_BYTE_LIMIT)
                self.assertEqual(derived["reject_loop_limit"], evolution.BASELINE_REJECT_LOOP_LIMIT)
                self.assertEqual(derived["evolution_design"], evolution.design_identity(stage))
                self.assertEqual(derived["parent_pilot"]["study_id"], "recovery-v2")
                self.assertEqual(derived["opencode"], str(root / "opencode"))
                self.assertEqual(len(config.schedule(derived)), 18)
                # Only the baseline freezes the driver guards; recovery derivations are untouched.
                recovered = config.evolution_config(parent(pilot, "parent-build"), manifest, "recovery-v3",
                                                    evolution.STAGE2_RECOVERY_MODE)
                for key in ("evidence_byte_limit", "reject_loop_limit"):
                    self.assertNotIn(key, recovered)
                for original in (pilot, dict(recovery, evaluation_mode=evolution.STAGE2_MODE,
                                             evolution_design=evolution.design_identity(evolution.STAGE2_MODE))):
                    original.pop("episode_limit", None)
                    self.assertEqual(config.evolution_config(parent(original), manifest, "baseline-v1", stage)
                                     ["evaluation_mode"], stage)
                with self.assertRaisesRegex(ValueError, "opencode"):
                    config.evolution_config(parent(dict(recovery, opencode=None)), manifest, "baseline-v1", stage)
                with self.assertRaisesRegex(ValueError, "descend"):
                    config.evolution_config(parent(dict(recovery, evaluation_mode=evolution.STAGE1_MODE)),
                                            manifest, "baseline-v1", stage)
            for sealed in evolution.SEALED_PREDECESSORS:
                with patch.object(config, "verify_binaries", return_value={"build_id": sealed}):
                    with self.assertRaisesRegex(ValueError, "sealed"):
                        config.evolution_config(parent(recovery, "other-parent"), manifest, "baseline-v1", stage)
            with patch.object(config, "verify_binaries", return_value={"build_id": "parent-build"}):
                with self.assertRaisesRegex(ValueError, "newly frozen"):
                    config.evolution_config(parent(recovery, "parent-build"), manifest, "baseline-v1", stage)

    RECOVERY_PREFLIGHT = Path(evolution.__file__).parents[2] / "target/harness-evolution-stage2-recovery-v2/preflight.json"

    @unittest.skipUnless(RECOVERY_PREFLIGHT.is_file(), "closed recovery campaign preflight is not on this machine")
    def test_baseline_config_derives_from_the_closed_recovery_preflight_on_disk(self):
        import json
        from unittest.mock import patch
        parent = json.loads(self.RECOVERY_PREFLIGHT.read_text())
        # The on-disk parent binds the frozen recovery identity; a changed hash would refuse it.
        self.assertEqual(parent["config"]["evolution_design"]["sha256"],
                         "763ccc521e31595001d67dc15c9bddcd0c288443ae70184d0a20195914f340db")
        with patch.object(config, "verify_binaries", return_value={"build_id": "fresh-build"}):
            derived = config.evolution_config(parent, self.RECOVERY_PREFLIGHT.with_name("manifest.json"),
                                              "baseline-probe", evolution.STAGE2_BASELINE_MODE)
        self.assertEqual(derived["evaluation_mode"], evolution.STAGE2_BASELINE_MODE)
        self.assertEqual(derived["evidence_byte_limit"], evolution.BASELINE_EVIDENCE_BYTE_LIMIT)
        self.assertEqual(derived["reject_loop_limit"], evolution.BASELINE_REJECT_LOOP_LIMIT)
        self.assertEqual(derived["evolution_design"], evolution.design_identity(evolution.STAGE2_BASELINE_MODE))
        self.assertEqual(derived["parent_pilot"]["build_id"], parent["binaries"]["build_id"])
        self.assertEqual(len(config.schedule(derived)), 18)


if __name__ == "__main__":
    unittest.main()
