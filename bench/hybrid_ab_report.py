"""Instrument B report: B2 hybrid (floor 0.5) vs pure-BM25F (floor 0.99), end-to-end.

Reads the floor-tagged runs_{bm25f,hybrid}.jsonl produced by run_hybrid_ab.sh and prints, per
(task, mode): n, currency rate (coverage==1 — Lesson 9fc21dc0; the stale flag is a hint, not a
gate), mean coverage, mean score, and mean agent tokens, with the hybrid−bm25f delta. Flags cells
where hybrid changes the currency outcome (the end-to-end payoff of the dense seed). No grading
here — uses the metrics run.py already logged (regrade-safe: edit this and re-run, no agent re-run).
"""
import collections
import functools
import json
import math
from pathlib import Path

import config

OUT = Path.home() / "code" / "moosedev_benches" / "trivyn-temporal" / "runs"
DRIFT = {"merge_ns_currency", "objprop_validate_currency", "nlq_deadline_currency", "align_confidence_currency"}
# A cell that reads external SOURCE to answer balloons its prompt; treat that as a memory MISS (the
# agent had to LEAVE memory). The cutoff is MODE-AWARE: an ORACLE cell gets one pushed context
# (clean ~50k), so 120k cleanly flags a source-escape (250k-900k). A TOOLUSE cell legitimately makes
# 3-4 MCP calls, and codex re-sends the whole transcript each turn, so honest multi-call cells reach
# 120-190k WITHOUT reading source — only nt<=2 cells at 363-709k actually shelled out. The escape
# proxy is ALSO gated on materialize_tree (see `materializes_tree`): a pure-memory task has no source
# tree, so a token balloon there is reasoning thrash, never an escape.
ESCAPE_TOK = {"oracle": 120_000, "tooluse": 250_000}
ESCAPE_TOK_DEFAULT = 250_000


@functools.lru_cache(maxsize=None)
def materializes_tree(task_id: str) -> bool:
    """Does this task lay down a source tree the agent could read? context_qa/constraint_code tasks
    materialize the (docs-stripped) tree BY DEFAULT (run.py), unless the task opts out with
    materialize_tree:false. A pure-memory task (false) has NO source to escape to, so the
    token-balloon escape proxy must NOT fire for it (high tokens there are internal reasoning thrash,
    not a memory miss — e.g. the 339k-token geospatial cell that still answered correctly)."""
    try:
        d = json.loads((config.corpus_tasks_path("trivyn-temporal") / f"{task_id}.json").read_text())
    except FileNotFoundError:
        return True  # unknown task: assume a tree exists (conservative — keep the escape guard on)
    return d.get("materialize_tree", True)


def load(label: str) -> list[dict]:
    p = OUT / f"runs_{label}.jsonl"
    return [json.loads(l) for l in p.read_text().splitlines() if l.strip()] if p.exists() else []


def strict_current(r: dict) -> bool:
    # Coverage of the current-state markers IS the currency verdict (Lesson 9fc21dc0). The `stale`
    # substring flag is NOT a gate: it false-positives on a correct answer that NARRATES the old
    # state to deny it (e.g. "no `debug_assert!` anymore") — backticks/markdown defeat the
    # negation window — and on stale markers that overlap the cited decision's OWN title. Hint only.
    return (r.get("metrics") or {}).get("coverage") == 1.0


def escaped(r: dict) -> bool:
    # An escape = the agent LEFT memory to read source. Only possible if a source tree was laid down:
    # a materialize_tree:false (pure-memory) cell has nothing to escape to, so a token balloon there
    # is internal reasoning thrash, not an escape — never count it as a memory miss.
    if not materializes_tree(r["task_id"]):
        return False
    t = r["tokens"]
    thr = ESCAPE_TOK.get(r.get("mode"), ESCAPE_TOK_DEFAULT)
    return (t["agent_prompt"] + t["agent_completion"]) > thr


def memory_answered(r: dict) -> bool:
    """Answered the current state correctly FROM MEMORY (not by escaping to read source)."""
    return strict_current(r) and not escaped(r)


