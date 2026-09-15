"""The exploratory field-check mode and its model table; every sealed identity stays byte-identical."""
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from bench.harness_study import config, evolution, field_check, intent, long_horizon, model_table
from bench.harness_study.artifacts import canonical_json
from bench.harness_study.scenario import MAINTENANCE

DOCS = Path(evolution.__file__).parent
TARGET = DOCS.parents[1] / "target"
PINNED = {
    "intent": "f28f83ca29deff64f2009817a34ac59f50347e939dff423ec5388811321f236f",
    evolution.STAGE1_MODE: "44b6175c2e4a0841844784d439dd696393ab9496a53d8fc32eb193f2371609f8",
    evolution.STAGE2_MODE: "e8fec638c4f691ad087c7b6ec5b81362b087d8c20a80079fb40c9b615509074c",
    evolution.STAGE2_RECOVERY_MODE: "763ccc521e31595001d67dc15c9bddcd0c288443ae70184d0a20195914f340db",
    evolution.STAGE2_BASELINE_MODE: "17205d5a09af1075de01548def24716cf1fb112e882eae6fc1fce3ae2eda1115",
}
SEALED_DOCS = {
    "BASELINE.md": "65862780eb3b2af6edd7e6ce2790b06d33d8f2a274c0139d6740925c0800df5e",
    "RECOVERY.md": "4a3c1ea031e4f69d05a432d70f8d93f76f238801f020c0e079a01046d9cfcf99",
    "EVOLUTION.md": "e912e64384d3c28908492e3a7178e78fdf9eaba2c6d55d5e843093fbae507386",
    "INTENT.md": "fd4289e894e6599b6b3f9abc7c49d9505b1d6649c4d0b0d8816581e7973d8dbe",
}
G31B = "gemma-4-31b-it"
Q9B = "qwen/qwen3.5-9b"
L70 = "llama-3.3-70b-instruct"
H70 = "nousresearch/hermes-4-70b"
A4B = "google/gemma-4-26b-a4b"
QWEN = "qwen/qwen3.8-27b"
HELPER = model_table.HELPER
FIXTURE_INDEX = "/fixture/.lmstudio/.internal/model-index-cache.json"
FIXTURE_ROOT = Path("/fixture/.lmstudio/models")


def field_config(models=(G31B,), scenarios=("retry_ledger", MAINTENANCE)):
    models, scenarios = list(models), list(scenarios)
    return {"evaluation_mode": field_check.MODE, "study_id": "field-check-fixture", "seed": 1,
            "lmstudio_index": FIXTURE_INDEX, "coding_models": models, "helper_model": HELPER,
            "local_models": model_table.config_entries([*models, HELPER], FIXTURE_ROOT),
            "scenario_ids": scenarios, "intent_policies": ["symbolic"], "episode_limit": 1,
            "episode_seconds": 1200, "context_tokens": 32768, "generation_policy": {"local_temperature": 0.0},
            "harness_response_policy": "reasoning-off",
            "evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
            "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT,
            "field_check_design": field_check.design_identity(models, scenarios)}


class SealedIdentityTests(unittest.TestCase):
    def test_existing_identities_documents_and_models_are_unchanged(self):
        self.assertEqual(intent.design_identity()["sha256"], PINNED["intent"])
        for mode in evolution.MODES:
            if mode in PINNED:
                self.assertEqual(evolution.design_identity(mode)["sha256"], PINNED[mode], mode)
        for name, digest in SEALED_DOCS.items():
            self.assertEqual(hashlib.sha256((DOCS / name).read_bytes()).hexdigest(), digest, name)
        self.assertEqual(intent.MODELS, (QWEN, "gemma-4-e4b-it-mlx"))
        self.assertNotIn(field_check.MODE, evolution.MODES)
        self.assertIs(field_check.ARMS, evolution.ARMS[evolution.SYMBOLIC_BASELINE_MODE])
        self.assertEqual(set(evolution.ARMS), {evolution.STAGE2_BASELINE_MODE, evolution.SYMBOLIC_BASELINE_MODE})
        self.assertIn(field_check.MODE, config.HARNESS_MODES)
        self.assertEqual(config.HARNESS_MODES[:-1], (intent.MODE, *evolution.MODES))


