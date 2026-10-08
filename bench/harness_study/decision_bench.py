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


def _filter_schema(prompt, keep):
    """The prompt with its action schema (after the last schema marker, in
    the stable head or at the end) holding only the actions `keep` accepts;
    a conversational schema's nested `action` is filtered too."""
    head, marker, schema = prompt.rpartition(SCHEMA_MARKER)
    if not marker:
        return prompt
    data, end = json.JSONDecoder().raw_decode(schema)
    arms = data if "oneOf" in data else data["properties"]["action"]
    arms["oneOf"] = [arm for arm in arms["oneOf"] if keep(arm["properties"]["action"].get("const"))]
    return head + marker + json.dumps(data, separators=(",", ":")) + schema[end:]


def remove_inspect(prompt, case):
    """No inspect action: out of the schema and the action lists, wherever
    they are (a tools request's inspect tool is removed from the body by
    `run`). Under the stable head the action lists follow the schema."""
    prompt = _filter_schema(prompt, lambda name: name != "inspect")
    return prompt.replace("inspect(event,offset), ", "").replace("read, search, inspect, ", "read, search, ")


# Rung 3 (the harness's MOOSEDEV_HARNESS_RUNG3): after a looking loop's
# recovery while a required check fails, the next step offers only edits.
RUNG3_ACTIONS = ("replace", "write", "replan", "apply_fix")
# One whole allowed-actions sentence: names, each with an optional
# parenthetical (which may hold a path with dots), up to the closing period.
ALLOWED_NOW = re.compile(r"Allowed actions now: (?:[a-z_]+(?: \([^)]*\))?, )*[a-z_]+(?: \([^)]*\))?\.")
STATE_HEADER = "\nCurrent harness state"
OBSERVATIONS_HEADER = "Recent observations (complete outputs remain in journal events"


def rung3_line(prompt, case):
    """Rung 3's `line` variant: the allowed-actions sentence lists only the
    edits; the schema (the cached head) is unchanged."""
    def line(match):
        offered = [name for name in RUNG3_ACTIONS if name != "apply_fix" or "apply_fix" in match.group(0)]
        return (f"Allowed actions now: {', '.join(offered)}. Looking is not offered this step: a required check "
                "fails against the current source, so the next step is the change it needs.")
    # The live offer is in the current harness state, before the
    # observations (which can echo an older line).
    state = prompt.rfind(STATE_HEADER)
    end = prompt.find(OBSERVATIONS_HEADER, max(state, 0))
    window = (state, end if end >= 0 else len(prompt)) if state >= 0 else (0, len(prompt))
    matches = [m for m in ALLOWED_NOW.finditer(prompt) if window[0] <= m.start() < window[1]]
    if not matches:
        return prompt
    last = matches[-1]
    return prompt[:last.start()] + line(last) + prompt[last.end():]


def rung3_schema(prompt, case):
    """Rung 3's `schema` variant: the line, and the schema too."""
    return _filter_schema(rung3_line(prompt, case), lambda name: name in RUNG3_ACTIONS)


def rung2_fresh(prompt, case):
    """Rung 2: a fresh prompt without the looping history: the recent
    observations list emptied and any recent conversation dropped; the Last
    result is kept."""
    start = prompt.rfind(OBSERVATIONS_HEADER)
    if start >= 0:
        open_list = prompt.index("[", start)
        _, length = json.JSONDecoder().raw_decode(prompt[open_list:])
        prompt = prompt[:open_list] + "[]" + prompt[open_list + length:]
    conversation = prompt.rfind("\nRecent conversation (historical context")
    if conversation >= 0:
        end = prompt.find("\nCurrent human guidance:", conversation)
        if end > conversation:
            prompt = prompt[:conversation] + prompt[end + 1:]
    return prompt


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


PAGED = re.compile(r"\[bytes \d+\.\.\d+ of (\d+) not shown here; inspect\((\d+), \d+\) pages them\]")
SOURCE_HELD = re.compile(r"^(?:Read|Applied edit|Served read of a file shown in full:|Served outlined read:|Served read outside scope:)"
                         r" ?([\w./-]+?)[: (\n]")


def _source_full(case):
    """The files the journaled request showed in full under Source."""
    if "source_full" not in case:
        entry = json.loads(Path(case["journal"]).read_text())["model_requests"][case["request"]]
        case["source_full"] = set(entry.get("source_full") or [])
    return case["source_full"]


