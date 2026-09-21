"""Frozen, scope-only simulated reviewer; never imports gold or reference code."""
from fnmatch import fnmatchcase
from pathlib import PurePosixPath

from .cause import last_symbolic_halt

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
        return {"terminal": "success", "cause": "success"}
    if phase == "Cancelled":
        return {"terminal": "agent_failure", "reason": "task cancelled", "cause": "cancelled"}
    recovery = task.get("recovery") or {}
    if recovery.get("status") in {"generating", "retrying"}:
        return None
    if recovery.get("status") == "awaiting_guidance":
        return {"terminal": "agent_failure", "reason": "native harness exhausted its repair budget: "
                + recovery.get("diagnostic", "human guidance required"), "cause": "model_repair_exhausted"}
    if phase == "AwaitingInput":
        # The symbolic harness parked itself after its autonomous bound. A
        # human would decide here; the reviewer records the halt instead of
        # supplying that decision with the frozen clarification.
        halt = last_symbolic_halt(task)
        if halt is not None:
            return {"terminal": "agent_failure", "cause": halt["kind"],
                    "reason": "native harness parked for human guidance: " + str(task.get("last_response", ""))}
    if task.get("last_error"):
        return {"terminal": "agent_failure", "reason": task["last_error"], "cause": "runner_error"}
    if phase == "AwaitingPermission":
        return {"input": "/deny",
                "reason": "frozen study denies unexpected permission requests",
                "kind": "permission_denial"}
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
        return {"terminal": "agent_failure", "reason": "reviewed scope is outside frozen episode allowance",
                "cause": "reviewer_scope_rejection"}
    if phase == "AwaitingReview":
        reviews = task.get("reviews", [])
        if reviews and reviews[0].get("capture_resolution"):
            outer = reviews[0]
            reuse = outer["capture_resolution"]
            proposal = reuse.get("original_proposal") or {}
            evidence_page = reuse.get("evidence_page")
            candidate_pages = reuse.get("candidate_pages")
            operation_id = reuse.get("operation_id")
            candidate_iri = reuse.get("candidate_iri")
            candidates = [candidate for page in candidate_pages or []
                          for candidate in page.get("candidates", [])
                          if isinstance(page, dict) and isinstance(page.get("candidates"), list)
                          and isinstance(candidate, dict) and candidate.get("iri") == candidate_iri]
            selected = candidates[0] if len(candidates) == 1 else {}
            revisions = {page.get("revision") for page in candidate_pages or []
                         if isinstance(page, dict)}
            files = proposal.get("files")
            valid_proposal = (proposal.get("kind") in KINDS and bool(proposal.get("title"))
                              and bool(proposal.get("description"))
                              and isinstance(proposal.get("evidence"), list)
                              and bool(proposal["evidence"])
                              and isinstance(files, list)
                              and all(in_scope(path, episode["allowed_paths"]) for path in files))
            valid_candidate = (len(candidates) == 1 and len(revisions) == 1
                               and all(revisions) and selected.get("title") == reuse.get("candidate_title")
                               and bool(selected.get("assertion_digest"))
                               and (selected.get("status") == "accepted"
                                    or (selected.get("status") == "proposed"
                                        and selected.get("owned_by_requester") is True)))
            valid = (bool(operation_id) and outer.get("request", {}).get("operation_id") == operation_id
                     and bool(candidate_iri) and bool(reuse.get("candidate_title"))
                     and bool(reuse.get("original_claim"))
                     and bool(reuse.get("existing_claim")) and bool(reuse.get("rationale"))
                     and reuse.get("recommendation_source") == "model"
                     and reuse.get("reuse_unchanged") is True
                     and isinstance(evidence_page, (dict, list))
                     and isinstance(candidate_pages, list) and bool(candidate_pages)
                     and valid_proposal and valid_candidate)
            return {"input": ("/accept" if valid else "/reject") + " " + str(operation_id or ""),
                    "reason": "simulated structural reuse review; semantic equivalence unassessed"}
        if reviews and reviews[0].get("intent_links"):
            links = reviews[0]["intent_links"]
            bindings = links.get("bindings", [])
            valid = bool(bindings) and all(binding.get("record_iri")
                and (binding.get("symbol") or binding.get("planned_name"))
                and in_scope(binding.get("file", ""), episode["allowed_paths"]) for binding in bindings)
            return {"input": ("/accept" if valid else "/reject") + " " + links["operation_id"],
                    "reason": "simulated structural link review; semantic relevance unassessed"}
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
