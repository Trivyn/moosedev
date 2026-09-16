"""The confirmatory floor-study mode: ordered tiers, repetitions, exact intervals, pooled scoring.

Every identity sealed before this mode existed must stay byte-identical, so the
first test re-derives them and the field-check designs frozen in preflights on
this machine.
"""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import config, evolution, field_check, floor_study, grading, intent, model_table
from bench.harness_study.artifacts import ArtifactStore, canonical_json

DOCS = Path(evolution.__file__).parent
TARGET = DOCS.parents[1] / "target"
PINNED = {
    "intent": "f28f83ca29deff64f2009817a34ac59f50347e939dff423ec5388811321f236f",
    evolution.STAGE1_MODE: "44b6175c2e4a0841844784d439dd696393ab9496a53d8fc32eb193f2371609f8",
    evolution.STAGE2_MODE: "e8fec638c4f691ad087c7b6ec5b81362b087d8c20a80079fb40c9b615509074c",
    evolution.STAGE2_RECOVERY_MODE: "763ccc521e31595001d67dc15c9bddcd0c288443ae70184d0a20195914f340db",
    evolution.STAGE2_BASELINE_MODE: "17205d5a09af1075de01548def24716cf1fb112e882eae6fc1fce3ae2eda1115",
}
Q9B = "qwen/qwen3.5-9b"
A4B = "google/gemma-4-26b-a4b"
QWEN = "qwen/qwen3.8-27b"
G31B = "gemma-4-31b-it"
HELPER = model_table.HELPER
FIXTURE_INDEX = "/fixture/.lmstudio/.internal/model-index-cache.json"
FIXTURE_ROOT = Path("/fixture/.lmstudio/models")


def floor_config(tiers=(Q9B, QWEN), scenarios=("retry_ledger",), repetitions=None,
                 response_policies=None, rule=None, response_policy=floor_study.RESPONSE_POLICY):
    tiers, scenarios = list(tiers), list(scenarios)
    design = floor_study.design_identity(tiers, scenarios, response_policy, repetitions,
                                         response_policies, rule)
    plan = design["payload"]["tiers"]
    return {"evaluation_mode": floor_study.MODE, "study_id": "floor-fixture", "seed": 1,
            "lmstudio_index": FIXTURE_INDEX, "tier_models": tiers, "coding_models": tiers,
            "helper_model": HELPER,
            "local_models": model_table.config_entries([*tiers, HELPER], FIXTURE_ROOT),
            "scenario_ids": scenarios, "intent_policies": ["symbolic"], "episode_limit": None,
            "episode_seconds": 1200, "context_tokens": 32768,
            "generation_policy": {"local_temperature": 0.0},
            "harness_response_policy": response_policy,
            "tier_repetitions": {tier["tier"]: tier["repetitions"] for tier in plan},
            "tier_response_policies": {tier["tier"]: tier["harness_response_policy"] for tier in plan},
            "floor_rule": {key: design["payload"]["floor_rule"][key]
                           for key in ("pass_rate_threshold", "native_margin")},
            "evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
            "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT,
            "floor_study_design": design}


