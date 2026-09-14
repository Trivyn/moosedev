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


DOSSIER = ("### late_fee (Unknown)\n`fees::FeePolicy::late_fee` - defined in `fees.py`\n\n**Records**\n"
           "- [Constraint] [FeePolicy keeps no state between calls](http://127.0.0.1:1/#/constraints/a) - accepted, 2026-09-07T00:00:00Z (via constrains)\n"
           "- [Requirement] [Fees are integer cents rounded half up](http://127.0.0.1:1/#/requirements/b) - accepted, 2026-09-07T00:00:00Z (via concerns)\n")
SCENARIO = {"id": "probe", "initial_facts": [
    {"id": "stateless", "kind": "Constraint", "title": "FeePolicy keeps no state between calls",
     "description": "FeePolicy holds no state.", "component": "Billing", "relations": []},
    {"id": "fees-cents", "kind": "Requirement", "title": "Fees are integer cents rounded half up",
     "description": "Percentages round half up to the cent.", "component": "Billing", "relations": []},
    {"id": "fees-np7", "kind": "Constraint", "title": TITLE, "description": CLAIM, "component": "Billing", "relations": []}]}
SOURCES = {
    "fees.py": "class FeePolicy:\n    def late_fee(self, account, invoice, today):\n        return 0\n",
    "accounts.py": 'SEGMENTS = {\n    "retail": "Retail",\n    "charity": "Registered charity",\n}\n\n\nclass Account:\n'
                   '    def __init__(self, account_id, segment):\n        if segment not in SEGMENTS:\n'
                   '            raise ValueError(f"unknown segment: {segment}")\n        self.segment = segment\n',
    "tests/test_visible.py": "import unittest\n\nfrom accounts import Account\nfrom fees import FeePolicy\n\n\n"
                             "class VisibleTests(unittest.TestCase):\n    def test_retail(self):\n"
                             "        self.assertEqual(FeePolicy().late_fee(Account('r', 'retail'), None, 130), 500)\n",
    "README.md": "# Late fees\n\n- `accounts.py`: account segments.\n",
}


class LeverDiagnosticTests(unittest.TestCase):
    """Diagnostic-only helpers for the dossier-claims and read-source recall levers."""

    def test_dossier_record_lines_are_parsed(self):
        self.assertEqual(crowding.dossier_records(DOSSIER), [
            {"kind": "Constraint", "title": "FeePolicy keeps no state between calls", "via": "constrains"},
            {"kind": "Requirement", "title": "Fees are integer cents rounded half up", "via": "concerns"}])
        self.assertEqual(crowding.dossier_records("No recorded entity knowledge is linked to this file."), [])

    def test_simulated_claim_dossiers_deliver_listed_records_only(self):
        files = [{"file": "fees.py", "dossier": DOSSIER, "policy": {"decision": "gate"}},
                 {"file": "README.md", "dossier": "No recorded entity knowledge is linked to this file.", "policy": {}}]
        result = crowding.simulate_claim_dossiers(files, SCENARIO)
        self.assertEqual(result["delivered_fact_ids"], ["stateless", "fees-cents"])
        self.assertEqual(result["today_bytes"], len(DOSSIER.encode()) + len("No recorded entity knowledge is linked to this file.".encode()))
        self.assertGreater(result["simulated_bytes"], result["today_bytes"])
        simulated = "\n".join(item["dossier"] for item in result["files"])
        self.assertIn("hasDescription: Percentages round half up to the cent.", simulated)
        self.assertNotIn(CLAIM, simulated)

    def test_source_identifiers_literals_and_imports(self):
        self.assertEqual(crowding.source_identifiers(SOURCES["accounts.py"]),
                         ["SEGMENTS", "Account", "__init__", "self", "account_id", "segment", "ValueError"])
        self.assertEqual(crowding.source_literals(SOURCES["accounts.py"]),
                         ["retail", "Retail", "charity", "Registered charity", "unknown segment: "])
        self.assertEqual(crowding.imported_project_files(SOURCES["tests/test_visible.py"], set(SOURCES)),
                         ["accounts.py", "fees.py"])

    def test_read_topics_by_variant(self):
        objective = "Implement FeePolicy.late_fee"
        fees = crowding.read_topic(objective, SOURCES, ["fees.py"], "2a")
        self.assertEqual(fees, objective + "\nFeePolicy late_fee self account invoice today")
        with_accounts = crowding.read_topic(objective, SOURCES, ["fees.py", "accounts.py"], "2b")
        self.assertIn("Registered charity", with_accounts)
        self.assertNotIn("Registered charity", crowding.read_topic(objective, SOURCES, ["fees.py", "accounts.py"], "2a"))
        plan = crowding.read_topic(objective, SOURCES, ["fees.py", "tests/test_visible.py"], "2c")
        self.assertIn("Registered charity", plan)  # the test file imports accounts.py
        self.assertNotIn("Registered charity", crowding.read_topic(objective, SOURCES, ["fees.py", "tests/test_visible.py"], "2b"))
        self.assertEqual(crowding.read_topic(objective, SOURCES, ["README.md"], "2a"), objective)
        raw = crowding.read_topic(objective, SOURCES, ["accounts.py", "fees.py"], "2d", raw_bytes=40)
        self.assertEqual(raw, objective + "\n" + (SOURCES["accounts.py"] + "\n" + SOURCES["fees.py"]).encode()[:40].decode())
        with self.assertRaises(ValueError):
            crowding.read_topic(objective, SOURCES, ["fees.py"], "2z")

    def test_added_records_exclude_the_objective_evidence(self):
        objective = crowding.split_context(context_text(header=False, evidence=[("Requirement", "Late fees are computed on demand", OTHER, ["hasDescription: x"])]))["evidence"]
        topic = context_text(header=False, evidence=[
            ("Requirement", "Late fees are computed on demand", OTHER, ["hasDescription: x"]),
            ("Constraint", TITLE, NP7, ["hasDescription: " + CLAIM])])
        added = crowding.added_records(topic, objective)
        self.assertEqual([item["iri"] for item in added["records"]], [NP7])
        self.assertEqual(added["titles"], [TITLE])
        self.assertGreater(added["bytes"], len(CLAIM))


