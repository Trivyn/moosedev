"""Bounded native-session observation and ordinary scripted reviewer inputs."""
import base64
import hashlib
import json
import os
import re
import selectors
import signal
import subprocess
import time

from .artifacts import canonical_json
from .adapters import normalize_event
from .cause import classify
from .reviewer import review_input
from .usage import UsageLedger, resource_metrics


def _gate_key(event, decision):
    """Gate identity excludes incidental transcript/output changes in snapshots."""
    task = event.get("task") or {}
    fields = ("id", "phase", "steps", "plan", "pending_edit", "knowledge_revision",
              "capture_request", "reviews", "capture_cursor", "capture_offset",
              "capture_end", "capture_end_offset", "capture_checkpoint_end",
              "last_response", "turn_finished")
    fields += ("intent_policy", "intent_cycle")
    return hashlib.sha256(canonical_json({"gate": {key: task.get(key) for key in fields},
                                          "input": decision["input"]})).hexdigest()


def _leader_exited(process):
    # WNOWAIT retains the leader as an unreaped child. Its PID cannot be reused
    # before the single group termination, even if descendants retain pipes.
    result = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
    return result is not None and result.si_pid == process.pid


def _group_exited(process):
    """Darwin may report EPERM for a group containing only zombies.

    The unreaped owned leader reserves the PGID. Ignore that error only after
    confirming the leader exited and every remaining group member is a zombie;
    denied inspection or any live member remains a cleanup failure.
    """
    try:
        if not _leader_exited(process):
            return False
        listing = subprocess.check_output(
            ["/bin/ps", "-axo", "pid=,pgid=,stat="], text=True, timeout=2,
            env={"PATH": "/usr/bin:/bin"},
        )
        rows = (line.split() for line in listing.splitlines())
        members = [row for row in rows if len(row) == 3 and row[1] == str(process.pid)]
        return (any(row[0] == str(process.pid) for row in members)
                and all(row[2].startswith("Z") for row in members))
    except (OSError, subprocess.SubprocessError):
        return False


MODEL_DECISION_PURPOSES = frozenset({
    "harness_action", "harness_capture", "harness_capture_resolution",
    "harness_purpose_selection", "harness_association_selection", "harness_capture_note"})
# Every structured decision the symbolic policy takes away from the coding model.
SYMBOLIC_STRUCTURED_PURPOSES = frozenset({
    "harness_capture", "harness_capture_resolution", "harness_purpose_selection",
    "harness_association_selection"})
SYMBOLIC_EVENT_KINDS = (
    "obligations_derived", "obligations_unresolved", "scope_escape_replan", "scope_escape_exhausted",
    "noop_edit_continuation", "association_derived", "association_none", "association_skipped",
    "association_unresolved", "capture_deferred", "capture_note", "capture_typed",
    "reconciled_restates", "reconciled_refines", "reconciled_distinct",
    "plan_check_rejected", "check_unrunnable", "replan_continuation", "replan_noop", "model_replan",
    "final_review_attested", "knowledge_search", "capture_anchored",
    # Delivery nudges: the standing guidance snapshot, plan coverage returns, edit-time grounding and
    # the grounding of a disputed approved plan. None is an autonomous recovery.
    "guidance_loaded", "constraint_coverage", "constraint_coverage_unmet", "edit_grounding", "plan_grounding",
    # A replace whose stray envelope junk the harness trimmed before materializing the edit; not an
    # autonomous recovery either.
    "replace_text_repair",
    # Tool-call decoding under the tools action contract: repaired arguments, extra calls that did not run,
    # a call written as text, and the provider's refusal of a required tool choice. None is an autonomous
    # recovery.
    "tool_arguments_repaired", "extra_tool_calls_ignored", "tool_call_from_content", "tool_choice_fallback")
# The runner journals one `capture_anchored` event per capture operation with this detail.
CAPTURE_ANCHOR_COUNTS = re.compile(r"^(\d+) definition anchors, (\d+) module anchors, (\d+) unanchored files, "
                                   r"(\d+) anchor notes, (\d+) restated links$")
CAPTURE_ANCHOR_FIELDS = ("definition_anchors", "module_anchors", "unanchored_files", "anchor_notes", "restated_links")


