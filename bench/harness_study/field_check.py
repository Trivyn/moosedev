"""Exploratory field check: selected model-table models on the approved packages, never scored.

The mode reuses the symbolic baseline's arms and bounds by reference and the
sealed intent overlay, and has its own design identity that a schema-3 human
approval binds. It is deliberately not a member of `evolution.MODES`, so no
scoring or evolution-assessment path accepts its runs. See FIELD_CHECK.md.
"""
import hashlib
from pathlib import Path

from .artifacts import canonical_json
from . import evolution, intent, model_table

MODE = "local-harness-field-check"
ARMS = evolution.ARMS[evolution.SYMBOLIC_BASELINE_MODE]
SCENARIOS = intent.SCENARIOS
POLICIES = ("symbolic",)
RESPONSE_POLICY = "reasoning-off"
EPISODE_LIMIT = 1
EPISODE_SECONDS = 1200
CLIENT_CONTEXT_TOKENS = 32768
DOCUMENT = Path(__file__).with_name("FIELD_CHECK.md")


def _selection(coding_models, scenarios):
    models, scenarios = list(coding_models), list(scenarios)
    if not models or len(set(models)) != len(models):
        raise ValueError("field check requires one or more distinct coding models")
    for model in models:
        model_table.row(model)
    if not scenarios or len(set(scenarios)) != len(scenarios) or any(name not in SCENARIOS for name in scenarios):
        raise ValueError("field check scenarios must be distinct approved packages")
    return models, scenarios


def design_identity(coding_models, scenarios):
    models, scenarios = _selection(coding_models, scenarios)
    payload = {
        "schema_version": 1,
        "mode": MODE,
        "interpretation": "exploratory; episode 1 only; never scored, never pooled, not evidence for any study claim",
        "coding_models": [model_table.row(model) for model in models],
        "helper_model": model_table.row(model_table.HELPER),
        "arms": [{"backend": backend, "condition": condition, "intent_policy": policy}
                 for backend, condition, policy in ARMS],
        "scenarios": scenarios,
        "schedule_order": "coding model, then scenario, then harness before native",
        "episode_limit": EPISODE_LIMIT,
        "harness_response_policy": RESPONSE_POLICY,
        "native_reasoning": "no reasoning option sent",
        "client_context_tokens": CLIENT_CONTEXT_TOKENS,
        "episode_seconds": EPISODE_SECONDS,
        "local_temperature": 0.0,
        "evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
        "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT,
        "inherits": {"mode": evolution.SYMBOLIC_BASELINE_MODE,
                     "design_sha256": evolution.design_identity(evolution.SYMBOLIC_BASELINE_MODE)["sha256"]},
        "intent_design_sha256": intent.design_identity()["sha256"],
        "document_sha256": hashlib.sha256(DOCUMENT.read_bytes()).hexdigest(),
    }
    return {"payload": payload, "sha256": hashlib.sha256(canonical_json(payload)).hexdigest()}


def verify_config(config):
    if config.get("evaluation_mode") != MODE:
        raise ValueError("not a field-check configuration")
    models, scenarios = _selection(config.get("coding_models") or [], config.get("scenario_ids") or [])
    if config.get("helper_model") != model_table.HELPER:
        raise ValueError("field check keeps the frozen daemon helper model")
    model_table.verify_entries(config, [*models, model_table.HELPER])
    frozen = {"intent_policies": list(POLICIES), "episode_limit": EPISODE_LIMIT,
              "harness_response_policy": RESPONSE_POLICY, "context_tokens": CLIENT_CONTEXT_TOKENS,
              "episode_seconds": EPISODE_SECONDS, "evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
              "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT}
    for key, value in frozen.items():
        if config.get(key) != value:
            raise ValueError(f"field check requires {key} = {value!r}")
    if (config.get("generation_policy") or {}).get("local_temperature") != 0.0:
        raise ValueError("field check requires local temperature 0.0")
    if config.get("field_check_design") != design_identity(models, scenarios):
        raise ValueError("field-check design identity changed")
    return config["field_check_design"]


def schedule(config):
    verify_config(config)
    cells = [{"model": model, "backend": backend, "condition": condition,
              "scenario_id": scenario, "intent_policy": policy}
             for model in config["coding_models"] for scenario in config["scenario_ids"]
             for backend, condition, policy in ARMS]
    return [dict(cell, schedule_index=index) for index, cell in enumerate(cells)]
