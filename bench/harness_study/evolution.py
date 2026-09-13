"""Frozen protocol identities and measurements for the approved harness evolution."""
from collections import Counter
import hashlib

from pathlib import Path

from .artifacts import canonical_json
from .cause import (CONTROLLER_CLASS, HARNESS_TERMINAL_CAUSES, RECOVERY_CONTROLLER_CLASS, SYMBOLIC_HALT_CAUSES,
                    SYMBOLIC_TERMINAL_CAUSES, TERMINAL_CAUSES, UNATTENDED_HALT_CLASS)
from . import intent

STAGE1_MODE = "local-harness-evolution-stage1"
STAGE2_MODE = "local-harness-evolution-stage2"
STAGE2_RECOVERY_MODE = "local-harness-evolution-stage2-recovery"
STAGE2_BASELINE_MODE = "local-harness-evolution-stage2-baseline"
# S8 made the symbolic policy the only harness; this mode compares it with the
# native agent under a new identity (AD 9dcaddeb, consequence b5a313eb).
SYMBOLIC_BASELINE_MODE = "local-harness-symbolic-baseline"
MODES = (STAGE1_MODE, STAGE2_MODE, STAGE2_RECOVERY_MODE, STAGE2_BASELINE_MODE, SYMBOLIC_BASELINE_MODE)
POLICIES = {STAGE1_MODE: ("current",), STAGE2_MODE: ("current", "change-level-v2"),
            STAGE2_RECOVERY_MODE: ("current", "change-level-v2"),
            STAGE2_BASELINE_MODE: ("current", "change-level-v2"),
            SYMBOLIC_BASELINE_MODE: ("symbolic",)}
# Multi-arm cells as (backend, condition, intent_policy); the native arm has no policy.
ARMS = {STAGE2_BASELINE_MODE: (("harness", "harness", "current"), ("harness", "harness", "change-level-v2"),
                               ("opencode", "without", None)),
        SYMBOLIC_BASELINE_MODE: (("harness", "harness", "symbolic"), ("opencode", "without", None))}
# Stage 2 builds whose controller defects motivated the recovery stage; never rerun.
# The recovery identity (763ccc52...) froze this pair; the recovery build joins the
# sealed set for the baseline without rewriting that identity.
SEALED_STAGE2 = ("2f108e95e5bc6588bcbe3052e55ad4788385661d87528ce539bf8df7364f49c2",
                 "364140e4a4f0d64573b3f7ca2c86d85486e3edc2c4aa02a4a89cd09995b01a73")
SEALED_PREDECESSORS = SEALED_STAGE2 + ("86dcd22618223c7eb686388ecea7607bd04dea4669115089fd671f823b109e69",
                                       "5cdf8bd6c2f85532d3d675e309824f3f5d9730ca7031016edb77ca39e52ea75a")
# Driver guards frozen for the baseline: a runner/reviewer loop once wrote a
# 43.5 GB event log that the whole-file seal could not read.
BASELINE_EVIDENCE_BYTE_LIMIT = 8 * 1024**3
BASELINE_REJECT_LOOP_LIMIT = 5
SHARED_FIXES = ("association prompt record-choice dedup",
                "review-blocking predicate limited to governing, reconciliation and missing-purpose work",
                "rejected reuse refetches candidates at current revision",
                "persisted reconciliation disposition replayed after restart",
                "driver computes journal metrics once from the final snapshot")
REQUIREMENT = "https://moosedev.dev/kg/Requirement/4ff3ef62-c7a9-4494-89d3-a91ea110c52a"
DECISION = "https://moosedev.dev/kg/ArchitecturalDecision/395c4e78-0413-4cb6-90a8-901f44d062f2"
# The symbolic baseline: its own sealed set (the three-arm baseline build joins
# without rewriting SEALED_PREDECESSORS, which the baseline identity hashes),
# governing records, runner bounds and frozen reconciliation thresholds.
SEALED_BASELINE = ("abe95d92da7994ec9a801073899e480d58a492f5987e1957fff67fa45a6e1fb6",)
# Attempt v1 of the symbolic baseline, stopped as a pilot (plan checks written as prose).
SEALED_SYMBOLIC_V1 = ("9aa4f75c5265db4c503ab319ffa7909393d593407f115865858b6c1b3f5fdb66",)
# Attempt v2, stopped after its Gemma block (a captured note was re-captured after governing review).
SEALED_SYMBOLIC_V2 = ("a0fcebdecde28c1f308a600654de15e035c968b1438035752923274b0cad9aed",)
SEALED_SYMBOLIC_PREDECESSORS = SEALED_PREDECESSORS + SEALED_BASELINE + SEALED_SYMBOLIC_V1 + SEALED_SYMBOLIC_V2
SYMBOLIC_REQUIREMENT = "https://moosedev.dev/kg/Requirement/9ae68a19-08c9-4317-aa75-029e08e2c5b6"
SYMBOLIC_DECISION = "https://moosedev.dev/kg/ArchitecturalDecision/9dcaddeb-4027-4178-90ab-4aee30199d45"
SYMBOLIC_BOUNDS = {"scope_escapes": 3, "retypes": 3, "noop_continuations": 1}
RECONCILIATION_THRESHOLDS = {"MOOSEDEV_RECONCILE_RESTATES": 0.80, "MOOSEDEV_RECONCILE_REFINES": 0.55,
                             "MOOSEDEV_RECONCILE_REFINES_CONTAINMENT": 0.60,
                             "MOOSEDEV_RECONCILE_TIEBREAK_BAND": 0.08}