def agg(rows: list[dict]) -> dict:
    n = len(rows)
    if not n:
        return {"n": 0}
    cur = sum(strict_current(r) for r in rows)
    mem = sum(memory_answered(r) for r in rows)
    esc = sum(escaped(r) for r in rows)
    cov = sum((r.get("metrics") or {}).get("coverage", 0.0) for r in rows) / n
    tok = sum(r["tokens"]["agent_prompt"] + r["tokens"]["agent_completion"] for r in rows) / n
    return {"n": n, "cur_pct": 100 * cur / n, "mem": mem, "mem_pct": 100 * mem / n,
            "esc": esc, "cov": cov, "tok": tok}


def wilson(k: int, n: int) -> tuple[float, float]:
    """95% Wilson interval for a k/n rate — honest small-N error bars."""
    if n == 0:
        return (0.0, 0.0)
    z = 1.96
    p = k / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    h = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return ((c - h) / d, (c + h) / d)


def main() -> None:
    bm, hy = load("bm25f"), load("hybrid")
    if not bm and not hy:
        print("no tagged rows yet (runs_bm25f.jsonl / runs_hybrid.jsonl empty)")
        return
    key = lambda r: (r["task_id"], r["mode"])
    bm_by, hy_by = collections.defaultdict(list), collections.defaultdict(list)
    for r in bm:
        bm_by[key(r)].append(r)
    for r in hy:
        hy_by[key(r)].append(r)

    print("\n=== Instrument B: end-to-end B2 hybrid(0.5) vs BM25F(0.99), trivyn-temporal, codex/gpt-5.4-mini ===")
    print("currency verdict = coverage of current-state markers (Lesson 9fc21dc0); the stale flag is a hint, not a gate.")
    print("mem% = current-by-coverage AND not an escape-to-source.")
    print(f"esc = read external source (tokens > {ESCAPE_TOK['oracle']//1000}k oracle / {ESCAPE_TOK['tooluse']//1000}k tooluse), gated on materialize_tree — memory MISS.")
    print("cur% = raw current-by-coverage (any means, incl. escape). Δ = hybrid − bm25f mem%.\n")
    hdr = (f"{'task':<26}{'mode':<8}{'n':>6}{'mem%b':>7}{'mem%h':>7}{'Δmem':>7}"
           f"{'esc b/h':>9}{'cur b/h':>9}{'tok b':>8}{'tok h':>8}")
    for group, title in ((DRIFT, "DRIFT (vocabulary-drift — the signal)"),
                         (None, "CURRENCY (self-announcing — regression floor)")):
        tasks = sorted({t for (t, _m) in set(bm_by) | set(hy_by)
                        if (t in DRIFT) == (group is DRIFT)})
        if not tasks:
            continue
        print(f"-- {title} --")
        print(hdr)
        for t in tasks:
            for mode in ("oracle", "tooluse"):
                b, h = agg(bm_by.get((t, mode), [])), agg(hy_by.get((t, mode), []))
                if not b["n"] and not h["n"]:
                    continue
                dmem = h.get("mem_pct", 0) - b.get("mem_pct", 0)
                nb = f"{b['n']}/{h['n']}"
                esc = f"{b.get('esc', 0)}/{h.get('esc', 0)}"
                cur = f"{b.get('cur_pct', 0):.0f}/{h.get('cur_pct', 0):.0f}"
                print(f"{t:<26}{mode:<8}{nb:>6}"
                      f"{b.get('mem_pct', 0):>6.0f}%{h.get('mem_pct', 0):>6.0f}%{dmem:>+7.0f}"
                      f"{esc:>9}{cur:>9}{b.get('tok', 0):>8.0f}{h.get('tok', 0):>8.0f}")
        print()

    # Headline: pooled memory-answered rate on DRIFT tasks (where the dense seed can move the needle).
    for mode in ("oracle", "tooluse"):
        b = [r for r in bm if r["task_id"] in DRIFT and r["mode"] == mode]
        h = [r for r in hy if r["task_id"] in DRIFT and r["mode"] == mode]
        if not b and not h:
            continue
        bk, hk = sum(map(memory_answered, b)), sum(map(memory_answered, h))
        bl, bu = wilson(bk, len(b))
        hl, hu = wilson(hk, len(h))
        print(f"POOLED drift/{mode} (mem-answered): bm25f {bk}/{len(b)} [{bl:.2f},{bu:.2f}]  "
              f"hybrid {hk}/{len(h)} [{hl:.2f},{hu:.2f}]")


if __name__ == "__main__":
    main()
