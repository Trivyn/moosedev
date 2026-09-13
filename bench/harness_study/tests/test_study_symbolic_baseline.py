"""The symbolic two-arm baseline mode and the sealed identities it must leave intact."""
from collections import Counter
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import config, evolution, intent
from bench.harness_study.artifacts import canonical_json
from bench.harness_study.cause import (HARNESS_TERMINAL_CAUSES, SYMBOLIC_HALT_CAUSES, SYMBOLIC_TERMINAL_CAUSES,
                                       TERMINAL_CAUSES, UNATTENDED_HALT_CLASS, classify, last_symbolic_halt)
from bench.harness_study.reviewer import review_input

STAGE = evolution.SYMBOLIC_BASELINE_MODE
DOCS = Path(evolution.__file__).parent
# Every closed campaign's identity and pre-registration, byte-pinned: the
# symbolic mode is additive and must leave these recomputable.
SEALED = {
    "baseline_sha256": "17205d5a09af1075de01548def24716cf1fb112e882eae6fc1fce3ae2eda1115",
    "recovery_sha256": "763ccc521e31595001d67dc15c9bddcd0c288443ae70184d0a20195914f340db",
    "BASELINE.md": "65862780eb3b2af6edd7e6ce2790b06d33d8f2a274c0139d6740925c0800df5e",
    "RECOVERY.md": "4a3c1ea031e4f69d05a432d70f8d93f76f238801f020c0e079a01046d9cfcf99",
    "EVOLUTION.md": "e912e64384d3c28908492e3a7178e78fdf9eaba2c6d55d5e843093fbae507386",
    "INTENT.md": "fd4289e894e6599b6b3f9abc7c49d9505b1d6649c4d0b0d8816581e7973d8dbe",
}
BASELINE_BUILD = "abe95d92da7994ec9a801073899e480d58a492f5987e1957fff67fa45a6e1fb6"
SYMBOLIC_V1_BUILD = "9aa4f75c5265db4c503ab319ffa7909393d593407f115865858b6c1b3f5fdb66"
SYMBOLIC_V2_BUILD = "a0fcebdecde28c1f308a600654de15e035c968b1438035752923274b0cad9aed"


def specification(stage=STAGE):
    return {"evaluation_mode": stage, "scenario_ids": list(intent.SCENARIOS),
            "local_models": [{"id": model} for model in intent.MODELS],
            "intent_policies": list(evolution.POLICIES[stage]), "seed": 20260913}


def parked(kind, *, after=(), phase="AwaitingInput"):
    events = [{"id": "1", "cycle": "c1", "kind": "cycle_started", "detail": ""},
              {"id": "2", "cycle": "c1", "kind": "plan_approved", "detail": ""},
              {"id": "3", "cycle": "c1", "kind": kind, "detail": "labels.py: escape 4, bound 3"}]
    events += [{"id": str(4 + index), "cycle": "c2", "kind": resume, "detail": ""}
               for index, resume in enumerate(after)]
    return {"phase": phase, "last_error_kind": None, "recovery": None, "intent_events": events,
            "last_response": "Provide guidance; pending work is preserved."}


class SealedIdentityTests(unittest.TestCase):
    def test_sealed_identities_and_preregistrations_are_byte_pinned(self):
        baseline = evolution.design_identity(evolution.STAGE2_BASELINE_MODE)
        self.assertEqual(baseline["sha256"], SEALED["baseline_sha256"])
        self.assertEqual(baseline["payload"]["preregistration_sha256"], SEALED["BASELINE.md"])
        self.assertEqual(evolution.design_identity(evolution.STAGE2_RECOVERY_MODE)["sha256"], SEALED["recovery_sha256"])
        for name in ("BASELINE.md", "RECOVERY.md", "EVOLUTION.md", "INTENT.md"):
            self.assertEqual(hashlib.sha256((DOCS / name).read_bytes()).hexdigest(), SEALED[name], name)
        # The sealed sets the old identities hash are untouched by the symbolic additions.
        self.assertEqual(len(evolution.SEALED_PREDECESSORS), 4)
        self.assertNotIn(BASELINE_BUILD, evolution.SEALED_PREDECESSORS)
        for key in ("halt_count", "symbolic_bounds", "reconciliation_thresholds", "runner_changes",
                    "reviewer_terminals", "daemon_llm_sensor"):
            self.assertNotIn(key, baseline["payload"])
            self.assertNotIn(key, baseline["payload"]["primary_outcome"])


