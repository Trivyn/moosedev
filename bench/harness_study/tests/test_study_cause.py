import unittest

from bench.harness_study.cause import CONTROLLER_CLASS, TERMINAL_CAUSES, classify


def state(**overrides):
    return {"fatal_error": False, "closed_seen": False, "suppressed_gate_repeats": 0,
            "last_suppressed_phase": None, **overrides}


class CauseTableTests(unittest.TestCase):
    def test_row1_infrastructure_from_status_fatal_error_or_unexplained_exit(self):
        self.assertEqual(classify({"status": "preflight_failure"}, None, None, state()), ("infrastructure", "preflight_failure"))
        self.assertEqual(classify({"status": "infrastructure_failure"}, {}, None, state())[0], "infrastructure")
        self.assertEqual(classify({"status": "agent_failure", "returncode": 0}, {}, None, state(fatal_error=True)),
                         ("infrastructure", "fatal native error"))
        self.assertEqual(classify({"status": "agent_failure", "returncode": 2}, {}, None, state()),
                         ("infrastructure", "returncode 2"))
        # A nonzero exit after a reviewer terminal is not, by itself, infrastructure.
        final = {"terminal": "agent_failure", "cause": "reviewer_scope_rejection", "reason": "scope"}
        self.assertEqual(classify({"status": "agent_failure", "returncode": 2}, {"phase": "AwaitingPlan"}, final, state())[0],
                         "reviewer_scope_rejection")

    def test_row2_success_requires_terminal_closed_and_status(self):
        final = {"terminal": "success", "cause": "success"}
        task = {"phase": "Complete"}
        self.assertEqual(classify({"status": "success", "returncode": 0}, task, final, state(closed_seen=True)),
                         ("success", "complete"))
        self.assertNotEqual(classify({"status": "success", "returncode": 0}, task, final, state())[0], "success")
        self.assertNotEqual(classify({"status": "agent_failure", "returncode": 0}, task, final, state(closed_seen=True))[0], "success")

    def test_row3_awaiting_guidance_is_model_repair_exhausted(self):
        task = {"phase": "AwaitingInput", "last_error": "bad edit", "last_error_kind": "controller_invariant",
                "recovery": {"status": "awaiting_guidance", "purpose": "harness_action"}}
        self.assertEqual(classify({"status": "agent_failure", "returncode": 0}, task, None, state()),
                         ("model_repair_exhausted", "harness_action"))

    def test_rows4_to_7_typed_last_error_kinds(self):
        for kind, expected in (("controller_invariant", ("controller_invariant", "controller_invariant")),
                               ("daemon_rejection", ("daemon_rejection", "daemon_rejection")),
                               ("service", ("infrastructure", "service")),
                               ("model_output", ("runner_error", "model_output")),
                               ("other", ("runner_error", "other"))):
            with self.subTest(kind=kind):
                task = {"phase": "Working", "last_error": "failed", "last_error_kind": kind}
                final = {"terminal": "agent_failure", "cause": "runner_error", "reason": "failed"}
                self.assertEqual(classify({"status": "agent_failure", "returncode": 0, "timed_out": True},
                                          task, final, state()), expected)

    def test_step_cap_is_named_within_the_runner_error_row(self):
        final = {"terminal": "agent_failure", "cause": "runner_error", "reason": "cap"}
        cap = {"phase": "Working", "steps": 256, "last_error_kind": "other",
               "last_error": "task reached 256 model steps; inspect and provide new guidance"}
        outcome = {"status": "agent_failure", "returncode": 0}
        self.assertEqual(classify(outcome, cap, final, state()), ("runner_error", "step_cap"))
        for task in (dict(cap, steps=12), dict(cap, last_error="other failure"), dict(cap, last_error_kind="model_output")):
            with self.subTest(task=task):
                self.assertNotEqual(classify(outcome, task, final, state())[1], "step_cap")
        self.assertIn("runner_error", TERMINAL_CAUSES)
        self.assertNotIn("step_cap", TERMINAL_CAUSES)

    def test_row8_untyped_last_error_is_unknown_only_when_key_is_absent(self):
        task = {"phase": "Working", "last_error": "failed"}
        self.assertEqual(classify({"status": "agent_failure", "returncode": 0}, task, None, state()),
                         ("unknown", "missing last_error_kind"))
        typed_none = dict(task, last_error_kind=None)
        final = {"terminal": "agent_failure", "cause": "reviewer_scope_rejection", "reason": "scope"}
        self.assertEqual(classify({"status": "agent_failure", "returncode": 0}, typed_none, final, state())[0],
                         "reviewer_scope_rejection")

    def test_row9_and_row10_reviewer_terminals(self):
        task = {"phase": "AwaitingPlan", "last_error": None, "last_error_kind": None}
        scope = {"terminal": "agent_failure", "cause": "reviewer_scope_rejection", "reason": "outside"}
        self.assertEqual(classify({"status": "agent_failure", "returncode": 0}, task, scope, state()),
                         ("reviewer_scope_rejection", "outside"))
        cap = {"terminal": "agent_failure", "cause": "clarification_cap", "reason": "exhausted"}
        self.assertEqual(classify({"status": "agent_failure", "returncode": 0}, dict(task, phase="AwaitingInput"),
                                  cap, state()), ("clarification_cap", "exhausted"))

    def test_row11_purpose_exhaustion_uses_the_last_cycle_event(self):
        events = [{"id": "1", "cycle": "c1", "kind": "cycle_started", "detail": ""},
                  {"id": "2", "cycle": "c1", "kind": "purpose_missing_rounds_exhausted", "detail": "3 rounds"},
                  {"id": "3", "cycle": "c1", "kind": "edit_applied", "detail": "cache.py"}]
        task = {"phase": "AwaitingInput", "last_error_kind": None, "intent_events": events}
        outcome = {"status": "agent_failure", "returncode": 0, "timed_out": True}
        self.assertEqual(classify(outcome, task, None, state()),
                         ("purpose_missing_exhausted", "purpose_missing_rounds_exhausted"))
        unresolved = task | {"intent_events": events[:1] + [dict(events[1], kind="purpose_missing_unresolved")]}
        self.assertEqual(classify(outcome, unresolved, None, state()),
                         ("purpose_inventory_empty", "purpose_missing_unresolved"))
        self.assertNotIn("purpose_inventory_empty", CONTROLLER_CLASS)
        restarted = task | {"intent_events": events + [{"id": "4", "cycle": "c2", "kind": "cycle_started", "detail": ""}]}
        self.assertEqual(classify(outcome, restarted, None, state())[0], "deadline_in_phase:AwaitingInput")
        other_phase = dict(task, phase="Working")
        self.assertEqual(classify(outcome, other_phase, None, state())[0], "deadline_in_phase:Working")

    def test_row12_idle_deadline_needs_a_suppressed_repeat_in_the_final_phase(self):
        task = {"phase": "AwaitingPlan", "last_error_kind": None}
        outcome = {"status": "agent_failure", "returncode": 0, "timed_out": True}
        idle = state(suppressed_gate_repeats=1, last_suppressed_phase="AwaitingPlan")
        self.assertEqual(classify(outcome, task, None, idle), ("reviewer_idle_deadline", "AwaitingPlan"))
        moved = state(suppressed_gate_repeats=1, last_suppressed_phase="AwaitingReview")
        self.assertEqual(classify(outcome, task, None, moved), ("deadline_in_phase:AwaitingPlan", "AwaitingPlan"))
        self.assertEqual(classify(outcome, task, None, state())[0], "deadline_in_phase:AwaitingPlan")

    def test_row13_deadline_without_a_snapshot_names_unknown_phase(self):
        outcome = {"status": "agent_failure", "returncode": None, "timed_out": True}
        self.assertEqual(classify(outcome, None, None, state()), ("deadline_in_phase:unknown", None))

    def test_row14_unknown_lists_absent_instrumentation(self):
        outcome = {"status": "agent_failure", "returncode": 0}
        self.assertEqual(classify(outcome, None, None, state()),
                         ("unknown", "final,last_task,last_error_kind,timed_out"))
        instrumented = {"phase": "Working", "last_error": None, "last_error_kind": None}
        cancelled = {"terminal": "agent_failure", "cause": "cancelled", "reason": "task cancelled"}
        self.assertEqual(classify(dict(outcome, timed_out=False), instrumented, cancelled, state()), ("unknown", ""))

    def test_closed_sets(self):
        from bench.harness_study.cause import (GUARD_TERMINAL_CAUSES, HARNESS_TERMINAL_CAUSES,
                                               NATIVE_TERMINAL_CAUSES, RECOVERY_CONTROLLER_CLASS)
        self.assertTrue(CONTROLLER_CLASS <= TERMINAL_CAUSES)
        self.assertEqual(RECOVERY_CONTROLLER_CLASS, {"controller_invariant", "daemon_rejection",
                                                     "reviewer_idle_deadline", "purpose_missing_exhausted"})
        self.assertEqual(CONTROLLER_CLASS, RECOVERY_CONTROLLER_CLASS | {"reviewer_reject_loop"})
        self.assertNotIn("evidence_limit", CONTROLLER_CLASS)
        self.assertIn("deadline_in_phase", TERMINAL_CAUSES)
        self.assertEqual(NATIVE_TERMINAL_CAUSES, {"native_no_completion", "deadline_native"})
        self.assertEqual(GUARD_TERMINAL_CAUSES, {"reviewer_reject_loop", "evidence_limit"})
        self.assertEqual(TERMINAL_CAUSES, HARNESS_TERMINAL_CAUSES | NATIVE_TERMINAL_CAUSES | GUARD_TERMINAL_CAUSES)
        self.assertFalse(NATIVE_TERMINAL_CAUSES & CONTROLLER_CLASS)
        # The recovery identity hashes the harness table; the guards never join it.
        self.assertFalse(GUARD_TERMINAL_CAUSES & HARNESS_TERMINAL_CAUSES)
        self.assertEqual(HARNESS_TERMINAL_CAUSES, {
            "infrastructure", "success", "model_repair_exhausted", "controller_invariant",
            "daemon_rejection", "runner_error", "reviewer_scope_rejection", "clarification_cap",
            "purpose_missing_exhausted", "purpose_inventory_empty", "reviewer_idle_deadline",
            "deadline_in_phase", "unknown"})

    def test_guard_rows_evidence_limit_and_reviewer_reject_loop(self):
        tripped = {"status": "agent_failure", "returncode": 0, "evidence_limit_exceeded": True,
                   "evidence_bytes": 4096}
        # Placed after the infrastructure rows and before success, for any backend.
        self.assertEqual(classify(tripped, {"phase": "Working"}, None, state()), ("evidence_limit", "4096 bytes"))
        self.assertEqual(classify(tripped, None, None, state(backend="opencode", completion_seen=True)),
                         ("evidence_limit", "4096 bytes"))
        self.assertEqual(classify(dict(tripped, status="infrastructure_failure"), None, None, state())[0],
                         "infrastructure")
        self.assertEqual(classify(dict(tripped, returncode=3), None, None, state()), ("infrastructure", "returncode 3"))
        success = {"terminal": "success", "cause": "success"}
        self.assertEqual(classify(dict(tripped, status="success"), {"phase": "Complete"}, success,
                                  state(closed_seen=True))[0], "evidence_limit")
        loop = {"terminal": "agent_failure", "cause": "reviewer_reject_loop",
                "reason": "reviewer rejected the same reuse candidate 5 consecutive times"}
        task = {"phase": "AwaitingReview", "last_error": None, "last_error_kind": None}
        self.assertEqual(classify({"status": "agent_failure", "returncode": 0, "timed_out": True}, task, loop, state()),
                         ("reviewer_reject_loop", loop["reason"]))
        # Typed last_error rows still precede the reviewer terminal.
        errored = dict(task, last_error="failed", last_error_kind="controller_invariant")
        self.assertEqual(classify({"status": "agent_failure", "returncode": 0}, errored, loop, state())[0],
                         "controller_invariant")
        self.assertIn("reviewer_reject_loop", CONTROLLER_CLASS)

    def test_native_rows_follow_infrastructure_and_precede_the_harness_table(self):
        native = lambda **overrides: state(**{"backend": "opencode", "completion_seen": False, **overrides})
        self.assertEqual(classify({"status": "infrastructure_failure", "returncode": 0}, None, None,
                                  native(completion_seen=True))[0], "infrastructure")
        self.assertEqual(classify({"status": "agent_failure", "returncode": 0}, None, None,
                                  native(fatal_error=True, completion_seen=True)), ("infrastructure", "fatal native error"))
        self.assertEqual(classify({"status": "agent_failure", "returncode": 3}, None, None, native()),
                         ("infrastructure", "returncode 3"))
        self.assertEqual(classify({"status": "success", "returncode": 0, "timed_out": False}, None, None,
                                  native(completion_seen=True)), ("success", "complete"))
        deadline = {"status": "agent_failure", "returncode": 0, "timed_out": True, "reason": "budget"}
        self.assertEqual(classify(deadline, None, None, native(completion_seen=True)), ("deadline_native", "budget"))
        exited = {"status": "agent_failure", "returncode": 0, "reason": "native protocol did not confirm completion"}
        self.assertEqual(classify(exited, None, None, native()),
                         ("native_no_completion", "native protocol did not confirm completion"))
        # Completion seen but the driver refused success (e.g. incomplete drain): not silently success.
        cause, detail = classify({"status": "agent_failure", "returncode": 0, "drain_incomplete": True},
                                 None, None, native(completion_seen=True))
        self.assertEqual(cause, "unknown")
        self.assertIn("completion_seen=True", detail)
        self.assertEqual(classify({"status": "agent_failure", "returncode": None}, None, None, native())[0], "unknown")

    def test_harness_rows_are_unchanged_when_backend_is_harness_or_absent(self):
        final = {"terminal": "success", "cause": "success"}
        task = {"phase": "Complete"}
        for extra in ({}, {"backend": "harness", "completion_seen": False}):
            with self.subTest(extra=extra):
                self.assertEqual(classify({"status": "success", "returncode": 0}, task, final,
                                          state(closed_seen=True, **extra)), ("success", "complete"))
                idle = state(suppressed_gate_repeats=1, last_suppressed_phase="AwaitingPlan", **extra)
                self.assertEqual(classify({"status": "agent_failure", "returncode": 0, "timed_out": True},
                                          {"phase": "AwaitingPlan", "last_error_kind": None}, None, idle),
                                 ("reviewer_idle_deadline", "AwaitingPlan"))
                # completion_seen never grants a harness run success without its terminal and closed.
                self.assertNotEqual(classify({"status": "success", "returncode": 0}, task, None,
                                             state(**{**extra, "completion_seen": True}))[0], "success")


if __name__ == "__main__":
    unittest.main()
