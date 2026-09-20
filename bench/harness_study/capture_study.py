"""Confirmatory capture study: does the harness make small models record what a generic harness does not.

AD 2399f114. Every campaign before this one measured RETRIEVAL. The harness
exists for the other half — small local models will not call the capture tooling
reliably, so the harness takes that decision away from them — and the arm that
would test it was deferred as Alternative 75fd7e89 because "local-model pull
reliability through MCP is a known confound". That confound is the hypothesis.

Three arms on the same model and the same seeded graph:

- B-mcp (`opencode_mcp`): OpenCode holding the MOOSEDev MCP tools. The model
  decides whether and what to capture.
- C-harness (`harness`): capture is structurally enforced, not chosen.
- A-notes (`opencode`): OpenCode with a notes file and no MOOSEDev, carried from
  AD f19ce43b so a task-capability cost stays visible.

B-mcp against C-harness is the comparison that answers the question. A-notes only
bounds what the enforcement costs.

The primary outcome is FUNCTIONAL — retention, a later episode's probe decided by
an earlier episode's captured reason — because a record that cannot be recalled
and used is worth nothing. Capture rate, validity and fidelity are mechanism
outcomes reported beside it and never in place of it.

Accumulation-track packages only: capture is meaningless without a later episode
to consume what was captured (Requirement d2a54c9c).

As with the floor study, the pre-registered protocol lives outside this package
and is hashed into the design identity, so revising it changes the identity
rather than silently rescoring finished runs (Lesson d650a3a1).
"""
import hashlib

from .artifacts import canonical_json
from .binaries import REPO
from . import evolution, intent, long_horizon, model_table

MODE = "local-harness-capture-study"
# The deferred third arm, added to the symbolic baseline's pair. OpenCode is the
# generic comparator at every tier (AD 3f450d51), so both non-harness arms are
# OpenCode on the same model and differ only in how memory is offered.
ARMS = (("harness", "harness", "symbolic"),
        ("opencode_mcp", "opencode_mcp", None),
        ("opencode", "without", None))
# Accumulation track only. `late_fees` and `ruleset_cache` seed their knowledge
# and measure inheritance; this study measures what the agent writes down itself.
SCENARIOS = ("entity_outbox", "supplier_quotes", "retry_ledger")
POLICIES = ("symbolic",)
RESPONSE_POLICY = "reasoning-off"
RESPONSE_POLICIES = (RESPONSE_POLICY, "provider-default")
EPISODE_LIMIT = None
EPISODE_POLICY = ("continue to the next episode after a hidden-check failure; stop only when an episode "
                  "fails to complete. A run passes when every attempted episode passes.")
EPISODE_SECONDS = 1200
CLIENT_CONTEXT_TOKENS = 32768
REPETITIONS_DEFAULT = 3
REPETITION_LIMIT = 10
# Pre-registered thresholds (docs/capture_study_protocol.md). A majority of
# episodes producing valid, recallable records is "adequate capture": reaching it
# unprompted is the first falsifier, because the model would have done without
# the harness what the harness exists to enforce.
ADEQUATE_CAPTURE = 0.5
# The second falsifier. C-harness winning on capture rate while not winning on
# retention means it manufactures records nobody can use, which is worse than the
# tie the floor study already reports.
RETENTION_MARGIN = 0.0
CONFIDENCE = 0.95
DOCUMENT = REPO / "docs/capture_study_protocol.md"


def tier_label(index):
    """Tier names are positional: the selection order is the capability order."""
    return f"T{index + 1}"


def _selection(tiers, scenarios):
    tiers, scenarios = list(tiers), list(scenarios)
    if not tiers or len(set(tiers)) != len(tiers):
        raise ValueError("capture study requires one or more distinct tier models in capability order")
    for model in tiers:
        model_table.row(model)
    if model_table.HELPER in tiers:
        raise ValueError("the daemon helper is never a tested tier")
    if not scenarios or len(set(scenarios)) != len(scenarios) or any(name not in SCENARIOS for name in scenarios):
        raise ValueError("capture study scenarios must be distinct reviewed accumulation packages")
    return tiers, scenarios


def capture_rule(rule=None):
    """The pre-registered decision rule, with both falsifiers stated up front."""
    rule = dict(rule or {})
    adequate = rule.pop("adequate_capture", ADEQUATE_CAPTURE)
    margin = rule.pop("retention_margin", RETENTION_MARGIN)
    if rule:
        raise ValueError(f"unknown capture rule keys: {', '.join(sorted(rule))}")
    for name, value in (("adequate_capture", adequate), ("retention_margin", margin)):
        if type(value) is not float or not 0.0 <= value <= 1.0:
            raise ValueError(f"capture rule {name} must be a float in 0.0..1.0")
    return {
        "primary": ("retention: the share of retention probes passed, pooled per arm. Mechanism "
                    "outcomes never substitute for it."),
        "expectation": ("C-harness exceeds B-mcp on capture rate trivially, since one enforces what the "
                        "other leaves to the model; that number alone proves nothing. The claim under "
                        "test is that C also wins on retention and on validity."),
        "premise_weakened_if": ("B-mcp reaches adequate capture unprompted: valid-capture rate at or "
                                "above adequate_capture"),
        "records_unusable_if": ("C-harness exceeds B-mcp on capture rate while its retention is below "
                                "B-mcp's by more than retention_margin"),
        "adequate_capture": adequate, "retention_margin": margin,
        "confidence": CONFIDENCE, "interval": "Clopper-Pearson exact",
    }


