"""Prefix-cache reuse of a harness task journal. Offline; never calls a model.

A local server such as LM Studio reuses the KV cache of the previous request
only up to the first byte where the new prompt differs. This report measures
that byte for each consecutive pair of action prompts in a task journal, so a
prompt-order change can be judged from a run's evidence rather than from
timings alone.
"""
import json
from pathlib import Path

PURPOSE = "harness_action"
# Section headers in the action prompt. The section a pair first differs in is
# the last header at or before that byte, whatever order the prompt uses.
SECTIONS = (
    ("conversation", "Recent conversation ("),
    ("repository paths", "Repository paths ("),
    ("role and guidance", "You are the coding sensor"),
    ("project rules", "Project rules ("),
    ("action meanings", "\nAction meanings:"),
    ("objective", "\nConfigured model ID:"),
    ("accepted knowledge", "Current accepted knowledge:"),
    ("entity dossiers", "\nEntity dossiers:"),
    ("source", "Current source, refreshed"),
    ("harness state", "\nCurrent harness state"),
    ("check results", "Required check results"),
    ("observations", "Recent observations ("),
    ("last result", "\nLast result:\n"),
)


def action_pairs(task):
    """Each action prompt with the request sent just before it, when that request
    went to the same endpoint and model. The server's cache holds only the last
    prompt it processed, so a capture note or a model switch in between is what
    the next action is compared against, not the previous action."""
    pairs, interrupted, previous = [], 0, None
    for request in task.get("model_requests", []):
        prompt = request.get("prompt")
        if not isinstance(prompt, str):
            continue
        server = (request.get("endpoint"), request.get("model"))
        if request.get("purpose") == PURPOSE and previous is not None:
            if previous[0] == server:
                pairs.append((previous[1], prompt))
            else:
                interrupted += 1
        previous = (server, prompt)
    return pairs, interrupted


def common_prefix(left, right):
    """Length in bytes of the longest common prefix of two byte strings."""
    limit = min(len(left), len(right))
    index = 0
    while index < limit and left[index] == right[index]:
        index += 1
    return index


def section_at(prompt, position):
    """Name of the section of `prompt` (bytes) holding byte `position`."""
    best, best_start = "preamble", -1
    for name, marker in SECTIONS:
        start = prompt.find(marker.encode())
        if best_start < start <= position:
            best, best_start = name, start
    return best


def line_start(prompt, marker):
    """The first place `marker` begins a line of `prompt`, or -1. Full source
    is one JSON line with no raw newlines, so a marker quoted inside a file
    never counts as a section boundary."""
    at = prompt.find(marker)
    while at > 0 and not marker.startswith("\n") and prompt[at - 1] != "\n":
        at = prompt.find(marker, at + 1)
    return at


def sections(prompt):
    """`prompt` as (name, text) pieces in the order they appear; joined, they
    are the prompt again. Text before the first marker is the preamble."""
    found = sorted(
        (at, name)
        for name, marker in SECTIONS
        if (at := line_start(prompt, marker)) >= 0
    )
    if not found or found[0][0] > 0:
        found.insert(0, (0, "preamble"))
    return [
        (name, prompt[start:found[index + 1][0] if index + 1 < len(found) else len(prompt)])
        for index, (start, name) in enumerate(found)
    ]


def reorder(prompt, move, after):
    """`prompt` with section `move` placed right after section `after`: what
    the harness would send with that order. Unchanged when either is absent."""
    parts = sections(prompt)
    names = [name for name, _ in parts]
    if move not in names or after not in names or move == after:
        return prompt
    moved = [part for part in parts if part[0] == move]
    out = []
    for part in parts:
        if part[0] == move:
            continue
        out.append(part)
        if part[0] == after:
            out.extend(moved)
    return "".join(text for _, text in out)


def churn(task):
    """Per section, how many consecutive action pairs changed its bytes; and
    for the project rules, a few examples of what changed, since a change at
    the top of the prompt costs the whole prefix."""
    pairs, _ = action_pairs(task)
    changed, examples = {}, []
    for index, (previous, current) in enumerate(pairs):
        before, after = dict(sections(previous)), dict(sections(current))
        for name in set(before) | set(after):
            if before.get(name) != after.get(name):
                changed[name] = changed.get(name, 0) + 1
        if before.get("project rules") != after.get("project rules") and len(examples) < 5:
            old = (before.get("project rules") or "").splitlines()
            new = (after.get("project rules") or "").splitlines()
            first = next(
                (line for line, (a, b) in enumerate(zip(old, new)) if a != b),
                min(len(old), len(new)),
            )
            examples.append({
                "pair": index,
                "bytes": [len((before.get("project rules") or "").encode()),
                          len((after.get("project rules") or "").encode())],
                "lines": [len(old), len(new)],
                "first_changed_line": first,
                "before": (old[first] if first < len(old) else "")[:160],
                "after": (new[first] if first < len(new) else "")[:160],
            })
    return {
        "changed_pairs": dict(sorted(changed.items(), key=lambda item: -item[1])),
        "rules_examples": examples,
    }