class SymbolicDesignTests(unittest.TestCase):
    def test_symbolic_identity_binds_arms_halts_bounds_and_thresholds(self):
        self.assertIn(STAGE, evolution.MODES)
        self.assertEqual(evolution.POLICIES[STAGE], ("symbolic",))
        self.assertFalse(evolution.postedit_association_contract(STAGE))
        identity = evolution.design_identity(STAGE)
        for other in (evolution.STAGE2_BASELINE_MODE, evolution.STAGE2_RECOVERY_MODE):
            self.assertNotEqual(identity["sha256"], evolution.design_identity(other)["sha256"])
        payload = identity["payload"]
        self.assertEqual(payload["stage"], STAGE)
        self.assertEqual(payload["requirement"], evolution.SYMBOLIC_REQUIREMENT)
        self.assertEqual(payload["decision"], evolution.SYMBOLIC_DECISION)
        self.assertIsNone(payload["treatment_difference"])
        self.assertEqual(payload["arms"], [
            {"backend": "harness", "condition": "harness", "intent_policy": "symbolic"},
            {"backend": "opencode", "condition": "without", "intent_policy": None}])
        self.assertIn("harness-symbolic vs opencode-without", payload["primary_outcome"]["definition"])
        self.assertEqual(payload["primary_outcome"]["halt_count"]["classes"], sorted(UNATTENDED_HALT_CLASS))
        self.assertEqual(payload["terminal_cause_classes"], sorted(SYMBOLIC_TERMINAL_CAUSES))
        self.assertEqual(payload["reviewer_terminals"],
                         sorted({"scope_escape_exhausted", "capture_retype_exhausted", "model_repair_exhausted"}))
        self.assertEqual(payload["symbolic_bounds"], {"scope_escapes": 3, "retypes": 3, "noop_continuations": 1})
        self.assertEqual(payload["reconciliation_thresholds"], {
            "MOOSEDEV_RECONCILE_RESTATES": 0.80, "MOOSEDEV_RECONCILE_REFINES": 0.55,
            "MOOSEDEV_RECONCILE_REFINES_CONTAINMENT": 0.60, "MOOSEDEV_RECONCILE_TIEBREAK_BAND": 0.08})
        self.assertEqual(payload["evidence_byte_limit"], evolution.BASELINE_EVIDENCE_BYTE_LIMIT)
        self.assertEqual(payload["reject_loop_limit"], evolution.BASELINE_REJECT_LOOP_LIMIT)
        self.assertEqual(payload["episode_limit"], 1)
        self.assertEqual(payload["runner_changes"], list(evolution.S8_RUNNER_CHANGES))
        self.assertEqual(len(payload["runner_changes"]), 8)
        self.assertEqual(payload["sealed_predecessors"],
                         list(evolution.SEALED_PREDECESSORS) + [BASELINE_BUILD, SYMBOLIC_V1_BUILD, SYMBOLIC_V2_BUILD])
        self.assertEqual(len(payload["sealed_predecessors"]), 7)
        self.assertEqual(payload["preregistration_sha256"],
                         hashlib.sha256((DOCS / "SYMBOLIC.md").read_bytes()).hexdigest())
        self.assertEqual(payload["historical_baseline"], intent.design_identity())

    def test_symbolic_schedule_has_twelve_cells_across_two_arms(self):
        cells = config.schedule(specification())
        self.assertEqual(len(cells), 12)
        self.assertEqual(cells, config.schedule(specification()))
        self.assertEqual([c["schedule_index"] for c in cells], list(range(12)))
        self.assertEqual(Counter((c["backend"], c["condition"], c["intent_policy"]) for c in cells),
                         {("harness", "harness", "symbolic"): 6, ("opencode", "without", None): 6})
        for model in intent.MODELS:
            for scenario in intent.SCENARIOS:
                pair = [c for c in cells if c["model"] == model and c["scenario_id"] == scenario]
                self.assertEqual({(c["backend"], c["intent_policy"]) for c in pair},
                                 {("harness", "symbolic"), ("opencode", None)})
        self.assertEqual(config.required_clients(specification()), ("opencode", "lms"))
        # The frozen arms carry the native arm; the schedule fallback never synthesises harness-only cells.
        self.assertIn(("opencode", "without", None), evolution.ARMS[STAGE])
        with self.assertRaises(ValueError):
            config.schedule(dict(specification(), intent_policies=["current"]))

    def test_symbolic_config_descends_from_the_baseline_parent_and_refuses_sealed_builds(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pilot = {"study_id": "pilot", "evaluation_mode": intent.MODE, "seed": 7,
                     "gold_approval": str(root / "approval.json"), "opencode": str(root / "opencode"),
                     "scenario_ids": list(intent.SCENARIOS), "local_models": [{"id": m} for m in intent.MODELS],
                     "generation_policy": {"local_temperature": 0.0}}
            (root / "approval.json").write_text(canonical_json(config.approval_payload("fixture", config=pilot)).decode())
            baseline = dict(pilot, study_id="baseline-v2", evaluation_mode=evolution.STAGE2_BASELINE_MODE,
                            intent_policies=list(evolution.POLICIES[evolution.STAGE2_BASELINE_MODE]),
                            episode_limit=1, evidence_byte_limit=evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
                            reject_loop_limit=evolution.BASELINE_REJECT_LOOP_LIMIT,
                            evolution_design=evolution.design_identity(evolution.STAGE2_BASELINE_MODE))

            def parent(original, build_id=BASELINE_BUILD):
                return {"ready": True, "config": original, "config_sha256": config.configuration_hash(original),
                        "binaries": {"build_id": build_id}}

            manifest = root / "manifest.json"
            with patch.object(config, "verify_binaries", return_value={"build_id": "fresh-build"}):
                derived = config.evolution_config(parent(baseline), manifest, "symbolic-v1", STAGE)
                self.assertEqual(derived["evaluation_mode"], STAGE)
                self.assertEqual(derived["episode_limit"], 1)
                self.assertEqual(derived["evidence_byte_limit"], evolution.BASELINE_EVIDENCE_BYTE_LIMIT)
                self.assertEqual(derived["reject_loop_limit"], evolution.BASELINE_REJECT_LOOP_LIMIT)
                self.assertEqual(derived["intent_policies"], ["symbolic"])
                self.assertEqual(derived["evolution_design"], evolution.design_identity(STAGE))
                self.assertEqual(derived["parent_pilot"]["study_id"], "baseline-v2")
                self.assertEqual(derived["opencode"], str(root / "opencode"))
                self.assertEqual(len(config.schedule(derived)), 12)
                for original in (pilot, dict(baseline, evaluation_mode=evolution.STAGE2_RECOVERY_MODE,
                                             evolution_design=evolution.design_identity(evolution.STAGE2_RECOVERY_MODE))):
                    self.assertEqual(config.evolution_config(parent(original), manifest, "symbolic-v1", STAGE)
                                     ["evaluation_mode"], STAGE)
                with self.assertRaisesRegex(ValueError, "opencode"):
                    config.evolution_config(parent(dict(baseline, opencode=None)), manifest, "symbolic-v1", STAGE)
                with self.assertRaisesRegex(ValueError, "descend"):
                    config.evolution_config(parent(dict(baseline, evaluation_mode=evolution.STAGE1_MODE)),
                                            manifest, "symbolic-v1", STAGE)
            for sealed in evolution.SEALED_SYMBOLIC_PREDECESSORS:
                with patch.object(config, "verify_binaries", return_value={"build_id": sealed}):
                    with self.assertRaisesRegex(ValueError, "sealed"):
                        config.evolution_config(parent(baseline, "other-parent"), manifest, "symbolic-v1", STAGE)
            self.assertIn(BASELINE_BUILD, evolution.SEALED_SYMBOLIC_PREDECESSORS)
            with patch.object(config, "verify_binaries", return_value={"build_id": "parent-build"}):
                with self.assertRaisesRegex(ValueError, "newly frozen"):
                    config.evolution_config(parent(baseline, "parent-build"), manifest, "symbolic-v1", STAGE)

    BASELINE_PREFLIGHT = DOCS.parents[1] / "target/harness-evolution-stage2-baseline-v2/preflight.json"

    @unittest.skipUnless(BASELINE_PREFLIGHT.is_file(), "closed baseline campaign preflight is not on this machine")
    def test_symbolic_config_derives_from_the_closed_baseline_preflight_on_disk(self):
        parent = json.loads(self.BASELINE_PREFLIGHT.read_text())
        self.assertEqual(parent["config"]["evolution_design"]["sha256"], SEALED["baseline_sha256"])
        self.assertEqual(parent["binaries"]["build_id"], BASELINE_BUILD)
        with patch.object(config, "verify_binaries", return_value={"build_id": "fresh-build"}):
            derived = config.evolution_config(parent, self.BASELINE_PREFLIGHT.with_name("manifest.json"),
                                              "symbolic-probe", STAGE)
        self.assertEqual(derived["evaluation_mode"], STAGE)
        self.assertEqual(derived["intent_policies"], ["symbolic"])
        self.assertEqual(derived["parent_pilot"]["build_id"], BASELINE_BUILD)
        self.assertEqual(len(config.schedule(derived)), 12)


class SymbolicHaltTests(unittest.TestCase):
    def test_closed_sets_are_supersets_of_the_sealed_tables(self):
        self.assertEqual(SYMBOLIC_HALT_CAUSES, {"scope_escape_exhausted", "capture_retype_exhausted"})
        self.assertEqual(UNATTENDED_HALT_CLASS, SYMBOLIC_HALT_CAUSES | {"model_repair_exhausted", "reviewer_idle_deadline"})
        self.assertEqual(SYMBOLIC_TERMINAL_CAUSES, TERMINAL_CAUSES | SYMBOLIC_HALT_CAUSES)
        self.assertFalse(SYMBOLIC_HALT_CAUSES & HARNESS_TERMINAL_CAUSES)
        self.assertNotIn("clarification_cap", UNATTENDED_HALT_CLASS)

    def test_park_rows_follow_purpose_exhaustion_inside_awaiting_input(self):
        outcome = {"status": "agent_failure", "returncode": 0, "timed_out": True}
        for kind in SYMBOLIC_HALT_CAUSES:
            self.assertEqual(classify(outcome, parked(kind), None, {}), (kind, "labels.py: escape 4, bound 3"))
        resumed = parked("scope_escape_exhausted", after=("plan_approved",))
        self.assertIsNone(last_symbolic_halt(resumed))
        self.assertEqual(classify(outcome, resumed, None, {})[0], "deadline_in_phase:AwaitingInput")
        self.assertEqual(classify(outcome, parked("scope_escape_exhausted", phase="Working"), None, {})[0],
                         "deadline_in_phase:Working")
        purpose = parked("scope_escape_exhausted")
        purpose["intent_events"].insert(2, {"id": "p", "cycle": "c1", "kind": "purpose_missing_rounds_exhausted",
                                            "detail": "3 rounds"})
        self.assertEqual(classify(outcome, purpose, None, {})[0], "purpose_missing_exhausted")
        errored = dict(parked("capture_retype_exhausted"), last_error="boom", last_error_kind="daemon_rejection")
        self.assertEqual(classify(outcome, errored, None, {})[0], "daemon_rejection")
        guidance = dict(parked("capture_retype_exhausted"), recovery={"status": "awaiting_guidance", "purpose": "harness_action"})
        self.assertEqual(classify(outcome, guidance, None, {})[0], "model_repair_exhausted")

    def test_reviewer_records_a_park_instead_of_supplying_the_missing_human(self):
        episode = {"allowed_paths": ["labels.py"]}
        for kind in SYMBOLIC_HALT_CAUSES:
            decision = review_input({"task": parked(kind)}, episode)
            self.assertEqual(decision["terminal"], "agent_failure")
            self.assertEqual(decision["cause"], kind)
            self.assertIn("parked for human guidance", decision["reason"])
        resumed = review_input({"task": parked("scope_escape_exhausted", after=("plan_approved",))}, episode)
        self.assertIn("input", resumed)
        self.assertIn("best judgment", resumed["input"])
        guidance = dict(parked("scope_escape_exhausted"), recovery={"status": "awaiting_guidance", "diagnostic": "d"})
        self.assertEqual(review_input({"task": guidance}, episode)["cause"], "model_repair_exhausted")
        self.assertIsNone(review_input({"task": parked("scope_escape_exhausted"), "busy": True}, episode))
