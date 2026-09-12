import copy
import hashlib
import json
from pathlib import Path
import shlex
import shutil
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import config, evolution, intent, indexing
from bench.harness_study.artifacts import ArtifactStore, canonical_json, sha256_file
from bench.harness_study.grading import record_review, report
from bench.harness_study.reviewer import review_input
from bench.harness_study.scenario import SCENARIOS, MAINTENANCE, load_scenario, tree_manifest
from bench.harness_study.seed import seed_graph, seed_iri


def specification():
    return {"evaluation_mode": intent.MODE, "scenario_ids": list(intent.SCENARIOS),
            "local_models": [{"id": model} for model in intent.MODELS], "seed": 20260909}


class DesignTests(unittest.TestCase):
    def test_exact_twelve_distinct_cells_twenty_eight_possible_episodes(self):
        cells = config.schedule(specification())
        self.assertEqual(len(cells), 12)
        self.assertEqual(len({(c["model"], c["intent_policy"], c["scenario_id"]) for c in cells}), 12)
        self.assertEqual(sum(len(load_scenario(c["scenario_id"])["episodes"]) for c in cells), 28)
        self.assertEqual(cells, config.schedule(specification()))
        self.assertNotEqual(cells, config.schedule(dict(specification(), seed=42)))
        self.assertEqual(config.required_clients(specification()), ("lms",))
        for update in ({"intent_policies": ["current"]}, {"scenario_ids": [MAINTENANCE]},
                       {"local_models": [{"id": "substitute"}, {"id": intent.MODELS[1]}]}):
            with self.assertRaises(ValueError):
                config.schedule(dict(specification(), **update))

    def test_legacy_schedules_and_approval_exclude_explicit_new_case(self):
        old = {"seed": 1, "frontier_model": "frontier", "local_models": [{"id": m} for m in ("a", "b", "c")]}
        self.assertEqual(len(config.schedule(old)), 16)
        self.assertEqual(len(config.schedule(dict(old, evaluation_mode="local-harness-development"))), 6)
        approval = config.approval_payload("fixture")
        self.assertEqual(set(approval["scenarios"]), {"ruleset_cache", "retry_ledger"})

    def test_approval_binds_selected_packages_and_shared_setup_design(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "approval.json"
            approved = config.approval_payload("fixture reviewer", config=specification())
            path.write_text(json.dumps(approved))
            self.assertEqual(config.verify_approval(path, config=specification()), approved)
            altered = copy.deepcopy(approved)
            altered["intent_design"]["payload"]["overlay"]["shared_entity_association"] = "treatment only"
            path.write_text(json.dumps(altered))
            with self.assertRaisesRegex(ValueError, "design hash"):
                config.verify_approval(path, config=specification())
            altered = copy.deepcopy(approved)
            altered["scenarios"][MAINTENANCE]["package_sha256"] = "wrong"
            path.write_text(json.dumps(altered))
            with self.assertRaisesRegex(ValueError, "hash"):
                config.verify_approval(path, config=specification())
            path.write_text(json.dumps(config.approval_payload("legacy")))
            with self.assertRaisesRegex(ValueError, "scoped"):
                config.verify_approval(path, config=specification())

    def test_only_named_maintenance_case_allows_one_episode(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            shutil.copytree(SCENARIOS / MAINTENANCE, directory / "other")
            path = directory / "other/scenario.json"
            value = json.loads(path.read_text())
            value["id"] = "other"
            path.write_text(json.dumps(value))
            with self.assertRaisesRegex(ValueError, "3 uniquely"):
                load_scenario("other", directory)

    def test_ledger_seed_stays_empty_and_overlay_carries_no_new_claim(self):
        ledger = load_scenario("retry_ledger")
        self.assertEqual(ledger["initial_facts"], [])
        graph = seed_graph(ledger)
        self.assertNotIn("hasDescription", graph)
        self.assertEqual(intent.SEED_ASSOCIATIONS["retry_ledger"], [])
        self.assertEqual(len(intent.SEED_ASSOCIATIONS[MAINTENANCE]), 4)


class IndexIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.repo = Path(self.temporary.name).resolve()
        self.root = self.repo / "target/indexer"
        self.root.mkdir(parents=True)
        self.node = self.root / "node"
        self.node.write_text("#!/bin/sh\necho v1.2.3\n")
        self.node.chmod(0o500)
        package = self.root / "node_modules"
        package.mkdir()
        entry = package / "index.js"
        entry.write_text("// fixture\n")
        metadata = package / "package.json"
        metadata.write_text('{"version":"0.6.6"}')
        launcher = self.root / "scip-python"
        launcher.write_text(f'#!/bin/sh\nexec {shlex.quote(str(self.node))} {shlex.quote(str(entry))} "$@"\n')
        launcher.chmod(0o500)
        self.identity = {"schema_version": 1, "directory": str(self.root),
            "node": {"path": str(self.node), "sha256": sha256_file(self.node), "version": "v1.2.3"},
            "launcher": {"path": str(launcher), "sha256": sha256_file(launcher)},
            "producer": {"entry_point": str(entry), "package_root": str(package),
                         "package_json": str(metadata), "version": "0.6.6", "files": tree_manifest(package)},
            "runtime_files": {}}

    def test_dependency_node_launcher_and_version_are_bound(self):
        with patch.object(indexing, "REPO", self.repo):
            self.assertEqual(indexing.verify_indexer(self.identity), self.identity)
            for section, key, value in [("node", "version", "v9"), ("node", "sha256", "wrong"),
                                        ("producer", "version", "wrong"), ("launcher", "sha256", "wrong")]:
                altered = copy.deepcopy(self.identity)
                altered[section][key] = value
                with self.assertRaises(ValueError):
                    indexing.verify_indexer(altered)
            Path(self.identity["producer"]["entry_point"]).write_text("changed")
            with self.assertRaisesRegex(ValueError, "dependency tree"):
                indexing.verify_indexer(self.identity)

    def test_rehashed_path_fallback_launcher_still_rejected(self):
        path = Path(self.identity["launcher"]["path"])
        path.chmod(0o700)
        path.write_text('#!/bin/sh\nexec npx scip-python "$@"\n')
        self.identity["launcher"]["sha256"] = sha256_file(path)
        with patch.object(indexing, "REPO", self.repo), self.assertRaisesRegex(ValueError, "exactly"):
            indexing.verify_indexer(self.identity)

    def test_system_interpreter_identity_detects_tampering_and_version_changes(self):
        interpreter = self.root / "system-python-resolved"
        interpreter.write_bytes(b"fixture interpreter version one")
        observed = {"executable": str(interpreter), "version": "fixture Python 3.9"}
        with patch.object(indexing, "SYSTEM_PYTHON", self.node), patch.object(
                indexing.subprocess, "check_output", return_value=json.dumps(observed)):
            original = indexing.system_python_identity()
            self.assertEqual(indexing.system_python_identity(original), original)
            interpreter.write_bytes(b"tampered interpreter")
            with self.assertRaisesRegex(ValueError, "changed after intent preflight"):
                indexing.system_python_identity(original)
            interpreter.write_bytes(b"fixture interpreter version one")
            with patch.object(indexing.subprocess, "check_output", return_value=json.dumps(
                    dict(observed, version="fixture Python 3.10"))):
                with self.assertRaisesRegex(ValueError, "changed after intent preflight"):
                    indexing.system_python_identity(original)

    def test_probe_socket_path_budget_is_independent_of_long_evidence_path(self):
        evidence = self.root / ("long-evidence-name-" * 8) / "daemon-runtime"
        with indexing.short_probe_runtime(evidence) as runtime:
            self.assertLess(len(str(runtime / "daemon.sock").encode()), 100)
            (runtime / "home").mkdir()
            (runtime / "home/receipt.json").write_text('{"kept":true}')
        self.assertFalse(runtime.exists())
        self.assertEqual((evidence / "home/receipt.json").read_text(), '{"kept":true}')

    def test_entity_resolution_cannot_silently_choose_wrong_member_or_stale_source(self):
        target = {"file": "service.py", "name": "Ledger.process"}
        entity = {"file": "service.py", "name": "process", "symbol": "scip-python python . . service/Ledger#process().",
                  "source_digest": "proof"}
        self.assertEqual(indexing._entity({"entities": [entity]}, target), entity)
        with self.assertRaises(RuntimeError):
            indexing._entity({"entities": [dict(entity, source_digest="")]}, target)
        with self.assertRaises(RuntimeError):
            indexing._entity({"entities": [dict(entity, symbol="service/Other#process().")]}, target)


class ReviewAndMeasurementTests(unittest.TestCase):
    def test_readiness_requires_actual_linked_dossiers_and_preserves_empty_ledger(self):
        scenario = load_scenario(MAINTENANCE)
        records = [seed_iri(MAINTENANCE + "/fact/" + fact["id"]) for fact in scenario["initial_facts"]]
        class Daemon:
            reviewed = False
            omit_dossier = False
            def _request(self, path, body):
                if path.endswith("/resolve"):
                    return {"revision": "revision", "records": records,
                            "entities": [{"file": "labels.py", "name": name, "symbol": name,
                                          "source_digest": "source-proof", "dossier_records":
                                          records if self.reviewed and not self.omit_dossier else []}
                                         for name in ("render_name", "render_names")]}
                if path.endswith("/link"):
                    self.bindings = body["bindings"]
                    return {"links": ["link"], "unresolved": []}
                self.reviewed = True
                return {"conforms": True, "durable": True, "pending": []}
        daemon = Daemon()
        result = indexing.ready_dossiers(daemon, scenario, seed=True)
        self.assertEqual(len(daemon.bindings), 4)
        self.assertEqual(len(result["seed_operations"]), 1)
        daemon.omit_dossier = True
        with self.assertRaisesRegex(RuntimeError, "dossier lacks"):
            indexing.ready_dossiers(daemon, scenario, seed=True)
        ledger = load_scenario("retry_ledger")
        response = {"revision": "revision", "records": [], "entities": [{"file": "service.py",
                    "name": "Ledger.process", "symbol": "ledger", "source_digest": "source-proof", "dossier_records": []}]}
        with patch.object(daemon, "_request", return_value=response):
            self.assertEqual(indexing.ready_dossiers(daemon, ledger, require_empty=True)["initial_knowledge"], "expected_empty")
            response["records"] = ["canary-leaked"]
            with self.assertRaisesRegex(RuntimeError, "without knowledge"):
                indexing.ready_dossiers(daemon, ledger, require_empty=True)

    def test_link_review_uses_only_scope_and_structure_for_both_policies(self):
        episode = {"allowed_paths": ["**/*.py"]}
        for policy in intent.POLICIES:
            state = {"task": {"phase": "AwaitingReview", "intent_policy": policy,
                      "reviews": [{"intent_links": {"operation_id": "op", "bindings": [
                          {"record_iri": "record", "file": "labels.py", "planned_name": "_normalize"}]}}]}}
            self.assertEqual(review_input(state, episode)["input"], "/accept op")
            state["task"]["reviews"][0]["intent_links"]["bindings"][0]["file"] = "../secret.py"
            self.assertEqual(review_input(state, episode)["input"], "/reject op")

    def test_gate_ids_deduplicate_resume_and_keep_dispositions_separate(self):
        events = [{"id": str(i), "cycle": "same-cycle", "kind": kind, "detail": "observed"}
                  for i, kind in enumerate(("cycle_started", "plan_approval_attempt", "plan_blocked",
                      "record_review", "link_review", "plan_approval_attempt", "plan_approved", "cycle_ended"))]
        metrics = intent.gate_metrics(events + events)
        self.assertEqual(metrics["gate_decisions"], 4)
        self.assertEqual(metrics["approval_cycles"], 1)
        self.assertEqual(metrics["plan_approval_attempts"], 2)
        with self.assertRaisesRegex(ValueError, "conflicting"):
            intent.gate_metrics(events + [dict(events[0], kind="other")])

    def test_primary_is_unknown_before_semantics_and_allows_justified_new_record(self):
        episode = {"status": "success", "checks": [{"passed": True}]}
        self.assertIsNone(intent.primary_outcome(episode)["primary"])
        semantic = {"helper_links_both_seeds": True,
                    "no_redundant_or_unsupported_accepted_knowledge": True, "new_record_count": 1}
        result = intent.primary_outcome(episode, semantic)
        self.assertTrue(result["primary"])
        self.assertFalse(result["zero_new_records_secondary"])
        self.assertFalse(intent.primary_outcome(dict(episode, status="agent_failure"), semantic)["primary"])
        self.assertFalse(intent.primary_outcome(episode, dict(semantic, helper_links_both_seeds=False))["primary"])

    def test_observed_repetition_does_not_conflate_changed_source(self):
        events = [{"message": message} for message in ["Read labels.py: old", "Read labels.py: old",
                  "Read labels.py: new", 'Model action: {"action":"command","command":"tests"}',
                  'Model action: {"action":"command","command":"tests"}']]
        measured = intent.activity_metrics(events)
        self.assertEqual(measured["source_reads"], 3)
        self.assertEqual(measured["source_rereads_unchanged"], 1)
        self.assertEqual(measured["repeat_command_requests"], 1)

    def test_offline_primary_requires_evidence_bound_semantic_assessment(self):
        with tempfile.TemporaryDirectory() as temporary:
            store = ArtifactStore(Path(temporary).resolve() / "store")
            run = store.create_run({"model": intent.MODELS[0], "backend": "harness", "condition": "harness",
                "intent_policy": "current", "scenario_id": MAINTENANCE, "evaluation_mode": intent.MODE,
                "scenario_gold_sha256": "a" * 64})
            store.put_bytes(run, "knowledge.txt", b"A reviewed source and graph evidence span.\n")
            store.put_bytes(run, "outcome.json", canonical_json({"status": "success", "episodes": [
                {"id": "e1", "status": "success", "checks": [{"passed": True}]}]}))
            store.seal_run(run)
            self.assertIsNone(report(store.root)["runs"][0]["intent_primary"]["primary"])
            span = {"path": "knowledge.txt", "start_line": 1, "end_line": 1}
            assessment = {"helper_links_both_seeds": True, "no_redundant_or_unsupported_accepted_knowledge": True,
                          "new_record_count": 0, "evidence": [span], "rationale": "Both existing claims concern this helper."}
            review = {"reviewer_id": "fixture", "scenario_gold_sha256": "a" * 64,
                      "claims": [{"claim_id": "fact", "verdict": "supported", "evidence": [span]}],
                      "intent_assessment": assessment}
            invalid = copy.deepcopy(review)
            invalid["intent_assessment"]["evidence"] = []
            with self.assertRaisesRegex(ValueError, "evidence"):
                record_review(store.root, run.name, invalid)
            record_review(store.root, run.name, review)
            self.assertTrue(report(store.root)["runs"][0]["intent_primary"]["primary"])

    def test_stage2_maintenance_accepts_intent_assessment_and_reports_primary(self):
        with tempfile.TemporaryDirectory() as temporary:
            store = ArtifactStore(Path(temporary).resolve() / "store")
            run = store.create_run({"model": intent.MODELS[0], "backend": "harness",
                "condition": "harness", "intent_policy": "change-level-v2",
                "scenario_id": MAINTENANCE, "evaluation_mode": evolution.STAGE2_MODE,
                "scenario_gold_sha256": "a" * 64})
            store.put_bytes(run, "knowledge.txt", b"sealed helper and graph evidence\n")
            store.put_bytes(run, "outcome.json", canonical_json({"status": "success", "episodes": [
                {"id": "e1", "status": "success", "checks": [{"passed": True}],
                 "evolution_reviews": {"events": []}}]}))
            store.seal_run(run)
            evidence = [{"path": "knowledge.txt", "start_line": 1, "end_line": 1}]
            semantic = {"helper_links_both_seeds": True,
                "no_redundant_or_unsupported_accepted_knowledge": True,
                "new_record_count": 0, "rationale": "Sealed evidence supports both judgments.",
                "evidence": evidence}
            review = {"reviewer_id": "fixture", "scenario_gold_sha256": "a" * 64,
                "claims": [{"claim_id": "fact", "verdict": "supported", "evidence": evidence}],
                "intent_assessment": semantic}
            record_review(store.root, run.name, review)
            self.assertTrue(report(store.root)["runs"][0]["intent_primary"]["primary"])


if __name__ == "__main__":
    unittest.main()
