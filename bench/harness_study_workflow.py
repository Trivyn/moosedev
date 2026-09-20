"""Offline supplementary workflow observations from verified pilot evidence.

python3 -m bench.harness_study_workflow --store EVIDENCE --output NEW_DIRECTORY

Counts refer to indexed observations, never comparable backend failure rates.
This companion does not execute agents, grading checks, or model requests.
"""
import argparse
import base64
from collections import Counter, defaultdict
import hashlib
import json
from pathlib import Path

from .harness_study import artifacts, grading


def _json(value):
    def reject_constant(constant):
        raise ValueError(f"invalid JSON constant: {constant}")
    return json.loads(value, parse_constant=reject_constant)


def _sealed_bytes(run, seal, name):
    content = (run / name).read_bytes()
    if hashlib.sha256(content).hexdigest() != seal["files"].get(name, {}).get("sha256"):
        raise ValueError(f"sealed artifact changed: {run.name}/{name}")
    return content


def _observation(event, kind, detail, **extra):
    return {"kind": kind, "episode": event["payload"].get("episode"),
            "source": {"path": "events.jsonl", "sequence": event["sequence"],
                       "channel": event["channel"]}, "detail": detail, **extra}


def _response_models(events):
    """Decode retained bytes, including SSE split arbitrarily across chunks."""
    chunks = [event["payload"] for event in events
              if event["payload"].get("event") == "response_chunk"]
    if not chunks:
        return [], "unavailable: no response chunks"
    try:
        content = b"".join(base64.b64decode(chunk["raw_base64"], validate=True) for chunk in chunks)
        if content.lstrip().startswith(b"{"):
            bodies = [_json(content)]
        else:
            bodies = [_json(line[5:].strip()) for line in content.splitlines()
                      if line.startswith(b"data:") and line[5:].strip() != b"[DONE]"]
        if not bodies or any(not isinstance(body, dict) or not isinstance(body.get("model"), str)
                             for body in bodies):
            return [], "unavailable: missing response model identity"
        return sorted({body["model"] for body in bodies}), "observed"
    except (ValueError, KeyError, TypeError):
        return [], "unavailable: response bytes could not be fully decoded"


def _requests(events, manifest, config):
    grouped = defaultdict(list)
    for event in events:
        payload = event["payload"]
        if event["channel"] == "model" and isinstance(payload.get("id"), str):
            grouped[(payload.get("episode"), payload.get("role"), payload["id"])].append(event)
    requests = []
    for (episode, role, request_id), members in grouped.items():
        by_type = defaultdict(list)
        for event in members:
            by_type[event["payload"].get("event")].append(event)
        bodies, body_status = [], "unavailable: no recorded request body"
        # request_bytes and request are two representations of one request.
        for event in by_type["request_bytes"] or by_type["request"]:
            payload = event["payload"]
            try:
                body = (_json(base64.b64decode(payload["raw_base64"], validate=True))
                        if "raw_base64" in payload else payload["body"])
                if not isinstance(body, dict):
                    raise ValueError("request body is not an object")
                bodies.append(body)
            except (ValueError, TypeError, KeyError):
                body_status = "unavailable: malformed recorded request body"
        body = bodies[0] if len(bodies) == 1 else None
        if len(bodies) > 1:
            body_status = "unavailable: repeated request body events for this identity"
        elif body is not None:
            body_status = "observed"
        expected_model = (manifest.get("model") if role == "agent" else
                          config.get("helper_model") if role == "helper" else None)
        policy = config.get("generation_policy", {})
        expected_temperature = policy.get("local_temperature")
        temperature = body.get("temperature") if body else None
        response_models, response_status = _response_models(members)
        requests.append({"episode": episode, "role": role, "request_id": request_id,
            "event_sequences": {key: [event["sequence"] for event in values]
                                for key, values in by_type.items() if values},
            "request_body_status": body_status,
            "requested_model": body.get("model") if body else None,
            "expected_model": expected_model,
            "request_identity_matches": (body.get("model") == expected_model
                                         if body is not None and expected_model is not None else None),
            "temperature": temperature, "expected_temperature": expected_temperature,
            "temperature_matches": (type(temperature) in (int, float) and temperature == expected_temperature
                                    if body is not None and expected_temperature is not None else None),
            "other_observed_sampling": {key: body[key] for key in ("top_p", "top_k", "seed", "max_tokens")
                                        if body and key in body},
            "response_models": response_models, "response_identity_status": response_status,
            "response_identity_matches": (response_models == [expected_model]
                                          if response_status == "observed" and expected_model is not None else None),
            "proxy_complete_observed": bool(by_type["complete"]),
            "proxy_errors": [event["payload"].get("error") for event in by_type["error"]]})
    return requests


