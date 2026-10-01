"""Replay saved harness decisions against a provider, and count what the model
does: the cheap way to test a harness change or a hypothesis about one
decision, without a full run.

A case is one decision: the next action request at a saved point. Two modes:

- `journal`: the request a past run sent, from a task journal
  (`model_requests[i].prompt`), optionally through a named transform. No
  daemon. It replays the old harness's prompt.
- `render`: a snapshot (`snapshot.py`) restored to a scratch directory, its
  next request built with a chosen `moosedev` binary (`moosedev code render`)
  and sent. It replays the new harness's prompt from the saved state.

Each case is sent `n` times, as the harness sends it (one user message,
temperature 0, the request's `max_tokens`, `reasoning_effort` and tools), to
a provider profile. Requests for one case are sent one after another, so a
local server's prompt cache is reused; OpenRouter's rate limits are retried
with backoff. Each answer is classified by the action it asks for.

Cases come from a JSON file, a list of objects:
  {"name", "mode": "journal"|"render", "journal": path, "request": index,
   "snapshot": name, "task": id, "check_event": int, "transforms": [names],
   "reasoning_effort": "none"|null (journal mode; default "none")}
`check_event` (optional) marks an `inspect` of that journal event, the failed
check's output, as `inspect(check)` apart from other inspects.
"""
from __future__ import annotations

import collections
import json
import os
import re
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

from . import snapshot

PROFILES = {
    "openrouter": {
        "endpoint": "https://openrouter.ai/api/v1",
        "model": "qwen/qwen3.8-27b",
        "api_key_env": "OPENROUTER_API_KEY",
        "provider": {"order": ["CoreWeave"], "allow_fallbacks": False},
    },
    "lmstudio": {
        "endpoint": "http://endor:1234/v1",
        "model": "qwen/qwen3.8-27b",
        "api_key_env": None,
        "provider": None,
    },
}

SCHEMA_MARKER = "Required JSON schema:\n"
PAGE_HINT = "; use inspect(event,offset) to page them"


# Transforms of a journaled prompt: hypotheses tested without a code change.

def drop_shortened_copies(prompt, case, hint=True):
    """Replace the shortened copies of the failed check's output (Recent
    observations, Check output previews) with a pointer to the Last result,
    which holds it whole; optionally drop the paging hint."""
    target = case.get("check_event")
    start = prompt.index("Recent observations")
    end = prompt.index("Last result:", start)
    section = prompt[start:end]
    open_list = section.index("[")
    entries, length = json.JSONDecoder().raw_decode(section[open_list:])
    pointer = "the failed required check; its whole output is the Last result below."
    entries = [f"Event {target}: Command: {entry.split(chr(10))[0].split('Command: ', 1)[1]} - {pointer}"
               if entry.startswith(f"Event {target}: Command: ") else entry for entry in entries]
    rest = section[open_list + length:]
    previews = rest.find("Check output previews:")
    if previews >= 0:
        rest = rest[:previews] + "Check output previews: " + pointer + "\n\n"
    head = section[:open_list]
    if not hint:
        head = head.replace(PAGE_HINT, "")
    return prompt[:start] + head + json.dumps(entries) + rest + prompt[end:]


def remove_inspect(prompt, case):
    """No inspect action: out of the schema and the action lists (a tools
    request's inspect tool is removed from the body by `run`)."""
    head, marker, schema = prompt.rpartition(SCHEMA_MARKER)
    if not marker:
        return prompt.replace("inspect(event,offset), ", "").replace("read, search, inspect, ", "read, search, ")
    data, end = json.JSONDecoder().raw_decode(schema)
    data["oneOf"] = [arm for arm in data["oneOf"] if arm["properties"]["action"].get("const") != "inspect"]
    head = head.replace("inspect(event,offset), ", "").replace("read, search, inspect, ", "read, search, ")
    return head + marker + json.dumps(data, separators=(",", ":")) + schema[end:]


