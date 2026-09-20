import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from bench.harness_study import capture, capture_study, judge
from bench.harness_study.artifacts import ArtifactStore, canonical_json
from bench.harness_study.grading import record_review

KG = "https://moosedev.dev/kg"
ARCH = "https://trivyn.io/ontologies/software/architecture"
GRAPH = "https://moosedev.dev/kg/project"


def quad(subject, predicate, value, *, iri=False):
    body = f"<{value}>" if iri else json.dumps(value)
    return f"<{subject}> <{predicate}> {body} <{GRAPH}> ."


def record_quads(iri, kind, title, description, *, links=True, status="accepted"):
    lines = [quad(iri, "http://www.w3.org/1999/02/22-rdf-syntax-ns#type", f"{ARCH}#{kind}", iri=True),
             quad(iri, f"{ARCH}#hasTitle", title),
             quad(iri, f"{ARCH}#hasDescription", description),
             quad(iri, f"{ARCH}#hasLifecycleStatus", status),
             # Provenance must never count as a link: every record has it.
             quad(iri, "http://www.w3.org/ns/prov#wasAttributedTo", f"{KG}/agent/x", iri=True)]
    if links:
        lines.append(quad(iri, f"{ARCH}#concerns", f"{KG}/SystemComponent/c1", iri=True))
    return lines


SEED = record_quads(f"{KG}/Constraint/seed", "Constraint", "Seeded sequence rule",
                    "Sequence numbers are contiguous per entity.")
GOOD = record_quads(f"{KG}/ArchitecturalDecision/new", "ArchitecturalDecision",
                    "Outbox emits the full document",
                    "Every event carries the entity's complete current data, because the indexer replaces.")


class ParsingTests(unittest.TestCase):
    def test_records_are_typed_titled_and_linked_by_local_name(self):
        found = capture.records("\n".join(SEED + GOOD))
        self.assertEqual(len(found), 2)
        new = found[f"{KG}/ArchitecturalDecision/new"]
        self.assertEqual(new["kind"], "ArchitecturalDecision")
        self.assertEqual(new["title"], "Outbox emits the full document")
        self.assertEqual(new["status"], "accepted")
        self.assertEqual(new["links"], ["concerns"])

    def test_provenance_is_not_a_link_so_an_orphan_is_detected(self):
        orphan = record_quads(f"{KG}/Lesson/orphan", "Lesson", "A lesson with nowhere to be found",
                              "It is real, and nothing points at it.", links=False)
        found = capture.records("\n".join(orphan))
        judged = capture.validity(found[f"{KG}/Lesson/orphan"])
        self.assertTrue(judged["titled"])
        self.assertTrue(judged["described"])
        self.assertFalse(judged["linked"])
        self.assertFalse(judged["valid"])

    def test_degenerate_titles_are_refused(self):
        for title in ("", "   ", "Test query", "Lesson"):
            found = capture.records("\n".join(record_quads(
                f"{KG}/Lesson/x", "Lesson", title, "described")))
            self.assertFalse(capture.validity(found[f"{KG}/Lesson/x"])["titled"], title)

    def test_a_title_echoing_the_prompt_is_degenerate(self):
        found = capture.records("\n".join(record_quads(
            f"{KG}/Lesson/x", "Lesson", "Add a delete_many method", "described")))
        judged = capture.validity(found[f"{KG}/Lesson/x"], prompt="Add a delete_many method\n\nmore text")
        self.assertFalse(judged["titled"])

    def test_untyped_and_unreadable_lines_are_ignored_not_guessed(self):
        text = "\n".join([*GOOD, "garbage", quad(f"{KG}/Thing/1", f"{ARCH}#hasTitle", "not a record")])
        self.assertEqual(set(capture.records(text)), {f"{KG}/ArchitecturalDecision/new"})


class EpisodeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.run = Path(self.temporary.name)

    def graph(self, relative, lines):
        path = self.run / relative / ".moosedev"
        path.mkdir(parents=True, exist_ok=True)
        (path / "kg.nq").write_text("\n".join(lines) + "\n")

    def events(self, entries):
        (self.run / "events.jsonl").write_text(
            "".join(json.dumps(entry) + "\n" for entry in entries))

    def test_seeded_records_are_never_counted_as_captured(self):
        self.graph("initial", SEED)
        self.graph("episodes/e1/workspace", SEED)
        rows = capture.episode_capture(self.run, backend="opencode_mcp", episodes=["e1"])
        self.assertEqual(rows[0]["records_created"], 0)
        self.assertEqual(capture.capture_summary(rows)["capture_rate"], 0.0)

    def test_a_new_valid_record_is_counted_once_and_not_again_next_episode(self):
        self.graph("initial", SEED)
        self.graph("episodes/e1/workspace", SEED + GOOD)
        self.graph("episodes/e2/workspace", SEED + GOOD)
        rows = capture.episode_capture(self.run, backend="opencode_mcp", episodes=["e1", "e2"])
        self.assertEqual([row["records_created"] for row in rows], [1, 0])
        self.assertEqual(rows[0]["records_valid"], 1)
        summary = capture.capture_summary(rows)
        self.assertEqual(summary["records_created"], 1)
        self.assertEqual(summary["capture_rate"], 0.5)
        self.assertEqual(summary["validity_rate"], 1.0)

    def test_a_restating_harness_episode_writes_nothing_and_reports_no_attempt_count(self):
        # A `Restates` disposition deliberately mints no record. Reported as zero
        # records, and attempts stay null because the harness never asks the model.
        self.graph("initial", SEED)
        self.graph("episodes/e1/workspace", SEED)
        rows = capture.episode_capture(self.run, backend="harness", episodes=["e1"])
        self.assertEqual(rows[0]["records_created"], 0)
        self.assertIsNone(rows[0]["attempts"])

    def test_a_missing_event_log_is_unknown_rather_than_zero_calls(self):
        self.graph("initial", SEED)
        self.graph("episodes/e1/workspace", SEED)
        rows = capture.episode_capture(self.run, backend="opencode_mcp", episodes=["e1"])
        self.assertIsNone(rows[0]["attempts"])

    def test_capture_attempts_and_errors_come_from_the_recorded_observations(self):
        self.graph("initial", SEED)
        self.graph("episodes/e1/workspace", SEED + GOOD)
        self.events([
            {"channel": "native", "payload": {"episode": "e1", "capture": {"t": 1}, "retrieval": None, "error": None}},
            {"channel": "native", "payload": {"episode": "e1", "capture": {"t": 2}, "retrieval": None,
                                              "error": "range check refused"}},
            {"channel": "native", "payload": {"episode": "e1", "capture": None, "retrieval": {"t": 3}, "error": None}},
            {"channel": "native", "payload": {"episode": "e2", "capture": {"t": 4}, "retrieval": None, "error": None}},
        ])
        rows = capture.episode_capture(self.run, backend="opencode_mcp", episodes=["e1"])
        self.assertEqual(rows[0]["attempts"], {"capture": 2, "retrieval": 1, "capture_errors": 1})

    def test_checkpoint_conformance_and_pending_reach_the_summary(self):
        self.graph("initial", SEED)
        self.graph("episodes/e1/workspace", SEED + GOOD)
        self.events([{"channel": "checkpoint",
                      "payload": {"episode": "e1", "conforms": False, "revision": "r1",
                                  "pending": [f"{KG}/ArchitecturalDecision/new"]}}])
        rows = capture.episode_capture(self.run, backend="opencode_mcp", episodes=["e1"])
        self.assertFalse(rows[0]["conforms"])
        summary = capture.capture_summary(rows)
        self.assertFalse(summary["conforms"])
        self.assertEqual(summary["pending"], 1)


