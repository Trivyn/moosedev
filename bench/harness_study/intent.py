"""Frozen design and offline measurements for the change-level intent pilot."""
from collections import Counter
import hashlib
import json

from .artifacts import canonical_json
from .scenario import MAINTENANCE

MODE = "local-intent-pilot"
POLICIES = ("current", "change-level")
MODELS = ("qwen/qwen3.8-27b", "gemma-4-e4b-it-mlx")
SCENARIOS = ("ruleset_cache", "retry_ledger", MAINTENANCE)
# Independent of hidden tests; reviewed associations select existing source only.
SEED_ASSOCIATIONS = {
    "ruleset_cache": [
        {"fact": "cache-identity", "file": "service.py", "name": "ScoringService.score"},
        {"fact": "cache-reuse", "file": "service.py", "name": "ScoringService.score"},
    ],
    "retry_ledger": [],
    MAINTENANCE: [
        {"fact": fact, "file": "labels.py", "name": name}
        for fact in ("label-normalization", "label-maintenance-contract")
        for name in ("render_name", "render_names")
    ],
}
RESOLUTION_TARGETS = {
    "ruleset_cache": [{"file": "service.py", "name": "ScoringService.score"}],
    "retry_ledger": [{"file": "service.py", "name": "Ledger.process"}],
    MAINTENANCE: [{"file": "labels.py", "name": name} for name in ("render_name", "render_names")],
}
OVERLAY = {"schema_version": 1, "python_manifest":
           '[project]\nname = "moosedev-intent-fixture"\nversion = "0.1.0"\nrequires-python = ">=3.10"\n',
           "seed_associations": SEED_ASSOCIATIONS, "resolution_targets": RESOLUTION_TARGETS,
           "ledger_initial_knowledge": "empty; separate disposable canary proves linked dossiers",
           "shared_entity_association": "MOOSEDEV_HARNESS_ENTITY_LINKS=1; optional reviewed associate action in both arms; only change-level requires approved intent before edit"}
PRIMARY = {"schema_version": 1, "scenario": MAINTENANCE,
           "definition": "within-budget completion AND behavioral/extraction checks AND new helper meaningfully linked to both seeds AND no redundant or unsupported accepted knowledge",
           "semantic_source": "independent evidence-bound review; unknown until adjudicated",
           "zero_new_records": "secondary only; justified new knowledge does not fail primary outcome",
           "approval_units": "each record/link disposition and plan approval attempt; cycles persist resume",
           "interpretation": "n=1 per cell; scope-only auto-accepting reviewer; diagnostic friction and resulting quality, not semantic protection"}


def design_identity():
    payload = {"mode": MODE, "models": list(MODELS), "policies": list(POLICIES),
               "scenarios": list(SCENARIOS), "overlay": OVERLAY, "primary": PRIMARY}
    return {"payload": payload, "sha256": hashlib.sha256(canonical_json(payload)).hexdigest()}


def gate_metrics(events):
    """Count durable gate event IDs once across repeated snapshots and resume."""
    unique = {}
    for event in events:
        if not isinstance(event, dict) or not isinstance(event.get("id"), str):
            raise ValueError("intent events require durable IDs")
        previous = unique.setdefault(event["id"], event)
        if previous != event:
            raise ValueError("conflicting intent event identity")
    kinds = Counter(event.get("kind", "unknown") for event in unique.values())
    cycles = {event["cycle"] for event in unique.values() if event.get("cycle")}
    return {"event_count": len(unique), "by_kind": dict(kinds),
            "plan_approval_attempts": kinds["plan_approval_attempt"],
            "record_dispositions": kinds["record_review"], "link_dispositions": kinds["link_review"],
            "accepted_revision_changes": kinds["knowledge_revision_changed"],
            "gate_decisions": sum(kinds[kind] for kind in ("plan_approval_attempt", "record_review", "link_review")),
            "approval_cycles": len(cycles), "events": list(unique.values())}


def activity_metrics(events):
    """Observable repetition, not a semantic judgment that repeated work was wasteful."""
    actions, commands = Counter(), Counter()
    reads = Counter()
    for event in events:
        message = event.get("message", "")
        if message.startswith("Model action: "):
            try:
                action = json.loads(message.removeprefix("Model action: "))
            except ValueError:
                continue
            kind = action.get("action", "unknown")
            actions[kind] += 1
            if kind == "command":
                commands[action.get("command", "")] += 1
        elif message.startswith("Read ") and ": " in message:
            file, source = message[5:].split(": ", 1)
            reads[(file, hashlib.sha256(source.encode()).hexdigest())] += 1
    return {"actions_by_kind": dict(actions), "source_reads": sum(reads.values()),
            "source_rereads_unchanged": sum(count - 1 for count in reads.values()),
            "command_requests": sum(commands.values()),
            "repeat_command_requests": sum(count - 1 for count in commands.values())}


def primary_outcome(episode, semantic=None):
    """A failed known constituent is false; absent semantic grading stays unknown."""
    semantic = semantic or {}
    constituents = {"within_budget_completion": episode.get("status") == "success",
                    "behavior_and_extraction": all(check.get("passed") is True for check in episode.get("checks", []))
                    if episode.get("checks") else None,
                    "helper_links_both_seeds": semantic.get("helper_links_both_seeds"),
                    "no_redundant_or_unsupported_accepted_knowledge": semantic.get("no_redundant_or_unsupported_accepted_knowledge")}
    values = list(constituents.values())
    primary = False if False in values else (True if all(value is True for value in values) else None)
    return {"primary": primary, "constituents": constituents,
            "zero_new_records_secondary": semantic["new_record_count"] == 0 if "new_record_count" in semantic else None,
            "new_record_count": semantic.get("new_record_count")}