def drop_focus_block(prompt, case):
    """No focus block (the failing test and the code it calls, which may end
    in a "bytes not shown" cut) at the head of the Last result."""
    start = prompt.find("[Harness: a required check failed:")
    if start < 0:
        start = prompt.find("[Harness: the same failure again")
    if start < 0:
        return prompt
    closing = "edit the code it points at.]\n"
    end = prompt.find(closing, start)
    return prompt if end < 0 else prompt[:start] + prompt[end + len(closing):]


TRANSFORMS = {
    "as_is": lambda prompt, case: prompt,
    "no_shortened_copies": drop_shortened_copies,
    "no_shortened_copies_no_hint": lambda prompt, case: drop_shortened_copies(prompt, case, hint=False),
    "no_inspect": remove_inspect,
    "no_focus": drop_focus_block,
}


def apply_transforms(prompt, case, name):
    """`a+b` applies a, then b."""
    for part in name.split("+"):
        prompt = TRANSFORMS[part](prompt, case)
    return prompt


def journal_request(case):
    """The body a journaled request was sent with (the prompt; the rest as the
    harness sends it), and its journal entry."""
    task = json.loads(Path(case["journal"]).read_text())
    entry = task["model_requests"][case["request"]]
    if entry.get("contract") == "tools" and not case.get("tools"):
        raise ValueError(f"{case['name']}: a tools-contract request needs its tool definitions (case 'tools': a JSON file); "
                         "the journal does not store them")
    # The journal does not record the response policy: the case says it
    # (the replicates ran reasoning-off).
    body = {"messages": [{"role": "user", "content": entry["prompt"]}], "temperature": 0.0,
            "max_tokens": entry.get("max_output_tokens"),
            "reasoning_effort": case.get("reasoning_effort", "none")}
    if entry.get("contract") == "tools" and case.get("tools"):
        body.update({"tools": json.loads(Path(case["tools"]).read_text()), "tool_choice": "required",
                     "parallel_tool_calls": False})
    return body, entry


def render_request(case, exe, port, root=snapshot.DEFAULT_ROOT, keep=False):
    """Restore the case's snapshot to a scratch directory, render its next
    request with `exe`, stop the daemon, and return the rendered request."""
    scratch = Path(tempfile.mkdtemp(prefix="decision-bench-")) / "project"
    pid = snapshot.restore(case["snapshot"], scratch, port, root=root, exe=exe)
    try:
        out = scratch.parent / "rendered.json"
        done = subprocess.run([str(exe), "code", "--project", str(scratch), "render", case["task"], str(out)],
                              capture_output=True, text=True)
        if done.returncode != 0:
            raise RuntimeError(f"render failed for {case['name']}: {done.stderr.strip()[-600:]}")
        return json.loads(out.read_text())
    finally:
        snapshot.stop(pid)
        if not keep:
            subprocess.run(["rm", "-rf", str(scratch.parent)])


def send(profile, body, timeout=900, attempts=8):
    """(content, usage, seconds) for one request; content is `ERROR ...` on a
    failure that retries did not clear."""
    payload = {k: v for k, v in body.items() if v is not None}
    payload["model"] = profile["model"]
    if profile.get("provider"):
        payload["provider"] = profile["provider"]
    else:
        payload.pop("provider", None)
    headers = {"Content-Type": "application/json"}
    if profile.get("api_key_env"):
        headers["Authorization"] = "Bearer " + os.environ[profile["api_key_env"]]
    request = urllib.request.Request(profile["endpoint"].rstrip("/") + "/chat/completions",
                                     data=json.dumps(payload).encode(), headers=headers)
    for attempt in range(attempts):
        started = time.monotonic()
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                reply = json.load(response)
            message = reply["choices"][0]["message"]
            content = message.get("content") or ""
            if message.get("tool_calls"):
                content = json.dumps({"tool_calls": message["tool_calls"], "content": content})
            return content, reply.get("usage") or {}, time.monotonic() - started
        except urllib.error.HTTPError as error:
            if error.code in (429, 502, 503) and attempt < attempts - 1:
                time.sleep(min(60, 5 * 2 ** attempt))
                continue
            return f"ERROR HTTP {error.code}", {}, time.monotonic() - started
        except Exception as error:  # noqa: BLE001 - recorded, not raised: one case must not end the bench
            return f"ERROR {error}", {}, time.monotonic() - started
    return "ERROR retries exhausted", {}, 0.0