class DesignTests(unittest.TestCase):
    def test_three_arms_with_the_deferred_mcp_arm_between_harness_and_notes(self):
        self.assertEqual(capture_study.ARMS,
                         (("harness", "harness", "symbolic"),
                          ("opencode_mcp", "opencode_mcp", None),
                          ("opencode", "without", None)))

    def test_only_accumulation_packages_are_admissible(self):
        self.assertEqual(set(capture_study.SCENARIOS), {"entity_outbox", "supplier_quotes", "retry_ledger"})
        with self.assertRaises(ValueError):
            capture_study.design_identity(["qwen/qwen3.5-9b"], ["late_fees"])

    def test_identity_binds_the_protocol_and_both_falsifiers(self):
        design = capture_study.design_identity(["qwen/qwen3.5-9b"], ["retry_ledger"])
        payload = design["payload"]
        self.assertEqual(payload["mode"], capture_study.MODE)
        self.assertEqual(payload["decision"], "2399f114")
        self.assertEqual(payload["track"], "accumulation")
        self.assertEqual(len(payload["protocol_sha256"]), 64)
        rule = payload["capture_rule"]
        self.assertIn("premise_weakened_if", rule)
        self.assertIn("records_unusable_if", rule)
        self.assertEqual(rule["adequate_capture"], capture_study.ADEQUATE_CAPTURE)

    def test_changing_a_threshold_changes_the_identity(self):
        first = capture_study.design_identity(["qwen/qwen3.5-9b"], ["retry_ledger"])
        second = capture_study.design_identity(["qwen/qwen3.5-9b"], ["retry_ledger"],
                                               rule={"adequate_capture": 0.75})
        self.assertNotEqual(first["sha256"], second["sha256"])

    def test_schedule_runs_every_arm_of_every_scenario_in_each_repetition(self):
        design = capture_study.design_identity(["qwen/qwen3.5-9b", "qwen/qwen3.8-27b"],
                                               ["retry_ledger", "entity_outbox"],
                                               repetitions={"T1": 2, "T2": 1})
        config = {"evaluation_mode": capture_study.MODE,
                  "scenario_ids": ["retry_ledger", "entity_outbox"]}
        with patch.object(capture_study, "verify_config", return_value=design):
            cells = capture_study.schedule(config)
        # (2 reps + 1 rep) tiers x 2 scenarios x 3 arms
        self.assertEqual(len(cells), 18)
        self.assertEqual([cell["schedule_index"] for cell in cells], list(range(18)))
        self.assertEqual({cell["backend"] for cell in cells},
                         {"harness", "opencode_mcp", "opencode"})
        # Tiers stay contiguous so only one model is loaded at a time.
        self.assertEqual([cell["tier"] for cell in cells], ["T1"] * 12 + ["T2"] * 6)
        # The three arms alternate inside each repetition, so a warm cache
        # cannot favour one of them.
        self.assertEqual([cell["backend"] for cell in cells[:3]],
                         [backend for backend, _, _ in capture_study.ARMS])
        mcp = [cell for cell in cells if cell["backend"] == "opencode_mcp"]
        self.assertEqual(len(mcp), 6)
        self.assertTrue(all(cell["intent_policy"] is None for cell in mcp))


