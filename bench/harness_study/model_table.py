"""Admissible local models for study modes that select their own coding models.

Each row pins the weights directory (relative to LM Studio's models root), the
context LM Studio actually loads the model at, and the preflight weights
fingerprint (sha256 of the canonical tree manifest of that directory). Pinning
the fingerprint here lets a human approval cover the exact weight bytes before
any preflight runs. Changing a row changes every design identity that selects it.
"""
import json
from pathlib import Path

HELPER = "gemma-4-e4b-it-mlx"

MODELS = {
    "qwen/qwen3.8-27b": {
        "weights": "lmstudio-community/Qwen3.8-27B-MLX-5bit",
        "description": "dense 27B, 5-bit MLX",
        "runtime_context_tokens": 262144,
        "weights_sha256": "4b280d6d35317f83db2295bc9c3cdd2e538cdf6f93294f8d1be8235a82f4d8db",
    },
    "google/gemma-4-26b-a4b": {
        "weights": "lmstudio-community/gemma-4-26B-A4B-it-MLX-5bit",
        "description": "mixture of experts 26B, about 4B active, 5-bit MLX",
        "runtime_context_tokens": 262144,
        "weights_sha256": "a2252b8f773d8e6f5d6b74ef1ad650a703b1dbc9a1a657014a761c68748181a5",
    },
    "gemma-4-31b-it": {
        "weights": "mlx-community/gemma-4-31b-it-5bit",
        "description": "dense 31B, 5-bit MLX",
        "runtime_context_tokens": 262144,
        "weights_sha256": "8252068e715e84786f0f662f6d6320155d0105ae3a6b769860f6f80d2096ca19",
    },
    "qwen/qwen3.5-9b": {
        "weights": "lmstudio-community/Qwen3.5-9B-MLX-4bit",
        "description": "dense 9B, 4-bit MLX; floor-bracketing model",
        "runtime_context_tokens": 262144,
        "weights_sha256": "1952ccec586ce748a84783c67919ea70d18eeaf677f05b895d5ef46964338e36",
    },
    "llama-3.3-70b-instruct": {
        "weights": "mlx-community/Llama-3.3-70B-Instruct-4bit",
        "description": "dense 70B, 4-bit MLX; upper size tier",
        "runtime_context_tokens": 32768,
        "weights_sha256": "39d2d119eaac71c81972fa908b007a40bc6b723a0e888e25d1fb8bbb6e9b2c75",
    },
    "gemma-4-e4b-it-mlx": {
        "weights": "lmstudio-community/gemma-4-E4B-it-MLX-4bit",
        "description": "Gemma 4 E4B, 4-bit MLX; the daemon helper",
        "runtime_context_tokens": 131072,
        "weights_sha256": "efee60910c77a6ff9497b225c693efa5975bbe8761cd8de8ebe15dca517ad560",
    },
}


def row(model_id):
    if model_id not in MODELS:
        raise ValueError(f"model is not in the model table: {model_id}")
    return {"id": model_id, **MODELS[model_id]}


def models_root(config):
    """LM Studio's models root: the app's `downloadsFolder` setting beside the index cache the
    configuration names, else the default `models` folder there."""
    home = Path(config["lmstudio_index"]).parents[1]
    settings = home / "settings.json"
    if settings.is_file():
        folder = json.loads(settings.read_text()).get("downloadsFolder")
        if isinstance(folder, str) and folder.strip():
            if not Path(folder).is_absolute():
                raise ValueError("LM Studio downloadsFolder must be an absolute path")
            return Path(folder)
    return home / "models"


def config_entries(model_ids, root):
    """`local_models` entries in the shape load, association and fingerprint code read."""
    return [{"id": spec["id"], "weights": str(Path(root) / spec["weights"]),
             "runtime_context_tokens": spec["runtime_context_tokens"]}
            for spec in (row(model_id) for model_id in dict.fromkeys(model_ids))]


def verify_entries(config, model_ids):
    expected = config_entries(model_ids, models_root(config))
    if config.get("local_models") != expected:
        raise ValueError("local_models must equal the model table entries for the selected models and helper")
    return expected


def verify_fingerprints(config, preflight_result):
    rows = []
    for index, model in enumerate(config["local_models"]):
        expected = row(model["id"])
        observed = preflight_result.get(f"local_model_{index}") or {}
        if observed.get("weights_sha256") != expected["weights_sha256"]:
            raise ValueError(f"weights fingerprint differs from the model table: {model['id']}")
        rows.append(expected)
    return rows