class SealedIdentityTests(unittest.TestCase):
    def test_earlier_identities_and_modes_are_unchanged(self):
        self.assertEqual(intent.design_identity()["sha256"], PINNED["intent"])
        for mode in evolution.MODES:
            if mode in PINNED:
                self.assertEqual(evolution.design_identity(mode)["sha256"], PINNED[mode], mode)
        self.assertNotIn(floor_study.MODE, evolution.MODES)
        self.assertNotEqual(floor_study.MODE, field_check.MODE)
        self.assertIn(floor_study.MODE, config.HARNESS_MODES)
        self.assertIs(floor_study.ARMS, evolution.ARMS[evolution.SYMBOLIC_BASELINE_MODE])
        self.assertIs(config.frozen_arms(floor_study.MODE), floor_study.ARMS)
        self.assertFalse(evolution.postedit_association_contract(floor_study.MODE))
        self.assertEqual(config.required_clients({"evaluation_mode": floor_study.MODE}), ("opencode", "lms"))

    PREFLIGHTS = sorted(TARGET.glob("**/preflight.json"))
    # The field-check identities this campaign still runs under. Preflights taken
    # before commit 7dd4c2d carry no `scenario_tables` key and keep their own
    # superseded identity: they are evidence, not a recomputable design.
    CURRENT_DESIGNS = ("03bda54a", "0f5910b9", "4c20d14a", "c9cb6682", "e203feec")

    @unittest.skipUnless(PREFLIGHTS, "no preflights are on this machine")
    def test_current_field_check_designs_still_recompute_unchanged(self):
        found = set()
        for path in self.PREFLIGHTS:
            try:
                cfg = (json.loads(path.read_text()) or {}).get("config") or {}
            except ValueError:
                continue
            if cfg.get("evaluation_mode") != field_check.MODE:
                continue
            design = cfg["field_check_design"]
            if "scenario_tables" not in design["payload"]:
                continue
            with self.subTest(path.parent.name):
                self.assertEqual(design, field_check.design_identity(
                    cfg["coding_models"], cfg["scenario_ids"],
                    cfg.get("harness_response_policy", field_check.RESPONSE_POLICY)), path)
            found.add(design["sha256"][:8])
        if not found:
            self.skipTest("no current field-check preflights are on this machine")
        self.assertTrue(set(self.CURRENT_DESIGNS) & found,
                        f"none of the campaign's field-check identities were found: {sorted(found)}")


