"""Isolated three-episode runs. Gold is used only after the agent has stopped."""
from contextlib import ExitStack
import fcntl
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import tempfile

from .adapters import build_command
from .artifacts import ArtifactStore, canonical_json, sha256_file
from .binaries import REPO, verify_binaries
from .clients import verify_client
from .client_archive import archive_clients
from .config import configuration_hash, inventory, model_associations, required_clients, verify_approval
from .daemon import OwnedDaemon
from .isolation import sandbox_command
from .process import observe
from .proxy import ModelProxy
from .hosted_proxy import DomainProxy
from .privacy import CredentialFilter
from .scenario import SCENARIOS, load_scenario, relative_file, tree_manifest
from .seed import episode_prompt, prepare_workspace
from .validation import execute_check
from .usage import UsageLedger, resource_metrics


def snapshot(store, run, source, prefix, *, runtime=False, credentials=None):
    """Archive source/notes/durable evidence; never traverse agent-created aliases."""
    excluded = {".git", "__pycache__", ".cache", "node_modules", "target", "scratch"}
    for parent, directories, files in os.walk(source, followlinks=False):
        directories[:] = sorted(name for name in directories if name not in excluded)
        for name in list(directories):
            path = Path(parent) / name
            if path.is_symlink():
                raise ValueError(f"snapshot refuses directory alias: {path}")
        for name in sorted(files):
            path = Path(parent) / name
            relative = path.relative_to(source)
            if ".moosedev" in relative.parts and not (name == "kg.nq" or "harness" in relative.parts):
                continue
            if runtime and (name in {"auth.json", "auth.json.lock"} or "auth" in relative.parts):
                continue
            mode = path.lstat().st_mode
            if not stat.S_ISREG(mode):
                if stat.S_ISSOCK(mode) and runtime:
                    continue
                raise ValueError(f"snapshot refuses nonregular evidence: {path}")
            content = path.read_bytes()
            if credentials:
                content = credentials.bytes(content)
            store.put_bytes(run, prefix + "/" + relative.as_posix(), content)


def runtime_assets(executable):
    """Grant a selected client installation, never its containing home directory."""
    path = Path(executable).resolve(strict=True)
    for parent in path.parents:
        if parent.parent == REPO / "target/harness-study/clients":
            return [parent]
        if parent.name == "node_modules":
            # Include node itself and both platform packages selected by native
            # launchers. This root is installation code, not user configuration.
            installation = parent.parent.parent
            return [installation]
    return [path.parent]


def reset_episode(workspace):
    # Prior native history is already outside the agent view. Keep the project's
    # durable graph and ordinary notes, remove only the task/conversation store.
    from .isolation import _checked_path
    data = workspace / ".moosedev"
    if data.exists() or data.is_symlink():
        _checked_path(data, directory=True)
    history = data / "harness"
    if history.exists():
        if history.is_symlink():
            raise ValueError("episode history must not be a symlink")
        shutil.rmtree(history)
    for name in ("http.addr", "moosedev-serve.pid"):
        (workspace / ".moosedev" / name).unlink(missing_ok=True)


def load_models(config, cell, record, *, executable=None):
    required = [] if cell["backend"].startswith("codex") else [cell["model"]]
    if cell["condition"] != "without":
        required.append(config["helper_model"])
    required = list(dict.fromkeys(required))
    requested_context = config["context_tokens"]
    runtime_contexts = {}
    for model in config.get("local_models", []):
        context = model.get("runtime_context_tokens", requested_context)
        if type(context) is not int or context < requested_context:
            raise ValueError(f"runtime_context_tokens for {model['id']} must be an integer >= context_tokens")
        runtime_contexts[model["id"]] = context
    expected_contexts = {model: runtime_contexts.get(model, requested_context) for model in required}
    record("model_context_policy", {"client_context_tokens": requested_context,
                                    "requested_load_context_tokens": requested_context,
                                    "expected_runtime_context_tokens": expected_contexts})
    before = inventory(config["endpoint"])
    record("model_inventory_before", before)
    by_key = {model["key"]: model for model in before["models"]}
    for model in required:
        metadata = by_key.get(model)
        if not metadata:
            raise ValueError(f"exact model is unavailable: {model}")
        loaded = [instance for instance in metadata["loaded_instances"] if instance["id"] == model]
        if loaded:
            if loaded[0]["config"].get("context_length") != expected_contexts[model]:
                raise ValueError(f"loaded model {model} has a different context; unload it before this frozen run")
            continue
        command = [str(executable or config["lms"]), "load", model, "--identifier", model,
                   "--context-length", str(requested_context), "--parallel", "1"]
        result = subprocess.run(command, capture_output=True, timeout=300)
        record("model_setup", {"command": command, "returncode": result.returncode,
                               "stdout": result.stdout.decode(errors="replace"),
                               "stderr": result.stderr.decode(errors="replace")})
        if result.returncode:
            raise RuntimeError("model loading failed; no substitution is permitted")
    after = inventory(config["endpoint"])
    record("model_inventory", after)
    by_key = {model["key"]: model for model in after["models"]}
    for model in required:
        if not any(instance["id"] == model and instance["config"].get("context_length") == expected_contexts[model]
                   for instance in by_key.get(model, {}).get("loaded_instances", [])):
            raise ValueError("LM Studio did not load the exact requested identity/context")


