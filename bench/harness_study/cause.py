"""Deterministic terminal-cause classification from the final journal snapshot.

Pure functions over recorded dictionaries: no process, clock, or model access.
The first matching row wins; ordering is part of the frozen study identity.

Harness rows (backend "harness" or absent): infrastructure, evidence_limit
(driver evidence-volume guard, any backend), success, model_repair_exhausted,
typed last_error kinds (the runner's step cap is named by its detail,
runner_error/step_cap), reviewer terminals (reviewer_scope_rejection,
clarification_cap, reviewer_reject_loop), purpose exhaustion, symbolic parks
(scope_escape_exhausted, capture_retype_exhausted, check_ungrantable: the runner
parked itself in AwaitingInput after its autonomous bound, or because a required
check's sandbox denial named nothing a grant could cover), deadlines, unknown. Native rows (any
other backend) are decided before the harness rows: infrastructure ->
evidence_limit -> success (completion stop reason and success status) ->
deadline_native -> native_no_completion (exited without a stop reason) ->
unknown. Neither native class is controller class.
"""

# The recovery identity (763ccc52...) hashes this four-class list; later guard
# classes join CONTROLLER_CLASS without rewriting it.
RECOVERY_CONTROLLER_CLASS = frozenset({"controller_invariant", "daemon_rejection",
                                       "reviewer_idle_deadline", "purpose_missing_exhausted"})
# The runner minted unbounded fresh review cards for one rejected candidate.
CONTROLLER_CLASS = RECOVERY_CONTROLLER_CLASS | {"reviewer_reject_loop"}
# The harness table alone is the recovery identity's class list; the native
# classes join the full set for the three-arm baseline. The driver guard
# classes join the full set too; the harness table stays byte-identical.
HARNESS_TERMINAL_CAUSES = frozenset({
    "infrastructure", "success", "model_repair_exhausted", "controller_invariant",
    "daemon_rejection", "runner_error", "reviewer_scope_rejection", "clarification_cap",
    "purpose_missing_exhausted", "purpose_inventory_empty", "reviewer_idle_deadline",
    "deadline_in_phase", "unknown"})
NATIVE_TERMINAL_CAUSES = frozenset({"native_no_completion", "deadline_native"})
GUARD_TERMINAL_CAUSES = frozenset({"reviewer_reject_loop", "evidence_limit"})
TERMINAL_CAUSES = HARNESS_TERMINAL_CAUSES | NATIVE_TERMINAL_CAUSES | GUARD_TERMINAL_CAUSES
# The symbolic harness parks itself after its autonomous bounds (three plan-scope
# escapes, three capture retypes) and when a required check's sandbox denial
# names nothing a grant could cover. All are journaled intent events; none sets
# recovery or last_error. They join a superset for the symbolic identity; the
# sealed recovery and baseline identities hash the sets above unchanged.
SYMBOLIC_HALT_CAUSES = frozenset({"scope_escape_exhausted", "capture_retype_exhausted",
                                  "check_ungrantable"})
# A run that ended because the harness needed a human and none was there. The
# frozen clarification cap is reported separately: it is a reviewer budget.
UNATTENDED_HALT_CLASS = SYMBOLIC_HALT_CAUSES | {"model_repair_exhausted", "reviewer_idle_deadline"}
SYMBOLIC_TERMINAL_CAUSES = TERMINAL_CAUSES | SYMBOLIC_HALT_CAUSES
# After a park, any of these means a human continued the task.
SYMBOLIC_RESUME_KINDS = frozenset({"cycle_started", "plan_approved", "obligations_derived", "edit_applied"})
# Three missing rounds against offered candidates charge the controller; an empty
# accepted inventory is a scenario/knowledge outcome and is reported separately.
PURPOSE_EXHAUSTION_KINDS = {"purpose_missing_rounds_exhausted": "purpose_missing_exhausted",
                            "purpose_missing_unresolved": "purpose_inventory_empty"}
PURPOSE_CYCLE_KINDS = set(PURPOSE_EXHAUSTION_KINDS) | {"cycle_started", "purpose_selection_ready"}