class ModelTableTests(unittest.TestCase):
    def test_rows_entries_and_unknown_models(self):
        self.assertEqual(HELPER, intent.MODELS[1])
        self.assertEqual(set(model_table.MODELS), {QWEN, A4B, G31B, Q9B, L70, H70, HELPER})
        for model in intent.MODELS:
            self.assertIn(model, model_table.MODELS)
        for model, row in model_table.MODELS.items():
            self.assertEqual(len(row["weights_sha256"]), 64, model)
            int(row["weights_sha256"], 16)
            self.assertIs(type(row["runtime_context_tokens"]), int)
        self.assertEqual({model: row["runtime_context_tokens"] for model, row in model_table.MODELS.items()},
                         {QWEN: 262144, A4B: 262144, G31B: 262144, Q9B: 262144, L70: 32768, H70: 32768, HELPER: 131072})
        self.assertEqual(model_table.models_root({"lmstudio_index": FIXTURE_INDEX}), FIXTURE_ROOT)
        self.assertEqual(model_table.config_entries([HELPER, HELPER], FIXTURE_ROOT), [
            {"id": HELPER, "weights": str(FIXTURE_ROOT / "lmstudio-community/gemma-4-E4B-it-MLX-4bit"),
             "runtime_context_tokens": 131072}])
        self.assertEqual(model_table.row(G31B)["weights"], "mlx-community/gemma-4-31b-it-5bit")
        with self.assertRaisesRegex(ValueError, "not in the model table"):
            model_table.row("unknown/model")

    def test_fingerprints_must_equal_the_table(self):
        cfg = {"local_models": model_table.config_entries([G31B, HELPER], FIXTURE_ROOT)}
        result = {f"local_model_{index}": {"weights_sha256": model_table.row(model["id"])["weights_sha256"]}
                  for index, model in enumerate(cfg["local_models"])}
        self.assertEqual([row["id"] for row in model_table.verify_fingerprints(cfg, result)], [G31B, HELPER])
        result["local_model_1"]["weights_sha256"] = "0" * 64
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            model_table.verify_fingerprints(cfg, result)
        with self.assertRaisesRegex(ValueError, "fingerprint"):
            model_table.verify_fingerprints(cfg, {})

    PREFLIGHTS = [TARGET / "harness-fixes-check-v1/preflight.json", TARGET / "harness-a4b-check-v1/preflight.json"]

    @unittest.skipUnless(all(path.is_file() for path in PREFLIGHTS), "field-check preflights are not on this machine")
    def test_pins_equal_fingerprints_frozen_by_ready_preflights(self):
        seen = set()
        for path in self.PREFLIGHTS:
            frozen = json.loads(path.read_text())
            self.assertTrue(frozen["ready"], path)
            for index, model in enumerate(frozen["config"]["local_models"]):
                row = model_table.row(model["id"])
                self.assertEqual(frozen[f"local_model_{index}"]["weights_sha256"], row["weights_sha256"], model["id"])
                self.assertEqual(model["runtime_context_tokens"], row["runtime_context_tokens"], model["id"])
                self.assertTrue(model["weights"].endswith(row["weights"]), model["id"])
                seen.add(model["id"])
        self.assertEqual(seen, {QWEN, A4B, HELPER})


