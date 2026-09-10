"""Native client launch descriptions, with explicit per-run configuration.

The caller supplies a scrubbed base environment, verified binaries, isolated
workspaces, and enforced confinement. Returned environment values are additions,
not a copy of os.environ. This module never starts agents or reads user credentials.
CLI flags were checked against installed help; config references:
https://learn.chatgpt.com/docs/config-file/config-reference
https://opencode.ai/docs/config/
"""
from __future__ import annotations

import json
import os
from pathlib import Path
import ssl
from urllib.parse import urlsplit

REPOSITORY = Path(__file__).resolve().parents[2]
BACKENDS = {"codex", "codex_mcp", "opencode", "harness"}


def _path(value: Path, *, executable: bool = False, repository_build: bool = False) -> Path:
    path = Path(value)
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError(f"explicit absolute path required: {path}")
    resolved = path.resolve(strict=True)
    if executable and (not resolved.is_file() or not os.access(resolved, os.X_OK)):
        raise ValueError(f"executable file required: {path}")
    if repository_build and not resolved.is_relative_to(REPOSITORY / "target"):
        raise ValueError("MOOSEDev executables must resolve inside repository target")
    return resolved


def _directory(path: Path) -> Path:
    if not path.is_absolute() or ".." in path.parts:
        raise ValueError(f"explicit absolute runtime path required: {path}")
    if path.resolve() != path:
        raise ValueError(f"runtime path must not traverse symlinks: {path}")
    path.mkdir(mode=0o700, parents=True, exist_ok=True)
    if not path.is_dir():
        raise ValueError(f"runtime path must be a directory: {path}")
    return path


def _config(path: Path, data: object) -> None:
    encoded = (json.dumps(data, indent=2, ensure_ascii=False) + "\n").encode()
    if path.is_symlink():
        raise ValueError(f"runtime configuration must not be a symlink: {path}")
    try:
        with path.open("xb") as stream:
            os.chmod(path, 0o600)
            stream.write(encoded)
    except FileExistsError:
        if path.read_bytes() != encoded:
            raise ValueError(f"runtime configuration already differs: {path}") from None


def _endpoint(value: str | None, name: str) -> str:
    if not value:
        raise ValueError(f"{name} is required")
    parsed = urlsplit(value)
    if parsed.scheme not in {"http", "https"} or not parsed.hostname:
        raise ValueError(f"{name} must be an explicit HTTP(S) URL")
    if parsed.username or parsed.password or parsed.query or parsed.fragment:
        raise ValueError(f"{name} must not contain credentials, query, or fragment")
    return value.rstrip("/")


def _copy_ca_bundle(source: Path, runtime: Path) -> Path:
    encoded = _path(source).read_bytes()
    if b"PRIVATE KEY" in encoded or b"-----BEGIN CERTIFICATE-----" not in encoded:
        raise ValueError("CA bundle must contain public PEM certificates only")
    try:
        ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT).load_verify_locations(cadata=encoded.decode("ascii"))
    except (ValueError, UnicodeError, ssl.SSLError) as error:
        raise ValueError("CA bundle must contain valid PEM certificates") from error
    destination = runtime / "codex-ca.pem"
    if destination.is_symlink():
        raise ValueError("runtime CA bundle must not be a symlink")
    try:
        with destination.open("xb") as stream:
            os.fchmod(stream.fileno(), 0o400)
            stream.write(encoded)
    except FileExistsError:
        if destination.read_bytes() != encoded:
            raise ValueError("runtime CA bundle already differs") from None
    return destination