def latency(task, receipts):
    """How step time splits between prefill and generation, from the usage
    receipts: elapsed seconds fitted to a + b * uncached KB + c * completion
    tokens, over action requests in order. The server does not report cached
    tokens, so uncached bytes against the previous prompt stand in for the
    prefill it had to do. None when the receipts do not line up."""
    uncached, previous = [], None
    for request in task.get("model_requests", []):
        prompt = request.get("prompt")
        if not isinstance(prompt, str):
            continue
        server = (request.get("endpoint"), request.get("model"))
        current = prompt.encode()
        if request.get("purpose") == PURPOSE:
            shared = (
                common_prefix(previous[1], current)
                if previous is not None and previous[0] == server
                else 0
            )
            uncached.append(len(current) - shared)
        previous = (server, current)
    # One completed receipt per journaled request: an attempt that failed in
    # transport has a receipt of its own but no journal entry.
    actions = sorted(
        (r for r in receipts
         if r.get("context", {}).get("purpose") == PURPOSE
         and r.get("status") == "completed" and r.get("elapsed_ms")),
        key=lambda r: r["started_at"],
    )
    if len(actions) != len(uncached):
        return None
    # A step without a completion count is left out, not counted as zero.
    rows = [
        (1.0, bytes_ / 1000, float(r["tokens"]["completion_tokens"]), r["elapsed_ms"] / 1000)
        for bytes_, r in zip(uncached, actions)
        if (r.get("tokens") or {}).get("completion_tokens") is not None
    ]
    if len(rows) < 4:
        return None
    coefficients = least_squares([row[:3] for row in rows], [row[3] for row in rows])
    if coefficients is None:
        return None
    fixed, per_kb, per_token = coefficients
    elapsed = sum(row[3] for row in rows)
    return {
        "steps": len(rows),
        "seconds_total": round(elapsed),
        "seconds_per_uncached_kb": round(per_kb, 3),
        "seconds_per_completion_token": round(per_token, 4),
        "seconds_fixed_per_step": round(fixed, 2),
        "prefill_share_percent": round(100 * per_kb * sum(row[1] for row in rows) / elapsed, 1),
        "generation_share_percent": round(100 * per_token * sum(row[2] for row in rows) / elapsed, 1),
        "median_uncached_kb": round(sorted(row[1] for row in rows)[len(rows) // 2], 1),
        "median_completion_tokens": sorted(row[2] for row in rows)[len(rows) // 2],
    }


def least_squares(xs, ys):
    """Coefficients minimising squared error of ys against rows xs, by the
    normal equations; None when they are singular."""
    n = len(xs[0])
    a = [[sum(x[i] * x[j] for x in xs) for j in range(n)] + [sum(x[i] * y for x, y in zip(xs, ys))]
         for i in range(n)]
    for column in range(n):
        pivot = max(range(column, n), key=lambda row: abs(a[row][column]))
        if abs(a[pivot][column]) < 1e-12:
            return None
        a[column], a[pivot] = a[pivot], a[column]
        for row in range(n):
            if row != column:
                factor = a[row][column] / a[column][column]
                a[row] = [value - factor * base for value, base in zip(a[row], a[column])]
    return [a[i][n] / a[i][i] for i in range(n)]


def report(task, move=None):
    """Reuse over action prompts: overall share, uncached bytes per step, divergence sections.
    With `move` = (section, after), every prompt is first reordered that way:
    the reuse the harness would get with that section order."""
    pairs, interrupted = action_pairs(task)
    if move is not None:
        pairs = [(reorder(a, *move), reorder(b, *move)) for a, b in pairs]
    reused = total = 0
    diverged = {}
    for previous, current in pairs:
        previous, current = previous.encode(), current.encode()
        shared = common_prefix(previous, current)
        reused += shared
        total += len(current)
        section = section_at(current, shared)
        diverged[section] = diverged.get(section, 0) + 1
    return {
        "pairs": len(pairs),
        "after_other_server": interrupted,
        "reuse_percent": round(100 * reused / total, 1) if total else None,
        "uncached_bytes_per_step": round((total - reused) / len(pairs)) if pairs else None,
        "mean_prompt_bytes": round(total / len(pairs)) if pairs else None,
        "first_divergence": dict(sorted(diverged.items(), key=lambda item: -item[1])),
    }


def report_file(path, move=None, details=False):
    task = json.loads(Path(path).read_text())
    result = report(task, move)
    if details:
        result["churn"] = churn(task)
        usage = Path(str(path).removesuffix(".json") + ".usage.jsonl")
        if usage.exists():
            receipts = {}
            for line in usage.read_text().splitlines():
                if line.strip():
                    receipt = json.loads(line)
                    receipts[receipt["id"]] = receipt
            result["latency"] = latency(task, list(receipts.values()))
    return result