class DesignIdentityTests(unittest.TestCase):
    def test_design_binds_the_selection_and_its_inherited_identities(self):
        identity = field_check.design_identity([G31B], ["retry_ledger", MAINTENANCE])
        payload = identity["payload"]
        self.assertEqual(identity["sha256"], hashlib.sha256(canonical_json(payload)).hexdigest())
        self.assertEqual(payload["mode"], field_check.MODE)
        self.assertIn("never scored", payload["interpretation"])
        self.assertIn("never pooled", payload["interpretation"])
        self.assertEqual(payload["coding_models"], [model_table.row(G31B)])
        self.assertEqual(payload["helper_model"], model_table.row(HELPER))
        self.assertEqual(payload["arms"], [
            {"backend": "harness", "condition": "harness", "intent_policy": "symbolic"},
            {"backend": "opencode", "condition": "without", "intent_policy": None}])
        self.assertEqual(payload["scenarios"], ["retry_ledger", MAINTENANCE])
        self.assertEqual(payload["episode_limit"], 1)
        self.assertEqual(payload["harness_response_policy"], "reasoning-off")
        self.assertEqual(payload["client_context_tokens"], 32768)
        self.assertEqual(payload["episode_seconds"], 1200)
        self.assertEqual(payload["evidence_byte_limit"], evolution.BASELINE_EVIDENCE_BYTE_LIMIT)
        self.assertEqual(payload["reject_loop_limit"], evolution.BASELINE_REJECT_LOOP_LIMIT)
        self.assertEqual(payload["inherits"], {
            "mode": evolution.SYMBOLIC_BASELINE_MODE,
            "design_sha256": evolution.design_identity(evolution.SYMBOLIC_BASELINE_MODE)["sha256"]})
        self.assertEqual(payload["intent_design_sha256"], PINNED["intent"])
        self.assertEqual(payload["document_sha256"],
                         hashlib.sha256((DOCS / "FIELD_CHECK.md").read_bytes()).hexdigest())
        self.assertEqual(payload["scenario_tables"], {})

    def test_long_horizon_selection_binds_its_resolution_tables(self):
        payload = field_check.design_identity([G31B], ["retry_ledger", "late_fees"])["payload"]
        self.assertEqual(payload["scenario_tables"], {"late_fees": {
            "resolution_targets": long_horizon.RESOLUTION_TARGETS["late_fees"],
            "seed_associations": long_horizon.SEED_ASSOCIATIONS["late_fees"]}})
        base = field_check.design_identity([G31B], ["late_fees"])["sha256"]
        with patch.dict(long_horizon.SEED_ASSOCIATIONS, {"late_fees": []}):
            self.assertNotEqual(base, field_check.design_identity([G31B], ["late_fees"])["sha256"])
        for name in (*long_horizon.SCENARIOS, *getattr(long_horizon, "EXPLORATORY", ())):
            with self.subTest(name=name):
                self.assertIn(name, field_check.SCENARIOS)

    def test_design_changes_with_order_scenarios_and_table_rows(self):
        base = field_check.design_identity([G31B, A4B], ["retry_ledger"])["sha256"]
        self.assertNotEqual(base, field_check.design_identity([A4B, G31B], ["retry_ledger"])["sha256"])
        self.assertNotEqual(base, field_check.design_identity([G31B, A4B], ["retry_ledger", MAINTENANCE])["sha256"])
        changed = dict(model_table.MODELS[G31B], runtime_context_tokens=131072)
        with patch.dict(model_table.MODELS, {G31B: changed}):
            self.assertNotEqual(base, field_check.design_identity([G31B, A4B], ["retry_ledger"])["sha256"])
        self.assertEqual(base, field_check.design_identity([G31B, A4B], ["retry_ledger"])["sha256"])

    def test_design_refuses_invalid_selections(self):
        for models, scenarios in (([], ["retry_ledger"]), ([G31B, G31B], ["retry_ledger"]),
                                  (["unknown/model"], ["retry_ledger"]), ([G31B], ["no_such_package"]),
                                  ([G31B], []), ([G31B], ["retry_ledger", "retry_ledger"])):
            with self.subTest(models=models, scenarios=scenarios), self.assertRaises(ValueError):
                field_check.design_identity(models, scenarios)