def symbolic_metrics(events, model_requests):
    """Per-run counts of the symbolic policy's derived decisions and recoveries.

    ``structured_model_decisions`` must be zero for a compliant run: the coding
    model answered only actions and the one capture note.
    """
    unique = {}
    for event in events:
        if isinstance(event, dict):
            unique[event.get("id", id(event))] = event
    kinds = {}
    for event in unique.values():
        kind = event.get("kind", "unknown")
        kinds[kind] = kinds.get(kind, 0) + 1
    metrics = {kind: kinds.get(kind, 0) for kind in SYMBOLIC_EVENT_KINDS}
    metrics.update({field: 0 for field in CAPTURE_ANCHOR_FIELDS})
    for event in unique.values():
        if event.get("kind") != "capture_anchored":
            continue
        counts = CAPTURE_ANCHOR_COUNTS.match(str(event.get("detail", "")))
        if counts is None:
            raise ValueError(f"unrecognized capture_anchored detail: {event.get('detail')!r}")
        for field, value in zip(CAPTURE_ANCHOR_FIELDS, counts.groups()):
            metrics[field] += int(value)
    metrics["capture_notes"] = sum(1 for request in model_requests if isinstance(request, dict)
                                   and request.get("purpose") == "harness_capture_note")
    metrics["structured_model_decisions"] = sum(
        1 for request in model_requests if isinstance(request, dict)
        and request.get("purpose") in SYMBOLIC_STRUCTURED_PURPOSES)
    metrics["autonomous_recoveries"] = (metrics["scope_escape_replan"] + metrics["noop_edit_continuation"]
                                        + metrics["replan_continuation"])
    return metrics


def graph_authority_metrics(task):
    """Whether graph answers were used instead of combing source, per task.

    ``knowledge_answered_searches`` counts searches that returned accepted
    knowledge; ``unplanned_unedited_reads`` counts file reads of files no plan
    named and no edit touched.
    """
    answered = 0
    for event in task.get("intent_events") or []:
        if isinstance(event, dict) and event.get("kind") == "knowledge_search":
            records = str(event.get("detail", "")).split(" ", 1)[0]
            if records.isdigit() and int(records) > 0:
                answered += 1
    events = [event.get("message", "") for event in task.get("events") or [] if isinstance(event, dict)]
    planned = set((task.get("plan") or {}).get("files") or [])
    for message in events:
        if message.startswith("Proposed plan: "):
            try:
                plan = json.loads(message[len("Proposed plan: "):])
            except ValueError:
                continue
            if isinstance(plan, dict):
                planned.update(plan.get("files") or [])
    edited = {edit.get("file") for edit in task.get("edits") or [] if isinstance(edit, dict)}
    reads = 0
    for message in events:
        if message.startswith("Read ") and ": " in message:
            file = message[len("Read "):].split(": ", 1)[0]
            if file not in planned and file not in edited:
                reads += 1
    return {"knowledge_answered_searches": answered, "unplanned_unedited_reads": reads}


def _journal_metrics(outcome, task):
    """Derive intent/evolution/recovery metrics once from the final state snapshot."""
    if task is None:
        return
    if "intent_events" in task:
        from .intent import gate_metrics, activity_metrics
        from .evolution import review_metrics
        outcome["intent_gates"] = gate_metrics(task["intent_events"])
        for field in ("plan_approval_attempts", "record_dispositions", "link_dispositions",
                      "gate_decisions", "approval_cycles", "accepted_revision_changes"):
            outcome["metrics"]["intent_" + field] = outcome["intent_gates"][field]
        outcome["intent_activity"] = activity_metrics(task.get("events", []))
        for field, value in outcome["intent_activity"].items():
            if isinstance(value, int):
                outcome["metrics"]["intent_" + field] = value
        if (task.get("schema", 1) >= 2 or task.get("capture_contract", 1) >= 2
                or task.get("intent_policy") == "change-level-v2"):
            outcome["evolution_reviews"] = review_metrics(task["intent_events"])
            for field in ("record_dispositions", "attached_link_dispositions", "reuse_dispositions",
                          "individual_dispositions", "plan_approval_attempts", "gate_decisions",
                          "review_interactions", "approval_cycles"):
                outcome["metrics"]["evolution_" + field] = outcome["evolution_reviews"][field]
    if (task.get("schema", 1) >= 2 or task.get("intent_policy") == "symbolic") and "intent_events" in task:
        outcome["symbolic"] = symbolic_metrics(task["intent_events"], task.get("model_requests") or [])
        outcome["symbolic"].update(graph_authority_metrics(task))
        for field, value in outcome["symbolic"].items():
            if isinstance(value, int):
                outcome["metrics"]["symbolic_" + field] = value
    requests = task.get("model_requests") or []
    purposes = {}
    decisions = {}
    for request in requests:
        purpose = request.get("purpose", "unknown") if isinstance(request, dict) else "unknown"
        purposes[purpose] = purposes.get(purpose, 0) + 1
        if purpose in MODEL_DECISION_PURPOSES and request.get("decision_id"):
            decision = decisions.setdefault(request["decision_id"], {"purpose": purpose, "attempts": []})
            decision["attempts"].append(request.get("attempt"))
    candidates = [request for request in requests if isinstance(request, dict)
                  and request.get("purpose") in MODEL_DECISION_PURPOSES]
    outcome["harness_recovery"] = {"last_state": task.get("recovery"), "model_requests_by_purpose": purposes,
        "decisions": decisions,
        "repair_generations": sum(request["attempt"] > 1 for request in candidates)
        if candidates and all(type(request.get("attempt")) is int for request in candidates) else None}


