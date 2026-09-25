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


def report(task):
    """Reuse over action prompts: overall share, uncached bytes per step, divergence sections."""
    pairs, interrupted = action_pairs(task)
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


def report_file(path):
    return report(json.loads(Path(path).read_text()))