S8_RUNNER_CHANGES = ("symbolic-only harness; task journal schema 2",
                     "plan-scope escape replans autonomously, three per task, then parks",
                     "first no-op edit runs the required checks instead of spending repair budget",
                     "abandoned link review resets the derived association for re-derivation",
                     "daemon-rejected or colliding typed capture retypes under fresh ids, three per note, then parks",
                     "typing invalidated on source or knowledge change without a model call",
                     "plan checks must be runnable commands; a required check the shell cannot start is reported as invalid",
                     "a captured final note is never invalidated or submitted again; a colliding title keeps its qualifier within the cap")


def postedit_association_contract(mode):
    """Enable the mandatory post-edit contract only for the Stage 2 identities."""
    return mode in (STAGE2_MODE, STAGE2_RECOVERY_MODE, STAGE2_BASELINE_MODE)


def design_identity(stage):
    if stage not in MODES:
        raise ValueError("unknown harness evolution stage")
    payload = {
        "schema_version": 1,
        "stage": stage,
        "requirement": REQUIREMENT,
        "decision": DECISION,
        "models": list(intent.MODELS),
        "scenarios": list(intent.SCENARIOS),
        "policies": list(POLICIES[stage]),
        "capture_contract": "candidate reconciliation with immutable reviewed reuse dispositions",
        "postedit_contract": ("legacy optional model-authored associate action" if stage == STAGE1_MODE else
                              "deterministic entity discovery and reviewed associations shared by both arms"),
        "treatment_difference": None if stage == STAGE1_MODE else
            "only change-level-v2 requires approved pre-edit semantic purpose, system-derived scope, and resolution of resulting intent obligations",
        "historical_baseline": intent.design_identity(),
        "interpretation": "new exploratory identities; never pooled with the frozen intent pilot",
    }
    if stage == STAGE2_RECOVERY_MODE:
        payload.update({
            "primary_outcome": {
                "definition": "treatment run: first_edit_reached AND terminal_cause not in controller class",
                "controller_class": sorted(RECOVERY_CONTROLLER_CLASS),
                "control_arm": "outcome degenerate; baselines outcomes 6-9 and shared fixes"},
            "terminal_cause_classes": sorted(HARNESS_TERMINAL_CAUSES),
            "episode_limit": 1,
            "shared_fixes": list(SHARED_FIXES),
            "sealed_predecessors": list(SEALED_STAGE2),
            "preregistration_sha256": hashlib.sha256(
                Path(__file__).with_name("RECOVERY.md").read_bytes()).hexdigest(),
        })
    if stage == STAGE2_BASELINE_MODE:
        payload.update({
            "arms": [dict(zip(("backend", "condition", "intent_policy"), arm)) for arm in ARMS[stage]],
            "primary_outcome": {
                "definition": "per model and package: completion AND hidden-check pass; "
                              "harness-current vs opencode-without, change-level-v2 reported alongside",
                "baseline_knowledge_outcomes": "not applicable",
                "prompt_rule": "task text and clarifications identical across arms; condition guidance frozen from the pilot"},
            "terminal_cause_classes": sorted(TERMINAL_CAUSES),
            "controller_class": sorted(CONTROLLER_CLASS),
            "native_terminal_causes": ["success", "native_no_completion", "deadline_native", "infrastructure", "unknown"],
            "evidence_byte_limit": BASELINE_EVIDENCE_BYTE_LIMIT,
            "reject_loop_limit": BASELINE_REJECT_LOOP_LIMIT,
            "episode_limit": 1,
            "shared_fixes": list(SHARED_FIXES),
            "sealed_predecessors": list(SEALED_PREDECESSORS),
            "preregistration_sha256": hashlib.sha256(
                Path(__file__).with_name("BASELINE.md").read_bytes()).hexdigest(),
        })
    if stage == SYMBOLIC_BASELINE_MODE:
        payload.update({
            "requirement": SYMBOLIC_REQUIREMENT,
            "decision": SYMBOLIC_DECISION,
            "capture_contract": "one prose capture note typed by the daemon; symbolic same-kind "
                                "reconciliation with durable receipts (restates, refines, distinct)",
            "postedit_contract": "deterministic kind-filtered associations derived after finish; no post-edit probe",
            "treatment_difference": None,
            "arms": [dict(zip(("backend", "condition", "intent_policy"), arm)) for arm in ARMS[stage]],
            "primary_outcome": {
                "definition": "per model and package: within-budget completion AND hidden-check pass; "
                              "harness-symbolic vs opencode-without, six pairs",
                "halt_count": {
                    "definition": "number of symbolic runs whose terminal cause is in the unattended halt class",
                    "classes": sorted(UNATTENDED_HALT_CLASS)},
                "baseline_knowledge_outcomes": "not applicable",
                "prompt_rule": "task text and clarifications identical across arms; condition guidance frozen from the pilot"},
            "terminal_cause_classes": sorted(SYMBOLIC_TERMINAL_CAUSES),
            "controller_class": sorted(CONTROLLER_CLASS),
            "native_terminal_causes": ["success", "native_no_completion", "deadline_native", "infrastructure", "unknown"],
            "reviewer_terminals": sorted(SYMBOLIC_HALT_CAUSES | {"model_repair_exhausted"}),
            "symbolic_bounds": dict(SYMBOLIC_BOUNDS),
            "reconciliation_thresholds": dict(RECONCILIATION_THRESHOLDS),
            "daemon_llm_sensor": "helper model from configuration; MOOSEDEV_LLM_ASSIST_LEVEL unset -> Sensor",
            "evidence_byte_limit": BASELINE_EVIDENCE_BYTE_LIMIT,
            "reject_loop_limit": BASELINE_REJECT_LOOP_LIMIT,
            "episode_limit": 1,
            "runner_changes": list(S8_RUNNER_CHANGES),
            "sealed_predecessors": list(SEALED_SYMBOLIC_PREDECESSORS),
            "preregistration_sha256": hashlib.sha256(
                Path(__file__).with_name("SYMBOLIC.md").read_bytes()).hexdigest(),
        })
    return {"payload": payload, "sha256": hashlib.sha256(canonical_json(payload)).hexdigest()}


