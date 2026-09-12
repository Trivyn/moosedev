"""Exercise the frozen harness's actual intent schemas before scored runs.

These neutral requests execute no coding action and see no scenario or graph.
Provider bytes, failures, native receipts and exact executable identity remain
available even when preflight fails. This is setup cost, never episode usage.
Valid production-contract responses pass even when the model chooses another
allowed semantic branch; that mismatch remains a diagnostic in the receipt.
"""
from datetime import datetime, timezone
import json
from pathlib import Path
import subprocess
import time
import uuid

from .artifacts import _directory, canonical_json, sha256_file
from .binaries import REPO
from .proxy import ModelProxy

EXPECTED_PROBES = {"association_associate", "association_none_applies", "purpose_select",
                   "purpose_done", "purpose_missing_empty"}
PROBE_TIMEOUT_SECONDS = 570


def validate_receipt(value):
    if not isinstance(value, dict) or value.get("passed") is not True:
        raise ValueError("native intent contract did not pass")
    attempts = value.get("attempts")
    if (not isinstance(attempts, list) or len(attempts) != len(EXPECTED_PROBES)
            or any(not isinstance(attempt, dict) or not isinstance(attempt.get("name"), str)
                   for attempt in attempts)):
        raise ValueError("native intent contract requires every semantic probe")
    names = [attempt.get("name") for attempt in attempts]
    if set(names) != EXPECTED_PROBES or any(attempt.get("passed") is not True for attempt in attempts):
        raise ValueError("native intent contract probe coverage is incomplete")
    return value


def probe_native_contracts(config, binaries, *, output_root=None):
    parent = _directory(output_root or REPO / "target/harness-study/native-contract-probes", create=True)
    root = parent / str(uuid.uuid4())
    root.mkdir(mode=0o700)
    binary = Path(binaries["binaries"]["session"])
    expected = binaries["binary_hashes"]["session"]
    if sha256_file(binary) != expected:
        raise ValueError(f"native probe executable changed; evidence: {root}")
    result = {"schema_version": 1, "purpose": "neutral native intent contract preflight",
              "build_id": binaries["build_id"], "executable": str(binary),
              "executable_sha256": expected, "directory": str(root), "models": []}
    for index, model in enumerate(config["local_models"]):
        directory = root / str(index)
        directory.mkdir(mode=0o700)
        (directory / "home").mkdir(mode=0o700)
        (directory / "tmp").mkdir(mode=0o700)
        observation = {"model": model["id"], "passed": False}
        started = time.monotonic()
        with (directory / "events.jsonl").open("xb") as events:
            def record(payload):
                events.write(canonical_json({"timestamp": datetime.now(timezone.utc).isoformat(),
                                             "payload": payload}))
                events.flush()

            try:
                with ModelProxy(config["endpoint"], model["id"], record, "neutral_preflight",
                                expected_temperature=0.0) as proxy:
                    command = [str(binary), "--probe-intent-contracts", "--model", model["id"],
                               "--endpoint", proxy.url]
                    environment = {"HOME": str(directory / "home"), "TMPDIR": str(directory / "tmp"),
                                   "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8",
                                   "MOOSEDEV_LLM_API_KEY": "local-study",
                                   "MOOSEDEV_LLM_CONTEXT_WINDOW_TOKENS": str(config["context_tokens"]),
                                   "MOOSEDEV_HARNESS_RESPONSE_POLICY": config.get("harness_response_policy", "auto")}
                    (directory / "command.json").write_bytes(canonical_json(
                        {"command": command, "environment": environment, "timeout_seconds": PROBE_TIMEOUT_SECONDS}))
                    try:
                        completed = subprocess.run(command, cwd=directory, env=environment,
                                                   capture_output=True, timeout=PROBE_TIMEOUT_SECONDS, check=False)
                        stdout, stderr = completed.stdout, completed.stderr
                        observation["returncode"] = completed.returncode
                    except subprocess.TimeoutExpired as error:
                        stdout, stderr = error.stdout or b"", error.stderr or b""
                        observation.update(returncode=None, timed_out=True)
                    (directory / "stdout.json").write_bytes(stdout)
                    (directory / "stderr.txt").write_bytes(stderr)
                observation["proxy_failures"] = proxy.failure_details
                if observation.get("returncode") != 0 or proxy.failures:
                    raise ValueError("native process or provider request failed")
                observation["receipt"] = validate_receipt(json.loads(stdout))
                observation["passed"] = True
            except Exception as error:
                observation["error"] = str(error)
        observation["elapsed_seconds"] = time.monotonic() - started
        observation["files"] = {path.name: sha256_file(path) for path in directory.iterdir() if path.is_file()}
        result["models"].append(observation)
        result["passed"] = len(result["models"]) == len(config["local_models"]) and all(
            item["passed"] for item in result["models"])
        (root / "receipt.json").write_bytes(canonical_json(result))
        if not observation["passed"]:
            raise ValueError(f"native intent contract incompatible for {model['id']}; evidence: {root}")
    if sha256_file(binary) != expected:
        raise ValueError(f"native probe executable changed during execution; evidence: {root}")
    return result