def pointer_line(event, text, size, source_full, plan_whole):
    """How the pointer renderer shows a recent event that does not fit whole:
    where its whole text is, never half of it."""
    first = text.split("\n", 1)[0][:160]
    held = SOURCE_HELD.match(text)
    action = None
    if text.startswith("Model action: "):
        try:
            action = json.loads(text[len("Model action: "):].split("\n", 1)[0])
        except json.JSONDecodeError:
            action = None
    file = held.group(1) if held else (action or {}).get("file")
    if file and file in source_full and (held or (action or {}).get("action") in ("edit", "replace", "write")):
        label = first if held else f"Model action: {action['action']} {file}"
        return f"Event {event}: {label} - the file's current text is under Source."
    if plan_whole and ((action or {}).get("action") in ("plan", "replan") or text.startswith("Proposed plan:")):
        return f"Event {event}: your plan - shown above as the plan."
    return f"Event {event}: {first} ({size} bytes; inspect({event}, 0) shows it whole)"


def pointer_observations(prompt, case):
    """Recent observations without half copies: each cut preview becomes a
    pointer to where the whole text is (Source, the plan, or the journal)."""
    start = prompt.find("Recent observations")
    if start < 0:
        return prompt
    end = prompt.index("Last result:", start)
    section = prompt[start:end]
    open_list = section.index("[")
    entries, length = json.JSONDecoder().raw_decode(section[open_list:])
    source_full = _source_full(case)
    plan_whole = "[Plan shown in part" not in prompt and "[Plan cut" not in prompt
    rewritten = []
    for entry in entries:
        paged = PAGED.search(entry)
        if not paged:
            rewritten.append(entry)
            continue
        event, size = int(paged.group(2)), int(paged.group(1))
        text = entry.split(": ", 1)[1]
        rewritten.append(pointer_line(event, text, size, source_full, plan_whole))
    return prompt[:start] + section[:open_list] + json.dumps(rewritten) + section[open_list + length:] + prompt[end:]


def pointer_checks(prompt, case):
    """Check output previews without half copies: a cut preview becomes one
    line naming the event that holds the whole output."""
    start = prompt.find("Check output previews:")
    if start < 0:
        return prompt
    end = prompt.index("Last result:", start)
    body = prompt[start + len("Check output previews:"):end]
    parts = re.split(r"(?m)^(?=Check \d+: )", body)
    out = []
    for part in parts:
        paged = PAGED.search(part)
        if part.startswith("Check ") and paged:
            label = part.split(":", 1)[0]
            out.append(f"{label}: ({paged.group(1)} bytes; inspect({paged.group(2)}, 0) shows it whole)\n")
        else:
            out.append(part)
    return prompt[:start] + "Check output previews:" + "".join(out) + prompt[end:]


TRANSFORMS = {
    "as_is": lambda prompt, case: prompt,
    "no_shortened_copies": drop_shortened_copies,
    "no_shortened_copies_no_hint": lambda prompt, case: drop_shortened_copies(prompt, case, hint=False),
    "no_inspect": remove_inspect,
    "no_focus": drop_focus_block,
    "pointer_observations": pointer_observations,
    "pointer_checks": pointer_checks,
    "rung3_line": rung3_line,
    "rung3_schema": rung3_schema,
    "rung2_fresh": rung2_fresh,
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
    # Short on purpose: the daemon's socket lives under the restored project,
    # and macOS's per-user temp directory is too deep for a Unix socket path.
    base = Path(os.environ.get("DECISION_BENCH_SCRATCH", "/tmp"))
    base.mkdir(parents=True, exist_ok=True)
    scratch = Path(tempfile.mkdtemp(prefix="db-", dir=base)) / "project"
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
            parts = transform.split("+")
            if body.get("tools") and ("no_inspect" in parts or "rung3_schema" in parts):
                def offered(name):
                    if "rung3_schema" in parts and name not in RUNG3_ACTIONS:
                        return False
                    return not ("no_inspect" in parts and name == "inspect")
                body["tools"] = [tool for tool in body["tools"] if offered((tool.get("function") or {}).get("name"))]
            counts = collections.Counter()
            for sample in range(n):
                text, usage, seconds = send(profile, body)
                label = classify(text, case.get("check_event"))
                counts[label] += 1
                raw.append({"case": case["name"], "transform": transform, "sample": sample, "class": label,
                            "seconds": round(seconds, 2), "usage": usage, "text": text[:20000]})
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
