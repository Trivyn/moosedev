"""Offline plan-coverage calibration: the Python port matches the Rust scorer's unit cases."""
from pathlib import Path
import unittest

from bench.harness_study import coverage_calibration as calibration
from bench.harness_study.scenario import load_scenario

REPO = Path(__file__).resolve().parents[3]
PLANS = REPO / "target/harness-crowded-probe-v1/tiers-v1-plans.json"
# The same neutral fixture as src/harness/coverage.rs unit tests.
OBJECTIVE = "Add a retry limit to the upload client so a failing upload stops after five attempts."
LABEL = "Uploads resume from the last acknowledged chunk"
CLAIM = ("hasDescription: An interrupted upload resumes from the last chunk the server acknowledged and never "
         "restarts from zero.\nconcerns: https://example.org/kg/component-1234\n")


def check(summary):
    return calibration.assess(summary, OBJECTIVE, "urn:rule", LABEL, CLAIM)


class PortTests(unittest.TestCase):
    def test_stems_fold_plural_and_tense_suffixes(self):
        self.assertEqual([calibration.stem(word) for word in ("uploads", "retries", "acknowledged", "resuming", "class", "is")],
                         ["upload", "retry", "acknowledg", "resum", "class", "is"])

    def test_the_rust_unit_cases_score_the_same(self):
        receipt = check("Add a retry counter to the upload client; uploads stop after five tries.")
        self.assertFalse(receipt["covered"])
        self.assertNotIn("upload", receipt["label_matched"])
        receipt = check("Retry failed uploads; each retry resumes from the last acknowledged chunk.")
        self.assertTrue(receipt["covered"])
        self.assertGreaterEqual(len(receipt["label_matched"]), 2)
        receipt = check("Never restart an interrupted transfer from zero; the server state wins.")
        self.assertTrue(receipt["covered"])
        self.assertFalse(any("example" in token for token in receipt["claim_matched"]))
        self.assertTrue(check("Retry limit only. The acknowledged-chunk resume rule does not apply here.")["covered"])
        receipt = check("Wrap send() in a loop with attempt counter max_attempts = 5 and backoff.")
        self.assertFalse(receipt["covered"])
        self.assertTrue(receipt["label_distinctive"] >= 2 and receipt["claim_distinctive"] >= 2)
        restated = calibration.assess("anything", "retry the upload", "urn:r", "Retry upload",
                                      "hasDescription: Retry the upload.\n")
        self.assertTrue(restated["covered"])
        self.assertEqual((restated["label_distinctive"], restated["claim_distinctive"]), (0, 0))

    def test_governing_rules_are_the_deciding_fact_components_constraints_read_from_the_package(self):
        scenario = load_scenario("late_fees_crowded")
        rules = calibration.governing_rules(scenario, "fees-np7")
        facts = {fact["id"]: fact for fact in scenario["initial_facts"]}
        component = facts["fees-np7"]["component"]
        self.assertEqual(sorted(rule["fact"] for rule in rules),
                         sorted(fact["id"] for fact in scenario["initial_facts"]
                                if fact["kind"] == "Constraint" and fact["component"] == component))
        np7 = next(rule for rule in rules if rule["fact"] == "fees-np7")
        self.assertTrue(np7["claim"].startswith("hasDescription: " + facts["fees-np7"]["description"] + "\n"))

    @unittest.skipUnless(PLANS.is_file(), "recorded tiers-v1 plans are not present under target/")
    def test_the_deciding_rule_returns_on_every_recorded_plan_at_the_defaults(self):
        result = calibration.calibrate("late_fees_crowded", "fees-np7", PLANS)
        defaults = next(cell for cell in result["matrix"]
                        if (cell["label_min"], cell["claim_min"]) == (calibration.DEFAULT_LABEL_MIN,
                                                                      calibration.DEFAULT_CLAIM_MIN))
        self.assertEqual(defaults["deciding_returns"], defaults["plans"])
        self.assertEqual(defaults["plans"], 6)


if __name__ == "__main__":
    unittest.main()
