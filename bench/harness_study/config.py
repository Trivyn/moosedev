"""Study configuration, exact local model inventory, and frozen approval inputs."""
from datetime import datetime, timezone
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import platform
import random
import shutil
import ssl
import subprocess
from urllib.request import urlopen

from .artifacts import canonical_json, sha256_file
from .binaries import REPO, verify_binaries
from .scenario import MAINTENANCE, list_scenarios, load_scenario, tree_manifest
from . import intent
from . import evolution


def read_json_url(url):
    with urlopen(url, timeout=10) as response:
        return json.load(response)


def inventory(endpoint):
    from urllib.parse import urlsplit
    address = urlsplit(endpoint)
    if address.hostname != "127.0.0.1" or address.scheme != "http" or not address.port:
        raise ValueError("pilot LM Studio endpoint must be explicit http://127.0.0.1:PORT/v1")
    return read_json_url(f"{address.scheme}://{address.netloc}/api/v1/models")


def template():
    home = Path.home()
    model_root = home / ".lmstudio/models/lmstudio-community"
    return {
        "schema_version": 1, "study_id": "harness-pilot-v1", "seed": 20260907,
        "episode_seconds": 1200, "context_tokens": 32768,
        "endpoint": "http://127.0.0.1:1234/v1",
        "binary_manifest": None, "gold_approval": None,
        "helper_model": "gemma-4-e4b-it-mlx",
        "local_models": [
            {"id": "qwen/qwen3.8-27b", "weights": str(model_root / "Qwen3.8-27B-MLX-5bit")},
            {"id": "google/gemma-4-26b-a4b", "weights": str(model_root / "gemma-4-26B-A4B-it-MLX-5bit")},
            {"id": "gemma-4-e4b-it-mlx", "weights": str(model_root / "gemma-4-E4B-it-MLX-4bit")},
        ],
        "frontier_model": "gpt-5.6-sol",
        "codex": shutil.which("codex"), "opencode": shutil.which("opencode"),
        "lms": str(home / ".lmstudio/bin/lms"),
        "codex_auth": str(home / ".codex/auth.json"),
        "lmstudio_index": str(home / ".lmstudio/.internal/model-index-cache.json"),
        "hosted_endpoints": ["https://chatgpt.com", "https://api.openai.com", "https://auth.openai.com"],
        "embedding_source": str(REPO.parent / "moose/models/snowflake-arctic-embed-s"),
        "generation_policy": {"local_temperature": 0.0, "frontier_reasoning": "medium",
                              "other_settings": "native defaults; preserve requests and runtime config"},
    }


def configuration_hash(config):
    return hashlib.sha256(canonical_json(config)).hexdigest()