class DesignIdentityTests(unittest.TestCase):
    def test_design_binds_the_ordered_tiers_rule_and_protocol(self):
        identity = floor_study.design_identity([Q9B, QWEN], ["retry_ledger", "late_fees"],
                                               repetitions={"T1": 5})
        payload = identity["payload"]
        self.assertEqual(identity["sha256"], hashlib.sha256(canonical_json(payload)).hexdigest())
        self.assertEqual(payload["mode"], floor_study.MODE)
        self.assertIn("scored and pooled", payload["interpretation"])
        self.assertNotIn("never scored", payload["interpretation"])
        self.assertEqual(payload["tiers"], [
            {"tier": "T1", "model": model_table.row(Q9B), "repetitions": 5,
             "harness_response_policy": "reasoning-off"},
            {"tier": "T2", "model": model_table.row(QWEN), "repetitions": 3,
             "harness_response_policy": "reasoning-off"}])
        self.assertEqual(payload["helper_model"], model_table.row(HELPER))
        self.assertEqual(payload["arms"], [
            {"backend": "harness", "condition": "harness", "intent_policy": "symbolic"},
            {"backend": "opencode", "condition": "without", "intent_policy": None}])
        self.assertEqual(payload["scenarios"], ["retry_ledger", "late_fees"])
        self.assertEqual(set(payload["scenario_tables"]), {"late_fees"})
        self.assertIsNone(payload["episode_limit"])
        self.assertIn("continue", payload["episode_policy"])
        self.assertEqual(payload["floor_rule"]["pass_rate_threshold"], 0.8)
        self.assertEqual(payload["floor_rule"]["native_margin"], 0.1)
        self.assertEqual(payload["floor_rule"]["interval"], "Clopper-Pearson exact")
        self.assertEqual(payload["intent_design_sha256"], intent.design_identity()["sha256"])
        self.assertEqual(payload["inherits"]["design_sha256"],
                         evolution.design_identity(evolution.SYMBOLIC_BASELINE_MODE)["sha256"])
        self.assertEqual(payload["protocol_sha256"],
                         hashlib.sha256(floor_study.DOCUMENT.read_bytes()).hexdigest())

    def test_identity_changes_with_order_repetitions_policy_and_thresholds(self):
        base = floor_study.design_identity([Q9B, QWEN], ["retry_ledger"])["sha256"]
        variants = {
            "tier order": floor_study.design_identity([QWEN, Q9B], ["retry_ledger"]),
            "tier set": floor_study.design_identity([Q9B, QWEN, G31B], ["retry_ledger"]),
            "scenarios": floor_study.design_identity([Q9B, QWEN], ["retry_ledger", "late_fees"]),
            "repetitions": floor_study.design_identity([Q9B, QWEN], ["retry_ledger"], repetitions={"T1": 5}),
            "response policy": floor_study.design_identity([Q9B, QWEN], ["retry_ledger"],
                                                           response_policies={"T2": "provider-default"}),
            "threshold": floor_study.design_identity([Q9B, QWEN], ["retry_ledger"],
                                                     rule={"pass_rate_threshold": 0.9}),
            "margin": floor_study.design_identity([Q9B, QWEN], ["retry_ledger"],
                                                  rule={"native_margin": 0.05}),
        }
        for name, identity in variants.items():
            with self.subTest(name):
                self.assertNotEqual(identity["sha256"], base)
        self.assertEqual(len(set(identity["sha256"] for identity in variants.values())), len(variants))

    def test_design_refuses_invalid_selections(self):
        refusals = {
            "capability order": ([], ["retry_ledger"], {}),
            "distinct tier models": ([Q9B, Q9B], ["retry_ledger"], {}),
            "never a tested tier": ([Q9B, HELPER], ["retry_ledger"], {}),
            "not in the model table": (["unknown/model"], ["retry_ledger"], {}),
            "distinct reviewed packages": ([Q9B], ["not_a_package"], {}),
        }
        for message, (tiers, scenarios, extra) in refusals.items():
            with self.subTest(message), self.assertRaisesRegex(ValueError, message):
                floor_study.design_identity(tiers, scenarios, **extra)
        with self.assertRaisesRegex(ValueError, "names no tier"):
            floor_study.design_identity([Q9B], ["retry_ledger"], repetitions={"T4": 3})
        with self.assertRaisesRegex(ValueError, "names no tier"):
            floor_study.design_identity([Q9B], ["retry_ledger"], response_policies={"T9": "provider-default"})
        for count in (0, 11, 2.0, "3"):
            with self.subTest(count), self.assertRaisesRegex(ValueError, "repetitions must be"):
                floor_study.design_identity([Q9B], ["retry_ledger"], repetitions={"T1": count})
        with self.assertRaisesRegex(ValueError, "harness_response_policy"):
            floor_study.design_identity([Q9B], ["retry_ledger"], response_policies={"T1": "auto"})
        with self.assertRaisesRegex(ValueError, "harness_response_policy"):
            floor_study.design_identity([Q9B], ["retry_ledger"], "auto")
        with self.assertRaisesRegex(ValueError, "unknown floor rule keys"):
            floor_study.design_identity([Q9B], ["retry_ledger"], rule={"threshold": 0.8})
        for value in (1.5, -0.1, 1, "0.8"):
            with self.subTest(value), self.assertRaisesRegex(ValueError, "must be a float"):
                floor_study.design_identity([Q9B], ["retry_ledger"], rule={"pass_rate_threshold": value})


