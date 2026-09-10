import base64
from pathlib import Path
import sys
import tempfile
import textwrap
import unittest
from unittest import mock

from bench.harness_study.process import _gate_key, _group_exited, observe


class ObserverTests(unittest.TestCase):
    def test_permission_error_is_ignorable_only_for_confirmed_zombie_group(self):
        process = mock.Mock(pid=99)
        with mock.patch("bench.harness_study.process._leader_exited", return_value=True), \
                mock.patch("bench.harness_study.process.subprocess.check_output", return_value="99 99 Zs\n100 99 Z\n"):
            self.assertTrue(_group_exited(process))
        for listing in ["99 99 Zs\n100 99 S\n", "100 99 Z\n", ""]:
            with self.subTest(listing=listing), \
                    mock.patch("bench.harness_study.process._leader_exited", return_value=True), \
                    mock.patch("bench.harness_study.process.subprocess.check_output", return_value=listing):
                self.assertFalse(_group_exited(process))
        with mock.patch("bench.harness_study.process._leader_exited", return_value=False):
            self.assertFalse(_group_exited(process))
        with mock.patch("bench.harness_study.process._leader_exited", return_value=True), \
                mock.patch("bench.harness_study.process.subprocess.check_output", side_effect=PermissionError):
            self.assertFalse(_group_exited(process))

    def run_client(self, source, *, backend="codex", seconds=3, expected_model=None):
        with tempfile.TemporaryDirectory() as temporary:
            workspace = Path(temporary).resolve()
            script = workspace / "fake_client.py"
            script.write_text(textwrap.dedent(source))
            records = []
            result = observe([sys.executable, "-u", str(script)], backend=backend,
                             workspace=workspace,
                             environment={"PATH": "/usr/bin:/bin", "HOME": str(workspace)},
                             prompt="fixed prompt", episode={"allowed_paths": ["*.py"]},
                             record=lambda kind, value: records.append((kind, value)),
                             seconds=seconds, expected_model=expected_model)
            return result, records

    def test_zero_exit_empty_or_unframed_output_is_not_success(self):
        for source in ["pass", "print('not a native protocol')", "print('{}')"]:
            with self.subTest(source=source):
                result, _ = self.run_client(source)
                self.assertEqual(result["status"], "agent_failure")
                self.assertEqual(result["returncode"], 0)

    def test_final_unterminated_event_is_parsed_and_usage_is_not_double_counted(self):
        result, records = self.run_client('''
            import json, sys
            event = {"type": "turn.completed", "turn_id": "one", "usage": {
                "input_tokens": 7, "output_tokens": 3, "cached_input_tokens": 2}}
            print(json.dumps(event))
            sys.stdout.write(json.dumps(event))
        ''')
        self.assertEqual(result["status"], "success")
        self.assertEqual(result["metrics"]["input_tokens"], 7)
        self.assertEqual(result["metrics"]["output_tokens"], 3)
        self.assertEqual(result["metrics"]["cache_read_tokens"], 2)
        self.assertEqual(sum(kind == "native" for kind, _ in records), 2)

    def test_stderr_adapter_error_and_nonzero_exit_are_failures(self):
        for source in [
            '''import json, sys
print(json.dumps({"type":"turn.completed"}))
print(json.dumps({"type":"error", "message":"native adapter failed"}), file=sys.stderr)''',
            '''import json, sys
print(json.dumps({"type":"turn.completed"}))
sys.exit(2)''',
        ]:
            with self.subTest(source=source):
                result, records = self.run_client(source)
                self.assertEqual(result["status"], "agent_failure")
                self.assertTrue(any(kind == "native" for kind, _ in records))

    def test_recovered_tool_error_does_not_override_native_completion(self):
        result, _ = self.run_client('''
            import json
            print(json.dumps({"type":"tool_use", "part":{"tool":"bash",
                "state":{"status":"error", "error":"test failed before repair"}}}))
            print(json.dumps({"type":"step_finish", "part":{"id":"finished", "reason":"stop",
                "tokens":{"input":9,"output":4}}}))
        ''', backend="opencode")
        self.assertEqual(result["status"], "success")
        self.assertIn("test failed before repair", result["observed_errors"])

    def test_native_harness_final_state_and_closed_are_preserved(self):
        result, records = self.run_client('''
            import json, sys
            json.loads(sys.stdin.readline())
            print(json.dumps({"type":"state", "model":"frozen-model", "busy":False,
                "task":{"id":"one", "phase":"Complete"}}), flush=True)
            command = json.loads(sys.stdin.readline())
            assert command["type"] == "quit"
            print(json.dumps({"type":"progress", "kind":"status", "text":"saved final journal"}))
            sys.stdout.write(json.dumps({"type":"closed"}))
        ''', backend="harness", expected_model="frozen-model")
        self.assertEqual(result["status"], "success")
        self.assertEqual(sum(kind == "input" and value["type"] == "quit" for kind, value in records), 1)
        native = [value["event_type"] for kind, value in records if kind == "native"]
        self.assertEqual(native, ["state", "progress", "closed"])

    def test_model_mismatch_is_preflight_failure_even_with_complete_state(self):
        result, _ = self.run_client('''
            import json, sys
            json.loads(sys.stdin.readline())
            print(json.dumps({"type":"state", "model":"wrong", "busy":False,
                "task":{"id":"one", "phase":"Complete"}}), flush=True)
            json.loads(sys.stdin.readline())
            print(json.dumps({"type":"closed"}))
        ''', backend="harness", expected_model="frozen-model")
        self.assertEqual(result["status"], "preflight_failure")
        self.assertEqual(result["observed_model"], "wrong")

    def test_harness_repair_waits_without_driver_guidance_and_retains_receipt(self):
        result, records = self.run_client('''
            import json, sys
            json.loads(sys.stdin.readline())
            task = {"id":"repair", "phase":"Working", "last_error":"bad replacement",
                "recovery":{"status":"retrying", "attempts":1, "purpose":"harness_action"},
                "response_receipt":{"requested":"auto", "resolved":"reasoning-off", "attempts":[]},
                "model_requests":[{"purpose":"harness_action", "attempt":1}]}
            state = {"type":"state", "model":"model", "busy":False, "task":task}
            print(json.dumps(state), flush=True)
            print(json.dumps(state), flush=True)
            task.update(phase="Complete", last_error=None, recovery=None)
            task["model_requests"].append({"purpose":"harness_action", "attempt":2})
            print(json.dumps(state), flush=True)
            command = json.loads(sys.stdin.readline())
            assert command["type"] == "quit", command
            print(json.dumps({"type":"closed"}))
        ''', backend="harness", expected_model="model")
        self.assertEqual(result["status"], "success")
        self.assertEqual(result["harness_recovery"]["repair_generations"], 1)
        self.assertEqual(result["harness_recovery"]["model_requests_by_purpose"], {"harness_action": 2})
        self.assertEqual(result["harness_response_receipt"]["resolved"], "reasoning-off")
        self.assertEqual(sum(kind == "harness_response_compatibility" for kind, _ in records), 1)
        self.assertEqual(sum(kind == "harness_recovery" for kind, _ in records), 2)
        self.assertEqual(sum(kind == "input" and value.get("type") == "input" for kind, value in records), 1)

    def test_harness_exhaustion_does_not_renew_budget_with_generic_clarification(self):
        result, records = self.run_client('''
            import json, sys
            json.loads(sys.stdin.readline())
            print(json.dumps({"type":"state", "model":"model", "busy":False,
                "task":{"id":"repair", "phase":"AwaitingInput", "recovery":{
                    "status":"awaiting_guidance", "attempts":3, "diagnostic":"bad target"}}}), flush=True)
            command = json.loads(sys.stdin.readline())
            assert command["type"] == "quit", command
            print(json.dumps({"type":"closed"}))
        ''', backend="harness", expected_model="model")
        self.assertEqual(result["status"], "agent_failure")
        self.assertIn("bad target", result["reason"])
        self.assertEqual(result["harness_recovery"]["last_state"]["attempts"], 3)
        self.assertEqual(sum(kind == "input" and value.get("type") == "input" for kind, value in records), 1)

    def test_repeated_gate_does_not_queue_duplicate_approval(self):
        result, records = self.run_client('''
            import json, sys
            json.loads(sys.stdin.readline())
            task = {"id":"one", "phase":"AwaitingPlan", "steps":1,
                    "plan":{"summary":"small change", "files":["cache.py"], "checks":["python -m unittest"]}}
            state = {"type":"state", "model":"frozen-model", "busy":False, "task":task}
            print(json.dumps(state), flush=True)
            task["events"] = [{"message":"an incidental event"}]
            task["model_requests"] = ["journal detail"]
            print(json.dumps(state), flush=True)
            approval = json.loads(sys.stdin.readline())
            assert approval["text"] == "/approve"
            task["phase"] = "Complete"
            print(json.dumps(state), flush=True)
            command = json.loads(sys.stdin.readline())
            assert command["type"] == "quit", command
            print(json.dumps({"type":"closed"}))
        ''', backend="harness", expected_model="frozen-model")
        self.assertEqual(result["status"], "success")
        self.assertEqual(result["metrics"]["simulated_approvals"], 1)
        self.assertEqual(sum(kind == "input" and value.get("text") == "/approve" for kind, value in records), 1)

    def test_different_capture_page_is_a_new_gate(self):
        state = {"task": {"id":"one", "phase":"AwaitingReview", "capture_cursor":1}}
        decision = {"input":"/no-knowledge"}
        previous = _gate_key(state, decision)
        state["task"]["capture_cursor"] = 2
        self.assertNotEqual(previous, _gate_key(state, decision))

    def test_timeout_drains_native_cancellation_before_shutdown(self):
        result, records = self.run_client('''
            import json, sys
            json.loads(sys.stdin.readline())
            command = json.loads(sys.stdin.readline())
            assert command["type"] == "interrupt"
            print(json.dumps({"type":"state", "model":"frozen-model", "busy":False,
                "task":{"id":"one", "phase":"Cancelled", "cleanup_pending":False}}), flush=True)
            command = json.loads(sys.stdin.readline())
            assert command["type"] == "quit"
            print(json.dumps({"type":"closed"}))
        ''', backend="harness", seconds=0.1, expected_model="frozen-model")
        self.assertTrue(result["timed_out"])
        self.assertEqual(result["status"], "agent_failure")
        raw = b"".join(base64.b64decode(value["raw_base64"]) for kind, value in records if kind == "stdout")
        self.assertIn(b'"Cancelled"', raw)
        self.assertIn(b'"closed"', raw)


if __name__ == "__main__":
    unittest.main()