def observe(events, manifest, config):
    """Index raw protocol evidence; deliberately ignore normalized native copies."""
    observations, approvals, ignored, state_errors, journal = [], [], [], {}, {}
    for expected, event in enumerate(events, 1):
        if event.get("sequence") != expected or not isinstance(event.get("payload"), dict):
            raise ValueError("invalid event sequence or payload")
        payload, channel = event["payload"], event.get("channel")
        if channel == "input" and payload.get("type") == "input" and str(payload.get("text", "")).startswith("/"):
            approvals.append(_observation(event, "simulated_reviewer_input",
                                          {"text": payload["text"], "reason": payload.get("reason")}))
        if channel not in {"stdout", "stderr"}:
            continue
        try:
            native = _json(payload.get("text", ""))
        except (TypeError, ValueError):
            ignored.append(event["sequence"])
            continue
        if not isinstance(native, dict):
            ignored.append(event["sequence"])
            continue
        kind, detail = None, None
        if native.get("type") in {"error", "turn.failed"}:
            kind, detail = "native_error", native.get("error", native.get("message"))
        part, item = native.get("part") or {}, native.get("item") or {}
        if str(manifest.get("backend") or "").startswith("opencode") and native.get("type") == "tool_use":
            state = part.get("state") or {}
            if state.get("status") == "error":
                kind, detail = "native_tool_error", {"tool": part.get("tool"), "error": state.get("error")}
        if manifest.get("backend") in {"codex", "codex_mcp"} and native.get("type") == "item.completed":
            if item.get("type") == "error":
                kind, detail = "native_error", item.get("message")
            elif item.get("type") == "mcp_tool_call" and (item.get("error") or item.get("status") == "failed"):
                kind, detail = "native_tool_error", item
            elif item.get("type") == "command_execution" and type(item.get("exit_code")) is int and item["exit_code"] != 0:
                kind, detail = "native_command_nonzero_exit", item
        if kind:
            observations.append(_observation(event, kind, detail,
                native_identity=part.get("callID") or part.get("id") or item.get("id")))
        if manifest.get("backend") != "harness" or native.get("type") != "state":
            continue
        task = native.get("task") or {}
        identity = (payload.get("episode"), task.get("id"))
        if task.get("last_error"):
            key = (*identity, json.dumps(task["last_error"], sort_keys=True))
            if key not in state_errors:
                state_errors[key] = _observation(event, "harness_state_error", task["last_error"],
                    task_id=task.get("id"), source_sequences=[])
            state_errors[key]["source_sequences"].append(event["sequence"])
        # Journal position gives identity despite repeated whole-task snapshots.
        for index, entry in enumerate(task.get("events", [])):
            key = (*identity, index)
            if key in journal:
                if journal[key]["detail"] != entry:
                    raise ValueError("harness task journal changed at an existing index")
                continue
            journal[key] = _observation(event, "harness_journal_entry", entry,
                                       task_id=task.get("id"), journal_index=index)
    journal_observations = []
    for entry in journal.values():
        message = entry["detail"].get("message", "")
        if message.startswith("Capture rejected before persistence;"):
            entry["kind"] = "harness_capture_rejection_journal_entry"
            journal_observations.append(entry)
        elif message.startswith("Step rejected or interrupted:"):
            entry["kind"] = "harness_step_rejection_journal_entry"
            journal_observations.append(entry)
    requests = _requests(events, manifest, config)
    streams = []
    for role in sorted({request["role"] for request in requests}, key=str):
        members = [request for request in requests if request["role"] == role]
        streams.append({"role": role, "observed_request_identities": len(members),
            "proxy_complete_observed": sum(request["proxy_complete_observed"] for request in members),
            "requests_with_proxy_error": sum(bool(request["proxy_errors"]) for request in members),
            "validation": {field: {"matched": sum(request[field] is True for request in members),
                "mismatched": sum(request[field] is False for request in members),
                "unavailable": sum(request[field] is None for request in members)}
                for field in ("request_identity_matches", "temperature_matches", "response_identity_matches")}})
    return {"event_count": len(events), "channel_event_counts": dict(Counter(e["channel"] for e in events)),
            "native_error_observations": observations, "native_error_observation_count": len(observations),
            "native_error_observation_kinds": dict(Counter(entry["kind"] for entry in observations)),
            "harness_state_error_observations": list(state_errors.values()),
            "harness_journal_observations": journal_observations,
            "harness_journal_observation_kinds": dict(Counter(entry["kind"] for entry in journal_observations)),
            "simulated_reviewer_inputs": approvals, "simulated_reviewer_input_count": len(approvals),
            "proxy_requests": requests, "proxy_streams": streams,
            "unparsed_raw_event_sequences": ignored,
            "unavailable": {"comparable_tool_attempt_count": None, "tool_failure_rate": None,
                "capture_attempt_count": None, "capture_failure_rate": None,
                "malformed_action_count": None, "hosted_model_request_count": None,
                "primary_agent_turn_count": None}}


