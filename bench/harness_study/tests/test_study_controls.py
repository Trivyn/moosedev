"""Offline controls for frozen study inputs, factual parity, and blind review."""
import copy
from collections import Counter
import hashlib
import json
from pathlib import Path
import re
import ssl
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import config, reviewer, seed
from bench.harness_study.artifacts import canonical_json
from bench.harness_study.scenario import load_scenario, seed_notes


class ConfigurationControls(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.scenarios = {
            "one": {"package_sha256": "package-one", "gold_sha256": "gold-one"},
            "two": {"package_sha256": "package-two", "gold_sha256": "gold-two"},
        }
        self.patches = [patch.object(config, "list_scenarios", return_value=["one", "two"]),
                        patch.object(config, "load_scenario", side_effect=lambda name: self.scenarios[name])]
        for replacement in self.patches:
            replacement.start()
            self.addCleanup(replacement.stop)

    def approve(self):
        path = self.root / "approval.json"
        approval = config.approval_payload("reviewer-fixture")
        path.write_text(json.dumps(approval))
        return path, approval

    def test_ca_bundle_freezes_public_anchors_and_detects_tampering(self):
        anchors = ssl.create_default_context().get_ca_certs(binary_form=True)
        self.assertTrue(anchors, "test environment needs a public trust store")
        source = self.root / "public-ca.pem"
        source.write_text(ssl.DER_cert_to_PEM_cert(anchors[0]))
        with patch.object(config, "REPO", self.root):
            identity = config.freeze_ca_bundle(source)
            frozen = Path(identity["path"])
            self.assertEqual(frozen.read_bytes(), source.read_bytes())
            self.assertEqual(identity["sha256"], hashlib.sha256(source.read_bytes()).hexdigest())
            self.assertEqual(identity["certificates"], 1)
            self.assertEqual(config.freeze_ca_bundle(source), identity)
            frozen.chmod(0o600)
            frozen.write_text("changed")
            with self.assertRaisesRegex(ValueError, "differs"):
                config.freeze_ca_bundle(source)

    def test_ca_bundle_rejects_private_keys_malformed_data_and_aliases(self):
        source = self.root / "not-public.pem"
        with patch.object(config, "REPO", self.root):
            source.write_text("-----BEGIN PRIVATE KEY-----")
            with self.assertRaisesRegex(ValueError, "private keys"):
                config.freeze_ca_bundle(source)
            source.write_text("not a certificate")
            with self.assertRaises(ssl.SSLError):
                config.freeze_ca_bundle(source)
            alias = self.root / "alias.pem"
            alias.symlink_to(source)
            with self.assertRaisesRegex(ValueError, "regular PEM"):
                config.freeze_ca_bundle(alias)

    def test_human_approval_binds_both_gold_and_complete_package(self):
        path, approved = self.approve()
        self.assertEqual(config.verify_approval(path), approved)
        for field in ("package_sha256", "gold_sha256"):
            with self.subTest(field=field):
                original = self.scenarios["one"][field]
                self.scenarios["one"][field] = "changed"
                with self.assertRaisesRegex(ValueError, "hash"):
                    config.verify_approval(path)
                self.scenarios["one"][field] = original
        self.assertEqual(config.verify_approval(path), approved)

    def test_approval_fails_for_added_or_removed_scenario(self):
        path, _ = self.approve()
        with patch.object(config, "list_scenarios", return_value=["one"]):
            with self.assertRaises(ValueError):
                config.verify_approval(path)
        self.scenarios["three"] = {"package_sha256": "third", "gold_sha256": "third-gold"}
        with patch.object(config, "list_scenarios", return_value=["one", "two", "three"]):
            with self.assertRaises(ValueError):
                config.verify_approval(path)

    def test_approval_requires_named_reviewer_and_timestamp(self):
        with self.assertRaises(ValueError):
            config.approval_payload(" \t")
        path, approval = self.approve()
        for field in ("reviewer", "approved_at"):
            altered = dict(approval)
            altered.pop(field)
            path.write_text(json.dumps(altered))
            with self.assertRaises(ValueError):
                config.verify_approval(path)

    def test_schedule_has_sixteen_unique_cells_and_eight_setups(self):
        specification = {"seed": 20260907, "frontier_model": "gpt-5.6-sol",
                         "local_models": [{"id": "qwen/exact"}, {"id": "gemma/moe"}, {"id": "gemma/e4b"}]}
        cells = config.schedule(specification)
        identities = [(cell["model"], cell["backend"], cell["condition"], cell["scenario_id"]) for cell in cells]
        self.assertEqual(len(cells), 16)
        self.assertEqual(len(set(identities)), 16)
        self.assertEqual(len({row[:3] for row in identities}), 8)
        self.assertEqual(Counter(cell["scenario_id"] for cell in cells), {"one": 8, "two": 8})
        self.assertEqual([cell["schedule_index"] for cell in cells], list(range(16)))
        self.assertEqual(config.schedule(specification), cells)
        changed = config.schedule(dict(specification, seed=17))
        self.assertNotEqual(changed, cells)
        self.assertEqual({(c["model"], c["backend"], c["condition"], c["scenario_id"]) for c in changed}, set(identities))
        self.assertEqual({c["backend"] for c in cells if c["model"] == "gpt-5.6-sol"}, {"codex", "codex_mcp"})
        for model in specification["local_models"]:
            self.assertEqual({c["backend"] for c in cells if c["model"] == model["id"]}, {"opencode", "harness"})

    def test_configuration_hash_is_canonical_and_changes_with_effective_setting(self):
        first = {"model": "exact", "temperature": 0, "nested": {"b": 2, "a": 1}}
        second = {"nested": {"a": 1, "b": 2}, "temperature": 0, "model": "exact"}
        self.assertEqual(config.configuration_hash(first), config.configuration_hash(second))
        self.assertNotEqual(config.configuration_hash(first), config.configuration_hash(dict(first, temperature=1)))

    def test_development_schedule_has_only_six_local_harness_cells(self):
        specification = {"seed": 20260907, "evaluation_mode": "local-harness-development",
                         "local_models": [{"id": "qwen"}, {"id": "a4b"}, {"id": "e4b"}]}
        cells = config.schedule(specification)
        self.assertEqual(len(cells), 6)
        self.assertEqual({(c["backend"], c["condition"]) for c in cells}, {("harness", "harness")})
        self.assertEqual(Counter(c["model"] for c in cells), {"qwen": 2, "a4b": 2, "e4b": 2})
        self.assertEqual(config.schedule(specification), cells)
        self.assertEqual(config.required_clients(specification), ("lms",))
        for models in ([{"id": "qwen"}], [{"id": "same"}] * 3):
            with self.assertRaisesRegex(ValueError, "six distinct"):
                config.schedule(dict(specification, local_models=models))
        with self.assertRaisesRegex(ValueError, "evaluation_mode"):
            config.schedule(dict(specification, evaluation_mode="typo"))

    def test_development_derivation_preserves_parent_and_requires_new_build(self):
        approval, _ = self.approve()
        original = {"study_id": "parent", "gold_approval": str(approval), "seed": 1,
                    "local_models": [{"id": "qwen"}, {"id": "a4b"}, {"id": "e4b"}]}
        parent = {"ready": True, "config": original, "config_sha256": config.configuration_hash(original),
                  "binaries": {"build_id": "old-build"}}
        unchanged = copy.deepcopy(parent)
        with patch.object(config, "verify_binaries", return_value={"build_id": "new-build"}):
            derived = config.development_config(parent, self.root / "manifest.json", "development-v1")
            self.assertEqual(derived["parent_pilot"]["config_sha256"], parent["config_sha256"])
            self.assertEqual(derived["harness_response_policy"], "auto")
            self.assertEqual(derived["gold_approval"], str(approval))
            self.assertEqual(len(config.schedule(derived)), 6)
            self.assertEqual(parent, unchanged)
            with self.assertRaisesRegex(ValueError, "distinct"):
                config.development_config(parent, self.root / "manifest.json", "parent")
            with self.assertRaisesRegex(ValueError, "intact"):
                config.development_config(dict(parent, config_sha256="changed"), self.root / "manifest.json", "new")
        with patch.object(config, "verify_binaries", return_value={"build_id": "old-build"}):
            with self.assertRaisesRegex(ValueError, "newly frozen"):
                config.development_config(parent, self.root / "manifest.json", "new")

    def model_fixture(self):
        weights = self.root / "weights"
        weights.mkdir()
        return {"id": "vendor/exact", "weights": str(weights)}, {
            "models": [{"key": "vendor/exact", "type": "llm", "format": "mlx",
                        "quantization": "4bit", "loaded_instances": [{"id": "volatile"}]}]}

    def test_model_fingerprint_keeps_exact_identity_and_weight_hashes(self):
        model, available = self.model_fixture()
        files = {"model.safetensors": "a" * 64, "config.json": "b" * 64}
        with patch.object(config, "tree_manifest", return_value=files) as manifest:
            result = config.fingerprint_model(model, available)
        manifest.assert_called_once_with(Path(model["weights"]))
        self.assertEqual(result["id"], "vendor/exact")
        self.assertEqual(result["files"], files)
        self.assertEqual(result["weights_sha256"], hashlib.sha256(canonical_json(files)).hexdigest())
        self.assertEqual(result["runtime_metadata"]["quantization"], "4bit")
        self.assertNotIn("loaded_instances", result["runtime_metadata"])

    def test_model_alias_type_and_ambiguous_inventory_fail_before_hashing(self):
        model, available = self.model_fixture()
        cases = [{"models": []}, {"models": [dict(available["models"][0], key="vendor/other")]},
                 {"models": [dict(available["models"][0], key="VENDOR/EXACT")]},
                 {"models": [dict(available["models"][0], type="embedding")]},
                 {"models": available["models"] * 2}]
        for inventory in cases:
            with self.subTest(inventory=inventory), patch.object(config, "tree_manifest") as hashing:
                with self.assertRaises(ValueError):
                    config.fingerprint_model(model, inventory)
                hashing.assert_not_called()

    def test_incomplete_weights_are_not_treated_as_a_model(self):
        model, available = self.model_fixture()
        for files in ({"config.json": "digest"},
                      {"model.safetensors": "digest", "download.part": "partial"},
                      {"model.gguf": "digest", "download.tmp": "partial"}):
            with self.subTest(files=files), patch.object(config, "tree_manifest", return_value=files):
                with self.assertRaises(ValueError):
                    config.fingerprint_model(model, available)

    def test_model_weight_symlink_is_rejected(self):
        model, available = self.model_fixture()
        alias = self.root / "alias"
        alias.symlink_to(model["weights"], target_is_directory=True)
        with self.assertRaises(ValueError):
            config.fingerprint_model(dict(model, weights=str(alias)), available)


class SeedControls(unittest.TestCase):
    @staticmethod
    def graph_rows(graph):
        """Minimal N-Quads reader for assertions; no RDF dependency or network."""
        rows = []
        for line in graph.splitlines():
            match = re.fullmatch(r"<([^>]+)> <([^>]+)> (.+) <([^>]+)> \.", line)
            if not match:
                raise AssertionError(f"invalid fixture N-Quad: {line}")
            subject, predicate, obj, graph_name = match.groups()
            rows.append((subject, predicate, obj, graph_name))
        return rows

    def test_inherited_notes_and_graph_represent_the_same_seed_facts(self):
        scenario = load_scenario("ruleset_cache")
        notes = seed_notes(scenario["initial_facts"])
        graph = seed.seed_graph(scenario)
        rows = self.graph_rows(graph)
        for fact in scenario["initial_facts"]:
            subject = seed.seed_iri(scenario["id"] + "/fact/" + fact["id"])
            by_predicate = {p: obj for s, p, obj, _ in rows if s == subject}
            self.assertEqual(by_predicate[seed.RDF_TYPE], f"<{seed.ARCH}{fact['kind']}>")
            self.assertEqual(json.loads(by_predicate[seed.ARCH + "hasTitle"]), fact["title"])
            self.assertEqual(json.loads(by_predicate[seed.ARCH + "hasDescription"]), fact["description"])
            self.assertEqual(json.loads(by_predicate[seed.ARCH + "hasLifecycleStatus"]), "accepted")
            for value in (fact["title"], fact["description"], fact["kind"], fact["component"]):
                self.assertIn(value, notes)
            for evidence in fact.get("evidence", []):
                self.assertEqual(evidence in notes, json.dumps(evidence, ensure_ascii=False) in graph,
                                 "source evidence must be included equally or withheld equally")
        self.assertEqual(graph, seed.seed_graph(scenario))

    def test_seed_graph_uses_required_component_name_and_record_timestamp(self):
        scenario = load_scenario("ruleset_cache")
        rows = self.graph_rows(seed.seed_graph(scenario))
        for fact in scenario["initial_facts"]:
            component = seed.seed_iri(scenario["id"] + "/component/" + fact["component"])
            self.assertIn((component, seed.ARCH + "hasComponentName", json.dumps(fact["component"]), seed.GRAPH), rows)
            subject = seed.seed_iri(scenario["id"] + "/fact/" + fact["id"])
            timestamps = [obj for s, predicate, obj, _ in rows
                          if s == subject and predicate == seed.ARCH + "hasTimestamp"]
            self.assertEqual(len(timestamps), 1, "InformationRecordShape requires hasTimestamp")
            self.assertTrue(timestamps[0].endswith('^^<http://www.w3.org/2001/XMLSchema#dateTime>'))

    def test_seed_relations_keep_target_identity_in_both_representations(self):
        scenario = load_scenario("ruleset_cache")
        scenario = copy.deepcopy(scenario)
        left, right = scenario["initial_facts"]
        left["relations"] = [{"predicate": "constrains", "target": right["id"]}]
        notes = seed_notes(scenario["initial_facts"])
        rows = self.graph_rows(seed.seed_graph(scenario))
        subject = seed.seed_iri(scenario["id"] + "/fact/" + left["id"])
        target = seed.seed_iri(scenario["id"] + "/fact/" + right["id"])
        self.assertIn((subject, seed.ARCH + "constrains", f"<{target}>", seed.GRAPH), rows)
        self.assertIn(f"Relation: constrains -> {right['id']}", notes)

    def test_accumulation_has_no_scored_seed_or_future_episode_hint(self):
        scenario = load_scenario("retry_ledger")
        self.assertEqual(scenario["initial_facts"], [])
        graph = seed.seed_graph(scenario)
        notes = seed_notes(scenario["initial_facts"])
        for kind in reviewer.KINDS:
            self.assertNotIn(f"<{seed.ARCH}{kind}>", graph)
        # This fixture's full title reveals account evolution introduced only in e2.
        self.assertNotIn(scenario["title"], graph)
        self.assertNotIn(scenario["title"], notes)
        self.assertNotIn("retry-account", graph)

    def test_condition_preparation_contains_only_its_seed_representation(self):
        scenario = load_scenario("ruleset_cache")
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for condition in ("without", "codex_mcp", "opencode_mcp", "harness"):
                workspace = root / condition
                workspace.mkdir()
                seed.prepare_workspace(workspace, scenario, condition)
                if condition == "without":
                    self.assertTrue((workspace / "PROJECT_NOTES.md").is_file())
                    self.assertFalse((workspace / ".moosedev").exists())
                else:
                    self.assertTrue((workspace / ".moosedev/kg.nq").is_file())
                    self.assertFalse((workspace / "PROJECT_NOTES.md").exists())
                self.assertFalse((workspace / "gold.json").exists())
                self.assertFalse((workspace / "SOURCE_MAPPING.md").exists())

    def test_prompts_share_requirements_and_clarifications_but_not_gold(self):
        scenario = load_scenario("retry_ledger")
        for episode in scenario["episodes"]:
            for condition in ("without", "codex_mcp", "opencode_mcp", "harness"):
                prompt = seed.episode_prompt(episode, condition)
                self.assertTrue(prompt.startswith(episode["prompt"]))
                for clarification in episode["clarifications"].values():
                    self.assertIn(clarification, prompt)
                self.assertNotIn(episode["hidden_test"], prompt)
                self.assertNotIn("forbidden_claims", prompt)

    def test_both_mcp_arms_are_asked_for_exactly_the_same_thing(self):
        # The two MCP conditions are offered the same tools. If their guidance
        # ever diverged the arms would stop being comparable, silently.
        self.assertEqual(seed.GUIDANCE["opencode_mcp"], seed.GUIDANCE["codex_mcp"])
        episode = load_scenario("retry_ledger")["episodes"][0]
        self.assertEqual(seed.episode_prompt(episode, "opencode_mcp"),
                         seed.episode_prompt(episode, "codex_mcp"))
        self.assertNotEqual(seed.episode_prompt(episode, "opencode_mcp"),
                            seed.episode_prompt(episode, "without"))


class ReviewerControls(unittest.TestCase):
    def setUp(self):
        self.episode = {"allowed_paths": ["**/*.py", "README.md", "PROJECT_NOTES.md"]}

    @staticmethod
    def state(phase, **fields):
        return {"busy": False, "task": dict(phase=phase, **fields)}

    def test_in_scope_plan_and_exact_policy_edit_are_approved(self):
        state = self.state("AwaitingPlan", plan={"summary": "Add cache", "files": ["service.py", "tests/test_a.py"],
                                                  "checks": ["python3 -m unittest discover -s tests"]})
        self.assertEqual(reviewer.review_input(state, self.episode)["input"], "/approve")
        edit = self.state("AwaitingPolicy", pending_edit={"file": "service.py", "before": "old", "after": "new"})
        self.assertEqual(reviewer.review_input(edit, self.episode)["input"], "/approve")

    def test_scope_review_does_not_grade_false_or_stale_claims(self):
        # Deliberately wrong claim: valid simulation acceptance must earn no semantic credit.
        request = {"operation_id": "capture-1", "proposals": [{"kind": "Constraint", "title": "Global IDs forever",
                   "description": "IDs remain global across accounts and retries should apply twice.",
                   "files": ["service.py"], "evidence": ["The model asserted this unsupported rule."]}]}
        state = self.state("AwaitingReview", reviews=[{"request": request}])
        choice = reviewer.review_input(state, self.episode)
        self.assertEqual(choice["input"], "/accept capture-1")
        self.assertIn("semantic truth unassessed", choice["reason"])
        self.assertNotIn("score", choice)
        self.assertNotIn("correct", choice)

    def test_busy_controller_never_receives_approval_or_terminal_verdict(self):
        for phase in ("AwaitingPlan", "AwaitingPolicy", "AwaitingPermission", "AwaitingReview", "Complete"):
            state = self.state(phase)
            state["busy"] = True
            self.assertIsNone(reviewer.review_input(state, self.episode))

    def test_unexpected_permission_request_is_deterministically_denied(self):
        state = self.state("AwaitingPermission", id="task-7", pending_permission={
            "request_id": "permission-3", "command": ["git", "fetch"],
            "justification": "refresh refs", "read_paths": [], "write_paths": [], "network": True})
        self.assertEqual(reviewer.review_input(state, self.episode), {
            "input": "/deny",
            "reason": "frozen study denies unexpected permission requests",
            "kind": "permission_denial",
        })

    def test_outside_scope_cannot_be_approved(self):
        for path in ("/tmp/out.py", "../out.py", "pkg/../../out.py", "pkg\\out.py", "secrets.txt", ""):
            with self.subTest(path=path):
                self.assertFalse(reviewer.in_scope(path, self.episode["allowed_paths"]))
                state = self.state("AwaitingPolicy", pending_edit={"file": path, "after": "new"})
                choice = reviewer.review_input(state, self.episode)
                self.assertEqual(choice["terminal"], "agent_failure")
                self.assertNotIn("input", choice)
        plan = self.state("AwaitingPlan", plan={"summary": "Out of scope", "files": ["secret.txt"], "checks": ["true"]})
        self.assertEqual(reviewer.review_input(plan, self.episode)["terminal"], "agent_failure")

    def test_malformed_or_outside_scope_proposals_are_rejected(self):
        valid = {"kind": "Lesson", "title": "Title", "description": "Description", "evidence": ["event"], "files": ["service.py"]}
        for malformed in (dict(valid, kind="Freeform"), dict(valid, evidence=[]), dict(valid, title=""),
                          dict(valid, files=["../outside.py"])):
            state = self.state("AwaitingReview", reviews=[{"request": {"operation_id": "op", "proposals": [malformed]}}])
            self.assertEqual(reviewer.review_input(state, self.episode)["input"], "/reject op")

    def test_no_change_is_an_explicit_simulation_confirmation(self):
        state = self.state("AwaitingReview", reviews=[], capture_request=None)
        choice = reviewer.review_input(state, self.episode)
        self.assertEqual(choice["input"], "/no-knowledge")
        self.assertIn("completeness unassessed", choice["reason"])

    def test_terminal_failure_and_fixed_clarification(self):
        self.assertEqual(reviewer.review_input(self.state("Complete"), self.episode),
                         {"terminal": "success", "cause": "success"})
        self.assertEqual(reviewer.review_input(self.state("Cancelled"), self.episode)["terminal"], "agent_failure")
        errored = self.state("AwaitingInput", last_error="capture unavailable")
        self.assertEqual(reviewer.review_input(errored, self.episode)["reason"], "capture unavailable")
        first = reviewer.review_input(self.state("AwaitingInput"), self.episode)
        second = reviewer.review_input(self.state("Working", turn_finished=True), self.episode)
        self.assertEqual(first, second)
        self.assertEqual(first["reason"], "frozen clarification response")

    def test_native_repair_controls_waiting_and_exhaustion_without_new_guidance(self):
        for status in ("generating", "retrying"):
            state = self.state("Working", last_error="previous rejection",
                               recovery={"status": status, "attempts": 2})
            self.assertIsNone(reviewer.review_input(state, self.episode))
        exhausted = self.state("AwaitingInput", recovery={"status": "awaiting_guidance", "attempts": 3,
                                                        "diagnostic": "unknown capture target"})
        choice = reviewer.review_input(exhausted, self.episode)
        self.assertEqual(choice["terminal"], "agent_failure")
        self.assertNotIn("input", choice)
        self.assertIn("unknown capture target", choice["reason"])
        paused = self.state("Working", last_error="daemon unavailable", recovery={"status": "paused"})
        self.assertEqual(reviewer.review_input(paused, self.episode)["reason"], "daemon unavailable")


if __name__ == "__main__":
    unittest.main()