class TierDiagnosticTests(unittest.TestCase):
    """Diagnostic-only helpers for tiered, precise-first delivery."""

    def test_tier1_queries_walk_from_plan_files_one_hop_at_a_time(self):
        direct = crowding.tier1_query("direct", files=["fees.py", "tests/test_visible.py"])
        self.assertIn('VALUES ?path { "fees.py" "tests/test_visible.py" }', direct)
        self.assertIn("VALUES ?predicate { arch:concerns arch:constrains }", direct)
        self.assertIn("code:definedInPath", direct)
        expected = {"motivating": "arch:isMotivatedBy", "component_constraints": "a arch:Constraint",
                    "supersession_heads": "arch:supersedes+", "lessons": "arch:learnedFrom"}
        for hop, fragment in expected.items():
            query = crowding.tier1_query(hop, records=[OTHER, NP7])
            self.assertIn(f"VALUES ?record {{ <{OTHER}> <{NP7}> }}", query)
            self.assertIn(fragment, query)
            self.assertIn('?found arch:hasLifecycleStatus "accepted"', query)
        with self.assertRaises(ValueError):
            crowding.tier1_query("reads")

    def test_bindings_and_claim_records_prefer_knowledge_kinds(self):
        text = json.dumps({"head": {"vars": ["record", "kind", "title", "description"]}, "results": {"bindings": [
            {"record": {"value": NP7}, "kind": {"value": crowding.ARCH_NS + "KnowledgeRecord"}, "title": {"value": TITLE},
             "description": {"value": CLAIM}},
            {"record": {"value": NP7}, "kind": {"value": crowding.ARCH_NS + "Constraint"}, "title": {"value": TITLE},
             "description": {"value": CLAIM}},
            {"record": {"value": OTHER}, "kind": {"value": crowding.ARCH_NS + "Requirement"}, "title": {"value": "Other"}}]}})
        rows = crowding.sparql_bindings(text, "record", "kind", "title", "description")
        self.assertEqual(rows[2], (OTHER, crowding.ARCH_NS + "Requirement", "Other", None))
        records = crowding.claim_records(rows)
        self.assertEqual(records[NP7]["kind"], "Constraint")
        self.assertEqual(records[OTHER]["description"], "")
        self.assertEqual(crowding.render_claim(records[NP7]), f"[Constraint] {TITLE} ({NP7})\nhasDescription: {CLAIM}\n")

    def test_tier1_counts_each_record_once_in_hop_order(self):
        claims = {iri: {"iri": iri, "kind": "Requirement", "title": iri[-5:], "description": "claim " + iri[-5:]}
                  for iri in (OTHER, NP7, WALKED)}
        tier1 = crowding.assemble_tier1({"direct": [OTHER], "component_constraints": [OTHER, NP7], "lessons": [WALKED],
                                         "motivating": ["https://moosedev.dev/kg/study/unrendered"]}, claims)
        self.assertEqual(tier1["hops"]["direct"]["records"], [OTHER])
        self.assertEqual(tier1["hops"]["component_constraints"]["records"], [NP7])
        self.assertEqual(tier1["hops"]["motivating"]["records"], [])
        self.assertEqual(tier1["records"], [OTHER, NP7, WALKED])
        self.assertEqual(tier1["bytes"], sum(tier1["hops"][hop]["bytes"] for hop in crowding.TIER1_HOPS))
        self.assertEqual(tier1["text"], "".join(crowding.render_claim(claims[iri]) for iri in (OTHER, NP7, WALKED)))

    def test_delivery_stages_and_cumulative_rows(self):
        facts = SCENARIO["initial_facts"]
        known = crowding.fact_iris(SCENARIO)
        np7 = next(iri for iri, fact_id in known.items() if fact_id == "fees-np7")
        cents = next(iri for iri, fact_id in known.items() if fact_id == "fees-cents")
        titles = crowding.text_stage(f"[Constraint] {TITLE} ({np7})\n", 40, known)
        claims = crowding.text_stage(f"hasDescription: {CLAIM}\n({cents}) Percentages round half up to the cent.\n", 60, known)
        self.assertEqual(titles["iris"], [np7])
        row = crowding.stage_row(titles, facts, deciding_fact="fees-np7", expected=["fees-np7"])
        self.assertEqual((row["deciding_claim"], row["records"], row["bytes"], row["other_claims"]), (False, 1, 40, 0))
        both = crowding.combine([titles, claims])
        self.assertEqual((both["bytes"], both["iris"]), (100, [np7, cents]))
        row = crowding.stage_row(both, facts, deciding_fact="fees-np7", expected=["fees-np7"])
        self.assertTrue(row["deciding_claim"])
        self.assertEqual((row["delivered_fact_ids"], row["other_claims"], row["outside_expected"]),
                         (["fees-cents", "fees-np7"], 1, ["fees-cents"]))

    def test_nlq_question_uses_only_symbolic_state(self):
        self.assertEqual(crowding.nlq_question(["fees::FeePolicy::late_fee", "fees::FeePolicy::late_fee"], ["fees.py"]),
                         "Which constraints and requirements govern fees.FeePolicy.late_fee?")
        self.assertEqual(crowding.nlq_question([], ["fees.py", "tests/test_fees.py"]),
                         "Which constraints and requirements govern fees.py, tests/test_fees.py?")

    def test_timed_call_records_a_failed_tool_call_without_raising(self):
        text, error, seconds = crowding.timed_call(lambda: "answer")
        self.assertEqual((text, error), ("answer", None))
        self.assertGreaterEqual(seconds, 0)

        def failing():
            raise RuntimeError("MCP tool query failed: could not regularize")
        text, error, seconds = crowding.timed_call(failing)
        self.assertEqual((text, error), ("", "MCP tool query failed: could not regularize"))
        self.assertGreaterEqual(seconds, 0)

    def test_helper_must_already_be_loaded_at_its_pinned_context(self):
        loaded = {"id": crowding.HELPER_MODEL, "state": "loaded", "loaded_context_length": 131072}
        self.assertTrue(crowding.helper_ready({"data": [{"id": "google/gemma-4-26b-a4b", "state": "loaded"}, loaded]}))
        self.assertFalse(crowding.helper_ready({"data": [dict(loaded, state="not-loaded")]}))
        self.assertFalse(crowding.helper_ready({"data": [dict(loaded, loaded_context_length=32768)]}))
        self.assertFalse(crowding.helper_ready({"data": []}))

    def test_summary_rows_cover_every_stage(self):
        cell = {"bytes": 1, "records": 1, "deciding_claim": False, "other_claims": 0, "delivered_fact_ids": [],
                "outside_expected": []}
        entry = {"plan": {"name": "blind-1", "files": ["fees.py"], "summary": "s"}, "push": cell,
                 "tier1": dict(cell, hops={hop: cell for hop in crowding.TIER1_HOPS}, errors={}),
                 "tier2": {"run": False, "note": "not run: helper not loaded", "question": "q"},
                 "tier3": {"5": dict(cell, ranked=5), "10": dict(cell, deciding_claim=True, ranked=10)},
                 "cumulative": {"tier1": cell, "tier1+tier3@5": cell, "tier1+tier3@10": dict(cell, deciding_claim=True)}}
        [row] = crowding.tier_summary({"plans": [entry]})
        self.assertEqual(row["tier2"], "not run: helper not loaded")
        failed = dict(entry, tier2=dict(cell, run=True, question="q", seconds=1.0, error="could not regularize"),
                      cumulative={"tier1+tier2": cell})
        [failed_row] = crowding.tier_summary({"plans": [failed]})
        self.assertEqual(failed_row["tier2"], "failed")
        self.assertIn("tier2", failed_row["stages"])
        self.assertEqual(list(row["stages"]), ["push", "tier1", *(f"tier1.{hop}" for hop in crowding.TIER1_HOPS),
                                               "tier3@5", "tier3@10", "cumulative:tier1", "cumulative:tier1+tier3@5",
                                               "cumulative:tier1+tier3@10"])
        self.assertTrue(row["stages"]["cumulative:tier1+tier3@10"]["deciding_claim"])
        self.assertEqual(set(row["stages"]["push"]), {"bytes", "records", "deciding_claim", "other_claims"})


class CommandTests(unittest.TestCase):
    def test_commands_are_registered(self):
        from bench.harness_study import __main__ as cli
        source = Path(cli.__file__).read_text()
        self.assertIn('"crowding-gate"', source)
        self.assertIn('"crowding-report"', source)
        self.assertIn('"crowding-levers"', source)
        self.assertIn('"crowding-tiers"', source)


if __name__ == "__main__":
    unittest.main()
