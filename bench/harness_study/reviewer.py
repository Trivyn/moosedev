"""Frozen, scope-only simulated reviewer; never imports gold or reference code."""
from fnmatch import fnmatchcase
from pathlib import PurePosixPath

KINDS = {"ArchitecturalDecision", "Constraint", "Requirement", "Lesson", "Pattern", "AntiPattern"}


def in_scope(path, patterns):
    parsed = PurePosixPath(path)
    if not path or parsed.is_absolute() or ".." in parsed.parts or "\\" in path:
        return False
    return any(fnmatchcase(path, pattern) or
               (pattern.startswith("**/") and fnmatchcase(path, pattern[3:]))
               for pattern in patterns)


def review_input(state, episode):
    """Return an ordinary controller input and reason, or a terminal observation.

    Structurally valid proposals can still be false. Acceptance is a simulation
    event, never semantic credit. Unknown questions receive one fixed response.
    """
    if state.get("busy"):
        return None
    task = state.get("task")
    if not task:
        return None
    phase = task["phase"]
    if phase == "Complete":
        return {"terminal": "success"}
    if phase == "Cancelled":
        return {"terminal": "agent_failure", "reason": "task cancelled"}
    recovery = task.get("recovery") or {}
    if recovery.get("status") in {"generating", "retrying"}:
        return None
    if recovery.get("status") == "awaiting_guidance":
        return {"terminal": "agent_failure", "reason": "native harness exhausted its repair budget: "
                + recovery.get("diagnostic", "human guidance required")}
    if task.get("last_error"):
        return {"terminal": "agent_failure", "reason": task["last_error"]}
    if phase in ("AwaitingPlan", "AwaitingPolicy"):
        if phase == "AwaitingPlan":
            plan = task.get("plan") or {}
            paths = plan.get("files", [])
            valid = bool(plan.get("summary")) and bool(plan.get("checks"))
        else:
            edit = task.get("pending_edit") or {}
            paths = [edit.get("file", "")]
            valid = bool(edit)
        if valid and all(in_scope(path, episode["allowed_paths"]) for path in paths):
            return {"input": "/approve", "reason": "simulated in-scope approval"}
        return {"terminal": "agent_failure", "reason": "reviewed scope is outside frozen episode allowance"}
    if phase == "AwaitingReview":
        reviews = task.get("reviews", [])
        request = reviews[0]["request"] if reviews else task.get("capture_request")
        if request:
            proposals = request.get("proposals", [])
            valid = bool(proposals) and all(
                p.get("kind") in KINDS and p.get("title") and p.get("description")
                and p.get("evidence") and all(in_scope(f, episode["allowed_paths"])
                                              for f in p.get("files", []))
                for p in proposals)
            suffix = " " + request["operation_id"] if reviews else ""
            return {"input": ("/accept" if valid else "/reject") + suffix,
                    "reason": "simulated structural proposal review; semantic truth unassessed"}
        return {"input": "/no-knowledge", "reason": "simulated no-change confirmation; completeness unassessed"}
    if phase == "AwaitingInput" or task.get("turn_finished"):
        return {"input": "All available requirements and clarifications were supplied. Use your best judgment within that scope and finish the task.",
                "reason": "frozen clarification response"}
    return None
