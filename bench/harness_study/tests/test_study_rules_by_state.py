"""Rules-by-state estimate, on a scripted two-journal conversation only."""
import json
import unittest

from bench.harness_study import rules_by_state

HEADER = "\nProject rules (hard requirements for any change that touches them):\n"
OUTPUT = "Call exactly one tool for your next action.\n"
REQ = "https://moosedev.dev/kg/Requirement/r1"
REQ2 = "https://moosedev.dev/kg/Requirement/r2"
CON = "https://moosedev.dev/kg/Constraint/c1"
RULES = [("Requirement", "Parse the map", REQ), ("Requirement", "Write the map", REQ2),
         ("Constraint", "No dependencies", CON)]
CLAIM = "hasDescription: " + "a long claim. " * 20 + "\n\nEvidence:\n- spec.md:1\n"


def rules_text(rules=RULES):
    # As project_rules renders them: "\n[kind] label (iri)\nvia\nclaim".
    return HEADER + "".join(f"\n[{kind}] {label} ({iri})\nvia: component map\n{CLAIM}"
                            for kind, label, iri in rules)


def prompt(mode, plan=None, amend=False):
    marker = "Approved plan (amend it; keep what still holds)" if amend else "Plan"
    state = "\nThe displayed plan is approved. Allowed actions now: read." if mode == "Auto" else \
        "\nAllowed actions now: read, search, inspect, question, reply, plan."
    return ("You are the coding sensor in MOOSEDev.\n" + rules_text() + OUTPUT
            + "\nAction meanings: read(file).\n"
            + f"\nConfigured model ID: m\nCurrent human objective: o\nCurrent accepted knowledge:\nk\n"
            + f"{marker}: {json.dumps(plan)}\n"
            + "\nCurrent source, refreshed before this action:\n{}\n"
            + "\nCurrent human guidance: none\nCurrent harness state (observed results):\n"
            + f"Mode: {mode}\nPhase: Working\n" + state)


def request(text):
    return {"purpose": "harness_action", "prompt": text}


PLAN_1 = {"summary": "s", "files": ["a.rs"], "checks": ["cargo test"], "addresses": [REQ, CON]}
STEP_1 = {"model_requests": [request(prompt("Plan")),
                             request(prompt("Auto", PLAN_1)),
                             {"purpose": "harness_capture_note", "prompt": "note"}],
          "approved_plans": [{"addresses": [REQ, CON], "files": ["a.rs"]}]}
PLAN_2 = {"summary": "t", "files": ["b.rs"], "checks": [], "addresses": [REQ2, REQ]}
STEP_2 = {"model_requests": [request(prompt("Plan")),
                             request(prompt("Auto", PLAN_2)),
                             request(prompt("Plan", PLAN_2, amend=True))]}