def tier_plan(tiers, repetitions=None, response_policy=RESPONSE_POLICY, response_policies=None):
    """Ordered tier rows with the repetitions and response policy each was registered with."""
    repetitions, response_policies = dict(repetitions or {}), dict(response_policies or {})
    labels = [tier_label(index) for index in range(len(tiers))]
    for mapping, what in ((repetitions, "repetition"), (response_policies, "response policy")):
        unknown = sorted(set(mapping) - set(labels))
        if unknown:
            raise ValueError(f"{what} override names no tier in this study: {', '.join(unknown)}")
    plan = []
    for label, model in zip(labels, tiers):
        count = repetitions.get(label, REPETITIONS_DEFAULT)
        if type(count) is not int or not 1 <= count <= REPETITION_LIMIT:
            raise ValueError(f"tier repetitions must be an integer in 1..{REPETITION_LIMIT}")
        policy = response_policies.get(label, response_policy)
        if policy not in RESPONSE_POLICIES:
            raise ValueError(f"capture study harness_response_policy must be one of {RESPONSE_POLICIES!r}")
        plan.append({"tier": label, "model": model_table.row(model),
                     "repetitions": count, "harness_response_policy": policy})
    return plan


def design_identity(tiers, scenarios, response_policy=RESPONSE_POLICY, repetitions=None,
                    response_policies=None, rule=None):
    tiers, scenarios = _selection(tiers, scenarios)
    if response_policy not in RESPONSE_POLICIES:
        raise ValueError(f"capture study harness_response_policy must be one of {RESPONSE_POLICIES!r}")
    payload = {
        "schema_version": 1,
        "mode": MODE,
        "interpretation": ("confirmatory; every episode of each selected package; runs are scored and pooled "
                           "with the runs of this design identity alone"),
        "requirement": "f4b3e3d6",
        "decision": "2399f114",
        "comparator": ("opencode with the MOOSEDev MCP against the harness on the same model, with an "
                       "opencode notes arm bounding the enforcement cost (AD 3f450d51, AD f19ce43b)"),
        "tiers": tier_plan(tiers, repetitions, response_policy, response_policies),
        "tier_order": "ascending capability",
        "helper_model": model_table.row(model_table.HELPER),
        "arms": [{"backend": backend, "condition": condition, "intent_policy": policy}
                 for backend, condition, policy in ARMS],
        "scenarios": scenarios,
        "track": "accumulation",
        "scenario_tables": {name: {"resolution_targets": long_horizon.RESOLUTION_TARGETS[name],
                                   "seed_associations": long_horizon.SEED_ASSOCIATIONS[name]}
                            for name in scenarios if name not in intent.SCENARIOS},
        "schedule_order": "tier, then repetition, then scenario, then the arms in their frozen order",
        "episode_limit": EPISODE_LIMIT,
        "episode_policy": EPISODE_POLICY,
        "harness_response_policy": response_policy,
        "native_reasoning": "no reasoning option sent",
        "client_context_tokens": CLIENT_CONTEXT_TOKENS,
        "episode_seconds": EPISODE_SECONDS,
        "local_temperature": 0.0,
        "evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
        "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT,
        "capture_rule": capture_rule(rule),
        "inherits": {"mode": evolution.SYMBOLIC_BASELINE_MODE,
                     "design_sha256": evolution.design_identity(evolution.SYMBOLIC_BASELINE_MODE)["sha256"]},
        "intent_design_sha256": intent.design_identity()["sha256"],
        "protocol_sha256": hashlib.sha256(DOCUMENT.read_bytes()).hexdigest(),
    }
    return {"payload": payload, "sha256": hashlib.sha256(canonical_json(payload)).hexdigest()}


def verify_config(config):
    if config.get("evaluation_mode") != MODE:
        raise ValueError("not a capture-study configuration")
    tiers, scenarios = _selection(config.get("tier_models") or [], config.get("scenario_ids") or [])
    if config.get("coding_models") != tiers:
        raise ValueError("capture study coding models must equal its ordered tier models")
    if config.get("helper_model") != model_table.HELPER:
        raise ValueError("capture study keeps the frozen daemon helper model")
    model_table.verify_entries(config, [*tiers, model_table.HELPER])
    frozen = {"intent_policies": list(POLICIES), "episode_limit": EPISODE_LIMIT,
              "context_tokens": CLIENT_CONTEXT_TOKENS, "episode_seconds": EPISODE_SECONDS,
              "evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
              "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT}
    for key, value in frozen.items():
        if config.get(key) != value:
            raise ValueError(f"capture study requires {key} = {value!r}")
    if (config.get("generation_policy") or {}).get("local_temperature") != 0.0:
        raise ValueError("capture study requires local temperature 0.0")
    design = design_identity(tiers, scenarios, config.get("harness_response_policy", RESPONSE_POLICY),
                             config.get("tier_repetitions"), config.get("tier_response_policies"),
                             config.get("capture_rule"))
    if config.get("capture_study_design") != design:
        raise ValueError("capture-study design identity changed")
    return design


def schedule(config):
    """Tiers stay contiguous so one model is loaded at a time; within a tier the three
    arms alternate inside each repetition, so a warm cache never favours one arm."""
    design = verify_config(config)
    cells = [{"model": tier["model"]["id"], "backend": backend, "condition": condition,
              "scenario_id": scenario, "intent_policy": policy, "tier": tier["tier"],
              "repetition": repetition, "harness_response_policy": tier["harness_response_policy"]}
             for tier in design["payload"]["tiers"]
             for repetition in range(1, tier["repetitions"] + 1)
             for scenario in config["scenario_ids"]
             for backend, condition, policy in ARMS]
    return [dict(cell, schedule_index=index) for index, cell in enumerate(cells)]
