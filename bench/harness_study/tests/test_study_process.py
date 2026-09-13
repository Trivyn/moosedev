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

    def run_client(self, source, *, backend="codex", seconds=3, expected_model=None, **guards):
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
                             seconds=seconds, expected_model=expected_model, **guards)
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

    def test_opencode_edit_marks_first_edit_and_stop_reason_is_native_success(self):
        result, records = self.run_client('''
            import json
            print(json.dumps({"type":"tool_use", "part":{"tool":"read", "state":{"status":"completed"}}}))
            print(json.dumps({"type":"tool_use", "part":{"tool":"edit",
                "state":{"status":"completed", "input":{"filePath":"cache.py"}}}}))
            print(json.dumps({"type":"step_finish", "part":{"id":"finished", "reason":"stop",
                "tokens":{"input":9,"output":4}}}))
        ''', backend="opencode")
        self.assertEqual(result["status"], "success")
        self.assertTrue(result["first_edit_reached"])
        self.assertIsInstance(result["first_edit_seconds"], float)
        self.assertLessEqual(result["first_edit_seconds"], result["metrics"]["elapsed_seconds"])
        self.assertEqual((result["terminal_cause"], result["terminal_detail"]), ("success", "complete"))
        self.assertEqual(sum(kind == "native" and "edit" in value for kind, value in records), 1)
        self.assertEqual(sum(kind == "input" for kind, _ in records), 0)

    def test_opencode_exit_without_stop_reason_is_native_no_completion(self):
        result, _ = self.run_client('''
            import json
            print(json.dumps({"type":"tool_use", "part":{"tool":"bash", "state":{"status":"completed"}}}))
        ''', backend="opencode")
        self.assertEqual(result["status"], "agent_failure")
        self.assertEqual(result["returncode"], 0)
        self.assertFalse(result["first_edit_reached"])
        self.assertEqual(result["terminal_cause"], "native_no_completion")
        self.assertEqual(result["terminal_detail"], "native protocol did not confirm completion")

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


    def test_idle_deadline_counts_suppressed_repeat_and_classifies_reviewer_idle(self):
        result, records = self.run_client("""
            import json, sys
            json.loads(sys.stdin.readline())
            task = {"id":"one", "phase":"AwaitingPlan", "steps":1, "edits":[],
                    "plan":{"summary":"small change", "files":["cache.py"], "checks":["python -m unittest"]}}
            state = {"type":"state", "model":"frozen-model", "busy":False, "task":task}
            print(json.dumps(state), flush=True)
            print(json.dumps(state), flush=True)
            while json.loads(sys.stdin.readline())["type"] != "quit":
                pass
            print(json.dumps({"type":"closed"}))
        """, backend="harness", seconds=0.3, expected_model="frozen-model")
        self.assertTrue(result["timed_out"])
        self.assertEqual(result["metrics"]["simulated_approvals"], 1)
        self.assertEqual(result["suppressed_gate_repeats"], 1)
        self.assertEqual(result["last_suppressed_phase"], "AwaitingPlan")
        self.assertEqual(result["terminal_cause"], "reviewer_idle_deadline")
        self.assertEqual(result["terminal_detail"], "AwaitingPlan")
        self.assertFalse(result["first_edit_reached"])
        self.assertIsNone(result["first_edit_seconds"])
        self.assertEqual(sum(kind == "input" and value.get("text") == "/approve" for kind, value in records), 1)

    def test_deadline_snapshot_is_taken_before_the_interrupt_cancels_a_busy_task(self):
        result, _ = self.run_client("""
            import json, sys
            json.loads(sys.stdin.readline())
            task = {"id":"one", "phase":"Planning", "steps":3, "edits":[], "intent_events":[]}
            print(json.dumps({"type":"state", "model":"frozen-model", "busy":True, "task":task}), flush=True)
            assert json.loads(sys.stdin.readline())["type"] == "interrupt"
            task["phase"] = "Cancelled"
            print(json.dumps({"type":"state", "model":"frozen-model", "busy":False, "task":task}), flush=True)
            assert json.loads(sys.stdin.readline())["type"] == "quit"
            print(json.dumps({"type":"closed"}))
        """, backend="harness", seconds=0.3, expected_model="frozen-model")
        self.assertTrue(result["timed_out"])
        self.assertEqual(result["terminal_cause"], "deadline_in_phase:Planning")
        self.assertEqual(result["terminal_detail"], "Planning")

    def test_first_applied_edit_is_timed_relative_to_episode_start(self):
        result, _ = self.run_client("""
            import json, sys
            json.loads(sys.stdin.readline())
            task = {"id":"one", "phase":"Working", "edits":[]}
            state = {"type":"state", "model":"model", "busy":True, "task":task}
            print(json.dumps(state), flush=True)
            task.update(edits=[{"file":"cache.py"}])
            print(json.dumps(state), flush=True)
            task.update(phase="Complete")
            state["busy"] = False
            print(json.dumps(state), flush=True)
            assert json.loads(sys.stdin.readline())["type"] == "quit"
            print(json.dumps({"type":"closed"}))
        """, backend="harness", expected_model="model")
        self.assertEqual(result["status"], "success")
        self.assertTrue(result["first_edit_reached"])
        self.assertIsInstance(result["first_edit_seconds"], float)
        self.assertGreaterEqual(result["first_edit_seconds"], 0)
        self.assertLessEqual(result["first_edit_seconds"], result["metrics"]["elapsed_seconds"])
        self.assertEqual((result["terminal_cause"], result["terminal_detail"]), ("success", "complete"))
        self.assertEqual(result["suppressed_gate_repeats"], 0)

    def test_clarification_cap_is_a_typed_terminal_cause(self):
        result, records = self.run_client("""
            import json, sys
            json.loads(sys.stdin.readline())
            for step in range(4):
                print(json.dumps({"type":"state", "model":"model", "busy":False,
                    "task":{"id":"one", "phase":"AwaitingInput", "steps":step,
                            "last_error":None, "last_error_kind":None}}), flush=True)
                if json.loads(sys.stdin.readline())["type"] == "quit":
                    break
            print(json.dumps({"type":"closed"}))
        """, backend="harness", expected_model="model")
        self.assertEqual(result["status"], "agent_failure")
        self.assertEqual(result["reason"], "exhausted frozen clarification responses")
        self.assertEqual(result["terminal_cause"], "clarification_cap")
        self.assertEqual(result["terminal_detail"], result["reason"])
        # The frozen prompt plus exactly three clarification responses; the fourth gate quits.
        self.assertEqual(sum(kind == "input" and value.get("type") == "input" for kind, value in records), 4)
        self.assertEqual(sum(kind == "input" and value.get("type") == "quit" for kind, value in records), 1)

    def test_evidence_volume_guard_interrupts_and_classifies_evidence_limit(self):
        result, records = self.run_client("""
            import json, sys
            json.loads(sys.stdin.readline())
            for step in range(5):
                print(json.dumps({"type":"progress", "kind":"status", "text":"x" * 4000}), flush=True)
            assert json.loads(sys.stdin.readline())["type"] == "interrupt"
            assert json.loads(sys.stdin.readline())["type"] == "quit"
            print(json.dumps({"type":"closed"}))
        """, backend="harness", expected_model="model", evidence_byte_limit=10_000)
        self.assertEqual(result["status"], "agent_failure")
        self.assertTrue(result["evidence_limit_exceeded"])
        self.assertEqual(result["error"], "evidence volume limit exceeded")
        self.assertEqual(result["evidence_byte_limit"], 10_000)
        self.assertGreater(result["evidence_bytes"], 10_000)
        self.assertEqual(result["terminal_cause"], "evidence_limit")
        self.assertEqual(result["terminal_detail"], f"{result['evidence_bytes']} bytes")
        self.assertFalse(result.get("timed_out"))
        inputs = [value["type"] for kind, value in records if kind == "input"]
        self.assertEqual(inputs, ["input", "interrupt", "quit"])
        # Every recorded raw line counts toward the guard, including the drained tail.
        recorded = sum(len(base64.b64decode(value["raw_base64"])) for kind, value in records
                       if kind in {"stdout", "stderr"})
        self.assertEqual(result["evidence_bytes"], recorded)

    def test_evidence_guard_is_disabled_by_default_and_volume_is_still_reported(self):
        result, _ = self.run_client('''
            import json
            print(json.dumps({"type": "turn.completed", "turn_id": "one"}))
        ''')
        self.assertEqual(result["status"], "success")
        self.assertIsNone(result["evidence_byte_limit"])
        self.assertNotIn("evidence_limit_exceeded", result)
        self.assertGreater(result["evidence_bytes"], 0)
        self.assertEqual((result["reject_loop_limit"], result["reject_loop_max_streak"]), (5, 0))

    REJECT_LOOP_CLIENT = """
        import json, sys
        json.loads(sys.stdin.readline())
        candidates = {candidates!r}
        operation = 0
        while True:
            operation += 1
            if operation > len(candidates):
                task = {{"id":"one", "phase":"Complete", "last_error":None, "last_error_kind":None}}
            else:
                reuse = {{"operation_id": f"op-{{operation}}", "candidate_iri": candidates[operation - 1],
                         "candidate_title": "Existing", "recommendation_source": "human_required"}}
                task = {{"id":"one", "phase":"AwaitingReview", "last_error":None, "last_error_kind":None,
                        "reviews":[{{"capture_resolution": reuse,
                                    "request": {{"operation_id": f"op-{{operation}}", "proposals": []}}}}]}}
            print(json.dumps({{"type":"state", "model":"model", "busy":False, "task":task}}), flush=True)
            command = json.loads(sys.stdin.readline())
            if command["type"] == "quit":
                break
            assert command["text"] == f"/reject op-{{operation}}", command
        print(json.dumps({{"type":"closed"}}))
    """

    def test_reviewer_reject_loop_guard_stops_repeated_rejection_of_one_candidate(self):
        # The runner keeps minting fresh cards for one candidate the reviewer must refuse.
        result, records = self.run_client(self.REJECT_LOOP_CLIENT.format(candidates=["urn:existing"] * 50),
                                          backend="harness", expected_model="model")
        self.assertEqual(result["status"], "agent_failure")
        self.assertEqual(result["reason"], "reviewer rejected the same reuse candidate 5 consecutive times")
        self.assertEqual(result["terminal_cause"], "reviewer_reject_loop")
        self.assertEqual(result["terminal_detail"], result["reason"])
        self.assertEqual(result["reject_loop_max_streak"], 5)
        self.assertEqual(result["reject_loop_limit"], 5)
        self.assertFalse(result.get("timed_out"))
        rejects = [value["text"] for kind, value in records if kind == "input" and value.get("type") == "input"
                   and value.get("text", "").startswith("/reject")]
        self.assertEqual(rejects, [f"/reject op-{n}" for n in range(1, 6)])
        self.assertEqual(sum(kind == "input" and value["type"] == "quit" for kind, value in records), 1)

    def test_reject_streak_resets_when_the_candidate_changes(self):
        candidates = ["urn:a", "urn:b"] * 3
        result, records = self.run_client(self.REJECT_LOOP_CLIENT.format(candidates=candidates),
                                          backend="harness", expected_model="model")
        self.assertEqual(result["status"], "success", result)
        self.assertEqual(result["reject_loop_max_streak"], 1)
        self.assertEqual(result["terminal_cause"], "success")
        self.assertEqual(sum(kind == "input" and value.get("text", "").startswith("/reject")
                             for kind, value in records), 6)
        # The same run with a limit of two stops at the second candidate change-free streak.
        result, _ = self.run_client(self.REJECT_LOOP_CLIENT.format(candidates=["urn:a", "urn:a", "urn:b"]),
                                    backend="harness", expected_model="model", reject_loop_limit=2)
        self.assertEqual(result["terminal_cause"], "reviewer_reject_loop")
        self.assertEqual(result["reject_loop_max_streak"], 2)
        self.assertEqual(result["reason"], "reviewer rejected the same reuse candidate 2 consecutive times")
        # None disables the guard entirely.
        result, _ = self.run_client(self.REJECT_LOOP_CLIENT.format(candidates=["urn:a"] * 7),
                                    backend="harness", expected_model="model", reject_loop_limit=None)
        self.assertEqual(result["status"], "success", result)
        self.assertEqual(result["reject_loop_max_streak"], 7)
        self.assertIsNone(result["reject_loop_limit"])

    def test_symbolic_policy_metrics_count_derived_decisions_and_model_purposes(self):
        import json
        from bench.harness_study.process import symbolic_metrics
        final_task = {"id": "one", "phase": "Complete", "schema": 2,
                      "recovery": None, "response_receipt": {"requested": "auto", "resolved": "reasoning-off"},
                      "intent_events": [
                          {"id": "a", "cycle": "c1", "kind": "obligations_derived", "detail": "1 files"},
                          {"id": "b", "cycle": "c1", "kind": "scope_escape_replan", "detail": "other.py: escape 1 of 3"},
                          {"id": "c", "cycle": None, "kind": "association_derived", "detail": "labels.py normalize"},
                          {"id": "d", "cycle": None, "kind": "capture_deferred", "detail": "3 events"},
                          {"id": "d", "cycle": None, "kind": "capture_deferred", "detail": "3 events"},
                          {"id": "e", "cycle": None, "kind": "capture_note", "detail": "80 bytes"},
                          {"id": "f", "cycle": None, "kind": "capture_typed", "detail": "SymbolicOnly, 2 proposals"},
                          {"id": "g", "cycle": None, "kind": "reconciled_restates", "detail": "x restates y"},
                          {"id": "h", "cycle": None, "kind": "reconciled_distinct", "detail": "Lesson z"},
                          {"id": "i", "cycle": None, "kind": "link_review", "detail": "accepted"}],
                      "events": [{"message": "Read labels.py: source"}],
                      "model_requests": [{"purpose": "harness_action", "attempt": 1, "decision_id": "d1"},
                                         {"purpose": "harness_capture_note", "attempt": 1, "decision_id": "d2"}]}
        expected = {"obligations_derived": 1, "obligations_unresolved": 0, "scope_escape_replan": 1,
                    "scope_escape_exhausted": 0, "noop_edit_continuation": 0, "association_derived": 1,
                    "association_none": 0, "association_skipped": 0, "association_unresolved": 0,
                    "capture_deferred": 1, "capture_note": 1, "capture_typed": 1, "reconciled_restates": 1,
                    "reconciled_refines": 0, "reconciled_distinct": 1, "plan_check_rejected": 0,
                    "check_unrunnable": 0, "capture_notes": 1,
                    "structured_model_decisions": 0, "autonomous_recoveries": 1}
        self.assertEqual(symbolic_metrics(final_task["intent_events"], final_task["model_requests"]), expected,
                         "duplicate journal ids count once")
        result, records = self.run_client(f"""
            import json, sys
            json.loads(sys.stdin.readline())
            print(json.dumps({{"type":"state", "model":"model", "busy":False, "task":json.loads({json.dumps(final_task)!r})}}), flush=True)
            assert json.loads(sys.stdin.readline())["type"] == "quit"
            print(json.dumps({{"type":"closed"}}))
        """, backend="harness", expected_model="model")
        self.assertEqual(result["status"], "success")
        self.assertEqual(result["symbolic"], expected)
        self.assertEqual(result["metrics"]["symbolic_structured_model_decisions"], 0)
        self.assertEqual(result["metrics"]["symbolic_capture_notes"], 1)
        self.assertEqual(result["harness_recovery"]["decisions"]["d2"],
                         {"purpose": "harness_capture_note", "attempts": [1]})

    def test_schema_2_journals_derive_review_metrics_without_a_capture_contract(self):
        import json
        from bench.harness_study.evolution import review_metrics
        events = [{"id": "a", "cycle": "c1", "kind": "plan_approval_attempt", "detail": "1"},
                  {"id": "b", "cycle": "c1", "kind": "record_review", "detail": "accepted", "interaction": "r1"},
                  {"id": "c", "cycle": "c1", "kind": "edit_applied", "detail": "cache.py"}]
        for schema, expected in ((2, True), (1, False)):
            task = {"id": "one", "schema": schema, "phase": "Complete", "recovery": None, "intent_events": events,
                    "events": [], "model_requests": [{"purpose": "harness_action", "attempt": 1, "decision_id": "d1"}]}
            result, _ = self.run_client(f"""
                import json, sys
                json.loads(sys.stdin.readline())
                print(json.dumps({{"type":"state", "model":"model", "busy":False, "task":{json.dumps(task)!r} and json.loads({json.dumps(task)!r})}}), flush=True)
                assert json.loads(sys.stdin.readline())["type"] == "quit"
                print(json.dumps({{"type":"closed"}}))
            """, backend="harness", expected_model="model")
            self.assertEqual(result["status"], "success")
            self.assertEqual("evolution_reviews" in result, expected, schema)
            if expected:
                self.assertEqual(result["evolution_reviews"], review_metrics(events))
                self.assertEqual(result["metrics"]["evolution_review_interactions"], 1)
                self.assertEqual(result["metrics"]["symbolic_structured_model_decisions"], 0)

    def test_journal_metrics_are_computed_once_from_the_final_snapshot(self):
        import json
        from bench.harness_study.evolution import review_metrics
        from bench.harness_study.intent import activity_metrics, gate_metrics
        final_task = {"id": "one", "phase": "Complete", "capture_contract": 2, "intent_policy": "change-level-v2",
                      "recovery": None, "response_receipt": {"requested": "auto", "resolved": "reasoning-off"},
                      "intent_events": [
                          {"id": "a", "cycle": "c1", "kind": "plan_approval_attempt", "detail": "1"},
                          {"id": "b", "cycle": "c1", "kind": "record_review", "detail": "accepted", "interaction": "r1"},
                          {"id": "c", "cycle": "c1", "kind": "reuse_review", "detail": "accepted op", "interaction": "r1"},
                          {"id": "d", "cycle": "c1", "kind": "edit_applied", "detail": "cache.py"}],
                      "events": [{"message": "Read cache.py: source"}, {"message": "Read cache.py: source"},
                                 {"message": "Model action: {\"action\": \"command\", \"command\": \"pytest\"}"}],
                      "model_requests": [{"purpose": "harness_action", "attempt": 1, "decision_id": "d1"},
                                         {"purpose": "harness_action", "attempt": 2, "decision_id": "d1"},
                                         {"purpose": "harness_capture", "attempt": 1, "decision_id": "d2"}]}
        early = dict(final_task, phase="Working", intent_events=final_task["intent_events"][:1],
                     events=final_task["events"][:1], model_requests=final_task["model_requests"][:1],
                     recovery={"status": "retrying", "attempts": 1})
        result, records = self.run_client(f"""
            import json, sys
            json.loads(sys.stdin.readline())
            for task in ({json.dumps(early)!r}, {json.dumps(final_task)!r}):
                print(json.dumps({{"type":"state", "model":"model", "busy":False, "task":json.loads(task)}}), flush=True)
            assert json.loads(sys.stdin.readline())["type"] == "quit"
            print(json.dumps({{"type":"closed"}}))
        """, backend="harness", expected_model="model")
        self.assertEqual(result["status"], "success")
        self.assertEqual(result["intent_gates"], gate_metrics(final_task["intent_events"]))
        self.assertEqual(result["intent_activity"], activity_metrics(final_task["events"]))
        self.assertEqual(result["evolution_reviews"], review_metrics(final_task["intent_events"]))
        self.assertEqual(result["metrics"]["intent_gate_decisions"], 2)
        self.assertEqual(result["metrics"]["evolution_review_interactions"], 1)
        self.assertEqual(result["metrics"]["intent_source_rereads_unchanged"], 1)
        self.assertEqual(result["harness_recovery"], {
            "last_state": None, "model_requests_by_purpose": {"harness_action": 2, "harness_capture": 1},
            "decisions": {"d1": {"purpose": "harness_action", "attempts": [1, 2]},
                          "d2": {"purpose": "harness_capture", "attempts": [1]}},
            "repair_generations": 1})
        self.assertEqual(sum(kind == "harness_recovery" for kind, _ in records), 2)
        self.assertEqual(sum(kind == "harness_response_compatibility" for kind, _ in records), 1)


if __name__ == "__main__":
    unittest.main()
