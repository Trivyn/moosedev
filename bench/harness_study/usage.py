"""Replayable request accounting. Observation never changes provider requests.

Proxy HTTP requests and native client turns are different measurement boundaries;
they are reported separately and never added together. Missing fields are unknown.
"""
import base64
from collections import Counter
import copy
import hashlib
import json
import threading

FIELDS = ("input_tokens", "output_tokens", "cache_read_tokens", "cache_write_tokens", "reasoning_tokens", "total_tokens")
SEMANTICS = {
    "openai_chat": "OpenAI-compatible field names retained; cache and reasoning inclusion require provider contract verification",
    "codex": "native input/cache/output/reasoning retained separately; inclusion is not inferred; unexported auxiliary requests excluded",
    "opencode": "native input/output/cache/reasoning categories retained separately; do not add to provider totals",
}


def number(value):
    return value if type(value) is int and value >= 0 else None


def normalize(raw, source):
    raw = raw if isinstance(raw, dict) else {}
    if source == "openai_chat":
        details = raw.get("prompt_tokens_details") or {}
        completion = raw.get("completion_tokens_details") or {}
        values = (raw.get("prompt_tokens"), raw.get("completion_tokens"),
                  details.get("cached_tokens") if isinstance(details, dict) else None,
                  raw.get("cache_creation_input_tokens"), completion.get("reasoning_tokens") if isinstance(completion, dict) else None,
                  raw.get("total_tokens"))
    elif source == "codex":
        values = (raw.get("input_tokens"), raw.get("output_tokens"), raw.get("cached_input_tokens"),
                  raw.get("cache_write_input_tokens"), raw.get("reasoning_output_tokens"), raw.get("total_tokens"))
    else:
        cache = raw.get("cache") or {}
        values = (raw.get("input"), raw.get("output"), cache.get("read") if isinstance(cache, dict) else None,
                  cache.get("write") if isinstance(cache, dict) else None, raw.get("reasoning"), raw.get("total"))
    return dict(zip(FIELDS, map(number, values)))


def summarize(requests):
    fields = {}
    for field in FIELDS:
        observed = [r["tokens"][field] for r in requests if r["tokens"].get(field) is not None]
        final = [r for r in requests if r["usage_final"] and r["tokens"].get(field) is not None]
        fields[field] = {"observed_requests": len(observed), "final_requests": len(final),
                         "observed_sum": sum(observed) if observed else None,
                         "total": sum(observed) if requests and len(final) == len(requests) else None}
    return {"requests": len(requests), "statuses": dict(Counter(r["status"] for r in requests)), "fields": fields}


def grouped(requests, key):
    return {value: summarize([r for r in requests if str(r.get(key) or "unknown") == value])
            for value in sorted({str(r.get(key) or "unknown") for r in requests})}


def section(requests):
    return {"requests": requests, "summary": summarize(requests),
            "by_role": grouped(requests, "role"), "by_purpose": grouped(requests, "purpose"),
            "by_repair": grouped(requests, "repair")}