def schedule(config):
    if config.get("evaluation_mode") in evolution.MODES:
        stage = config["evaluation_mode"]
        scenarios = config.get("scenario_ids")
        models = [model["id"] for model in config["local_models"]]
        policies = config.get("intent_policies", list(evolution.POLICIES[stage]))
        if (scenarios != list(intent.SCENARIOS) or models != list(intent.MODELS)
                or policies != list(evolution.POLICIES[stage])):
            raise ValueError("evolution study requires the frozen ordered models, policies, and scenarios")
        arms = evolution.ARMS.get(stage) or tuple(("harness", "harness", policy) for policy in policies)
        cells = [{"model": model, "backend": backend, "condition": condition,
                  "scenario_id": scenario, "intent_policy": policy}
                 for scenario in scenarios for model in models for backend, condition, policy in arms]
        expected = {evolution.STAGE1_MODE: 6, evolution.STAGE2_BASELINE_MODE: 18,
                    evolution.SYMBOLIC_BASELINE_MODE: 12}.get(stage, 12)
        identities = {(c["model"], c["backend"], c["condition"], c["intent_policy"], c["scenario_id"]) for c in cells}
        if len(cells) != expected or len(identities) != expected:
            raise ValueError("evolution schedule has duplicated or missing cells")
        random.Random(config["seed"]).shuffle(cells)
        return [dict(cell, schedule_index=index) for index, cell in enumerate(cells)]
    if config.get("evaluation_mode") == intent.MODE:
        scenarios = config.get("scenario_ids")
        models = [model["id"] for model in config["local_models"]]
        policies = config.get("intent_policies", list(intent.POLICIES))
        if (not isinstance(scenarios, list) or len(scenarios) != 3 or set(scenarios) != set(intent.SCENARIOS)
                or len(models) != 2 or set(models) != set(intent.MODELS)
                or len(policies) != 2 or set(policies) != set(intent.POLICIES)):
            raise ValueError("intent pilot requires the approved two models, two policies, and three scenarios")
        cells = [{"model": model, "backend": "harness", "condition": "harness", "scenario_id": scenario,
                  "intent_policy": policy} for scenario in scenarios for model in models for policy in policies]
        random.Random(config["seed"]).shuffle(cells)
        return [dict(cell, schedule_index=index) for index, cell in enumerate(cells)]
    development = config.get("evaluation_mode", "pilot") == "local-harness-development"
    if config.get("evaluation_mode", "pilot") not in {"pilot", "local-harness-development"}:
        raise ValueError("unknown evaluation_mode")
    setups = [] if development else [(config["frontier_model"], "codex", "without"),
                                    (config["frontier_model"], "codex_mcp", "codex_mcp")]
    for model in config["local_models"]:
        if not development:
            setups.append((model["id"], "opencode", "without"))
        setups.append((model["id"], "harness", "harness"))
    cells = [{"model": model, "backend": backend, "condition": condition, "scenario_id": scenario}
             for scenario in list_scenarios() if scenario != MAINTENANCE for model, backend, condition in setups]
    if development and (len(cells) != 6 or len({(c["model"], c["scenario_id"]) for c in cells}) != 6):
        raise ValueError("local harness development requires exactly six distinct model/scenario cells")
    random.Random(config["seed"]).shuffle(cells)
    return [dict(cell, schedule_index=index) for index, cell in enumerate(cells)]


def required_clients(config):
    mode = config.get("evaluation_mode")
    if any(backend == "opencode" for backend, _, _ in evolution.ARMS.get(mode, ())):
        return ("opencode", "lms")
    return ("lms",) if mode in ("local-harness-development", intent.MODE, *evolution.MODES) else ("codex", "opencode", "lms")


def evolution_config(parent, binary_manifest, study_id, stage):
    """Derive a new six-, twelve- or eighteen-cell evolution study from sealed parent inputs."""
    if stage not in evolution.MODES:
        raise ValueError("unknown harness evolution stage")
    original = parent.get("config")
    if not parent.get("ready") or not isinstance(original, dict) or parent.get("config_sha256") != configuration_hash(original):
        raise ValueError("evolution configuration requires an intact successful parent preflight")
    single_episode = stage in (evolution.STAGE2_RECOVERY_MODE, evolution.STAGE2_BASELINE_MODE,
                               evolution.SYMBOLIC_BASELINE_MODE)
    parents = {evolution.STAGE2_RECOVERY_MODE: (intent.MODE, evolution.STAGE2_MODE),
               evolution.STAGE2_BASELINE_MODE: (intent.MODE, evolution.STAGE2_MODE, evolution.STAGE2_RECOVERY_MODE),
               evolution.SYMBOLIC_BASELINE_MODE: (intent.MODE, evolution.STAGE2_MODE, evolution.STAGE2_RECOVERY_MODE,
                                                  evolution.STAGE2_BASELINE_MODE),
               }.get(stage, (intent.MODE,))
    sealed = (evolution.SEALED_SYMBOLIC_PREDECESSORS if stage == evolution.SYMBOLIC_BASELINE_MODE
              else evolution.SEALED_PREDECESSORS)
    if original.get("evaluation_mode") not in parents:
        raise ValueError("evolution configuration must descend from the frozen intent pilot")
    if not study_id.strip() or study_id == original.get("study_id"):
        raise ValueError("evolution study requires a distinct nonempty study_id")
    build = verify_binaries(Path(binary_manifest))
    if build["build_id"] == parent["binaries"]["build_id"]:
        raise ValueError("evolution study requires a newly frozen build")
    if single_episode and build["build_id"] in sealed:
        raise ValueError(f"{stage} refuses a sealed predecessor build")
    verify_approval(Path(original["gold_approval"]), config=original)
    result = deepcopy(original)
    result.update(study_id=study_id, evaluation_mode=stage,
                  binary_manifest=str(Path(binary_manifest).resolve()),
                  local_models=[model for model in original["local_models"] if model["id"] in intent.MODELS],
                  scenario_ids=list(intent.SCENARIOS), intent_policies=list(evolution.POLICIES[stage]),
                  episode_seconds=1200, context_tokens=32768,
                  generation_policy={**original.get("generation_policy", {}), "local_temperature": 0.0},
                  evolution_design=evolution.design_identity(stage),
                  **({"episode_limit": 1} if single_episode else {}),
                  **({"evidence_byte_limit": evolution.BASELINE_EVIDENCE_BYTE_LIMIT,
                      "reject_loop_limit": evolution.BASELINE_REJECT_LOOP_LIMIT}
                     if stage in (evolution.STAGE2_BASELINE_MODE, evolution.SYMBOLIC_BASELINE_MODE) else {}),
                  parent_pilot={"study_id": original["study_id"], "config_sha256": parent["config_sha256"],
                                "build_id": parent["binaries"]["build_id"],
                                "intent_design": intent.design_identity()})
    if "opencode" in required_clients(result) and not result.get("opencode"):
        raise ValueError("baseline stage requires the parent configuration's opencode client path")
    schedule(result)
    return result