class ScheduleTests(unittest.TestCase):
    def test_tiers_stay_contiguous_and_arms_alternate_inside_each_repetition(self):
        cfg = floor_config((Q9B, QWEN), ("retry_ledger", "late_fees"), repetitions={"T1": 2, "T2": 1})
        cells = floor_study.schedule(cfg)
        self.assertEqual(len(cells), (2 + 1) * 2 * 2)
        self.assertEqual([cell["schedule_index"] for cell in cells], list(range(len(cells))))
        self.assertEqual([cell["tier"] for cell in cells], ["T1"] * 8 + ["T2"] * 4)
        self.assertEqual([cell["model"] for cell in cells], [Q9B] * 8 + [QWEN] * 4)
        self.assertEqual([(cell["repetition"], cell["scenario_id"], cell["backend"]) for cell in cells[:4]],
                         [(1, "retry_ledger", "harness"), (1, "retry_ledger", "opencode"),
                          (1, "late_fees", "harness"), (1, "late_fees", "opencode")])
        self.assertEqual([cell["repetition"] for cell in cells[:8]], [1, 1, 1, 1, 2, 2, 2, 2])
        self.assertEqual({cell["intent_policy"] for cell in cells if cell["backend"] == "harness"}, {"symbolic"})
        self.assertEqual({cell["intent_policy"] for cell in cells if cell["backend"] != "harness"}, {None})
        identities = {(cell["tier"], cell["repetition"], cell["scenario_id"], cell["backend"]) for cell in cells}
        self.assertEqual(len(identities), len(cells))
        self.assertEqual(config.schedule(cfg), cells)

    def test_each_tier_carries_its_own_response_policy(self):
        cfg = floor_config((QWEN, G31B), ("retry_ledger",), response_policies={"T2": "provider-default"})
        policies = {(cell["tier"], cell["harness_response_policy"]) for cell in floor_study.schedule(cfg)}
        self.assertEqual(policies, {("T1", "reasoning-off"), ("T2", "provider-default")})

    def test_schedule_refuses_drift_from_the_design(self):
        drifts = {
            "not a floor-study configuration": {"evaluation_mode": field_check.MODE},
            "coding models must equal": {"coding_models": [QWEN, Q9B]},
            "daemon helper": {"helper_model": QWEN},
            "local_models must equal": {"local_models": []},
            "requires episode_seconds": {"episode_seconds": 600},
            "requires context_tokens": {"context_tokens": 8192},
            "requires intent_policies": {"intent_policies": ["current"]},
            "temperature": {"generation_policy": {"local_temperature": 0.2}},
            "design identity changed": {"tier_repetitions": {"T1": 4, "T2": 3}},
        }
        for message, change in drifts.items():
            with self.subTest(message), self.assertRaisesRegex(ValueError, message):
                floor_study.schedule({**floor_config(), **change})
        with self.assertRaisesRegex(ValueError, "design identity changed"):
            floor_study.schedule({**floor_config(), "floor_rule": {"pass_rate_threshold": 0.9,
                                                                   "native_margin": 0.1}})


class ConfigDerivationTests(unittest.TestCase):
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

            manifest, approval = root / "manifest.json", root / "study/approval.json"
            with patch.object(config, "verify_binaries", return_value={"build_id": "parent-build"}):
                derived = config.floor_study_config(parent(symbolic), manifest, "floor-v1", [Q9B, QWEN],
                                                    ["retry_ledger"], approval, repetitions={"T1": 5, "T2": 3})
                self.assertEqual(derived["evaluation_mode"], floor_study.MODE)
                self.assertEqual(derived["tier_models"], [Q9B, QWEN])
                self.assertEqual(derived["coding_models"], [Q9B, QWEN])
                self.assertEqual(derived["helper_model"], HELPER)
                self.assertEqual(derived["local_models"],
                                 model_table.config_entries([Q9B, QWEN, HELPER], root / ".lmstudio/models"))
                self.assertIsNone(derived["episode_limit"])
                self.assertEqual(derived["tier_repetitions"], {"T1": 5, "T2": 3})
                self.assertEqual(derived["tier_response_policies"], {"T1": "reasoning-off", "T2": "reasoning-off"})
                self.assertEqual(derived["floor_rule"], {"pass_rate_threshold": 0.8, "native_margin": 0.1})
                self.assertEqual(derived["floor_study_design"], floor_study.design_identity(
                    [Q9B, QWEN], ["retry_ledger"], repetitions={"T1": 5, "T2": 3}))
                self.assertEqual(derived["gold_approval"], str(approval.resolve()))
                self.assertNotIn("evolution_design", derived)
                self.assertEqual(derived["parent_pilot"]["study_id"], "symbolic-parent")
                self.assertEqual(len(config.schedule(derived)), (5 + 3) * 2)
                for message, bad in {"symbolic-baseline": parent(pilot),
                                     "intact": dict(parent(symbolic), config_sha256="0" * 64)}.items():
                    with self.subTest(message), self.assertRaisesRegex(ValueError, message):
                        config.floor_study_config(bad, manifest, "floor-v1", [Q9B], ["retry_ledger"], approval)
                with self.assertRaisesRegex(ValueError, "distinct"):
                    config.floor_study_config(parent(symbolic), manifest, "symbolic-parent", [Q9B],
                                              ["retry_ledger"], approval)
                with self.assertRaisesRegex(ValueError, "opencode"):
                    config.floor_study_config(parent(dict(symbolic, opencode=None)), manifest, "floor-v1",
                                              [Q9B], ["retry_ledger"], approval)
            for sealed in evolution.SEALED_SYMBOLIC_PREDECESSORS:
                with patch.object(config, "verify_binaries", return_value={"build_id": sealed}):
                    with self.assertRaisesRegex(ValueError, "sealed"):
                        config.floor_study_config(parent(symbolic), manifest, "floor-v1", [Q9B],
                                                  ["retry_ledger"], approval)


class ApprovalTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def write(self, name, value):
        path = self.root / name
        path.write_text(canonical_json(value).decode())
        return path

    def test_floor_study_approval_round_trips_and_says_results_are_scored(self):
        cfg = floor_config((Q9B, QWEN), ("retry_ledger", "late_fees"))
        approved = config.approval_payload("James Adam", config=cfg)
        self.assertEqual(approved["schema_version"], 4)
        self.assertEqual(approved["intent_design"], intent.design_identity())
        self.assertEqual(approved["floor_study_design"], cfg["floor_study_design"])
        self.assertIn("ARE scored and pooled", approved["scope"])
        self.assertEqual(set(approved["scenarios"]), {"retry_ledger", "late_fees"})
        for name, hashes in approved["scenarios"].items():
            scenario = config.load_scenario(name)
            self.assertEqual(hashes, {"package_sha256": scenario["package_sha256"],
                                      "gold_sha256": scenario["gold_sha256"]})
        self.assertEqual(config.verify_approval(self.write("floor.json", approved), config=cfg), approved)

    def test_each_mode_accepts_only_its_own_approval_schema(self):
        floor = floor_config((Q9B,), ("retry_ledger",))
        field = {"evaluation_mode": field_check.MODE, "study_id": "f", "seed": 1,
                 "lmstudio_index": FIXTURE_INDEX, "coding_models": [Q9B], "helper_model": HELPER,
                 "local_models": model_table.config_entries([Q9B, HELPER], FIXTURE_ROOT),
                 "scenario_ids": ["retry_ledger"], "intent_policies": ["symbolic"], "episode_limit": 1,
                 "episode_seconds": 1200, "context_tokens": 32768,
                 "generation_policy": {"local_temperature": 0.0},
                 "harness_response_policy": "reasoning-off",
                 "evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
                 "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT,
                 "field_check_design": field_check.design_identity([Q9B], ["retry_ledger"])}
        pilot = {"evaluation_mode": intent.MODE, "scenario_ids": list(intent.SCENARIOS),
                 "local_models": [{"id": model} for model in intent.MODELS], "seed": 1}
        floor_approval = self.write("floor.json", config.approval_payload("James Adam", config=floor))
        field_approval = self.write("field.json", config.approval_payload("James Adam", config=field))
        pilot_approval = self.write("pilot.json", config.approval_payload("James Adam", config=pilot))
        # The field-check guard is tested first, so it names both mixes that involve
        # a field check; either way neither approval is accepted.
        with self.assertRaisesRegex(ValueError, "field-check runs require"):
            config.verify_approval(field_approval, config=floor)
        with self.assertRaisesRegex(ValueError, "field-check runs require"):
            config.verify_approval(floor_approval, config=field)
        # The floor-study guard names the rest: a floor run demands schema 4, and
        # no other mode accepts one.
        with self.assertRaisesRegex(ValueError, "floor-study runs require"):
            config.verify_approval(pilot_approval, config=floor)
        with self.assertRaisesRegex(ValueError, "floor-study runs require"):
            config.verify_approval(floor_approval, config=pilot)

    def test_approval_must_bind_the_configured_design_and_scenarios(self):
        cfg = floor_config((Q9B,), ("retry_ledger",))
        approved = config.approval_payload("James Adam", config=cfg)
        moved = floor_config((Q9B,), ("retry_ledger",), repetitions={"T1": 5})
        with self.assertRaisesRegex(ValueError, "does not match the current floor-study design"):
            config.verify_approval(self.write("a.json", approved), config=moved)
        wrong_scenarios = dict(approved, scenarios={"late_fees": approved["scenarios"]["retry_ledger"]})
        with self.assertRaisesRegex(ValueError, "must bind the configured scenario set"):
            config.verify_approval(self.write("b.json", wrong_scenarios), config=cfg)
        stale = dict(approved, scenarios={"retry_ledger": {"package_sha256": "0" * 64, "gold_sha256": "0" * 64}})
        with self.assertRaisesRegex(ValueError, "current scenario package hashes"):
            config.verify_approval(self.write("c.json", stale), config=cfg)