class UsageLedger:
    def __init__(self, backend):
        self.backend = backend
        self._lock = threading.RLock()
        self._proxy = {}
        self._native = {}
        self._turn = {}
        self._receipts = {}
        self._pending_turns = {}
        self._harness_diagnostics = {}

    def consume(self, channel, event):
        with self._lock:
            if channel == "model":
                self._model(event)
            elif channel in {"stdout", "stderr"}:
                try:
                    raw = base64.b64decode(event["raw_base64"], validate=True)
                    value = json.loads(raw)
                except (KeyError, ValueError, TypeError, UnicodeError):
                    return
                if isinstance(value, dict) and (channel == "stdout" or value.get("type") in {"error", "turn.failed"}):
                    self.native(value, event.get("episode"))

    def native(self, event, episode=None):
        with self._lock:
            kind = event.get("type")
            if self.backend == "harness":
                task = event.get("task") or {}
                if kind == "state" and isinstance(task, dict):
                    ledger = task.get("token_usage")
                    if isinstance(ledger, dict):
                        task_id = task.get("id") if isinstance(task.get("id"), str) else None
                        self._harness_diagnostics[(episode, task_id)] = {
                            "episode": episode, "task_id": task_id, "legacy_gap": ledger.get("legacy_gap"),
                            "persistence_errors": copy.deepcopy(ledger.get("persistence_errors"))}
                    requests = ledger.get("requests") if isinstance(ledger, dict) else None
                    for receipt in requests if isinstance(requests, list) else []:
                        if isinstance(receipt, dict) and isinstance(receipt.get("id"), str) and receipt["id"]:
                            self._receipts[receipt["id"]] = {**copy.deepcopy(receipt), "episode": episode}
                return
            codex = self.backend.startswith("codex")
            if kind == "turn.started":
                self._turn[episode] = self._turn.get(episode, 0) + 1
                if codex:
                    if episode in self._pending_turns:
                        previous = self._pending_turns[episode]
                        self._native[(episode, previous["id"])] = previous
                    identifier = event.get("turn_id") or event.get("id") or f"unfinished-{self._turn[episode]}"
                    self._pending_turns[episode] = {"id": str(identifier), "episode": episode,
                        "role": "agent", "purpose": "native_turn", "repair": "unknown", "status": "unfinished",
                        "raw_usage": None, "tokens": normalize(None, "codex"), "usage_final": False,
                        "semantics": SEMANTICS["codex"]}
            if kind not in ({"turn.completed", "turn.failed"} if codex else {"step_finish"}):
                return
            self._pending_turns.pop(episode, None)
            part = event.get("part") if isinstance(event.get("part"), dict) else {}
            raw = event.get("usage") if codex else part.get("tokens")
            identifier = event.get("turn_id") or part.get("id") or event.get("id")
            identifier = identifier or self._turn.get(episode) or hashlib.sha256(json.dumps(event, sort_keys=True).encode()).hexdigest()
            key = (episode, str(identifier))
            source = "codex" if codex else "opencode"
            self._native[key] = {"id": str(identifier), "episode": episode, "role": "agent",
                "purpose": "native_turn" if codex else "native_step", "repair": "unknown",
                "status": "error" if kind == "turn.failed" else "completed",
                "raw_usage": copy.deepcopy(raw), "tokens": normalize(raw, source),
                "usage_final": isinstance(raw, dict), "semantics": SEMANTICS[source],
                "identity": "native_id" if event.get("turn_id") or part.get("id") or event.get("id") else "turn_sequence_or_event_digest"}

    def _model(self, event):
        kind, episode, identifier = event.get("event"), event.get("episode"), event.get("id")
        if kind == "shutdown":
            for request in self._proxy.values():
                if request["episode"] == episode and request["role"] == event.get("role") and request["status"] == "started":
                    request["status"] = "cancelled"
            return
        if not identifier:
            return
        request = self._proxy.setdefault(identifier, {"id": identifier, "episode": episode,
            "role": event.get("role"), "model": event.get("model"), "purpose": "unknown", "repair": "unknown",
            "status": "started", "usage_final": False, "raw_usage": None, "usage_snapshots": [],
            "tokens": normalize(None, "openai_chat"), "semantics": SEMANTICS["openai_chat"],
            "_buffer": b"", "_data": [], "_done": False, "_parse_error": False, "stream": False})
        if kind == "request":
            body = event.get("body") or {}
            request["stream"] = body.get("stream", False)
            metadata = event.get("usage_metadata") or {}
            request.update({key: metadata.get(key) for key in ("client_request_id", "decision_id", "candidate_attempt")})
            request["purpose"] = metadata.get("purpose") or ("helper" if request["role"] == "helper" else "unknown")
            candidate = number(request.get("candidate_attempt"))
            request["repair"] = "repair" if candidate and candidate > 1 else "initial" if candidate == 1 else "unknown"
        elif kind == "response_headers":
            request["http_status"] = event.get("status")
            request["content_type"] = event.get("content_type")
            request["_sse"] = request["stream"] and "application/json" not in (event.get("content_type") or "").lower()
        elif kind == "response_chunk":
            try:
                request["_buffer"] += base64.b64decode(event["raw_base64"], validate=True)
            except (KeyError, ValueError):
                request["_parse_error"] = True
                return
            if request.get("_sse", request["stream"]) and request.get("http_status") == 200:
                while b"\n" in request["_buffer"]:
                    line, request["_buffer"] = request["_buffer"].split(b"\n", 1)
                    line = line.rstrip(b"\r")
                    if line.startswith(b"data:"):
                        data = line[5:]
                        request["_data"].append(data[1:] if data.startswith(b" ") else data)
                    elif not line:
                        if request["_data"]:
                            data = b"\n".join(request["_data"])
                            request["_data"] = []
                            if request["_done"]:
                                request["_parse_error"] = True
                            elif data == b"[DONE]":
                                request["_done"] = True
                            else:
                                self._snapshot(request, data)
                    elif not line.startswith((b":", b"event:", b"id:", b"retry:")):
                        request["_parse_error"] = True
        elif kind in {"response_body_complete", "complete", "error"}:
            if not request.get("_sse", request["stream"]) or request.get("http_status") != 200:
                if request["_buffer"]:
                    self._snapshot(request, request["_buffer"])
                    request["_buffer"] = b""
            if kind in {"response_body_complete", "complete"}:
                request["usage_final"] = (request["raw_usage"] is not None and not request["_parse_error"]
                    and (not request.get("_sse", request["stream"]) or request.get("http_status") != 200 or request["_done"])
                    and not request["_buffer"] and not request["_data"])
            if kind == "complete":
                request["status"] = "completed"
                request["elapsed_seconds"] = event.get("elapsed_seconds")
            elif kind == "error":
                request["status"] = "error"
                request["error"] = event.get("error")
                request["compatibility_rejection"] = event.get("compatibility_rejection", False)
                request["elapsed_seconds"] = event.get("elapsed_seconds")

    @staticmethod
    def _snapshot(request, data):
        try:
            value = json.loads(data)
        except (ValueError, UnicodeError):
            request["_parse_error"] = True
            return
        if not isinstance(value, dict):
            request["_parse_error"] = True
            return
        raw = value.get("usage")
        if isinstance(raw, dict):
            request["usage_snapshots"].append(copy.deepcopy(raw))
            request["raw_usage"] = copy.deepcopy(raw)
            request["tokens"] = normalize(raw, "openai_chat")
        choices = value.get("choices")
        reasons = [choice.get("finish_reason") for choice in choices if isinstance(choice, dict) and choice.get("finish_reason")] if isinstance(choices, list) else []
        if reasons:
            request["finish_reasons"] = reasons

    def report(self, episode=None):
        with self._lock:
            requests = [{key: copy.deepcopy(value) for key, value in r.items() if not key.startswith("_")}
                        for r in self._proxy.values() if episode is None or r["episode"] == episode]
            for request in requests:
                receipt = self._receipts.get(request.get("client_request_id"))
                if receipt is not None:
                    request["harness_receipt"] = copy.deepcopy(receipt)
            native = [copy.deepcopy(r) for r in (*self._native.values(), *self._pending_turns.values()) if episode is None or r["episode"] == episode]
            receipts = [copy.deepcopy(r) for r in self._receipts.values() if episode is None or r["episode"] == episode]
            linked = {r.get("client_request_id") for r in requests}
            unmatched = [r["id"] for r in receipts if r["id"] not in linked]
            wire_attempts = Counter()
            for request in requests:
                if request.get("decision_id") and request.get("candidate_attempt"):
                    key = (request["decision_id"], request["candidate_attempt"])
                    wire_attempts[key] += 1
                    request["wire_attempt"] = wire_attempts[key]
            return {"version": 1, "sources": {"proxy": section(requests), "native": section(native)},
                    "harness_receipts": receipts,
                    "reconciliation": {"harness_receipts_without_proxy": unmatched,
                        "harness_journals": [copy.deepcopy(value) for value in self._harness_diagnostics.values()
                                             if episode is None or value["episode"] == episode]},
                    "authority": {"agent": "native" if self.backend.startswith("codex") else "proxy", "helper": "proxy"},
                    "exclusions": ["Native client usage cannot establish counts or cost of unexported auxiliary requests.",
                                   "Local provider token units are not interchangeable with hosted token units or dollar charges."]}


