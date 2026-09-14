"""Crowding gate verdicts and offline delivery report, on scripted fixtures only."""
import json
from pathlib import Path
import tempfile
import unittest

from bench.harness_study import crowding

NP7 = "https://moosedev.dev/kg/study/np7"
OTHER = "https://moosedev.dev/kg/study/other"
WALKED = "https://moosedev.dev/kg/study/walked"
TITLE = "Regulation NP-7 compliance for account billing"
CLAIM = "No late fee or penalty fee of any kind may be charged to an account in a registered non-profit segment."


def context_text(*, inventory=(), evidence=(), header=True):
    lines = []
    if header:
        lines.append("Recall: get_relevant_context(no topic, limit=100) inventory, then topic recall (limit=12).\n"
                     "The broad inventory is bounded and contains names only.\n\nCurrent knowledge inventory:")
        lines.extend(f"[{kind}] {label} ({iri})" for kind, label, iri in inventory)
        lines.append("\nTopic evidence (complete claims; up to six relationships per record):")
    text = "\n".join(lines) + ("\n" if lines else "")
    for kind, label, iri, body in evidence:
        text += f"\n[{kind}] {label} ({iri})\n" + "".join(line + "\n" for line in body)
    return text


class ContextParsingTests(unittest.TestCase):
    def test_sections_and_membership(self):
        response = {"context": context_text(
            inventory=[("Constraint", TITLE, NP7), ("Requirement", "Late fees are computed on demand", OTHER)],
            evidence=[("Requirement", "Late fees are computed on demand", OTHER, ["hasDescription: computed on demand"]),
                      ("Lesson", "Walked lesson", WALKED, ["hasDescription: walked", "linkedVia: " + OTHER])]),
            "files": [{"file": "fees.py", "dossier": "- [Requirement] Late fees are computed on demand - accepted",
                       "policy": {"decision": "Inject", "reason": "linked records"}}]}
        parsed = crowding.split_context(response["context"])
        self.assertEqual([item["iri"] for item in parsed["inventory"]], [NP7, OTHER])
        self.assertEqual([(item["iri"], item["walked"]) for item in parsed["evidence"]], [(OTHER, False), (WALKED, True)])
        member = crowding.membership(response, iri=NP7, title=TITLE, claim=CLAIM)
        self.assertEqual(member, {"inventory": True, "topic_evidence": False, "walk": False,
                                  "dossier_title": False, "policy_reason": False, "claim_anywhere": False})
        leaked = dict(response, files=[{"file": "fees.py", "dossier": f"- [Constraint] {TITLE} - accepted",
                                        "policy": {"reason": f"Constraint {TITLE} requires ratification"}}])
        member = crowding.membership(leaked, iri=NP7, title=TITLE, claim=CLAIM)
        self.assertTrue(member["dossier_title"] and member["policy_reason"])
        evidence_only = {"context": context_text(header=False, evidence=[("Constraint", TITLE, NP7, ["hasDescription: " + CLAIM])]),
                         "files": []}
        member = crowding.membership(evidence_only, iri=NP7, title=TITLE, claim=CLAIM)
        self.assertTrue(member["topic_evidence"] and member["claim_anywhere"])
        self.assertFalse(member["inventory"])


class RankingTests(unittest.TestCase):
    MCP = ("Relevant recorded knowledge (3 items):\n\n"
           "• Requirement — \"Late fees are computed on demand\"\n  hasLifecycleStatus: accepted\n  " + OTHER + "\n\n"
           "• Lesson — \"Walked lesson\"\n  linkedVia: isMotivatedBy\n  " + WALKED + "\n\n"
           "• Constraint — \"" + TITLE + "\"\n  hasDescription: " + CLAIM + "\n  " + NP7 + "\n")

    def test_ranking_rank_and_cross_check(self):
        items = crowding.parse_ranking(self.MCP)
        self.assertEqual([(item["iri"], item["walked"]) for item in items], [(OTHER, False), (WALKED, True), (NP7, False)])
        self.assertEqual(crowding.rank_of(items, NP7), 2)  # search rank; walked records are not ranked
        self.assertIsNone(crowding.rank_of(items, "https://moosedev.dev/kg/study/absent"))
        evidence = crowding.split_context(context_text(header=False, evidence=[
            ("Requirement", "Late fees are computed on demand", OTHER, []),
            ("Lesson", "Walked lesson", WALKED, ["linkedVia: " + OTHER])]))["evidence"]
        self.assertTrue(crowding.cross_check(items, evidence))
        other = crowding.split_context(context_text(header=False, evidence=[("Constraint", TITLE, NP7, [])]))["evidence"]
        self.assertFalse(crowding.cross_check(items, other))