class ExactIntervalTests(unittest.TestCase):
    def test_clopper_pearson_matches_known_exact_bounds(self):
        for (successes, trials), (lower, upper) in {
                (11, 12): (0.61520, 0.99789), (12, 12): (0.73535, 1.0), (0, 12): (0.0, 0.26465),
                (8, 10): (0.44390, 0.97479), (1, 2): (0.01258, 0.98742)}.items():
            with self.subTest(f"{successes}/{trials}"):
                observed = grading.clopper_pearson(successes, trials)
                self.assertAlmostEqual(observed[0], lower, places=4)
                self.assertAlmostEqual(observed[1], upper, places=4)
        self.assertAlmostEqual(grading.regularized_incomplete_beta(2, 2, 0.5), 0.5, places=12)
        self.assertEqual(grading.regularized_incomplete_beta(3, 4, 0.0), 0.0)
        self.assertEqual(grading.regularized_incomplete_beta(3, 4, 1.0), 1.0)
        # A wider interval never sits inside a narrower one.
        narrow, wide = grading.clopper_pearson(8, 10, 0.80), grading.clopper_pearson(8, 10, 0.99)
        self.assertLess(wide[0], narrow[0])
        self.assertGreater(wide[1], narrow[1])

    def test_clopper_pearson_refuses_impossible_counts(self):
        for successes, trials in ((3, 2), (-1, 4), (0, 0), (1.0, 4), (1, 4.0)):
            with self.subTest(f"{successes}/{trials}"), self.assertRaisesRegex(ValueError, "clopper_pearson"):
                grading.clopper_pearson(successes, trials)
        with self.assertRaisesRegex(ValueError, "confidence"):
            grading.clopper_pearson(1, 4, 1.0)

    def test_horizon_reached_counts_only_leading_passes(self):
        def outcome(*statuses):
            return {"episodes": [{"id": str(i), "status": s} for i, s in enumerate(statuses)]}
        self.assertEqual(grading.horizon_reached(outcome("success", "success", "success")), 3)
        self.assertEqual(grading.horizon_reached(outcome("success", "agent_failure", "success")), 1)
        self.assertEqual(grading.horizon_reached(outcome("agent_failure", "success")), 0)
        self.assertEqual(grading.horizon_reached(None), 0)
        self.assertEqual(grading.horizon_reached({}), 0)