def analyze(store_root):
    root = Path(store_root).resolve()
    inventory = grading.report(root)
    runs = []
    for item in inventory["runs"]:
        row = {key: item[key] for key in ("run_id", "integrity", "status", "manifest")}
        runs.append(row)
        if item["integrity"] != "sealed":
            row["observations"] = None
            row["reason"] = item.get("error", "unfinished evidence excluded")
            continue
        run = root / "runs" / item["run_id"]
        seal = json.loads((run / "seal.json").read_bytes())
        row["evidence_sha256"] = seal["evidence_sha256"]
        config = {}
        if "preflight.json" in seal["files"]:
            frozen = json.loads(_sealed_bytes(run, seal, "preflight.json"))
            config = frozen["config"]
            digest = hashlib.sha256(artifacts.canonical_json(config)).hexdigest()
            if digest != frozen.get("config_sha256") or digest != item["manifest"].get("config_sha256"):
                raise ValueError("sealed preflight configuration identity mismatch")
        events = [json.loads(line) for line in _sealed_bytes(run, seal, "events.jsonl").splitlines()]
        row["events_sha256"] = seal["files"]["events.jsonl"]["sha256"]
        row["observations"] = observe(events, item["manifest"], config)
    return {"schema_version": 1, "runs": runs, "integrity_counts": inventory["integrity_counts"],
        "warnings": inventory["warnings"], "interpretation": [
            "All source sequence references identify the sealed run's events.jsonl; journal indexes are zero-based.",
            "Native errors index raw stdout/stderr only; normalized native events are duplicate representations.",
            "Native error observations are not deduplicated tool attempts, nor comparable failure rates.",
            "Harness state errors group equal text within a task; recurrence cannot establish independent attempts.",
            "Harness journal entries use task ID and journal index; capture and step rejections can describe the same action.",
            "Simulated reviewer inputs count recorded slash commands, including /no-knowledge; semantic approval is unassessed.",
            "Proxy request identities separate episode and role; request_bytes/request represent one request, including rejected requests.",
            "Agent proxy traffic can include client title generation; agent/helper roles do not identify primary coding turns.",
            "Request sampling checks compare only explicit frozen local_temperature; other sampling fields are observations.",
            "Response identity checks use decodable retained bytes; proxy completion is separate from partial-stream identity.",
            "No request observations does not establish zero hosted requests, tool failures, or capture omissions."]}


def write_report(store_root, output):
    root, output = Path(store_root).resolve(), Path(output)
    if output.resolve().is_relative_to(root):
        raise ValueError("workflow output must be outside the evidence store")
    result = analyze(root)
    output.mkdir(parents=True, exist_ok=False)
    source_root = Path(__file__).resolve().parent
    names = ["harness_study_workflow.py", "harness_study/grading.py", "harness_study/artifacts.py",
             "harness_study/__init__.py", "harness_study/process.py", "harness_study/adapters.py",
             "harness_study/proxy.py", "harness_study/run.py", "harness_study/reviewer.py"]
    provenance = {"command": "python3 -m bench.harness_study_workflow", "store": str(root),
        "replay_working_directory": "sources", "replay_arguments": ["--store", str(root), "--output", "NEW_OUTPUT_DIRECTORY"],
        "source_files": {}, "source_note": "Analyzer dependencies and protocol interpretation sources; frozen runtime identities remain in run manifests."}
    for name in names:
        source, relative = source_root / name, "sources/bench/" + name
        content = source.read_bytes()
        (output / relative).parent.mkdir(parents=True, exist_ok=True)
        with (output / relative).open("xb") as stream:
            stream.write(content)
        provenance["source_files"][relative] = {"original_path": str(source), "sha256": hashlib.sha256(content).hexdigest()}
    with (output / "workflow.json").open("xb") as stream:
        stream.write(artifacts.canonical_json(result))
    provenance["workflow_sha256"] = artifacts.sha256_file(output / "workflow.json")
    with (output / "provenance.json").open("xb") as stream:
        stream.write(artifacts.canonical_json(provenance))
    return output


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--store", type=Path, default=Path("target/harness-study/evidence"))
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args(argv)
    print(write_report(args.store, args.output))


if __name__ == "__main__":
    main()