class ScheduleTests(unittest.TestCase):
    def test_schedule_groups_by_model_then_scenario_with_harness_first(self):
        cfg = field_config((G31B, A4B))
        cells = config.schedule(cfg)
        self.assertEqual([(c["model"], c["scenario_id"], c["backend"], c["condition"], c["intent_policy"])
                          for c in cells], [
            (model, scenario, backend, condition, policy)
            for model in (G31B, A4B) for scenario in ("retry_ledger", MAINTENANCE)
            for backend, condition, policy in (("harness", "harness", "symbolic"), ("opencode", "without", None))])
        self.assertEqual([cell["schedule_index"] for cell in cells], list(range(8)))
        self.assertEqual(config.schedule(dict(cfg, seed=99)), cells)
        self.assertEqual(config.required_clients(cfg), ("opencode", "lms"))
        self.assertIs(config.frozen_arms(field_check.MODE), field_check.ARMS)
        self.assertIs(config.frozen_arms(evolution.SYMBOLIC_BASELINE_MODE),
                      evolution.ARMS[evolution.SYMBOLIC_BASELINE_MODE])
        self.assertIsNone(config.frozen_arms(intent.MODE))

    def test_schedule_refuses_any_drift_from_the_table_or_design(self):
        base = field_config()

        def entry(model, **changes):
            return dict(model_table.config_entries([model], FIXTURE_ROOT)[0], **changes)

        mutations = {
            "model outside table": dict(base, coding_models=["unknown/model"]),
            "wrong context": dict(base, local_models=[entry(G31B, runtime_context_tokens=131072), entry(HELPER)]),
            "wrong weights": dict(base, local_models=[entry(G31B, weights="/elsewhere/gemma"), entry(HELPER)]),
            "missing helper entry": dict(base, local_models=[entry(G31B)]),
            "extra entry": dict(base, local_models=[entry(G31B), entry(HELPER), entry(QWEN)]),
            "changed helper": dict(base, helper_model="other-helper"),
            "auto response policy": dict(base, harness_response_policy="auto"),
            "episode limit": dict(base, episode_limit=2),
            "policy": dict(base, intent_policies=["current"]),
            "client context": dict(base, context_tokens=16384),
            "episode seconds": dict(base, episode_seconds=600),
            "temperature": dict(base, generation_policy={"local_temperature": 0.7}),
            "evidence limit": dict(base, evidence_byte_limit=1),
            "stale design": dict(base, field_check_design=field_check.design_identity([G31B], ["retry_ledger"])),
        }
        self.assertEqual(len(config.schedule(base)), 4)
        for label, mutated in mutations.items():
            with self.subTest(label), self.assertRaises(ValueError):
                config.schedule(mutated)


