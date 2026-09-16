"""Confirmatory floor study: ordered model tiers against the generic harness, scored and pooled.

Unlike the exploratory field check (FIELD_CHECK.md), this mode is confirmatory.
Its runs are scored, and pooled with the runs of this design identity alone. The
pre-registered protocol in `docs/floor_study_protocol.md` is hashed into the
identity, so revising the protocol after sealing changes the identity rather
than silently rescoring finished runs.

Tiers are an ordered selection from the model table, smallest capability first.
The floor rule reads that order and reports a tier, never a parameter count.
Each tier carries its own repetition count and harness response policy, because
a hybrid-reasoning tier is matched across arms at its own setting while the
thinking-off tiers stay pinned to `reasoning-off`.

The protocol document deliberately lives outside this package: every file under
`bench/harness_study` is fingerprinted into each preflight, so a document kept
here would invalidate taken preflights whenever it was edited (Lesson d650a3a1).
"""
import hashlib

from .artifacts import canonical_json
from .binaries import REPO
from . import evolution, intent, long_horizon, model_table

MODE = "local-harness-floor-study"
# The symbolic baseline's arms: the MOOSEDev harness against OpenCode on the same
# model. AD 3f450d51 makes OpenCode the generic comparator at every tier.
ARMS = evolution.ARMS[evolution.SYMBOLIC_BASELINE_MODE]
SCENARIOS = (*intent.SCENARIOS, *long_horizon.SCENARIOS, *long_horizon.EXPLORATORY)
POLICIES = ("symbolic",)
RESPONSE_POLICY = "reasoning-off"
RESPONSE_POLICIES = (RESPONSE_POLICY, "provider-default")
# Every frozen episode is attempted: the floor study measures the long horizon,
# so it pins no episode limit.
EPISODE_LIMIT = None
EPISODE_POLICY = ("continue to the next episode after a hidden-check failure; stop only when an episode "
                  "fails to complete. A run passes when every attempted episode passes.")
EPISODE_SECONDS = 1200
CLIENT_CONTEXT_TOKENS = 32768
REPETITIONS_DEFAULT = 3
REPETITION_LIMIT = 10
# Pre-registered floor rule (docs/floor_study_protocol.md, H5). Overridable only
# before sealing: the chosen values are hashed into the design identity.
FLOOR_PASS_RATE = 0.8
FLOOR_NATIVE_MARGIN = 0.1
CONFIDENCE = 0.95
DOCUMENT = REPO / "docs/floor_study_protocol.md"


def tier_label(index):
    """Tier names are positional: the selection order is the capability order."""
    return f"T{index + 1}"


def _selection(tiers, scenarios):
    tiers, scenarios = list(tiers), list(scenarios)
    if not tiers or len(set(tiers)) != len(tiers):
        raise ValueError("floor study requires one or more distinct tier models in capability order")
    for model in tiers:
        model_table.row(model)
    if model_table.HELPER in tiers:
        raise ValueError("the daemon helper is never a tested tier")
    if not scenarios or len(set(scenarios)) != len(scenarios) or any(name not in SCENARIOS for name in scenarios):
        raise ValueError("floor study scenarios must be distinct reviewed packages")
    return tiers, scenarios


def floor_rule(rule=None):
    """The pre-registered H5 decision rule, with its thresholds and interval method."""
    rule = dict(rule or {})
    threshold = rule.pop("pass_rate_threshold", FLOOR_PASS_RATE)
    margin = rule.pop("native_margin", FLOOR_NATIVE_MARGIN)
    if rule:
        raise ValueError(f"unknown floor rule keys: {', '.join(sorted(rule))}")
    for name, value in (("pass_rate_threshold", threshold), ("native_margin", margin)):
        if type(value) is not float or not 0.0 <= value <= 1.0:
            raise ValueError(f"floor rule {name} must be a float in 0.0..1.0")
    return {"definition": ("the smallest tier whose pooled harness run-pass rate is at least "
                           "pass_rate_threshold and is not below its own native rate by more than "
                           "native_margin"),
            "pass_rate_threshold": threshold, "native_margin": margin,
            "confidence": CONFIDENCE, "interval": "Clopper-Pearson exact"}


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
            raise ValueError(f"floor study harness_response_policy must be one of {RESPONSE_POLICIES!r}")
        plan.append({"tier": label, "model": model_table.row(model),
                     "repetitions": count, "harness_response_policy": policy})
    return plan