def run_cell(store_root, frozen, cell, *, replacement_for=None):
    config = frozen["config"]
    if config.get("evaluation_mode") == "local-harness-development":
        if Path(store_root).resolve() == (REPO / "target/harness-study/evidence").resolve():
            raise ValueError("development reruns require a separate evidence store; preserve the pilot store")
        if cell["backend"] != "harness" or cell["condition"] != "harness":
            raise ValueError("development reruns are limited to local harness cells")
    scenario = load_scenario(cell["scenario_id"])
    store = ArtifactStore(Path(store_root).resolve())
    run = store.create_run({"schema_version": 1, "study_id": config["study_id"], **cell,
                            "evaluation_mode": config.get("evaluation_mode", "pilot"),
                            "parent_pilot": config.get("parent_pilot"),
                            "config_sha256": configuration_hash(config),
                            "scenario_gold_sha256": scenario["gold_sha256"],
                            "scenario_package_sha256": scenario["package_sha256"],
                            "replacement_for": replacement_for,
                            "native_internal_material": "unavailable unless present in exported evidence"})
    credentials = CredentialFilter()
    token_usage = UsageLedger(cell["backend"])

    def record(channel, event):
        safe = credentials.event(event)
        store.append_event(run, channel, safe)
        token_usage.consume(channel, safe)
    outcome = {"status": "preflight_failure", "episodes": []}
    capture_complete = False
    active_episode = None
    observation_complete = False
    lease = None
    # A permanent private execution directory survives exceptions. Only a verified
    # sealed bundle authorizes removing the disposable copy.
    execution = Path(tempfile.mkdtemp(prefix="md-study-", dir="/private/tmp"))
    workspace = execution / "work"
    try:
        # Shared across stores: LM Studio is a machine resource. Per-file evidence
        # locks do not serialize complete inference runs.
        lease_path = REPO / "target/harness-study/inference.lock"
        lease_path.parent.mkdir(parents=True, exist_ok=True)
        lease = open(lease_path, "a+b")
        fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        store.put_bytes(run, "preflight.json", canonical_json(frozen))
        store.put_bytes(run, "scenario.json", canonical_json(scenario))
        snapshot(store, run, SCENARIOS / scenario["id"], "scenario")
        snapshot(store, run, REPO / "bench/harness_study", "driver")
        store.put_bytes(run, "protocol.md", (REPO / "spec/harness_evaluation_protocol.md").read_bytes())
        if config.get("evaluation_mode") == "local-harness-development":
            store.put_bytes(run, "development-protocol.md",
                            (REPO / "bench/harness_study/DEVELOPMENT.md").read_bytes())
        if not frozen.get("ready") or frozen["config_sha256"] != configuration_hash(config):
            raise ValueError("run requires a successful frozen preflight")
        if "driver_files" in frozen and tree_manifest(REPO / "bench/harness_study") != frozen["driver_files"]:
            raise ValueError("study driver changed after preflight")
        if "model_associations" in frozen and model_associations(config) != frozen["model_associations"]:
            raise ValueError("LM Studio model-to-weight mapping changed after preflight")
        verify_approval(Path(config["gold_approval"]))
        binaries = verify_binaries(Path(config["binary_manifest"]))
        if binaries != frozen["binaries"]:
            raise ValueError("binary selection changed after preflight")
        for role in required_clients(config):
            if "runtime_root" in frozen[role]:
                verify_client(frozen[role])
            if sha256_file(Path(frozen[role]["path"])) != frozen[role]["sha256"]:
                raise ValueError(f"client executable changed after preflight: {role}")
            for path, digest in frozen[role].get("runtime_files", {}).items():
                if sha256_file(Path(path)) != digest:
                    raise ValueError(f"native client runtime changed after preflight: {path}")
        ca_bundle = None
        if "codex" in required_clients(config) and config.get("codex_ca_bundle"):
            identity = frozen["codex_ca_bundle"]
            ca_bundle = Path(identity["path"])
            if ca_bundle.is_symlink() or sha256_file(ca_bundle) != identity["sha256"]:
                raise ValueError("frozen Codex CA bundle changed after preflight")
            store.put_bytes(run, "hosted-ca-bundle.pem", ca_bundle.read_bytes())
        snapshot(store, run, Path(binaries["directory"]), "build")
        archive_clients(store, run, [frozen[role] for role in required_clients(config)])
        for name, hashes in frozen["assets"]["files"].items():
            if tree_manifest(Path(frozen["assets"]["directory"]) / name) != hashes:
                raise ValueError("daemon assets changed after preflight")
        snapshot(store, run, Path(frozen["assets"]["directory"]), "daemon-assets")
        for index, _ in enumerate(config["local_models"]):
            model = frozen[f"local_model_{index}"]
            if tree_manifest(Path(model["weights"])) != model["files"]:
                raise ValueError("local model weights changed after preflight")
        shutil.copytree(SCENARIOS / scenario["id"] / "project", workspace)
        subprocess.run(["/usr/bin/git", "init", "-q", str(workspace)], check=True, capture_output=True)
        prepare_workspace(workspace, scenario, cell["condition"])
        snapshot(store, run, workspace, "initial")
        load_models(config, cell, record, executable=frozen["lms"]["path"])
        outcome["status"] = "infrastructure_failure"
        for episode in scenario["episodes"]:
            episode_id = episode["id"]
            runtime = execution / episode_id
            runtime.mkdir()
            prompt = episode_prompt(episode, cell["condition"])
            store.put_bytes(run, f"episodes/{episode_id}/prompt.txt", prompt.encode())
            daemon = None
            with ExitStack() as stack:
                endpoint = None
                proxies = []
                hosted = None
                if cell["backend"].startswith("codex"):
                    hosted = stack.enter_context(DomainProxy(config["hosted_endpoints"],
                        lambda event: record("hosted_transport", dict(event, episode=episode_id))))
                if cell["backend"] in {"harness", "opencode"}:
                    proxy = stack.enter_context(ModelProxy(config["endpoint"], cell["model"],
                        lambda event: record("model", dict(event, episode=episode_id)), "agent",
                        expected_temperature=config.get("generation_policy", {}).get("local_temperature", 0.0)))
                    endpoint = proxy.url
                    proxies.append(proxy)
                if cell["condition"] != "without":
                    helper = stack.enter_context(ModelProxy(config["endpoint"], config["helper_model"],
                        lambda event: record("model", dict(event, episode=episode_id)), "helper",
                        expected_temperature=config.get("generation_policy", {}).get("local_temperature", 0.0)))
                    proxies.append(helper)
                    daemon = stack.enter_context(OwnedDaemon(
                        executable=Path(binaries["binaries"]["daemon"]), expected_sha256=binaries["binary_hashes"]["daemon"],
                        workspace=workspace, runtime=runtime, assets=Path(frozen["assets"]["directory"]),
                        helper_model=config["helper_model"], helper_endpoint=helper.url,
                        helper_context_tokens=config["context_tokens"], log_path=run / f"episodes/{episode_id}/daemon.log"))
                    record("daemon", dict(daemon.identity, episode=episode_id))
                executable = Path(binaries["binaries"]["session"] if cell["backend"] == "harness"
                                  else frozen["codex" if cell["backend"].startswith("codex") else "opencode"]["path"])
                command, environment = build_command(cell["backend"], executable=executable,
                    model=cell["model"], workspace=workspace, runtime=runtime, prompt=prompt,
                    endpoint=endpoint, daemon_url=daemon.url if daemon else None,
                    daemon_exe=Path(binaries["binaries"]["daemon"]),
                    daemon_socket=daemon.socket if daemon else None, context_tokens=config["context_tokens"],
                    harness_response_policy=config.get("harness_response_policy", "auto"),
                    ca_bundle=ca_bundle if cell["backend"].startswith("codex") else None)
                grants = [executable] if cell["backend"] == "harness" else runtime_assets(executable)
                network = [endpoint] if endpoint else [hosted.url]
                if daemon:
                    grants.append(Path(binaries["binaries"]["daemon"]))
                    network += [daemon.url, f"unix://{daemon.socket}"]
                if cell["backend"].startswith("codex"):
                    auth = Path(config["codex_auth"])
                    if auth.is_symlink() or not auth.is_file():
                        raise ValueError("explicit Codex auth file required for hosted pilot")
                    credentials.register_json(auth.read_bytes())
                    shutil.copyfile(auth, runtime / "codex/auth.json")
                    (runtime / "codex/auth.json").chmod(0o600)
                search_path = "/usr/bin:/bin:/usr/sbin:/sbin"
                if cell["backend"] != "harness":
                    node_bin = Path(frozen["codex"].get("runtime_root", Path(config["codex"]).parent)) / "bin"
                    search_path = str(node_bin) + ":" + search_path
                environment.update({"PATH": search_path,
                                    "LANG": "en_US.UTF-8", "LC_ALL": "en_US.UTF-8"})
                if hosted:
                    environment.update({"HTTPS_PROXY": hosted.url, "HTTP_PROXY": hosted.url,
                                        "ALL_PROXY": hosted.url, "NO_PROXY": "127.0.0.1,localhost,::1"})
                if cell["backend"] != "harness":
                    command = sandbox_command(command, workspace=workspace, runtime=runtime,
                                              readable_paths=grants, network_endpoints=network)
                record("confinement", {"episode": episode_id,
                    "owner": "native harness read/edit/command boundaries" if cell["backend"] == "harness"
                    else "study outer Seatbelt; native inner sandbox disabled"})
                active_episode = episode_id
                observation_complete = False
                result = observe(command, backend=cell["backend"], workspace=workspace, environment=environment,
                                 prompt=prompt, episode=episode,
                                 record=lambda channel, event: record(channel, dict(event, episode=episode_id)),
                                 seconds=config["episode_seconds"], expected_model=cell["model"])
                observation_complete = True
                if daemon:
                    record("checkpoint", dict(daemon.checkpoint(), episode=episode_id))
                if any(proxy.failures for proxy in proxies):
                    result.update(status="infrastructure_failure", error="model proxy recorded request/provider failure")
            result["request_usage"] = token_usage.report(episode_id)
            result["metrics"].update(resource_metrics(result["request_usage"]))
            # Gold is first consulted after the native agent and its daemon stop.
            snapshot(store, run, workspace, f"episodes/{episode_id}/workspace", credentials=credentials)
            # Never archive authentication material from runtime. Config and native
            # event exports are sufficient; history DBs may contain credential echoes.
            for path in (runtime / "codex-overrides.json", runtime / "opencode/opencode.json"):
                if path.is_file():
                    store.put_bytes(run, f"episodes/{episode_id}/config/{path.name}", path.read_bytes())
            check = execute_check(workspace, relative_file(SCENARIOS / scenario["id"], episode["hidden_test"]))
            result.update(id=episode_id, checks=[check])
            if result["status"] == "success" and not check["passed"]:
                result["status"] = check["status"]
            outcome["episodes"].append(result)
            outcome["status"] = result["status"]
            store.put_bytes(run, f"episodes/{episode_id}/outcome.json", canonical_json(result))
            active_episode = None
            if result["status"] != "success":
                break
            reset_episode(workspace)
        attempted = {episode["id"] for episode in outcome["episodes"]}
        outcome["episodes"].extend({"id": episode["id"], "status": "unattempted", "checks": [], "metrics": {}}
                                   for episode in scenario["episodes"] if episode["id"] not in attempted)
        capture_complete = True
    except BaseException as error:
        if outcome["status"] != "preflight_failure":
            outcome["status"] = "infrastructure_failure"
        outcome["error"] = f"{type(error).__name__}: {error}"
        if active_episode is not None and not any(episode["id"] == active_episode for episode in outcome["episodes"]):
            interrupted = {"id": active_episode, "status": "infrastructure_failure",
                           "error": outcome["error"], "observation_complete": observation_complete,
                           "request_usage": token_usage.report(active_episode),
                           "metrics": resource_metrics(token_usage.report(active_episode)), "checks": []}
            outcome["episodes"].append(interrupted)
            store.put_bytes(run, f"episodes/{active_episode}/outcome.json", canonical_json(credentials.event(interrupted)))
        if workspace.exists():
            try:
                snapshot(store, run, workspace, "interrupted-workspace", credentials=credentials)
                capture_complete = True
            except Exception as capture_error:
                outcome["snapshot_error"] = str(capture_error)
        else:
            capture_complete = True
    finally:
        try:
            if not capture_complete:
                outcome["retained_execution"] = str(execution)
            attempted = {episode["id"] for episode in outcome["episodes"]}
            outcome["episodes"].extend({"id": episode["id"], "status": "unattempted", "checks": [], "metrics": {}}
                                       for episode in scenario["episodes"] if episode["id"] not in attempted)
            outcome["request_usage"] = token_usage.report()
            store.put_bytes(run, "outcome.json", canonical_json(credentials.event(outcome)))
            store.seal_run(run)
            store.verify_run(run)
            if capture_complete:
                shutil.rmtree(execution)
        finally:
            if lease is not None:
                lease.close()
    return {"run_id": run.name, "path": str(run), **outcome}