class JudgeTests(unittest.TestCase):
    """The judge is a sensor: it picks verdicts, never evidence spans."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.store = ArtifactStore(Path(self.temporary.name).resolve() / "study")
        self.gold = {"schema_version": 1, "scenario_id": "retry_ledger", "review_status": "pending",
                     "facts": [{"id": "f-full", "kind": "ArchitecturalDecision",
                                "claim": "Every event carries the entity's complete current data.",
                                "evidence": [], "introduced_episode": 1, "seeded": False},
                               {"id": "f-other", "kind": "Constraint", "claim": "Fees are integer cents.",
                                "evidence": [], "introduced_episode": 1, "seeded": False}],
                     "forbidden_claims": [{"claim": "Seq is a global autoincrement.",
                                           "applies_from_episode": 1, "applies_through_episode": None}]}
        self.scenario = {"id": "retry_ledger", "episodes": [
            {"id": "e1", "expected_fact_ids": ["f-full", "f-other"]}], "initial_facts": []}

    def make_run(self, backend="opencode_mcp"):
        run = self.store.create_run({"model": "local", "backend": backend, "condition": backend,
                                     "scenario_id": "retry_ledger", "scenario_gold_sha256": "a" * 64})
        self.store.put_bytes(run, "scenario.json", canonical_json(self.scenario))
        self.store.put_bytes(run, "scenario/gold.json", canonical_json(self.gold))
        self.store.put_bytes(run, "outcome.json", canonical_json(
            {"status": "success", "episodes": [{"id": "e1", "status": "success"}]}))
        self.store.put_bytes(run, "initial/.moosedev/kg.nq", ("\n".join(SEED) + "\n").encode())
        self.store.put_bytes(run, "episodes/e1/workspace/.moosedev/kg.nq",
                             ("\n".join(SEED + GOOD) + "\n").encode())
        self.store.seal_run(run)
        return run

    def test_spans_are_contiguous_sealed_line_ranges(self):
        text = "\n".join(SEED + GOOD)
        found = judge.spans(text, f"{KG}/ArchitecturalDecision/new", "p")
        self.assertEqual(len(found), 1)
        self.assertEqual(found[0]["start_line"], len(SEED) + 1)
        self.assertEqual(found[0]["end_line"], len(SEED) + len(GOOD))

    def test_prompt_names_no_arm_model_or_backend(self):
        created = capture.records("\n".join(GOOD))
        prompt, facts, forbidden = judge.episode_question(self.gold, self.scenario, "e1", created)
        self.assertEqual({fact["id"] for fact in facts}, {"f-full", "f-other"})
        self.assertEqual(len(forbidden), 1)
        for leak in ("opencode", "harness", "backend", "arm", "qwen", "gemma"):
            self.assertNotIn(leak, prompt.lower(), leak)
        self.assertIn("Every event carries", prompt)

    def test_judgment_is_accepted_by_the_ordinary_review_store(self):
        run = self.make_run()
        answer = {"assessments": [
            {"fact_id": "f-full", "verdict": "supported",
             "record": f"{KG}/ArchitecturalDecision/new", "rationale": "States it in substance."},
            {"fact_id": "f-other", "verdict": "missing", "record": None,
             "rationale": "No record mentions cents."}], "forbidden": []}
        judgment = judge.judge_run(self.store.root, run.name, model="test/judge",
                                   base_url="http://unused", api_key="k",
                                   caller=lambda prompt: answer)
        self.assertEqual(judgment["reviewer_id"], "judge:test/judge")
        self.assertTrue(judgment["judge"]["blind_to_arm"])
        verdicts = {claim["fact_id"]: claim["verdict"] for claim in judgment["claims"]}
        self.assertEqual(verdicts, {"f-full": "supported", "f-other": "missing"})
        record_review(self.store.root, run.name, judgment)

    def test_a_supported_verdict_naming_no_real_record_cannot_stand(self):
        # The model may name a record that was not captured. Without evidence the
        # verdict is downgraded rather than filed as unsupportable credit.
        run = self.make_run()
        answer = {"assessments": [
            {"fact_id": "f-full", "verdict": "supported", "record": f"{KG}/Lesson/imaginary",
             "rationale": "Invented."}], "forbidden": []}
        judgment = judge.judge_run(self.store.root, run.name, model="test/judge",
                                   base_url="http://unused", api_key="k",
                                   caller=lambda prompt: answer)
        claim = judgment["claims"][0]
        self.assertEqual(claim["verdict"], "missing")
        self.assertEqual(claim["evidence"], [])
        record_review(self.store.root, run.name, judgment)

    def test_a_forbidden_claim_is_filed_as_unsupported_against_its_record(self):
        run = self.make_run()
        answer = {"assessments": [{"fact_id": "f-full", "verdict": "missing", "record": None,
                                   "rationale": "absent"}],
                  "forbidden": [{"claim": "Seq is a global autoincrement.",
                                 "asserted_by": f"{KG}/ArchitecturalDecision/new",
                                 "rationale": "The record asserts the rejected design."}]}
        judgment = judge.judge_run(self.store.root, run.name, model="test/judge",
                                   base_url="http://unused", api_key="k",
                                   caller=lambda prompt: answer)
        forbidden = [claim for claim in judgment["claims"] if claim["verdict"] == "unsupported"]
        self.assertEqual(len(forbidden), 1)
        self.assertTrue(forbidden[0]["evidence"])
        record_review(self.store.root, run.name, judgment)

    def test_a_run_with_nothing_captured_files_no_judgment(self):
        run = self.store.create_run({"model": "local", "backend": "opencode", "condition": "without",
                                     "scenario_id": "retry_ledger", "scenario_gold_sha256": "a" * 64})
        self.store.put_bytes(run, "scenario.json", canonical_json(self.scenario))
        self.store.put_bytes(run, "scenario/gold.json", canonical_json(self.gold))
        self.store.put_bytes(run, "outcome.json", canonical_json(
            {"status": "success", "episodes": [{"id": "e1", "status": "success"}]}))
        self.store.seal_run(run)
        with self.assertRaises(ValueError):
            judge.judge_run(self.store.root, run.name, model="test/judge",
                            base_url="http://unused", api_key="k", caller=lambda prompt: {})


if __name__ == "__main__":
    unittest.main()