EDITS = {"replace": "edit", "write": "edit", "edit": "edit", "apply_fix": "edit"}


def classify(text, check_event=None):
    """The action an answer asks for: edit, read, command, search, finish,
    reply, question, plan, inspect(check) / inspect(other), invalid or error."""
    if text.startswith("ERROR"):
        return "error"
    try:
        value = json.loads(text)
        calls = value.get("tool_calls") if isinstance(value, dict) else None
    except ValueError:
        calls = None
    if calls:
        # A reply beside an action narrates it; the harness runs the action.
        actions = [c for c in calls if (c.get("function") or {}).get("name") != "reply"]
        call = (actions or calls)[0].get("function", {})
        action = call.get("name")
        try:
            arguments = json.loads(call.get("arguments") or "{}")
        except ValueError:
            arguments = {}
    else:
        found = re.search(r"\{.*", text, re.S)
        try:
            arguments = json.JSONDecoder().raw_decode(found.group(0))[0] if found else None
        except ValueError:
            arguments = None
        if not isinstance(arguments, dict):
            return "invalid"
        if isinstance(arguments.get("action"), dict):
            arguments = arguments["action"]
        action = arguments.get("action") or arguments.get("name")
        if isinstance(arguments.get("arguments"), dict):
            arguments = arguments["arguments"]
    if action == "inspect":
        if check_event is not None and arguments.get("event") == check_event:
            return "inspect(check)"
        return "inspect(other)"
    return EDITS.get(action, action or "invalid")


def run(cases, profile_name, n=5, exe=None, port=7480, root=snapshot.DEFAULT_ROOT, out=None):
    """Send every case `n` times per transform; returns {(case, transform):
    Counter} and writes the raw answers to `out`."""
    profile = PROFILES[profile_name]
    results, raw = {}, []
    for case in cases:
        try:
            if case.get("mode", "journal") == "render":
                base = render_request(case, exe, port, root=root)["body"]
            else:
                base, _ = journal_request(case)
        except Exception as error:  # noqa: BLE001 - one case that cannot be built must not end the bench
            results[(case["name"], "unbuilt")] = collections.Counter({"unbuilt": 1})
            raw.append({"case": case["name"], "transform": None, "class": "unbuilt", "text": str(error)[:1500]})
            continue
        for transform in case.get("transforms") or ["as_is"]:
            body = json.loads(json.dumps(base))
            prompt = body["messages"][0]["content"]
            body["messages"][0]["content"] = apply_transforms(prompt, case, transform)
            if "no_inspect" in transform.split("+") and body.get("tools"):
                body["tools"] = [tool for tool in body["tools"]
                                 if (tool.get("function") or {}).get("name") != "inspect"]
            counts = collections.Counter()
            for sample in range(n):
                text, usage, seconds = send(profile, body)
                label = classify(text, case.get("check_event"))
                counts[label] += 1
                raw.append({"case": case["name"], "transform": transform, "sample": sample, "class": label,
                            "seconds": round(seconds, 2), "usage": usage, "text": text[:1500]})
            results[(case["name"], transform)] = counts
    if out:
        Path(out).write_text(json.dumps(raw, indent=1) + "\n")
    return results


def table(results):
    lines, totals = [], collections.defaultdict(collections.Counter)
    for (case, transform), counts in results.items():
        lines.append(f"{transform:30} {case:24} {dict(counts)}")
        totals[transform].update(counts)
    for transform, counts in totals.items():
        lines.append(f"{transform:30} {'TOTAL':24} {dict(counts)}")
    return "\n".join(lines)