def design_identity(tiers, scenarios, response_policy=RESPONSE_POLICY, repetitions=None,
                    response_policies=None, rule=None):
    tiers, scenarios = _selection(tiers, scenarios)
    if response_policy not in RESPONSE_POLICIES:
        raise ValueError(f"floor study harness_response_policy must be one of {RESPONSE_POLICIES!r}")
    payload = {
        "schema_version": 1,
        "mode": MODE,
        "interpretation": ("confirmatory; every episode of each selected package; runs are scored and pooled "
                           "with the runs of this design identity alone"),
        "requirement": "f4b3e3d6",
        "comparator": "opencode on the same model at every tier (AD 3f450d51)",
        "tiers": tier_plan(tiers, repetitions, response_policy, response_policies),
        "tier_order": "ascending capability; the floor rule reports the smallest qualifying tier",
        "helper_model": model_table.row(model_table.HELPER),
        "arms": [{"backend": backend, "condition": condition, "intent_policy": policy}
                 for backend, condition, policy in ARMS],
        "scenarios": scenarios,
        "scenario_tables": {name: {"resolution_targets": long_horizon.RESOLUTION_TARGETS[name],
                                   "seed_associations": long_horizon.SEED_ASSOCIATIONS[name]}
                            for name in scenarios if name not in intent.SCENARIOS},
        "schedule_order": "tier, then repetition, then scenario, then harness before native",
        "episode_limit": EPISODE_LIMIT,
        "episode_policy": EPISODE_POLICY,
        "harness_response_policy": response_policy,
        "native_reasoning": "no reasoning option sent",
        "client_context_tokens": CLIENT_CONTEXT_TOKENS,
        "episode_seconds": EPISODE_SECONDS,
        "local_temperature": 0.0,
        "evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
        "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT,
        "floor_rule": floor_rule(rule),
        "inherits": {"mode": evolution.SYMBOLIC_BASELINE_MODE,
                     "design_sha256": evolution.design_identity(evolution.SYMBOLIC_BASELINE_MODE)["sha256"]},
        "intent_design_sha256": intent.design_identity()["sha256"],
        "protocol_sha256": hashlib.sha256(DOCUMENT.read_bytes()).hexdigest(),
    }
    return {"payload": payload, "sha256": hashlib.sha256(canonical_json(payload)).hexdigest()}


def verify_config(config):
    if config.get("evaluation_mode") != MODE:
        raise ValueError("not a floor-study configuration")
    tiers, scenarios = _selection(config.get("tier_models") or [], config.get("scenario_ids") or [])
    if config.get("coding_models") != tiers:
        raise ValueError("floor study coding models must equal its ordered tier models")
    if config.get("helper_model") != model_table.HELPER:
        raise ValueError("floor study keeps the frozen daemon helper model")
    model_table.verify_entries(config, [*tiers, model_table.HELPER])
    frozen = {"intent_policies": list(POLICIES), "episode_limit": EPISODE_LIMIT,
              "context_tokens": CLIENT_CONTEXT_TOKENS, "episode_seconds": EPISODE_SECONDS,
              "evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
              "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT}
    for key, value in frozen.items():
        if config.get(key) != value:
            raise ValueError(f"floor study requires {key} = {value!r}")
    if (config.get("generation_policy") or {}).get("local_temperature") != 0.0:
        raise ValueError("floor study requires local temperature 0.0")
    design = design_identity(tiers, scenarios, config.get("harness_response_policy", RESPONSE_POLICY),
                             config.get("tier_repetitions"), config.get("tier_response_policies"),
                             config.get("floor_rule"))
    if config.get("floor_study_design") != design:
        raise ValueError("floor-study design identity changed")
    return design


def schedule(config):
    """Tiers stay contiguous so one model is loaded at a time; within a tier the arms
    alternate inside each repetition, so a warm cache never favours one arm."""
    design = verify_config(config)
    cells = [{"model": tier["model"]["id"], "backend": backend, "condition": condition,
              "scenario_id": scenario, "intent_policy": policy, "tier": tier["tier"],
              "repetition": repetition, "harness_response_policy": tier["harness_response_policy"]}
             for tier in design["payload"]["tiers"]
             for repetition in range(1, tier["repetitions"] + 1)
             for scenario in config["scenario_ids"]
             for backend, condition, policy in ARMS]
    return [dict(cell, schedule_index=index) for index, cell in enumerate(cells)]