def development_config(parent, binary_manifest, study_id):
    """Derive a new diagnostic evaluation without revising approved scenarios."""
    original = parent["config"]
    if not parent.get("ready") or parent.get("config_sha256") != configuration_hash(original):
        raise ValueError("development configuration requires a successful intact parent preflight")
    if not study_id.strip() or study_id == original["study_id"]:
        raise ValueError("development evaluation requires a distinct nonempty study_id")
    build = verify_binaries(Path(binary_manifest))
    if build["build_id"] == parent["binaries"]["build_id"]:
        raise ValueError("development evaluation requires a newly frozen build")
    verify_approval(Path(original["gold_approval"]))
    result = deepcopy(original)
    result.update(study_id=study_id, evaluation_mode="local-harness-development",
                  binary_manifest=str(Path(binary_manifest).resolve()), harness_response_policy="auto",
                  parent_pilot={"study_id": original["study_id"],
                                "config_sha256": parent["config_sha256"],
                                "build_id": parent["binaries"]["build_id"]})
    schedule(result)
    return result


def fingerprint_model(model, available):
    matches = [item for item in available["models"] if item["key"] == model["id"]]
    if len(matches) != 1 or matches[0]["type"] != "llm":
        raise ValueError(f"exact local model not available: {model['id']}")
    root = Path(model["weights"])
    if root.is_symlink() or not root.is_dir():
        raise ValueError(f"real local weight directory required: {root}")
    files = tree_manifest(root)
    if not any(name.endswith((".safetensors", ".gguf")) for name in files):
        raise ValueError(f"model has no complete weight files: {root}")
    if any(name.endswith((".part", ".tmp")) for name in files):
        raise ValueError(f"incomplete model download: {root}")
    metadata = {k: v for k, v in matches[0].items() if k != "loaded_instances"}
    return {"id": model["id"], "weights": str(root.resolve()), "files": files,
            "weights_sha256": hashlib.sha256(canonical_json(files)).hexdigest(),
            "runtime_metadata": metadata}


def client_identity(name):
    from .clients import freeze_client
    return freeze_client(Path(name))


def freeze_ca_bundle(source):
    """Freeze public trust anchors; never import a private key or user keychain."""
    source = Path(source)
    if not source.is_absolute() or source.is_symlink() or not source.is_file():
        raise ValueError("CA bundle must be an explicit regular PEM file")
    content = source.read_bytes()
    if b"PRIVATE KEY" in content:
        raise ValueError("CA bundle must not contain private keys")
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.load_verify_locations(cadata=content.decode("ascii"))
    count = context.cert_store_stats()["x509_ca"]
    if not count:
        raise ValueError("CA bundle contains no trust anchors")
    digest = hashlib.sha256(content).hexdigest()
    target = REPO / "target/harness-study/ca" / digest / "ca.pem"
    target.parent.mkdir(parents=True, exist_ok=True)
    try:
        with target.open("xb") as stream:
            stream.write(content)
        target.chmod(0o444)
    except FileExistsError:
        if target.is_symlink() or target.read_bytes() != content:
            raise ValueError("frozen CA bundle differs from its identity") from None
    return {"path": str(target), "sha256": digest, "source_path": str(source), "certificates": count}