def _reject_target(task, decision):
    """The reuse candidate a /reject decision refuses, or None for any other input."""
    if not decision["input"].startswith("/reject"):
        return None
    reviews = task.get("reviews") or []
    outer = reviews[0] if reviews and isinstance(reviews[0], dict) else {}
    resolution = outer.get("capture_resolution")
    return resolution.get("candidate_iri") if isinstance(resolution, dict) else None


def observe(command, *, backend, workspace, environment, prompt, episode,
            record, seconds=1200, expected_model=None, evidence_byte_limit=None,
            reject_loop_limit=5):
    """Guards: `evidence_byte_limit` bounds the bytes recorded across channels;
    `reject_loop_limit` bounds consecutive rejections of one reuse candidate.
    Either guard is disabled by None."""
    if seconds <= 0:
        raise ValueError("episode deadline must be positive")
    started = time.monotonic()
    process = subprocess.Popen(command, cwd=workspace, env=environment, stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               start_new_session=True, close_fds=True)
    selector = selectors.DefaultSelector()
    buffers = {"stdout": b"", "stderr": b""}
    pending_input = bytearray()
    outcome = {"status": "agent_failure", "returncode": None, "metrics": {
        "elapsed_seconds": None, "simulated_approvals": 0, "input_tokens": None,
        "output_tokens": None, "cache_read_tokens": None, "helper_tokens": None}}
    seen_reviews = set()
    token_usage = UsageLedger(backend)
    clarification_count = 0
    final = None
    shutdown_deadline = exit_time = None
    completion_seen = closed_seen = fatal_error = False
    native_events = 0
    last_recovery_observation = None
    last_response_receipt = None
    last_task = first_edit_monotonic = last_suppressed_phase = None
    suppressed_gate_repeats = 0
    evidence_bytes = 0
    reject_streak_candidate = None
    reject_streak = reject_loop_max_streak = 0

    def send(text=None, kind="input", reason=None):
        value = {"type": kind}
        if text is not None:
            value["text"] = text
        record("input", {**value, "reason": reason})
        if process.stdin.closed:
            return
        pending_input.extend(canonical_json(value))
        try:
            selector.get_key(process.stdin)
        except KeyError:
            selector.register(process.stdin, selectors.EVENT_WRITE, "stdin")

    def begin_shutdown(reason, *, interrupt=False):
        nonlocal shutdown_deadline
        if shutdown_deadline is not None:
            return
        shutdown_deadline = time.monotonic() + 5
        if backend == "harness":
            if interrupt:
                send(kind="interrupt", reason=reason)
            send(kind="quit", reason=reason)

    def consume(channel, line):
        nonlocal final, clarification_count, completion_seen, closed_seen
        nonlocal fatal_error, native_events
        nonlocal last_recovery_observation, last_response_receipt
        nonlocal last_task, first_edit_monotonic, last_suppressed_phase, suppressed_gate_repeats
        nonlocal evidence_bytes, reject_streak_candidate, reject_streak, reject_loop_max_streak
        record(channel, {"raw_base64": base64.b64encode(line).decode(),
                         "text": line.decode(errors="replace")})
        evidence_bytes += len(line)
        if (evidence_byte_limit is not None and evidence_bytes > evidence_byte_limit
                and shutdown_deadline is None):
            outcome.update(error="evidence volume limit exceeded", evidence_limit_exceeded=True,
                           evidence_bytes=evidence_bytes)
            begin_shutdown("evidence volume limit", interrupt=True)
        try:
            event = json.loads(line)
        except (ValueError, UnicodeError):
            return
        if not isinstance(event, dict):
            return
        # Adapter errors can appear on stderr. Diagnostic JSON there is retained
        # but cannot count as successful native protocol completion.
        if channel == "stderr" and event.get("type") not in {"error", "turn.failed"}:
            return
        observation = normalize_event(backend, event)
        record("native", {**observation, "channel": channel})
        native_events += 1
        if backend != "harness" and first_edit_monotonic is None and "edit" in observation:
            first_edit_monotonic = time.monotonic()
        event_type = event.get("type")
        token_usage.native(event)
        if observation.get("error"):
            outcome.setdefault("observed_errors", []).append(observation["error"])
        if event_type in {"error", "turn.failed"}:
            fatal_error = True
            outcome["observed_error"] = observation.get("error") or event_type
        if backend in {"codex", "codex_mcp"} and event_type == "turn.completed":
            completion_seen = True
        if backend in {"opencode", "opencode_mcp"} and event_type == "step_finish":
            part = event.get("part") if isinstance(event.get("part"), dict) else {}
            completion_seen = part.get("reason") in {"stop", "end_turn"}
        if backend == "harness" and event_type == "closed":
            closed_seen = True
        if backend != "harness" or event_type != "state":
            return
        task = event.get("task") or {}
        # The deadline interrupt cancels a busy task, and the session then emits
        # one more state whose phase is Cancelled. The terminal cause names the
        # phase at the deadline, so the last task freezes when shutdown begins.
        if shutdown_deadline is None or last_task is None:
            last_task = task
        if first_edit_monotonic is None and task.get("edits"):
            first_edit_monotonic = time.monotonic()
        recovery = task.get("recovery")
        receipt = task.get("response_receipt")
        if receipt is not None:
            outcome["harness_response_receipt"] = receipt
            if receipt != last_response_receipt:
                record("harness_response_compatibility", {"task_id": task.get("id"), "receipt": receipt})
                last_response_receipt = receipt
        observation = {"task_id": task.get("id"),
                       "model_request_count": len(task.get("model_requests") or []),
                       "recovery": recovery}
        if observation != last_recovery_observation:
            record("harness_recovery", observation)
            last_recovery_observation = observation
        if expected_model is not None and event.get("model") != expected_model:
            outcome.update(status="preflight_failure", error="harness state model differs from frozen model",
                           expected_model=expected_model, observed_model=event.get("model"))
            begin_shutdown("model identity mismatch")
            return
        if shutdown_deadline is not None:
            return
        decision = review_input(event, episode)
        if not decision:
            return
        if "terminal" in decision:
            final = decision
            begin_shutdown("terminal task state observed")
            return
        key = _gate_key(event, decision)
        if key in seen_reviews:
            suppressed_gate_repeats += 1
            last_suppressed_phase = task.get("phase")
            return
        seen_reviews.add(key)
        if decision["input"].startswith("/"):
            outcome["metrics"]["simulated_approvals"] += 1
        else:
            clarification_count += 1
            if clarification_count > 3:
                final = {"terminal": "agent_failure", "reason": "exhausted frozen clarification responses",
                         "cause": "clarification_cap"}
                begin_shutdown(final["reason"])
                return
        send(decision["input"], reason=decision["reason"])
        candidate = _reject_target(task, decision)
        if candidate is None:
            reject_streak_candidate, reject_streak = None, 0
        elif candidate == reject_streak_candidate:
            reject_streak += 1
        else:
            reject_streak_candidate, reject_streak = candidate, 1
        reject_loop_max_streak = max(reject_loop_max_streak, reject_streak)
        if reject_loop_limit is not None and reject_streak >= reject_loop_limit:
            final = {"terminal": "agent_failure", "cause": "reviewer_reject_loop",
                     "reason": f"reviewer rejected the same reuse candidate {reject_streak} consecutive times"}
            begin_shutdown(final["reason"])

    def drain(channel, *, eof=False):
        while b"\n" in buffers[channel]:
            line, buffers[channel] = buffers[channel].split(b"\n", 1)
            consume(channel, line + b"\n")
        if buffers[channel] and (eof or len(buffers[channel]) > 32 * 1024 * 1024):
            consume(channel, buffers[channel])
            buffers[channel] = b""

    try:
        saved_environment = {
            key: "<redacted>" if any(part in key.upper() for part in
                                      ("API_KEY", "TOKEN", "PASSWORD", "SECRET", "CREDENTIAL")) else value
            for key, value in environment.items()
        }
        record("process", {"pid": process.pid, "command": command, "environment": saved_environment})
        for channel, stream in (("stdout", process.stdout), ("stderr", process.stderr)):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, channel)
        os.set_blocking(process.stdin.fileno(), False)
        if backend == "harness":
            send(prompt, reason="frozen episode prompt")
        else:
            process.stdin.close()
        while True:
            now = time.monotonic()
            if _leader_exited(process):
                exit_time = exit_time or now
                if not selector.get_map():
                    break
                if now - exit_time >= 2:
                    outcome["drain_incomplete"] = True
                    break
            if shutdown_deadline is None and now - started >= seconds:
                outcome.update(error="episode wall-clock budget exhausted", timed_out=True,
                               _deadline_shutdown_monotonic=now)
                begin_shutdown("frozen episode deadline", interrupt=True)
            if shutdown_deadline is not None and now >= shutdown_deadline:
                outcome["shutdown_incomplete"] = True
                break
            for key, _ in selector.select(timeout=0.05):
                channel = key.data
                if channel == "stdin":
                    try:
                        count = os.write(key.fileobj.fileno(), pending_input)
                        del pending_input[:count]
                    except BrokenPipeError:
                        pending_input.clear()
                        outcome["input_delivery_failed"] = True
                    except BlockingIOError:
                        continue
                    if not pending_input:
                        selector.unregister(key.fileobj)
                    continue
                try:
                    chunk = os.read(key.fileobj.fileno(), 65536)
                except BlockingIOError:
                    continue
                if not chunk:
                    selector.unregister(key.fileobj)
                    drain(channel, eof=True)
                else:
                    buffers[channel] += chunk
                    drain(channel)
    except (OSError, ValueError, KeyError, TypeError) as error:
        outcome.update(status="infrastructure_failure", error=str(error))
    finally:
        # Stop before reaping: WNOWAIT above reserves this PID throughout output
        # draining. One kill covers descendants without targeting a reused group.
        shutdown_deadline = shutdown_deadline or time.monotonic()
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        except PermissionError as error:
            if not _group_exited(process):
                outcome.update(status="infrastructure_failure",
                               error=f"cannot terminate owned process group {process.pid}: {error}")
        except OSError as error:
            outcome.update(status="infrastructure_failure", error=f"process-group termination failed: {error}")
        try:
            outcome["returncode"] = process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            outcome.update(status="infrastructure_failure", error="owned process did not reap after termination")
        drain_deadline = time.monotonic() + 0.5
        for channel, stream in (("stdout", process.stdout), ("stderr", process.stderr)):
            while time.monotonic() < drain_deadline:
                try:
                    chunk = os.read(stream.fileno(), 65536)
                except BlockingIOError:
                    break
                if not chunk:
                    break
                buffers[channel] += chunk
                drain(channel)
            drain(channel, eof=True)
            stream.close()
        if not process.stdin.closed:
            process.stdin.close()
        selector.close()
        outcome["metrics"]["elapsed_seconds"] = time.monotonic() - started
    try:
        _journal_metrics(outcome, last_task)
    except (ValueError, KeyError, TypeError) as error:
        outcome.update(status="infrastructure_failure", error=str(error))
    outcome["request_usage"] = token_usage.report()
    outcome["metrics"].update(resource_metrics(outcome["request_usage"]))
    if outcome["status"] not in {"preflight_failure", "infrastructure_failure"}:
        complete = bool(final and final["terminal"] == "success" and closed_seen) if backend == "harness" else completion_seen
        if (complete and outcome["returncode"] == 0 and not fatal_error
                and not any(outcome.get(key) for key in ("timed_out", "drain_incomplete", "shutdown_incomplete"))):
            outcome["status"] = "success"
        if final and final.get("reason"):
            outcome["reason"] = final["reason"]
        if not native_events:
            outcome["reason"] = "process produced no native protocol events"
        elif not complete and not outcome.get("reason"):
            outcome["reason"] = "native protocol did not confirm completion"
    # Guard totals include the drained tail; classification reports the same numbers.
    outcome.update(evidence_byte_limit=evidence_byte_limit, evidence_bytes=evidence_bytes,
                   reject_loop_limit=reject_loop_limit, reject_loop_max_streak=reject_loop_max_streak)
    cause, detail = classify(outcome, last_task, final, {
        "backend": backend, "completion_seen": completion_seen,
        "fatal_error": fatal_error, "closed_seen": closed_seen,
        "suppressed_gate_repeats": suppressed_gate_repeats, "last_suppressed_phase": last_suppressed_phase})
    outcome.update(terminal_cause=cause, terminal_detail=detail,
                   first_edit_reached=first_edit_monotonic is not None,
                   first_edit_seconds=None if first_edit_monotonic is None else first_edit_monotonic - started,
                   suppressed_gate_repeats=suppressed_gate_repeats, last_suppressed_phase=last_suppressed_phase)
    return outcome