class RulesByStateTest(unittest.TestCase):
    def test_the_section_parses_into_rules_that_rejoin_it(self):
        section = rules_text() + OUTPUT
        head, rules, tail = rules_by_state.parse_rules(section)
        self.assertEqual([(r["kind"], r["label"], r["iri"]) for r in rules], RULES)
        self.assertEqual({r["via"] for r in rules}, {"via: component map"})
        self.assertEqual(tail, OUTPUT)
        self.assertEqual(head + "".join(r["text"] for r in rules) + tail, section)

    def test_the_unclaimed_closing_line_is_tail_not_claim(self):
        closing = "\n1 project rule(s) named without their claim (Requirement: 1); search\n"
        section = rules_text() + closing + OUTPUT
        _, rules, tail = rules_by_state.parse_rules(section)
        self.assertEqual(tail, closing + OUTPUT)
        self.assertNotIn("named without", rules[-1]["text"])

    def test_a_settled_requirement_renders_on_one_line_and_a_constraint_stays_full(self):
        section = rules_text() + OUTPUT
        text, full, one = rules_by_state.estimate(section, {REQ: "decided by X", CON: "decided by Y"})
        self.assertIn("\n[Requirement] Parse the map (" + REQ + ") — decided by X; via: component map", text)
        self.assertIn("[Constraint] No dependencies (" + CON + ")\nvia: component map\n" + CLAIM, text)
        self.assertIn("1 settled project rule(s) shown as one line (Requirement: 1)", text)
        self.assertEqual((full, one), (2, 1))
        self.assertTrue(text.endswith(OUTPUT))
        text_all, _, one_all = rules_by_state.estimate(section, {REQ: "x", CON: "y"},
                                                       kinds={"Requirement", "Constraint"})
        self.assertEqual(one_all, 2)
        self.assertLess(len(text_all), len(text))

    def test_nothing_settled_leaves_the_section_unchanged(self):
        section = rules_text() + OUTPUT
        self.assertEqual(rules_by_state.estimate(section, {})[0], section)

    def test_proxy_settles_an_earlier_journals_addresses_and_auto_keeps_the_current_plan(self):
        result = rules_by_state.conversation([("s1", STEP_1), ("s2", STEP_2)])
        rows = {(row["journal"], row["request"]): row for row in result["rows"]}
        self.assertEqual(len(rows), 5)  # the capture note is not an action request
        # Step 1 planning: nothing approved yet.
        self.assertEqual(rows["s1", 0]["n_one_line"], 0)
        self.assertEqual(rows["s1", 0]["estimated_bytes"], rows["s1", 0]["current_bytes"])
        # Step 1 building: its own plan's rules stay open (Auto exception).
        self.assertEqual(rows["s1", 1]["n_one_line_all_kinds"], 0)
        # Step 2 planning: step 1's addresses settle; only the Requirement shortens.
        self.assertEqual(rows["s2", 0]["settled_by"], {"earlier task's plan": 2})
        self.assertEqual((rows["s2", 0]["n_full"], rows["s2", 0]["n_one_line"]), (2, 1))
        self.assertEqual(rows["s2", 0]["n_one_line_all_kinds"], 2)
        self.assertLess(rows["s2", 0]["estimated_bytes"], rows["s2", 0]["current_bytes"])
        # Step 2 building: REQ is in the current plan, so open despite the proxy;
        # the Constraint is settled but never shortened.
        self.assertEqual(rows["s2", 1]["mode"], "Auto")
        self.assertEqual(rows["s2", 1]["n_one_line"], 0)
        self.assertEqual(rows["s2", 1]["settled_by"], {"earlier task's plan": 1})
        # Step 2 amending its approved plan (Plan mode): both Requirements settle,
        # REQ2 by this task's own approved plan.
        self.assertEqual(rows["s2", 2]["mode"], "Plan")
        self.assertEqual(rows["s2", 2]["n_one_line"], 2)
        self.assertEqual(rows["s2", 2]["settled_by"], {"earlier task's plan": 2, "this task's plan": 1})
        summary = result["journals"]["s2"]["plan_mode"]
        self.assertEqual(summary["prompts"], 2)
        self.assertLess(summary["estimated"]["max"], summary["current"]["max"])

    def test_no_proxy_settles_only_this_tasks_plans_and_the_decided_list(self):
        result = rules_by_state.conversation([("s1", STEP_1), ("s2", STEP_2)], proxy=False)
        rows = {(row["journal"], row["request"]): row for row in result["rows"]}
        self.assertEqual(rows["s2", 0]["n_one_line_all_kinds"], 0)
        self.assertEqual(rows["s2", 2]["settled_by"], {"this task's plan": 2})
        decided = rules_by_state.conversation([("s1", STEP_1), ("s2", STEP_2)],
                                              decided=frozenset({REQ2}), proxy=False)
        rows = {(row["journal"], row["request"]): row for row in decided["rows"]}
        self.assertEqual(rows["s2", 0]["settled_by"], {"decided": 1})
        self.assertEqual(rows["s1", 0]["settled_by"], {"decided": 1})
        # Auto exception overrides the decided list too: REQ2 is in step 2's plan.
        self.assertEqual(rows["s2", 1]["settled_by"], {})

    def test_the_table_names_both_variants(self):
        text = rules_by_state.table(rules_by_state.conversation([("s1", STEP_1), ("s2", STEP_2)]))
        self.assertIn("s2 plan_mode (2 prompts)", text)
        self.assertIn("if Constraints were shortened too", text)


if __name__ == "__main__":
    unittest.main()