def review_metrics(events):
    """Count durable dispositions, interactions, and gates without conflating them."""
    unique = {}
    for event in events:
        if not isinstance(event, dict) or not isinstance(event.get("id"), str) or not event["id"]:
            raise ValueError("evolution review events require durable IDs")
        previous = unique.setdefault(event["id"], event)
        if previous != event:
            raise ValueError("conflicting evolution event identity")
    kinds = Counter(event.get("kind", "unknown") for event in unique.values())
    dispositions = {name: kinds[name] for name in ("record_review", "link_review", "reuse_review")}
    candidate_events = {name: count for name, count in kinds.items()
                        if name in {"reuse_candidate", "association_candidate"}}
    interactions = {event.get("interaction") for event in unique.values()
                    if isinstance(event.get("interaction"), str) and event["interaction"]}
    cycles = {event.get("cycle") for event in unique.values()
              if isinstance(event.get("cycle"), str) and event["cycle"]}
    return {
        "event_count": len(unique), "by_kind": dict(kinds),
        "candidate_events": candidate_events,
        "record_dispositions": dispositions["record_review"],
        "attached_link_dispositions": dispositions["link_review"],
        "reuse_dispositions": dispositions["reuse_review"],
        "individual_dispositions": sum(dispositions.values()),
        "plan_approval_attempts": kinds["plan_approval_attempt"],
        "gate_decisions": sum(dispositions.values()) + kinds["plan_approval_attempt"],
        "review_interactions": (len(interactions) if interactions else
                                kinds["review_interaction"] if kinds["review_interaction"] else None),
        "approval_cycles": len(cycles),
        "events": list(unique.values()),
    }


def outcome_constituents(episode, semantic=None):
    """Retain known execution facts separately from later semantic assessment."""
    semantic = semantic or {}
    checks = episode.get("checks", [])
    return {
        "within_budget_completion": episode.get("status") == "success",
        "source_correctness": all(check.get("passed") is True for check in checks) if checks else None,
        "task_requirements_complete": semantic.get("task_requirements_complete"),
        "tests_complete": semantic.get("tests_complete"),
        "capture_reconciliation_correct": semantic.get("capture_reconciliation_correct"),
        "associations_relevant": semantic.get("associations_relevant"),
    }
