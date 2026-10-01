"""Decision bench: classification, transforms and the run loop, with the
provider replaced by a scripted function (no network)."""
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from bench.harness_study import decision_bench

SCHEMA = {"oneOf": [
    {"properties": {"action": {"const": "inspect"}}},
    {"properties": {"action": {"const": "replace"}}},
]}

PROMPT = (
    "Action meanings: read(file), search(query), inspect(event,offset), replace(...)\n"
    "Allowed actions now: read, search, inspect, replace.\n"
    "Recent observations (complete outputs remain in journal events; use inspect(event,offset) to page them):\n"
    + json.dumps(["Event 7: Command: cargo test\nrunning 3 tests\n[observation shortened]", "Event 8: other"])
    + "\nCheck output previews:\nCheck 0: running 3 tests\n[observation shortened]\n"
    "Last result:\nrunning 3 tests\ntest a ... FAILED\n"
    + decision_bench.SCHEMA_MARKER + json.dumps(SCHEMA)
)


class ClassifyTest(unittest.TestCase):
    def test_actions_in_every_answer_shape(self):
        c = decision_bench.classify
        self.assertEqual(c('{"action":"inspect","event":7,"offset":0}', 7), "inspect(check)")
        self.assertEqual(c('{"action":"inspect","event":8,"offset":0}', 7), "inspect(other)")
        self.assertEqual(c('<tool_call>\n{"action": "replace", "file": "a"}\n</tool_call>'), "edit")
        self.assertEqual(c('{"message":"m","action":{"action":"read","file":"a"}}'), "read")
        self.assertEqual(c('{"name":"command","arguments":{"command":"cargo test"}}'), "command")
        native = json.dumps({"tool_calls": [{"function": {"name": "write", "arguments": "{}"}}], "content": ""})
        self.assertEqual(c(native), "edit")
        narrated = json.dumps({"tool_calls": [{"function": {"name": "reply", "arguments": "{}"}},
                                              {"function": {"name": "replace", "arguments": "{}"}}], "content": ""})
        self.assertEqual(c(narrated), "edit")
        self.assertEqual(c("Let me look at the output."), "invalid")
        self.assertEqual(c("ERROR HTTP 429"), "error")


class TransformTest(unittest.TestCase):
    def test_shortened_copies_become_a_pointer_and_the_hint_can_go(self):
        out = decision_bench.drop_shortened_copies(PROMPT, {"check_event": 7}, hint=False)
        self.assertNotIn("[observation shortened]", out.split("Last result:")[0])
        self.assertIn("its whole output is the Last result below", out)
        self.assertNotIn(decision_bench.PAGE_HINT, out)
        self.assertIn("Event 8: other", out)
        self.assertTrue(out.endswith(json.dumps(SCHEMA)))

    def test_no_inspect_leaves_the_schema_and_lists_without_it(self):
        out = decision_bench.remove_inspect(PROMPT, {})
        schema = json.loads(out.rpartition(decision_bench.SCHEMA_MARKER)[2])
        self.assertEqual([arm["properties"]["action"]["const"] for arm in schema["oneOf"]], ["replace"])
        self.assertNotIn("inspect(event,offset), ", out.split("Recent observations")[0])
        self.assertIn("read, search, replace", out)


class JournalTest(unittest.TestCase):
    def test_a_tools_request_needs_its_definitions(self):
        journal = Path(tempfile.mkdtemp()) / "task.json"
        journal.write_text(json.dumps({"model_requests": [{"prompt": "p", "contract": "tools"}]}))
        with self.assertRaises(ValueError):
            decision_bench.journal_request({"name": "c", "journal": str(journal), "request": 0})

    def test_no_inspect_without_a_schema_edits_the_lists(self):
        out = decision_bench.remove_inspect("Allowed actions now: read, search, inspect, replace.", {})
        self.assertEqual(out, "Allowed actions now: read, search, replace.")


class RunTest(unittest.TestCase):
    def test_journal_cases_are_sent_per_transform_and_counted(self):
        directory = Path(tempfile.mkdtemp())
        journal = directory / "task.json"
        journal.write_text(json.dumps({"model_requests": [
            {"prompt": "earlier"}, {"prompt": PROMPT, "max_output_tokens": 16384, "contract": "json_schema"}]}))
        cases = [{"name": "stall", "journal": str(journal), "request": 1, "check_event": 7,
                  "transforms": ["as_is", "no_shortened_copies_no_hint"]}]
        sent = []

        def fake_send(profile, body, **_):
            sent.append(body)
            prompt = body["messages"][0]["content"]
            answer = '{"action":"inspect","event":7,"offset":0}' if "[observation shortened]" in prompt \
                else '{"action":"replace","file":"a","old_text":"x","new_text":"y"}'
            return answer, {"cost": 0.001}, 0.1

        with mock.patch.object(decision_bench, "send", fake_send):
            results = decision_bench.run(cases, "openrouter", n=2, out=directory / "raw.json")
        self.assertEqual(results[("stall", "as_is")], {"inspect(check)": 2})
        self.assertEqual(results[("stall", "no_shortened_copies_no_hint")], {"edit": 2})
        self.assertEqual(len(sent), 4)
        self.assertEqual(sent[0]["max_tokens"], 16384)
        self.assertEqual(sent[0]["reasoning_effort"], "none")
        self.assertEqual(len(json.loads((directory / "raw.json").read_text())), 4)
        self.assertIn("TOTAL", decision_bench.table(results))


if __name__ == "__main__":
    unittest.main()