def resource_metrics(report):
    """Only publish complete fields from each role's authoritative boundary."""
    result = {}
    for role, source in report["authority"].items():
        summary = report["sources"][source]["by_role"].get(role)
        fields = summary["fields"] if summary else {}
        if role == "agent":
            result.update({field: fields.get(field, {}).get("total") for field in FIELDS})
        else:
            result.update({"helper_" + field: fields.get(field, {}).get("total") for field in FIELDS})
    if report.get("reconciliation", {}).get("harness_receipts_without_proxy"):
        for field in FIELDS:
            result[field] = None
    # Historical helper_tokens had no defined input/output or reasoning semantics.
    result["helper_tokens"] = None
    return result


def replay(events_path, backend):
    ledger = UsageLedger(backend)
    with open(events_path) as stream:
        for line in stream:
            event = json.loads(line)
            ledger.consume(event["channel"], event["payload"])
    return ledger


def report_store(store_root):
    """New versioned offline analysis; never rewrite recorded outcomes or reports."""
    from pathlib import Path
    from .artifacts import ArtifactStore, sha256_file

    root = Path(store_root).resolve(strict=True)
    store = ArtifactStore(root)
    runs = []
    for run in sorted((root / "runs").iterdir()):
        result = {"run_id": run.name, "integrity": "invalid"}
        try:
            store.verify_run(run)
            manifest = json.loads((run / "manifest.json").read_text())
            ledger = replay(run / "events.jsonl", manifest["backend"])
            outcome = json.loads((run / "outcome.json").read_text())
            result.update(integrity="sealed", events_sha256=sha256_file(run / "events.jsonl"),
                          request_usage=ledger.report(),
                          episodes={episode["id"]: ledger.report(episode["id"]) for episode in outcome["episodes"]})
        except (OSError, ValueError, KeyError, TypeError) as error:
            result["error"] = str(error)
        runs.append(result)
    return {"version": 1, "analysis": "request_usage", "runs": runs}