def model_associations(config):
    """Bind LM Studio's model-key resolution to the fingerprinted concrete paths.

    This is local runtime metadata, not a cryptographic GPU attestation. Recheck
    it before loading, and archive the relevant model presets as well as weights.
    """
    index = json.loads(Path(config["lmstudio_index"]).read_text())
    result = {}
    for model in config["local_models"]:
        matches = [entry for entry in index["models"]
                   if model["id"] == entry.get("indexedModelIdentifier")
                   or model["id"] == entry.get("defaultIdentifier")]
        paths = {entry.get("concreteModelDirAbsolutePath") for entry in matches}
        if paths != {model["weights"]}:
            raise ValueError(f"LM Studio key does not resolve uniquely to frozen weights: {model['id']}")
        result[model["id"]] = [{"id": entry["indexedModelIdentifier"],
            "weights": entry["concreteModelDirAbsolutePath"], "quantization": entry.get("quant"),
            "virtual": entry.get("virtual"),
            "preset_files": {item["absPath"]: sha256_file(Path(item["absPath"]))
                             for item in entry.get("selfFiles", [])
                             if item["filename"] in {"model.yaml", "manifest.json"}}}
            for entry in matches]
    return result


def freeze_assets(build):
    """Copy non-source daemon dependencies into its explicitly allowed target tree."""
    config = build["config"]
    embedding = Path(config["embedding_source"]).resolve(strict=True)
    sources = {"ontologies": REPO / "ontologies", "skills": REPO / "skills",
               "models/" + embedding.name: embedding}
    hashes = {name: tree_manifest(path) for name, path in sources.items()}
    engine = REPO.parent / "moose/ontologies"
    for name, digest in tree_manifest(engine).items():
        if name in hashes["ontologies"] and hashes["ontologies"][name] != digest:
            raise ValueError(f"conflicting domain/engine ontology asset: {name}")
        hashes["ontologies"][name] = digest
    key = hashlib.sha256(canonical_json(hashes)).hexdigest()
    target = REPO / "target/harness-study/assets" / key
    target.mkdir(parents=True, exist_ok=True)
    for name, source in sources.items():
        destination = target / name
        if not destination.exists():
            shutil.copytree(source, destination)
            if name == "ontologies":
                shutil.copytree(engine, destination, dirs_exist_ok=True)
        if tree_manifest(destination) != hashes[name]:
            raise ValueError("frozen daemon assets do not match their source")
    return {"directory": str(target), "sha256": key, "files": hashes}