def build_command(
    backend: str, *, executable: Path, model: str, workspace: Path, runtime: Path,
    prompt: str, endpoint: str | None = None, daemon_url: str | None = None,
    daemon_exe: Path | None = None, daemon_socket: Path | None = None,
    context_tokens: int = 32768, ca_bundle: Path | None = None,
    harness_response_policy: str = "auto",
) -> tuple[list[str], dict[str, str]]:
    """Build argv and controlled env additions; write only runtime configuration.

    ``codex_mcp`` requires an explicit daemon executable and Unix socket. Harness
    stdin is owned by the caller: send {"type":"input","text":prompt} and keep
    it open for ordinary reviewer slash commands. A caller-supplied Codex auth
    file at runtime/codex/auth.json is retained without being read here.

    Native user/plugin discovery is disabled where supported. The outer sandbox
    must also hide parent project instructions and system managed configurations;
    HOME/XDG isolation cannot override mandatory administrator configuration.
    """
    if backend not in BACKENDS:
        raise ValueError(f"unknown backend: {backend}")
    if harness_response_policy not in {"auto", "provider-default", "reasoning-off"}:
        raise ValueError("unknown harness response policy")
    if not isinstance(model, str) or not model.strip() or model.startswith("-"):
        raise ValueError("an exact nonempty model identifier is required")
    if isinstance(context_tokens, bool) or not isinstance(context_tokens, int) or context_tokens < 4096:
        raise ValueError("context_tokens must be an integer of at least 4096")
    if not isinstance(prompt, str):
        raise ValueError("prompt must be text")
    binary = _path(executable, executable=True, repository_build=backend == "harness")
    workspace = _path(workspace)
    if not workspace.is_dir():
        raise ValueError("workspace must be a directory")
    runtime = _directory(runtime)
    if runtime.is_relative_to(workspace) or workspace.is_relative_to(runtime):
        raise ValueError("runtime and workspace must be separate, non-nested directories")
    directories = {
        "HOME": runtime / "home", "XDG_CONFIG_HOME": runtime / "config",
        "XDG_DATA_HOME": runtime / "data", "XDG_CACHE_HOME": runtime / "cache",
        "XDG_STATE_HOME": runtime / "state", "CODEX_HOME": runtime / "codex",
        "TMPDIR": runtime / "tmp",
    }
    env = {name: str(_directory(path)) for name, path in directories.items()}
    env.update({"NO_COLOR": "1", "TERM": "dumb", "PYTHONNOUSERSITE": "1"})

    if backend in {"codex", "codex_mcp"}:
        if ca_bundle is not None:
            env["CODEX_CA_CERTIFICATE"] = str(_copy_ca_bundle(ca_bundle, runtime))
        overrides = {
            "approval_policy": "never", "sandbox_workspace_write.network_access": False,
            "shell_environment_policy.experimental_use_profile": False,
            "shell_environment_policy.ignore_default_excludes": False,
            "web_search": "disabled", "model_reasoning_effort": "medium", "mcp_servers": {},
        }
        if backend == "codex_mcp":
            if daemon_exe is None or daemon_socket is None:
                raise ValueError("codex_mcp requires daemon_exe and daemon_socket")
            daemon = _path(daemon_exe, executable=True, repository_build=True)
            socket = Path(daemon_socket)
            if not socket.is_absolute() or ".." in socket.parts:
                raise ValueError("daemon_socket must be an explicit absolute path")
            proxy_env = {"MOOSEDEV_NO_AUTOSPAWN": "1", "MOOSEDEV_SOCKET": str(socket),
                         "MOOSEDEV_DATA_DIR": str(workspace / ".moosedev")}
            # TOML inline tables require '=' rather than JSON ':'. The complete
            # table replaces the user server map; no inherited entries are used.
            overrides["mcp_servers"] = {"moosedev": {
                "command": str(daemon), "args": ["--connect", str(socket)],
                "env": proxy_env, "required": True,
            }}
        _config(runtime / "codex-overrides.json", overrides)
        command = [str(binary), "exec", "--ignore-user-config", "--ignore-rules",
                   "--ephemeral", "--json", "--color", "never", "--sandbox", "danger-full-access",
                   "--skip-git-repo-check", "--cd", str(workspace), "--model", model]
        for key, value in overrides.items():
            command.extend(["-c", f"{key}={_toml(value)}"])
        command.extend(["--", prompt])
        return command, env

    endpoint = _endpoint(endpoint, "endpoint")
    if backend == "opencode":
        provider_model = f"study/{model}"
        settings = {
            "$schema": "https://opencode.ai/config.json", "model": provider_model,
            "small_model": provider_model, "enabled_providers": ["study"],
            "default_agent": "build",
            "agent": {name: {"temperature": 0.0} for name in ("build", "title", "summary", "compaction")},
            "provider": {"study": {"npm": "@ai-sdk/openai-compatible", "name": "Study local model",
                "options": {"baseURL": endpoint, "apiKey": "local-study"},
                "models": {model: {"id": model, "name": model, "temperature": True,
                    "limit": {"context": context_tokens, "output": min(8192, context_tokens // 4)}}}}},
            "plugin": [], "mcp": {}, "instructions": [], "autoupdate": False,
            "share": "disabled", "formatter": False, "lsp": False,
            "permission": {"*": "deny", "read": "allow", "edit": "allow", "glob": "allow",
                           "grep": "allow", "list": "allow", "bash": "allow",
                           "external_directory": "deny", "webfetch": "deny", "websearch": "deny"},
            "compaction": {"auto": True, "prune": True, "reserved": context_tokens // 4},
        }
        opencode_dir = _directory(runtime / "opencode")
        config_file = opencode_dir / "opencode.json"
        _config(config_file, settings)
        env.update({"OPENCODE_CONFIG": str(config_file), "OPENCODE_CONFIG_DIR": str(opencode_dir),
                    "OPENCODE_CONFIG_CONTENT": json.dumps(settings, separators=(",", ":")),
                    "OPENCODE_DISABLE_PROJECT_CONFIG": "1", "OPENCODE_DISABLE_CLAUDE_CODE": "1",
                    "OPENCODE_DISABLE_EXTERNAL_SKILLS": "1", "OPENCODE_DISABLE_DEFAULT_PLUGINS": "1",
                    "OPENCODE_DISABLE_MODELS_FETCH": "1", "OPENCODE_DISABLE_AUTOUPDATE": "1"})
        return [str(binary), "run", "--pure", "--format", "json", "--dir", str(workspace),
                "--model", provider_model, "--", prompt], env

    daemon_url = _endpoint(daemon_url, "daemon_url")
    if daemon_exe is None:
        raise ValueError("harness requires daemon_exe")
    daemon = _path(daemon_exe, executable=True, repository_build=True)
    env.update({"MOOSEDEV_NO_AUTOSPAWN": "1", "MOOSEDEV_DATA_DIR": str(workspace / ".moosedev"),
                "MOOSEDEV_LLM_BASE_URL": endpoint, "MOOSEDEV_LLM_MODEL": model,
                "MOOSEDEV_LLM_API_KEY": "local-study",
                "MOOSEDEV_HARNESS_RESPONSE_POLICY": harness_response_policy,
                "MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS": str(context_tokens)})
    return [str(binary), "--project", str(workspace), "--daemon", daemon_url,
            "--daemon-exe", str(daemon), "--model", model, "--endpoint", endpoint], env


def _toml(value: object) -> str:
    if isinstance(value, dict):
        return "{ " + ", ".join(f"{json.dumps(key)} = {_toml(item)}" for key, item in value.items()) + " }"
    if isinstance(value, list):
        return "[" + ", ".join(_toml(item) for item in value) + "]"
    return json.dumps(value, ensure_ascii=False)


def _tokens(usage: dict, *, codex: bool) -> dict:
    cache = usage.get("cache") if isinstance(usage.get("cache"), dict) else {}
    return {"input": usage.get("input_tokens" if codex else "input"),
            "output": usage.get("output_tokens" if codex else "output"),
            "cache_read": usage.get("cached_input_tokens") if codex else cache.get("read"),
            "cache_write": usage.get("cache_write_input_tokens") if codex else cache.get("write"),
            "reasoning": usage.get("reasoning_output_tokens" if codex else "reasoning")}


def normalize_event(backend: str, event: dict) -> dict:
    """Annotate one observable native event without guessing missing data.

    Results are observations, not counts. Streaming deltas and native repeated
    state snapshots must not be summed as independent actions. Raw events remain
    authoritative. Shell text such as `cat` is never inferred to be a file read.
    Harness state snapshots need the caller's journal-cursor logic; this stateless
    helper does not count their repeated task histories or infer token usage.
    """
    if backend not in BACKENDS or not isinstance(event, dict):
        raise ValueError("known backend and object event required")
    result = {"backend": backend, "event_type": event.get("type"), "tokens": None,
              "read": None, "retrieval": None, "capture": None, "command": None,
              "assistant": None, "error": None}
    event_type = event.get("type")
    if event_type in {"error", "turn.failed"}:
        result["error"] = event.get("error", event.get("message"))
    if backend in {"codex", "codex_mcp"}:
        if event_type == "turn.completed" and isinstance(event.get("usage"), dict):
            result["tokens"] = _tokens(event["usage"], codex=True)
        item = event.get("item") if isinstance(event.get("item"), dict) else {}
        if event_type == "item.completed":
            if item.get("type") == "agent_message":
                result["assistant"] = item.get("text")
            elif item.get("type") == "command_execution":
                result["command"] = item
            elif item.get("type") == "mcp_tool_call":
                _classify_tool(result, item.get("tool"), item, item.get("server") == "moosedev")
            elif item.get("type") == "error":
                result["error"] = item.get("message")
    elif backend == "opencode":
        part = event.get("part") if isinstance(event.get("part"), dict) else {}
        if event_type == "step_finish" and isinstance(part.get("tokens"), dict):
            result["tokens"] = _tokens(part["tokens"], codex=False)
        elif event_type == "text":
            result["assistant"] = part.get("text")
        elif event_type == "tool_use":
            tool = part.get("tool")
            if tool == "read":
                result["read"] = part
            elif tool == "bash":
                result["command"] = part
            state = part.get("state") if isinstance(part.get("state"), dict) else {}
            if state.get("status") == "error":
                result["error"] = state.get("error")
    elif event_type == "progress":
        if event.get("kind") == "assistant_delta":
            result["assistant"] = event.get("text")
        elif event.get("kind") == "command_output":
            result["command"] = {"output_delta": event.get("text")}
    return result


def _classify_tool(result: dict, name: object, item: dict, moosedev: bool) -> None:
    if not isinstance(name, str):
        return
    if name.startswith("mcp__moosedev__"):
        moosedev, name = True, name.removeprefix("mcp__moosedev__")
    if moosedev and name in {"get_relevant_context", "get_entity_dossier", "query", "sparql"}:
        result["retrieval"] = item
    elif moosedev and name in {"record_important_decision", "supersede_decision", "retract_decision", "link_code"}:
        result["capture"] = item
    if item.get("error") is not None:
        result["error"] = item["error"]