class FloorSummaryTests(unittest.TestCase):
    DESIGN = floor_study.design_identity([Q9B, QWEN, G31B], ["retry_ledger"])

    def attempt(self, tier, backend, status, *, integrity="sealed", scenario="retry_ledger", episodes=None):
        outcome = {"status": status, "episodes": episodes or [{"id": "e1", "status": status}]}
        return {"run_id": f"{tier}-{backend}-{status}-{len(self.attempts)}", "integrity": integrity,
                "status": status, "outcome": outcome if integrity == "sealed" else None,
                "manifest": {"evaluation_mode": floor_study.MODE, "study_id": "floor-v1", "tier": tier,
                             "backend": backend, "scenario_id": scenario}}

    def setUp(self):
        self.attempts = []

    def add(self, tier, backend, statuses, **kwargs):
        for status in statuses:
            self.attempts.append(self.attempt(tier, backend, status, **kwargs))

    def test_floor_is_the_smallest_tier_meeting_the_registered_rule(self):
        # T1 fails the threshold; T2 meets it but trails native beyond the margin; T3 qualifies.
        self.add("T1", "harness", ["success", "agent_failure", "agent_failure", "agent_failure"])
        self.add("T1", "opencode", ["success", "agent_failure", "agent_failure", "agent_failure"])
        self.add("T2", "harness", ["success"] * 4 + ["agent_failure"])
        self.add("T2", "opencode", ["success"] * 5)
        self.add("T3", "harness", ["success"] * 5)
        self.add("T3", "opencode", ["success"] * 4 + ["agent_failure"])
        summary = grading.floor_summary(self.attempts, self.DESIGN)
        tiers = {tier["tier"]: tier for tier in summary["tiers"]}
        self.assertEqual([tier["tier"] for tier in summary["tiers"]], ["T1", "T2", "T3"])
        self.assertEqual(tiers["T1"]["arms"]["harness"]["pass_rate"], 0.25)
        self.assertFalse(tiers["T1"]["meets_threshold"])
        self.assertTrue(tiers["T2"]["meets_threshold"])
        self.assertAlmostEqual(tiers["T2"]["difference"], -0.2)
        self.assertFalse(tiers["T2"]["within_native_margin"])
        self.assertTrue(tiers["T3"]["qualifies"])
        self.assertEqual(summary["floor_tier"], "T3")
        self.assertEqual(summary["floor_rule"], self.DESIGN["payload"]["floor_rule"])
        self.assertEqual(tiers["T3"]["model"], G31B)
        interval = tiers["T3"]["arms"]["harness"]["interval"]
        self.assertAlmostEqual(interval[0], grading.clopper_pearson(5, 5)[0])
        self.assertEqual(set(tiers["T3"]["scenarios"]), {"retry_ledger"})

    def test_margin_admits_a_harness_arm_that_trails_native_slightly(self):
        self.add("T1", "harness", ["success"] * 9 + ["agent_failure"])
        self.add("T1", "opencode", ["success"] * 10)
        summary = grading.floor_summary(self.attempts, self.DESIGN)
        tier = summary["tiers"][0]
        self.assertAlmostEqual(tier["difference"], -0.1)
        self.assertTrue(tier["within_native_margin"])
        self.assertEqual(summary["floor_tier"], "T1")

    def test_infrastructure_failures_and_unsealed_runs_are_excluded_not_failed(self):
        self.add("T1", "harness", ["success"] * 4)
        self.add("T1", "harness", ["infrastructure_failure", "preflight_failure"])
        self.attempts.append(self.attempt("T1", "harness", "unfinished", integrity="unsealed"))
        self.add("T1", "opencode", ["success"] * 4)
        arm = grading.floor_summary(self.attempts, self.DESIGN)["tiers"][0]["arms"]["harness"]
        self.assertEqual((arm["runs"], arm["scored"], arm["excluded"], arm["passed"]), (7, 4, 3, 4))
        self.assertEqual(arm["pass_rate"], 1.0)

    def test_horizon_is_reported_per_scored_run(self):
        self.add("T1", "harness", ["agent_failure"], episodes=[
            {"id": "e1", "status": "success"}, {"id": "e2", "status": "agent_failure"},
            {"id": "e3", "status": "success"}])
        self.add("T1", "opencode", ["success"], episodes=[{"id": "e1", "status": "success"}])
        tier = grading.floor_summary(self.attempts, self.DESIGN)["tiers"][0]
        self.assertEqual(tier["arms"]["harness"]["horizon_reached"], [1])
        self.assertEqual(tier["arms"]["harness"]["mean_horizon_reached"], 1)

    def test_a_tier_with_no_runs_never_qualifies(self):
        self.add("T1", "harness", ["success"] * 3)
        self.add("T1", "opencode", ["success"] * 3)
        summary = grading.floor_summary(self.attempts, self.DESIGN)
        empty = summary["tiers"][1]
        self.assertIsNone(empty["arms"]["harness"]["pass_rate"])
        self.assertIsNone(empty["difference"])
        self.assertFalse(empty["qualifies"])
        self.assertEqual(summary["floor_tier"], "T1")

    def test_summary_requires_the_sealed_design_and_its_rule(self):
        self.add("T1", "harness", ["success"])
        for bad in (None, {}, {"payload": {}}, {"payload": {"tiers": [], "floor_rule": {}}},
                    {"payload": {"tiers": self.DESIGN["payload"]["tiers"]}}):
            with self.subTest(str(bad)), self.assertRaisesRegex(ValueError, "pre-registered floor rule"):
                grading.floor_summary(self.attempts, bad)


class FloorStudyReportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.store = ArtifactStore(Path(self.temporary.name).resolve() / "study")
        self.design = floor_study.design_identity([Q9B, QWEN], ["retry_ledger"])

    def make_run(self, tier, backend, status, *, mode=floor_study.MODE, study_id="floor-v1", design=True):
        run = self.store.create_run({"model": tier, "backend": backend, "condition": "harness",
                                     "scenario_id": "retry_ledger", "intent_policy": "symbolic",
                                     "build_id": "sealed-build", "evaluation_mode": mode,
                                     "study_id": study_id, "tier": tier, "repetition": 1})
        if design:
            self.store.put_bytes(run, "floor-study-design.json", canonical_json(self.design))
        self.store.put_bytes(run, "outcome.json", canonical_json(
            {"status": status, "episodes": [{"id": "e1", "status": status, "checks": []}]}))
        self.store.seal_run(run)
        return run

    def test_report_scores_the_study_against_its_sealed_design(self):
        for _ in range(3):
            self.make_run("T1", "harness", "success")
            self.make_run("T1", "opencode", "agent_failure")
        result = grading.report(self.store.root)
        summary = result["floor_study"]
        self.assertEqual(summary["floor_tier"], "T1")
        self.assertEqual(summary["tiers"][0]["arms"]["harness"]["pass_rate"], 1.0)
        self.assertEqual(summary["tiers"][0]["arms"]["native"]["pass_rate"], 0.0)
        self.assertEqual(summary["tiers"][0]["difference"], 1.0)
        self.assertIsNone(summary["tiers"][1]["arms"]["harness"]["pass_rate"])
        self.assertEqual(summary["floor_rule"], self.design["payload"]["floor_rule"])

    def test_report_never_pools_floor_study_runs_with_another_study(self):
        self.make_run("T1", "harness", "success")
        self.make_run("T1", "harness", "success", study_id="other-floor-study")
        with self.assertRaisesRegex(ValueError, "own identity"):
            grading.report(self.store.root)

    def test_floor_study_runs_are_scored_unlike_field_checks(self):
        run = self.make_run("T1", "harness", "success")
        review = {"reviewer_id": "blinded-1", "scenario_gold_sha256": None,
                  "claims": [{"claim_id": "c1", "verdict": "supported",
                              "evidence": [{"path": "outcome.json", "start_line": 1, "end_line": 1}],
                              "rationale": "n/a"}]}
        # The field check refuses review outright; the floor study reaches gold-hash
        # validation instead, which is what a scored mode must do.
        with self.assertRaisesRegex(ValueError, "frozen scenario gold hash"):
            grading.record_review(self.store.root, run.name, review)


if __name__ == "__main__":
    unittest.main()