def preflight(config, *, fingerprint=True):
    """No model generation or model loading. Failures remain inspectable evidence."""
    checks = []
    result = {"schema_version": 1, "config": config, "config_sha256": configuration_hash(config),
              "timestamp": datetime.now(timezone.utc).isoformat(),
              "hardware": {"platform": platform.platform(), "machine": platform.machine(),
                           "python": platform.python_version(), "cpus": os.cpu_count()},
              "checks": checks, "schedule": schedule(config)}
    result["driver_files"] = tree_manifest(REPO / "bench/harness_study")
    if platform.system() == "Darwin":
        result["hardware"]["memory_bytes"] = int(subprocess.check_output(["/usr/sbin/sysctl", "-n", "hw.memsize"]))
        result["hardware"]["cpu"] = subprocess.check_output(["/usr/sbin/sysctl", "-n", "machdep.cpu.brand_string"], text=True).strip()

    def check(label, operation):
        try:
            value = operation()
            checks.append({"check": label, "passed": True})
            result[label] = value
        except Exception as error:
            checks.append({"check": label, "passed": False, "error": str(error)})

    check("binaries", lambda: verify_binaries(Path(config["binary_manifest"])))
    for name in required_clients(config):
        check(name, lambda name=name: client_identity(config[name]))
    if "codex" in required_clients(config) and config.get("codex_ca_bundle"):
        check("codex_ca_bundle", lambda: freeze_ca_bundle(config["codex_ca_bundle"]))
    check("lmstudio", lambda: inventory(config["endpoint"]))
    check("model_associations", lambda: model_associations(config))
    check("assets", lambda: freeze_assets(result))
    if fingerprint and "lmstudio" in result:
        for index, model in enumerate(config["local_models"]):
            check(f"local_model_{index}", lambda model=model: fingerprint_model(model, result["lmstudio"]))
    else:
        checks.append({"check": "weight_fingerprints", "passed": False, "error": "not fingerprinted"})
    check("gold_approval", lambda: verify_approval(Path(config["gold_approval"]), config=config))
    if config.get("evaluation_mode") == intent.MODE or config.get("evaluation_mode") in evolution.MODES:
        from .indexing import verify_indexer, probe_indexer, system_python_identity
        check("indexer", lambda: verify_indexer(config["indexer_manifest"]))
        check("indexer_system_python", system_python_identity)
        def matched_indexer():
            if result["binaries"].get("indexer") != result["indexer"]:
                raise ValueError("intent indexer identity must be included in the frozen binary manifest")
            return intent.design_identity()
        check("intent_design", matched_indexer)
        if config.get("evaluation_mode") in evolution.MODES:
            check("evolution_design", lambda: evolution.design_identity(config["evaluation_mode"])
                  if config.get("evolution_design") == evolution.design_identity(config["evaluation_mode"])
                  else (_ for _ in ()).throw(ValueError("evolution design identity changed")))
        check("indexer_probe", lambda: probe_indexer(result["indexer"], result["binaries"], result["assets"],
              evolution_contract=evolution.postedit_association_contract(config.get("evaluation_mode"))))
        if evolution.postedit_association_contract(config.get("evaluation_mode")):
            from .native_contracts import probe_native_contracts
            check("native_intent_contracts", lambda: probe_native_contracts(config, result["binaries"]))
    result["ready"] = all(item["passed"] for item in checks)
    return result


def approval_payload(reviewer, *, config=None):
    if not reviewer.strip():
        raise ValueError("named human reviewer required")
    experimental = config is not None and config.get("evaluation_mode") == intent.MODE
    selected = config["scenario_ids"] if experimental else [name for name in list_scenarios() if name != MAINTENANCE]
    if experimental:
        schedule(config)
    result = {"schema_version": 2 if experimental else 1, "reviewer": reviewer, "approved_at": datetime.now(timezone.utc).isoformat(),
            "scope": "synthetic reference behavior and claim rubric; not pilot outcomes",
            "scenarios": {name: {key: load_scenario(name)[key] for key in ("package_sha256", "gold_sha256")}
                          for name in selected}}
    if experimental:
        result["intent_design"] = intent.design_identity()
    return result


def verify_approval(path, *, config=None):
    value = json.loads(path.read_text())
    if value.get("schema_version") not in (1, 2) or not value.get("reviewer") or not value.get("approved_at"):
        raise ValueError("invalid human gold approval")
    evolution_run = config is not None and config.get("evaluation_mode") in evolution.MODES
    if value["schema_version"] == 2:
        if value.get("intent_design") != intent.design_identity():
            raise ValueError("intent approval does not match current design hash")
        selected = list(value["scenarios"])
        if set(selected) != set(intent.SCENARIOS):
            raise ValueError("intent approval must bind the selected scenario set")
        current = {name: {key: load_scenario(name)[key] for key in ("package_sha256", "gold_sha256")}
                   for name in selected}
    else:
        current = approval_payload(value["reviewer"])["scenarios"]
    if config is not None and config.get("evaluation_mode") == intent.MODE:
        if value["schema_version"] != 2 or set(config["scenario_ids"]) != set(value["scenarios"]):
            raise ValueError("intent pilot requires its explicitly scoped design and gold approval")
    if evolution_run and (value["schema_version"] != 2
            or set(config["scenario_ids"]) != set(value["scenarios"])
            or config.get("evolution_design") != evolution.design_identity(config["evaluation_mode"])):
        raise ValueError("evolution study requires original approved fixtures and its frozen new design")
    if value.get("scenarios") != current:
        raise ValueError("gold approval does not match current scenario package hashes")
    return value