class FieldCheckConfigTests(unittest.TestCase):
    def test_config_derives_from_a_symbolic_parent_and_refuses_bad_parents(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            pilot = {"study_id": "pilot", "evaluation_mode": intent.MODE, "seed": 7,
                     "gold_approval": str(root / "approval.json"), "opencode": str(root / "opencode"),
                     "scenario_ids": list(intent.SCENARIOS), "local_models": [{"id": m} for m in intent.MODELS],
                     "generation_policy": {"local_temperature": 0.0},
                     "lmstudio_index": str(root / ".lmstudio/.internal/model-index-cache.json")}
            (root / "approval.json").write_text(canonical_json(config.approval_payload("fixture", config=pilot)).decode())
            symbolic = dict(pilot, study_id="symbolic-parent", evaluation_mode=evolution.SYMBOLIC_BASELINE_MODE,
                            intent_policies=["symbolic"], episode_limit=1,
                            evidence_byte_limit=evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
                            reject_loop_limit=evolution.BASELINE_REJECT_LOOP_LIMIT,
                            evolution_design=evolution.design_identity(evolution.SYMBOLIC_BASELINE_MODE))

            def parent(original, build_id="parent-build"):
                return {"ready": True, "config": original, "config_sha256": config.configuration_hash(original),
                        "binaries": {"build_id": build_id}}

            manifest, approval = root / "manifest.json", root / "stage/approval.json"
            with patch.object(config, "verify_binaries", return_value={"build_id": "parent-build"}):
                derived = config.field_check_config(parent(symbolic), manifest, "g31b-v1", [G31B],
                                                    ["retry_ledger"], approval)
                self.assertEqual(derived["evaluation_mode"], field_check.MODE)
                self.assertEqual(derived["study_id"], "g31b-v1")
                self.assertEqual(derived["coding_models"], [G31B])
                self.assertEqual(derived["helper_model"], HELPER)
                self.assertEqual(derived["local_models"],
                                 model_table.config_entries([G31B, HELPER], root / ".lmstudio/models"))
                self.assertEqual(derived["scenario_ids"], ["retry_ledger"])
                self.assertEqual(derived["intent_policies"], ["symbolic"])
                self.assertEqual((derived["episode_limit"], derived["episode_seconds"], derived["context_tokens"]),
                                 (1, 1200, 32768))
                self.assertEqual(derived["harness_response_policy"], "reasoning-off")
                self.assertEqual(derived["field_check_design"], field_check.design_identity([G31B], ["retry_ledger"]))
                self.assertEqual(derived["gold_approval"], str(approval.resolve()))
                self.assertEqual(derived["binary_manifest"], str(manifest.resolve()))
                self.assertEqual(derived["opencode"], str(root / "opencode"))
                self.assertNotIn("evolution_design", derived)
                self.assertEqual(derived["parent_pilot"], {
                    "study_id": "symbolic-parent", "config_sha256": config.configuration_hash(symbolic),
                    "build_id": "parent-build", "evolution_design_sha256": symbolic["evolution_design"]["sha256"]})
                self.assertEqual(len(config.schedule(derived)), 2)
                refusals = {
                    "symbolic-baseline": parent(pilot),
                    "intact": dict(parent(symbolic), config_sha256="0" * 64),
                }
                for message, bad in refusals.items():
                    with self.subTest(message), self.assertRaisesRegex(ValueError, message):
                        config.field_check_config(bad, manifest, "g31b-v1", [G31B], ["retry_ledger"], approval)
                with self.assertRaisesRegex(ValueError, "intact"):
                    config.field_check_config(dict(parent(symbolic), ready=False), manifest, "g31b-v1", [G31B],
                                              ["retry_ledger"], approval)
                with self.assertRaisesRegex(ValueError, "distinct"):
                    config.field_check_config(parent(symbolic), manifest, "symbolic-parent", [G31B],
                                              ["retry_ledger"], approval)
                with self.assertRaisesRegex(ValueError, "opencode"):
                    config.field_check_config(parent(dict(symbolic, opencode=None)), manifest, "g31b-v1", [G31B],
                                              ["retry_ledger"], approval)
            for sealed in evolution.SEALED_SYMBOLIC_PREDECESSORS:
                with patch.object(config, "verify_binaries", return_value={"build_id": sealed}):
                    with self.assertRaisesRegex(ValueError, "sealed"):
                        config.field_check_config(parent(symbolic), manifest, "g31b-v1", [G31B],
                                                  ["retry_ledger"], approval)

    PARENT = TARGET / "harness-fixes-check-v1/preflight.json"

    @unittest.skipUnless(PARENT.is_file(), "field-check parent preflight is not on this machine")
    def test_config_derives_from_the_field_check_parent_on_disk(self):
        parent = json.loads(self.PARENT.read_text())
        if parent["config"].get("evolution_design") != evolution.design_identity(evolution.SYMBOLIC_BASELINE_MODE):
            self.skipTest("the on-disk parent predates the current symbolic design")
        with tempfile.TemporaryDirectory() as temporary, \
                patch.object(config, "verify_binaries", return_value={"build_id": "fresh-build"}):
            derived = config.field_check_config(parent, self.PARENT.with_name("manifest.json"), "g31b-probe",
                                                [G31B], ["retry_ledger"], Path(temporary) / "approval.json")
        root = model_table.models_root(parent["config"])
        self.assertEqual(derived["local_models"][0]["weights"], str(root / "mlx-community/gemma-4-31b-it-5bit"))
        self.assertEqual(len(config.schedule(derived)), 2)


class ApprovalTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write(self, name, value):
        path = self.root / name
        path.write_text(canonical_json(value).decode())
        return path

    def test_field_check_approval_round_trips(self):
        cfg = field_config((G31B,), ("retry_ledger",))
        approved = config.approval_payload("James Adam", config=cfg)
        self.assertEqual(approved["schema_version"], 3)
        self.assertEqual(approved["intent_design"], intent.design_identity())
        self.assertEqual(approved["field_check_design"], cfg["field_check_design"])
        self.assertEqual(set(approved["scenarios"]), {"retry_ledger"})
        self.assertIn("never scored", approved["scope"])
        self.assertEqual(config.verify_approval(self.write("approval.json", approved), config=cfg), approved)

    def test_field_check_approval_round_trips_for_a_long_horizon_package(self):
        cfg = field_config((G31B,), ("late_fees",))
        approved = config.approval_payload("James Adam", config=cfg)
        scenario = config.load_scenario("late_fees")
        self.assertEqual(approved["scenarios"], {"late_fees": {
            "package_sha256": scenario["package_sha256"], "gold_sha256": scenario["gold_sha256"]}})
        self.assertEqual(config.verify_approval(self.write("long.json", approved), config=cfg), approved)

    def test_approvals_bind_mode_models_scenarios_and_hashes(self):
        cfg = field_config((G31B,), ("retry_ledger",))
        approved = config.approval_payload("James Adam", config=cfg)
        pilot = {"evaluation_mode": intent.MODE, "scenario_ids": list(intent.SCENARIOS),
                 "local_models": [{"id": m} for m in intent.MODELS], "seed": 1}
        symbolic = dict(pilot, evaluation_mode=evolution.SYMBOLIC_BASELINE_MODE, intent_policies=["symbolic"],
                        evolution_design=evolution.design_identity(evolution.SYMBOLIC_BASELINE_MODE))
        intent_approval = self.write("intent.json", config.approval_payload("fixture", config=pilot))
        field_approval = self.write("field.json", approved)
        with self.assertRaisesRegex(ValueError, "field-check approval"):
            config.verify_approval(intent_approval, config=cfg)
        with self.assertRaisesRegex(ValueError, "field-check approval"):
            config.verify_approval(field_approval, config=symbolic)
        with self.assertRaisesRegex(ValueError, "field-check approval"):
            config.verify_approval(field_approval)
        with self.assertRaisesRegex(ValueError, "design"):
            config.verify_approval(field_approval, config=field_config((A4B,), ("retry_ledger",)))
        other_scenarios = dict(deepcopy(approved), scenarios={MAINTENANCE: approved["scenarios"]["retry_ledger"]})
        with self.assertRaisesRegex(ValueError, "scenario set"):
            config.verify_approval(self.write("scenarios.json", other_scenarios), config=cfg)
        with patch.dict(model_table.MODELS, {G31B: dict(model_table.MODELS[G31B], runtime_context_tokens=131072)}):
            altered = field_check.design_identity([G31B], ["retry_ledger"])
        with self.assertRaisesRegex(ValueError, "design"):
            config.verify_approval(self.write("context.json", dict(approved, field_check_design=altered)), config=cfg)
        with self.assertRaisesRegex(ValueError, "design"):
            config.verify_approval(field_approval, config=dict(cfg, field_check_design=altered))
        with self.assertRaisesRegex(ValueError, "intent design"):
            config.verify_approval(self.write("intent-design.json", dict(approved, intent_design={"sha256": "0"})),
                                   config=cfg)
        with patch.object(config, "load_scenario", return_value={"package_sha256": "0" * 64, "gold_sha256": "0" * 64}):
            with self.assertRaisesRegex(ValueError, "package hashes"):
                config.verify_approval(field_approval, config=cfg)


class PreflightTests(unittest.TestCase):
    def run_preflight(self, weights_sha256=None, scenarios=("retry_ledger",)):
        cfg = field_config((G31B,), scenarios)
        self.probe_calls = []
        cfg.update(binary_manifest="/fixture/manifest.json", gold_approval="/fixture/approval.json",
                   endpoint="http://127.0.0.1:1234/v1", opencode="/fixture/opencode", lms="/fixture/lms",
                   indexer_manifest="/fixture/indexer/manifest.json")
        indexer = {"directory": "/fixture/indexer"}

        def fingerprint(model, available):
            digest = model_table.row(model["id"])["weights_sha256"]
            if weights_sha256 and model["id"] == G31B:
                digest = weights_sha256
            return {"id": model["id"], "weights": model["weights"], "files": {}, "weights_sha256": digest}

        fake_indexing = SimpleNamespace(verify_indexer=lambda manifest: indexer,
                                        probe_indexer=lambda *args, **kwargs: self.probe_calls.append(kwargs) or {"ok": True},
                                        system_python_identity=lambda *args: {"path": "/usr/bin/python3"})
        with patch.object(config, "verify_binaries", return_value={"build_id": "b", "indexer": indexer}), \
                patch.object(config, "client_identity", return_value={"path": "/fixture/client"}), \
                patch.object(config, "inventory", return_value={"models": []}), \
                patch.object(config, "model_associations", return_value={}), \
                patch.object(config, "freeze_assets", return_value={"directory": "/fixture/assets"}), \
                patch.object(config, "fingerprint_model", side_effect=fingerprint), \
                patch.object(config, "verify_approval", return_value={"schema_version": 3}), \
                patch.dict(sys.modules, {"bench.harness_study.indexing": fake_indexing}):
            return config.preflight(cfg)

    def test_preflight_checks_design_and_table_weights_for_every_model(self):
        result = self.run_preflight()
        checks = {item["check"]: item["passed"] for item in result["checks"]}
        self.assertTrue(result["ready"], result["checks"])
        for name in ("field_check_design", "model_table_weights", "local_model_0", "local_model_1",
                     "intent_design", "indexer_probe", "gold_approval", "model_associations"):
            self.assertTrue(checks[name], name)
        self.assertNotIn("evolution_design", checks)
        self.assertNotIn("native_intent_contracts", checks)
        self.assertEqual(result["local_model_1"]["id"], HELPER)
        self.assertEqual(len(result["schedule"]), 2)

    def test_preflight_probes_intent_packages_plus_the_selected_ones(self):
        result = self.run_preflight(scenarios=("late_fees",))
        self.assertTrue(result["ready"], result["checks"])
        self.assertEqual(list(self.probe_calls[0]["scenarios"]), [*intent.SCENARIOS, "late_fees"])
        self.run_preflight(scenarios=("retry_ledger",))
        self.assertEqual(list(self.probe_calls[0]["scenarios"]), list(intent.SCENARIOS))

    def test_preflight_is_not_ready_when_weights_differ_from_the_table(self):
        result = self.run_preflight(weights_sha256="0" * 64)
        checks = {item["check"]: item["passed"] for item in result["checks"]}
        self.assertFalse(result["ready"])
        self.assertFalse(checks["model_table_weights"])

class ResponsePolicyVariantTests(unittest.TestCase):
    def test_default_policy_keeps_every_existing_identity(self):
        base = field_check.design_identity([G31B], ["retry_ledger"])
        self.assertEqual(base, field_check.design_identity([G31B], ["retry_ledger"], "reasoning-off"))
        self.assertNotIn("response_policy_deviation", base["payload"])

    def test_provider_default_is_a_distinct_design_that_records_its_deviation(self):
        base = field_check.design_identity([G31B], ["retry_ledger"])
        thinking = field_check.design_identity([G31B], ["retry_ledger"], "provider-default")
        self.assertNotEqual(base["sha256"], thinking["sha256"])
        self.assertEqual(thinking["payload"]["harness_response_policy"], "provider-default")
        self.assertIn("FIELD_CHECK.md", thinking["payload"]["response_policy_deviation"])
        with self.assertRaises(ValueError):
            field_check.design_identity([G31B], ["retry_ledger"], "auto")

    def test_provider_default_config_schedules_and_its_approval_round_trips(self):
        cfg = dict(field_config((G31B,), ("retry_ledger",)), harness_response_policy="provider-default",
                   field_check_design=field_check.design_identity([G31B], ["retry_ledger"], "provider-default"))
        self.assertEqual(len(config.schedule(cfg)), 2)
        with self.assertRaises(ValueError):
            config.schedule(dict(cfg, field_check_design=field_check.design_identity([G31B], ["retry_ledger"])))
        approved = config.approval_payload("James Adam", config=cfg)
        self.assertIn("provider-default", approved["scope"])
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "approval.json"
            path.write_text(json.dumps(approved))
            self.assertEqual(config.verify_approval(path, config=cfg), approved)
            with self.assertRaisesRegex(ValueError, "design"):
                config.verify_approval(path, config=field_config((G31B,), ("retry_ledger",)))

class ModelsRootTests(unittest.TestCase):
    def test_models_root_follows_lm_studio_downloads_folder(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory) / ".lmstudio"
            (home / ".internal").mkdir(parents=True)
            config_value = {"lmstudio_index": str(home / ".internal/model-index-cache.json")}
            self.assertEqual(model_table.models_root(config_value), home / "models")
            elsewhere = Path(directory) / "external" / "models"
            (home / "settings.json").write_text(json.dumps({"downloadsFolder": str(elsewhere)}))
            self.assertEqual(model_table.models_root(config_value), elsewhere)
            self.assertEqual(model_table.config_entries([HELPER], model_table.models_root(config_value))[0]["weights"],
                             str(elsewhere / model_table.row(HELPER)["weights"]))
            (home / "settings.json").write_text(json.dumps({"downloadsFolder": "relative/models"}))
            with self.assertRaisesRegex(ValueError, "absolute"):
                model_table.models_root(config_value)
            (home / "settings.json").write_text(json.dumps({"downloadsFolder": ""}))
            self.assertEqual(model_table.models_root(config_value), home / "models")


if __name__ == "__main__":
    unittest.main()