def _last_purpose_event_kind(task):
    kinds = [event.get("kind") for event in task.get("intent_events") or []
             if isinstance(event, dict) and event.get("kind") in PURPOSE_CYCLE_KINDS]
    return kinds[-1] if kinds else None


def last_symbolic_halt(task):
    """The park event when it is the latest park-or-resume event, else None."""
    events = [event for event in task.get("intent_events") or []
              if isinstance(event, dict) and event.get("kind") in SYMBOLIC_HALT_CAUSES | SYMBOLIC_RESUME_KINDS]
    return events[-1] if events and events[-1]["kind"] in SYMBOLIC_HALT_CAUSES else None


def classify(outcome, last_task, final, driver_state):
    """Return (cause, detail) for one observed episode; see the module table."""
    task = last_task or {}
    final = final or {}
    status = outcome.get("status")
    returncode = outcome.get("returncode")
    if status in {"preflight_failure", "infrastructure_failure"}:
        return "infrastructure", status
    if driver_state.get("fatal_error"):
        return "infrastructure", "fatal native error"
    if returncode not in (0, None) and not final:
        return "infrastructure", f"returncode {returncode}"
    if outcome.get("evidence_limit_exceeded"):
        return "evidence_limit", f"{outcome.get('evidence_bytes')} bytes"
    if driver_state.get("backend") not in (None, "harness"):
        return _classify_native(outcome, driver_state)
    if final.get("terminal") == "success" and driver_state.get("closed_seen") and status == "success":
        return "success", "complete"
    recovery = task.get("recovery") or {}
    if recovery.get("status") == "awaiting_guidance":
        return "model_repair_exhausted", recovery.get("purpose")
    if task.get("last_error"):
        kind = task.get("last_error_kind")
        if kind == "controller_invariant":
            return "controller_invariant", kind
        if kind == "daemon_rejection":
            return "daemon_rejection", kind
        if kind == "service":
            return "infrastructure", "service"
        if kind == "other" and str(task["last_error"]).startswith("task reached") \
                and (task.get("steps") or 0) >= 256:
            return "runner_error", "step_cap"
        # context_overflow: the runner stopped because the part of the prompt
        # it never cuts outgrew the budget (it parks in AwaitingInput).
        if kind in {"model_output", "other", "context_overflow"}:
            return "runner_error", kind
        if "last_error_kind" not in task:
            return "unknown", "missing last_error_kind"
    if final.get("cause") == "reviewer_scope_rejection":
        return "reviewer_scope_rejection", final.get("reason")
    if final.get("cause") == "clarification_cap":
        return "clarification_cap", final.get("reason")
    if final.get("cause") == "reviewer_reject_loop":
        return "reviewer_reject_loop", final.get("reason")
    phase = task.get("phase")
    if phase == "AwaitingInput":
        kind = _last_purpose_event_kind(task)
        if kind in PURPOSE_EXHAUSTION_KINDS:
            return PURPOSE_EXHAUSTION_KINDS[kind], kind
        halt = last_symbolic_halt(task)
        if halt is not None:
            return halt["kind"], halt.get("detail")
    if outcome.get("timed_out"):
        if driver_state.get("suppressed_gate_repeats", 0) > 0 and driver_state.get("last_suppressed_phase") == phase:
            return "reviewer_idle_deadline", phase
        return "deadline_in_phase:" + (phase or "unknown"), phase
    absent = [name for name, present in (("final", bool(final)), ("last_task", last_task is not None),
                                         ("last_error_kind", "last_error_kind" in task),
                                         ("timed_out", "timed_out" in outcome)) if not present]
    return "unknown", ",".join(absent)


def _classify_native(outcome, driver_state):
    """Native clients expose no journal; completion is their stop reason."""
    completion = bool(driver_state.get("completion_seen"))
    status = outcome.get("status")
    returncode = outcome.get("returncode")
    if completion and status == "success":
        return "success", "complete"
    if outcome.get("timed_out"):
        return "deadline_native", outcome.get("reason")
    if returncode is not None and not completion:
        return "native_no_completion", outcome.get("reason")
    return "unknown", f"completion_seen={completion},status={status},returncode={returncode}"
