"""Time a model server's prefill on two consecutive harness prompts, sent as
the harness sends them. Live: it calls the server, and never writes the
journal it reads.

The harness sends one user message with the whole prompt, temperature 0, and
in the tools contract the action tools with `tool_choice: "required"` and no
parallel calls (`src/llm/mod.rs`). Each request here asks for one output
token, so its time is prefill plus the server's fixed cost per request.

For the chosen pair of consecutive action prompts (previous P, current C),
every measurement starts both with a fresh marker line, so nothing is served
from an earlier cache (LM Studio keeps more than the last prompt):

- cold:     marker + C, never seen: the whole prompt is prefilled.
- warm:     marker + P, then marker + C: what a real step pays.
- cached:   the same marker + C again: the fixed cost per request.
- generate: marker + P, then marker + C streamed as the harness sends it,
            capped at `generate_tokens`: time to first token and to the end,
            and the tokens generated.

Each prefill measurement is timed with the tools and without, so a cost the
tool schema adds shows as the difference.
"""
from __future__ import annotations

import json
import time
import urllib.error
import urllib.request
import uuid

from .prefix_reuse import PURPOSE, common_prefix

FLUSH = "Reply with one word: ok."


def action_pairs(task):
    """(index, previous request, current request) for consecutive action
    requests to the same server with nothing between them."""
    requests = [r for r in task.get("model_requests", []) if isinstance(r.get("prompt"), str)]
    pairs = []
    for index, (previous, current) in enumerate(zip(requests, requests[1:])):
        same = (previous.get("endpoint"), previous.get("model")) == (current.get("endpoint"), current.get("model"))
        if previous.get("purpose") == PURPOSE and current.get("purpose") == PURPOSE and same:
            pairs.append((index, previous, current))
    return pairs


def uncached_bytes(previous, current):
    a, b = previous["prompt"].encode(), current["prompt"].encode()
    return len(b) - common_prefix(a, b)


def choose_pair(task, pair=None):
    """The pair asked for, else the one whose uncached bytes are the median:
    a typical step, not the cheapest or the worst."""
    pairs = action_pairs(task)
    if not pairs:
        raise ValueError("the journal has no consecutive action requests to one server")
    if pair is not None:
        return pairs[pair]
    ranked = sorted(pairs, key=lambda item: uncached_bytes(item[1], item[2]))
    return ranked[len(ranked) // 2]


def marked(prompt, marker):
    return f"[probe {marker}]\n{prompt}"


def body(model, prompt, tools=None, max_tokens=1):
    """The request body the harness sends, asking for `max_tokens` output
    tokens (None: no limit, as the harness asks)."""
    payload = {"model": model, "messages": [{"role": "user", "content": prompt}],
               "temperature": 0.0, "stream": False}
    if max_tokens is not None:
        payload["max_tokens"] = max_tokens
    if tools is not None:
        payload.update({"tools": tools, "tool_choice": "required", "parallel_tool_calls": False})
    return payload


def send(endpoint, payload, timeout=900):
    """Seconds the request took, the server's prompt-token count, and the
    HTTP status (a server that refuses one token for a required tool call
    still has to prefill first, which is what is timed)."""
    request = urllib.request.Request(
        endpoint.rstrip("/") + "/chat/completions", data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"})
    started = time.monotonic()
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            reply = json.loads(response.read() or b"{}")
            status = response.status
    except urllib.error.HTTPError as error:
        reply, status = {}, error.code
    elapsed = time.monotonic() - started
    return elapsed, (reply.get("usage") or {}).get("prompt_tokens"), status


def stream(endpoint, payload, timeout=1800):
    """Seconds to the first streamed chunk and to the end, and the usage the
    server reports, for a streamed request."""
    payload = {**payload, "stream": True, "stream_options": {"include_usage": True}}
    request = urllib.request.Request(
        endpoint.rstrip("/") + "/chat/completions", data=json.dumps(payload).encode(),
        headers={"Content-Type": "application/json"})
    started = time.monotonic()
    first, usage = None, {}
    with urllib.request.urlopen(request, timeout=timeout) as response:
        for raw in response:
            line = raw.decode(errors="replace").strip()
            if not line.startswith("data:") or line == "data: [DONE]":
                continue
            if first is None:
                first = time.monotonic() - started
            try:
                chunk = json.loads(line[5:])
            except ValueError:
                continue
            usage = chunk.get("usage") or usage
    return first, time.monotonic() - started, usage


def probe(task, tools, pair=None, repeat=2, endpoint=None, model=None, sender=send,
          streamer=stream, generate=True, generate_tokens=1024):
    index, previous, current = choose_pair(task, pair)
    endpoint = endpoint or current["endpoint"]
    model = model or current["model"]
    results = {}
    for label, schema in (("with_tools", tools), ("without_tools", None)):
        timed = {"cold": [], "warm": [], "cached": []}
        tokens = None
        for _ in range(repeat):
            cold = marked(current["prompt"], uuid.uuid4().hex)
            seconds, tokens, status = sender(endpoint, body(model, cold, schema))
            timed["cold"].append((seconds, status))
            marker = uuid.uuid4().hex
            sender(endpoint, body(model, marked(previous["prompt"], marker), schema))
            seconds, _, status = sender(endpoint, body(model, marked(current["prompt"], marker), schema))
            timed["warm"].append((seconds, status))
            seconds, _, status = sender(endpoint, body(model, marked(current["prompt"], marker), schema))
            timed["cached"].append((seconds, status))
        best = {name: round(min(s for s, _ in runs), 2) for name, runs in timed.items()}
        best["statuses"] = sorted({status for runs in timed.values() for _, status in runs})
        best["prompt_tokens"] = tokens
        results[label] = best
    if generate:
        marker = uuid.uuid4().hex
        sender(endpoint, body(model, marked(previous["prompt"], marker), tools))
        # Capped: the marker line can change what the model writes (an
        # uncapped probe once ran on for 15,837 tokens where the step wrote 191).
        first, total, usage = streamer(
            endpoint, body(model, marked(current["prompt"], marker), tools, generate_tokens))
        completion = usage.get("completion_tokens")
        results["generate"] = {
            "first_chunk_seconds": round(first, 2) if first is not None else None,
            "total_seconds": round(total, 2), "completion_tokens": completion,
            "seconds_per_token_after_first_chunk": round((total - first) / completion, 4)
            if completion and first is not None else None,
        }
    cold, cached = results["with_tools"]["cold"], results["with_tools"]["cached"]
    tokens = results["with_tools"]["prompt_tokens"]
    size = len(current["prompt"].encode())
    return {
        "pair": index, "endpoint": endpoint, "model": model,
        "prompt_bytes": size, "uncached_bytes": uncached_bytes(previous, current),
        "results": results,
        "prefill_seconds_per_kb": round((cold - cached) / (size / 1000), 3) if size else None,
        "prefill_tokens_per_second": round(tokens / (cold - cached), 1) if tokens and cold > cached else None,
        "tool_overhead_seconds": {name: round(results["with_tools"][name] - results["without_tools"][name], 2)
                                  for name in ("cold", "warm", "cached")},
    }


def probe_file(path, tools_path, **options):
    task = json.loads(open(path).read())
    tools = json.loads(open(tools_path).read())
    return probe(task, tools, **options)