class VerdictTests(unittest.TestCase):
    CLEAN = {"inventory": True, "topic_evidence": False, "walk": False, "dossier_title": False,
             "policy_reason": False, "claim_anywhere": False}

    def memberships(self, **change):
        return [{"topic": topic, "files": files, "membership": dict(self.CLEAN, **change)}
                for topic in ("objective", "bare") for files in ("none", "fees.py", "all")]

    def test_v1_requires_no_claim_delivery_and_a_rank_margin(self):
        ranks = {"objective": {"rank": 20, "cross_check": True}, "bare": {"rank": 16, "cross_check": True}}
        self.assertTrue(crowding.v1_verdict(self.memberships(), ranks)["passed"])
        for change in ({"claim_anywhere": True}, {"topic_evidence": True}, {"walk": True},
                       {"dossier_title": True}, {"policy_reason": True}):
            with self.subTest(change=change):
                self.assertFalse(crowding.v1_verdict(self.memberships(**change), ranks)["passed"])
        low = dict(ranks, bare={"rank": 15, "cross_check": True})
        self.assertFalse(crowding.v1_verdict(self.memberships(), low)["passed"])
        unavailable = {"objective": {"rank": None, "cross_check": False}, "bare": {"rank": 30, "cross_check": True}}
        verdict = crowding.v1_verdict(self.memberships(), unavailable)
        self.assertTrue(verdict["passed"])
        self.assertIn("rank unavailable", " ".join(verdict["notes"]))

    def test_v2_counts_plans_whose_topic_reaches_the_record(self):
        reach = [{"reaches": True}, {"reaches": False}, {"reaches": True}]
        self.assertTrue(crowding.v2_verdict(reach)["passed"])
        self.assertFalse(crowding.v2_verdict([{"reaches": True}, {"reaches": False}, {"reaches": False}])["passed"])
        self.assertFalse(crowding.v2_verdict([])["passed"])
        self.assertEqual(crowding.plan_topic({"summary": "Exempt charities", "files": ["fees.py", "accounts.py"]}),
                         "Exempt charities\nfees.py accounts.py")


def stdout(sequence, value):
    return {"channel": "stdout", "sequence": sequence, "payload": {"episode": "e1", "text": json.dumps(value)}}


class ReportTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.run = Path(self.temporary.name)

    def write_run(self, events, outcome):
        with (self.run / "events.jsonl").open("w") as stream:
            for event in events:
                stream.write(json.dumps(event) + "\n")
        (self.run / "outcome.json").write_text(json.dumps(outcome))

    def test_prompt_sections_locate_claims(self):
        prompt = ("You are the coding sensor.\nCurrent accepted knowledge:\n" + context_text(
            inventory=[("Constraint", TITLE, NP7)]) + "Entity dossiers:\n[]\n\nCurrent harness state (observed results):\n"
            "Mode: Plan\nRecent observations:\nAccepted project knowledge for 'charity' (authoritative):\n"
            "[Constraint] " + TITLE + " (" + NP7 + ")\nhasDescription: " + CLAIM + "\n")
        location = crowding.locate(prompt, iri=NP7, title=TITLE, claim=CLAIM)
        self.assertEqual(location["claim_sections"], ["observations"])
        self.assertIn("inventory", location["title_sections"])

    def test_harness_report_orders_delivery_against_the_first_edit(self):
        before = "Current accepted knowledge:\n" + context_text(inventory=[("Constraint", TITLE, NP7)]) + "Entity dossiers:\n[]\n"
        after = before + "Recent observations:\nhasDescription: " + CLAIM + "\n"
        task1 = {"phase": "Working", "edits": [], "model_requests": [{"purpose": "harness_action", "prompt": before}],
                 "events": [{"message": "Proposed plan: " + json.dumps({"summary": "Add the fee", "files": ["fees.py"],
                                                                      "checks": ["python3 -m unittest"]})}],
                 "intent_events": []}
        task2 = dict(task1, edits=[{"file": "fees.py"}],
                     model_requests=task1["model_requests"] + [{"purpose": "harness_action", "prompt": before}])
        task3 = dict(task2, model_requests=task2["model_requests"] + [{"purpose": "harness_action", "prompt": after}],
                     intent_events=[{"kind": "knowledge_search", "detail": "1 records, 0 repository matches: charity"}],
                     events=task1["events"] + [{"message": "Accepted project knowledge for 'charity' (authoritative):\n"
                                                           "[Constraint] " + TITLE + " (" + NP7 + ")"}])
        outcome = {"episodes": [{"id": "e1", "status": "agent_failure", "checks": [{"stderr": (
            "test_charity_pays_no_late_fee (__main__.LateFeeTests) ... FAIL\n"
            "test_percentage_rounds_half_up (__main__.LateFeeTests) ... ok\n")}]}]}
        self.write_run([stdout(1, {"type": "state", "task": task1}), stdout(2, {"type": "state", "task": task2}),
                        stdout(3, {"type": "state", "task": task3})], outcome)
        probes = [{"id": "e1-charity", "test": "LateFeeTests.test_charity_pays_no_late_fee"},
                  {"id": "e1-half-up", "test": "LateFeeTests.test_percentage_rounds_half_up"}]
        report = crowding.harness_report(self.run, iri=NP7, title=TITLE, claim=CLAIM, probes=probes)
        self.assertEqual(report["first_edit_sequence"], 2)
        self.assertEqual(report["requests_before_first_edit"], 2)
        self.assertFalse(report["claim_before_first_edit"])
        self.assertTrue(report["claim_after_first_edit"])
        self.assertEqual(report["searches"], ["1 records, 0 repository matches: charity"])
        self.assertTrue(report["search_returned_record"])
        self.assertEqual(report["plans"], [{"summary": "Add the fee", "files": ["fees.py"]}])
        self.assertEqual(report["probes"], {"e1-charity": "FAIL", "e1-half-up": "ok"})

    def test_native_report_finds_the_notes_read_before_the_first_edit(self):
        def tool(sequence, name, value):
            return stdout(sequence, {"type": "tool_use", "part": {"tool": name, "state": {"status": "completed", **value}}})
        events = [tool(1, "bash", {"input": {"command": "ls"}, "output": "fees.py"}),
                  tool(2, "read", {"input": {"filePath": "PROJECT_NOTES.md"}, "output": "## fees-np7: " + TITLE + "\n" + CLAIM}),
                  tool(3, "edit", {"input": {"filePath": "fees.py"}, "output": "ok"})]
        self.write_run(events, {"episodes": [{"id": "e1", "status": "success", "checks": [{"stderr": ""}]}]})
        report = crowding.native_report(self.run, claim=CLAIM, probes=[])
        self.assertEqual(report["first_edit_sequence"], 3)
        self.assertTrue(report["notes_read_before_first_edit"])
        self.assertTrue(report["claim_before_first_edit"])


class CommandTests(unittest.TestCase):
    def test_commands_are_registered(self):
        from bench.harness_study import __main__ as cli
        source = Path(cli.__file__).read_text()
        self.assertIn('"crowding-gate"', source)
        self.assertIn('"crowding-report"', source)


if __name__ == "__main__":
    unittest.main()
