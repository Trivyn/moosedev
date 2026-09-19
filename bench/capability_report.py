"""Capability benchmark report: B2 (graph) vs B1-rag (flat RAG) vs B0 (cold).

BOTH metric families co-primary. ACCURACY: mean set F1 / recall / precision + pass-rate (Wilson 95%).
EFFICIENCY: median agent tokens, B2's internal NLQ tokens (added into its total so it's never free),
tool-calls, steps; B1-rag/B2 ratios as headline columns. Regrade-safe — reads runs_regraded.jsonl
(metrics recomputed by regrade.py from the immutable final_text), so editing the grader + re-running
needs no agent re-run.
"""
import collections
import functools
import json
import statistics

import config
from hybrid_ab_report import wilson

# Default = the PUBLIC corpus shipped in bench/release, so a clone reproduces the published
# capability table with no extra data. `--corpus trivyn-temporal` for the private one.
CORPUS = "codegraph"
CLASSES = ["set_completeness", "negation", "supersession", "multi_hop"]
HARD = {"set_completeness", "negation", "multi_hop"}  # where the categorical win is pre-registered


@functools.lru_cache(maxsize=None)
def _class_of(task_id: str) -> str:
    """Capability class, resolved from the task JSON — fallback for rows logged before run.py
    started carrying `capability_class` (regrade-safe: grouping never needs an agent re-run)."""
    try:
        return json.loads((config.corpus_tasks_path(CORPUS) / f"{task_id}.json").read_text()
                          ).get("capability_class", "?")
    except (FileNotFoundError, json.JSONDecodeError):
        return "?"


def klass(r: dict) -> str:
    return r.get("capability_class") or _class_of(r["task_id"])


def memory_tool_calls(r: dict) -> int:
    """Calls the row made to its arm's MEMORY server, as opposed to filesystem/shell tools.

    This is the diagnostic the whole matrix turns on. A tooluse cell that scores 0 with zero memory
    calls failed to FETCH; one that scores 0 having called is a failure to USE. Only the first is
    an argument for pushing knowledge instead of offering a tool."""
    # opencode prefixes an MCP tool with its server name (moosedev_sparql); codex exposes it bare
    # (sparql). Matching only the prefixed form silently reports every codex row as zero calls —
    # which is the exact claim this number is used to make, so both spellings must count.
    bare = {"get_relevant_context", "query", "sparql", "get_entity_dossier", "get_provenance",
            "export_graph", "suggest_mappings", "search", "recall", "ping"}
    counts = r.get("tool_counts") or {}
    return sum(n for name, n in counts.items()
               if name.lower() in bare
               or any(k in name.lower() for k in ("moosedev", "freetext", "mem0", "memory")))


def load(model: str = None, mode: str = None) -> list[dict]:
    d = config.corpus_runs_path(CORPUS)
    p = d / "runs_regraded.jsonl"
    if not p.exists():
        p = d / "runs.jsonl"
    rows = [json.loads(l) for l in p.read_text().splitlines() if l.strip()] if p.exists() else []
    # runs_regraded.jsonl is a point-in-time snapshot and this reader PREFERS it, so every row
    # appended to runs.jsonl after the last regrade is invisible here -- a verdict computed over
    # a campaign that silently is not in the data (the stale-snapshot failure of Lesson 08540ceb).
    raw = d / "runs.jsonl"
    if p.name == "runs_regraded.jsonl" and raw.exists():
        have = {r.get("run_id") for r in rows}
        missing = sum(1 for l in raw.read_text().splitlines()
                      if l.strip() and json.loads(l).get("run_id") not in have)
        if missing:
            print(f"[!! STALE REGRADE: {missing} row(s) in runs.jsonl are NOT in runs_regraded.jsonl "
                  f"and are invisible to this report. Run: python regrade.py --corpus {CORPUS}]")
    rows = [r for r in rows if r.get("task_type") == "capability_qa"]
    # A cell whose agent never received the prompt (provider not found, model not loaded, backend
    # down) is an INFRASTRUCTURE failure. run.py still writes a row, and its score is 0.0 — which
    # reads as "the model could not answer" when in truth no model ever ran. Pooling those into a
    # mean is how a broken rig gets published as a result, so they are excluded and counted aloud.
    broken = [r for r in rows if (r.get("tokens") or {}).get("agent_prompt", 0) == 0]
    if broken:
        print(f"[excluded {len(broken)} infrastructure failure(s): the agent never received the "
              f"prompt — {sorted({r.get('agent_model') or '?' for r in broken})}]")
    rows = [r for r in rows if r not in broken]
    # A timed-out cell scores like a wrong answer but means "did not finish in the
    # wall-clock budget". The budget is a fair control only while it is disclosed:
    # averaged in silently it reads as a capability gap when it is a throughput one.
    # A tooluse cell in a memory arm that never called its memory server is ambiguous in a way
    # that matters: either the model declined to fetch — which is the premise finding this report
    # exists to detect — or the server was unreachable and the cell measured nothing. They must not
    # be silently excluded (that would erase the finding) nor silently averaged (that would invent
    # one). Surfaced with their window so the transcripts can settle it: a contiguous block across
    # consecutive cells, with neighbouring models fine, is infrastructure, not behaviour.
    MEM_ARMS = {"B2", "B1-rag", "B1-mem0"}
    mute = [r for r in rows if r.get("arm") in MEM_ARMS and r.get("mode") == "tooluse"
            and not any(k in t for t in (r.get("tool_counts") or {})
                        for k in ("moosedev", "freetext", "mem0"))]
    if mute:
        span = f"{min(r['ts'] for r in mute)[11:19]}..{max(r['ts'] for r in mute)[11:19]}"
        who = ", ".join(sorted({(r.get("agent_model") or "?").split("/")[-1] for r in mute}))
        print(f"[{len(mute)} tooluse cell(s) in a memory arm recorded NO call to their memory "
              f"server ({who}, {span}). Counted in the means below. Check the transcripts before "
              f"reading them as 'the model would not fetch' — a contiguous run of them is the "
              f"server being unreachable, not the model declining.]")
    timed = [r for r in rows if r.get("timed_out")]
    if timed:
        per_model = collections.Counter((r.get("agent_model") or "?").split("/")[-1]
                                        for r in timed)
        total = collections.Counter((r.get("agent_model") or "?").split("/")[-1]
                                    for r in rows)
        note = ", ".join(f"{m} {per_model[m]}/{total[m]}" for m in sorted(per_model))
        print(f"[TIMED OUT at the cell budget, counted in the means below: {note} — "
              f"a timeout is 'did not finish', not 'could not answer'; read any gap "
              f"that tracks these counts as throughput, not capability]")
    # A cell that ended its turn WITHOUT answering is a different outcome from one that
    # answered wrongly, and both score F1 0.000. The distinction separates two failure modes
    # the means otherwise merge: Qwen3.5-9B answers every time and is often wrong (0/78 empty),
    # while Qwen3.5-122B mostly does not answer at all (3/4). Those call for opposite responses.
    def answered_nothing(row):
        # The flag is authoritative on rows written after it existed; older rows are judged
        # from final_text directly, so the disclosure covers the campaigns already on disk.
        # Either way a TRUNCATED cell does not count: a timeout or a non-zero exit has no
        # final text because the cell was killed, not because the model declined, and
        # conflating the two is the mistake this disclosure exists to prevent.
        if row.get("timed_out") or row.get("opencode_exit") not in (0, None):
            return False
        if "empty_answer" in row:
            return row["empty_answer"]
        return "final_text" in row and not (row.get("final_text") or "").strip()

    silent = [r for r in rows if answered_nothing(r)]
    if silent:
        per_model = collections.Counter((r.get("agent_model") or "?").split("/")[-1]
                                        for r in silent)
        total = collections.Counter((r.get("agent_model") or "?").split("/")[-1]
                                    for r in rows)
        note = ", ".join(f"{m} {per_model[m]}/{total[m]}" for m in sorted(per_model))
        print(f"[EMPTY ANSWER — the cell ended its turn with no final text, counted in the "
              f"means below: {note} — scored 0.0 for producing NOTHING, not for producing "
              f"something wrong; do not read these as a wrong-answer rate]")
    if model:  # never pool two agent models: model size is a variable under test, not noise
        rows = [r for r in rows if model in (r.get("agent_model") or "")]
    if mode:   # nor two delivery modes: tooluse vs oracle IS the question
        rows = [r for r in rows if r.get("mode") == mode]
    return rows


# --- stability gate (AD ab564b4a, pre-registered 2026-09-18) ------------------
# A tier qualifies for the floor only if it answers RELIABLY, so the gate is on the WORST
# repetition of each cell, never the mean. A cell scoring 0.50 three times has sd 0.00 and
# would pass any variance gate while being useless every time — the same defect AD 33b3dbac
# rejected in mean F1, rotated onto the variance axis.
#
# TAU is inherited from the floor study's sealed rule (AD c944806b, T = 0.8), fixed before
# this data existed, which is the evidence it was not fitted. On the measured tiers every
# PHI in [0.2, 0.9] gives the same verdict, so PHI is not what decides them.
STABILITY_TAU = 0.80   # a cell is reliable if its WORST valid rep scores at least this
STABILITY_PHI = 0.80   # a tier qualifies if at least this fraction of cells are reliable
STABILITY_N = 3        # valid repetitions required per cell


def infrastructure_fault(row: dict) -> bool:
    """True only when the agent was killed by a signal the RUNNER did not send.

    run.py's `_terminate` sends SIGTERM and SIGKILL, and only from the deadline timer or the
    no-progress watcher, so a negative exit with neither flag set means something outside the
    rig killed the process. Deliberately narrow, because everything else is a model outcome:
    a timeout means the model had its whole budget and did not finish, an abort means it
    looped, and a positive non-zero exit still carries the model's answer.

    On the 2026-09-18 campaign this excludes exactly one row of 79 (an opencode/Bun SIGTRAP
    at 318 s of a 2400 s budget). Excluding every non-zero exit instead would have thrown
    away three genuine 9B failures at 0.00 after 48-57 tool calls — flattering precisely the
    model the gate exists to reject.
    """
    code = row.get("opencode_exit")
    return (isinstance(code, int) and code < 0
            and not row.get("timed_out") and not row.get("aborted"))


def stability(rows: list[dict], n: int = STABILITY_N, tau: float = STABILITY_TAU,
              phi: float = STABILITY_PHI, since: str = None) -> None:
    """Report the pre-registered stability verdict per (model, arm, mode).

    `since` (an ISO timestamp prefix) names the campaign window. Pass it. Without it the
    last n valid reps of each cell are used, and where a campaign lost a rep to an
    infrastructure fault the n-th comes from an EARLIER campaign on a different build —
    silently pooling a pre-fix run into a post-fix verdict. That is not a hypothetical: it
    moved Qwen3.8-27B from 12/13 to 11/13 the first time this was run. The rows carry
    `moosedev_binary_sha256` but it is unpopulated, so the window is the operator's to state
    until it is filled in.
    """
    if since:
        rows = [r for r in rows if (r.get("ts") or "") >= since]
    groups = collections.defaultdict(lambda: collections.defaultdict(list))
    for r in rows:
        key = ((r.get("agent_model") or "?").split("/")[-1], r.get("arm"), r.get("mode"))
        groups[key][r.get("task_id")].append(r)

    print(f"\n=== STABILITY GATE (AD ab564b4a): cell reliable if min(F1) over {n} valid "
          f"reps >= {tau:.2f}; tier qualifies at >= {phi:.0%} of cells ===")
    for (model, arm, mode), cells in sorted(groups.items()):
        reliable, underpowered, excluded, worst = 0, [], 0, []
        for task, rs in sorted(cells.items()):
            valid = [r for r in rs if not infrastructure_fault(r)]
            excluded += len(rs) - len(valid)
            valid.sort(key=lambda r: r.get("ts") or "")
            used = valid[-n:]
            if len(used) < n:
                underpowered.append(f"{task} ({len(used)}/{n})")
            if not used:
                continue
            mn = min((r.get("metrics") or {}).get("f1", 0.0) for r in used)
            worst.append((mn, task))
            if mn >= tau:
                reliable += 1
        if not worst:
            continue
        total = len(cells)
        frac = reliable / total
        span_rows = [r for rs in cells.values() for r in rs if r.get("ts")]
        span = (f"{min(r['ts'] for r in span_rows)[:16]}..{max(r['ts'] for r in span_rows)[11:16]}"
                if span_rows else "?")
        verdict = ("QUALIFIES" if frac >= phi and not underpowered
                   else "UNDER-POWERED" if underpowered and frac >= phi else "fails")
        print(f"\n  {model}  {arm}/{mode}  [{span}]")
        print(f"    reliable cells {reliable}/{total} = {frac:.2f}  ->  {verdict}")
        if not since and span_rows:
            lo, hi = min(r["ts"] for r in span_rows), max(r["ts"] for r in span_rows)
            if lo[:10] != hi[:10]:
                print(f"    !! reps span {lo[:10]}..{hi[:10]} and may mix BUILDS — pass "
                      f"--since to name one campaign; this verdict is not trustworthy")
        if excluded:
            print(f"    {excluded} rep(s) excluded as infrastructure faults (killed by an "
                  f"unsent signal)")
        if underpowered:
            print(f"    UNDER-POWERED, re-run before the verdict counts: "
                  f"{', '.join(underpowered)}")
        for mn, task in sorted(worst)[:4]:
            if mn < tau:
                print(f"    worst cell  min={mn:.2f}  {task}")


def _agent_tok(r):
    t = r["tokens"]
    return t["agent_prompt"] + t["agent_completion"]


def _nlq_tok(r):
    t = r["tokens"]
    return t["internal_prompt"] + t["internal_completion"]


def _med(xs):
    return statistics.median(xs) if xs else 0.0


def agg(rows: list[dict]) -> dict | None:
    n = len(rows)
    if not n:
        return None
    m = lambda r, k: (r.get("metrics") or {}).get(k, 0.0)
    npass = sum(1 for r in rows if r.get("passed"))
    return {
        "n": n,
        "f1": sum(r.get("score", 0.0) for r in rows) / n,
        "recall": sum(m(r, "recall") for r in rows) / n,
        "precision": sum(m(r, "precision") for r in rows) / n,
        "pass": npass, "wilson": wilson(npass, n),
        "agent_tok": _med([_agent_tok(r) for r in rows]),
        "nlq_tok": _med([_nlq_tok(r) for r in rows]),
        "tot_tok": _med([_agent_tok(r) + _nlq_tok(r) for r in rows]),
        "tools": _med([r.get("n_tool_calls", 0) for r in rows]),
        "steps": _med([r.get("agent_steps", 0) for r in rows]),
    }


def premise(rows: list[dict]) -> None:
    """Does the harness's premise hold — do small models fail to CALL the memory tooling?

    Per model and arm: tooluse F1 beside oracle F1, with the median memory-tool calls the tooluse
    cells actually made. The premise is supported where tooluse F1 is low, memory calls are ~0, and
    oracle F1 is materially higher — the knowledge was usable, the model just never fetched it.
    Where tooluse already calls and still fails, pushing the same knowledge will not rescue it."""
    models = sorted({r.get("agent_model") or "?" for r in rows})
    print("\n=== PREMISE: fetch failure or use failure? ===")
    print("tooluse F1 vs oracle F1, with the memory-tool calls the TOOLUSE cells made.")
    print("premise supported = low tooluse F1 + ~0 memory calls + materially higher oracle F1.\n")
    print(f"{'model':<26}{'class':<17}{'arm':<8}{'tool_F1':>8}{'orac_F1':>8}{'Δ':>7}"
          f"{'mem_calls':>10}{'n':>4}")
    for model in models:
        mrows = [r for r in rows if (r.get("agent_model") or "?") == model]
        for c in [k for k in CLASSES if any(klass(r) == k for r in mrows)]:
            for arm in [a for a in config.ARMS if any(r["arm"] == a for r in mrows)]:
                sel = [r for r in mrows if klass(r) == c and r["arm"] == arm]
                tl = [r for r in sel if r.get("mode") == "tooluse"]
                orc = [r for r in sel if r.get("mode") == "oracle"]
                if not tl and not orc:
                    continue
                f1 = lambda xs: (sum(x.get("score", 0.0) for x in xs) / len(xs)) if xs else float("nan")
                calls = _med([memory_tool_calls(r) for r in tl]) if tl else float("nan")
                delta = f1(orc) - f1(tl) if (tl and orc) else float("nan")
                print(f"{model.split('/')[-1]:<26}{c:<17}{arm:<8}{f1(tl):>8.2f}{f1(orc):>8.2f}"
                      f"{delta:>+7.2f}{calls:>10.1f}{len(sel):>4}")
        print()


def main() -> None:
    global CORPUS
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default=CORPUS)
    ap.add_argument("--model", help="restrict to one agent model (substring)")
    ap.add_argument("--mode", choices=["tooluse", "oracle"], help="restrict to one delivery mode")
    ap.add_argument("--since", metavar="ISO_TS",
                    help="only count reps at or after this timestamp — names the campaign "
                         "window so the verdict cannot pool two builds")
    ap.add_argument("--stability", action="store_true",
                    help="pre-registered stability verdict per tier (AD ab564b4a)")
    ap.add_argument("--premise", action="store_true",
                    help="tooluse-vs-oracle summary per model: did it fail to fetch, or to use?")
    args = ap.parse_args()
    CORPUS = args.corpus
    if args.premise:
        premise(load())
        return
    if args.stability:
        stability(load(args.model, args.mode), since=args.since)
        return
    rows = load(args.model, args.mode)
    if args.model or args.mode:
        print(f"\n[slice: model={args.model or 'all'} mode={args.mode or 'all'}]")
    if not rows:
        print("no capability_qa rows yet (run the matrix first)")
        return
    arms = [a for a in config.ARMS if any(r["arm"] == a for r in rows)]
    by = collections.defaultdict(lambda: collections.defaultdict(list))
    for r in rows:
        by[klass(r)][r["arm"]].append(r)
    classes = [c for c in CLASSES if c in by] + [c for c in by if c not in CLASSES]

    print(f"\n=== Capability benchmark: structure (B2) vs flat RAG (B1-rag) vs cold (B0), {CORPUS} ===")
    print("ACCURACY: mean set F1 / recall / precision + pass-rate [Wilson 95%].")
    print("EFFICIENCY: median tokens (agent | nlq | total), tool-calls, steps. B2 total INCLUDES its NLQ cost.\n")
    print(f"{'class':<17}{'arm':<8}{'n':>3}{'F1':>6}{'rec':>6}{'prc':>6}{'  pass [Wilson]':<18}"
          f"{'ag_tok':>9}{'nlq':>7}{'tot_tok':>9}{'tl':>4}{'st':>4}")
    for c in classes:
        for arm in arms:
            a = agg(by[c].get(arm, []))
            if not a:
                continue
            wl, wu = a["wilson"]
            pas = f"{a['pass']}/{a['n']} [{wl:.2f},{wu:.2f}]"
            print(f"{c:<17}{arm:<8}{a['n']:>3}{a['f1']:>6.2f}{a['recall']:>6.2f}{a['precision']:>6.2f}"
                  f"  {pas:<16}{a['agent_tok']:>9.0f}{a['nlq_tok']:>7.0f}{a['tot_tok']:>9.0f}"
                  f"{a['tools']:>4.0f}{a['steps']:>4.0f}")
        print()

    # Headline: per-class B1-rag vs B2 — accuracy gap AND efficiency ratio, side by side.
    print("HEADLINE — B1-rag vs B2 (accuracy gap + B1's effort tax):")
    for c in classes:
        b1, b2 = agg(by[c].get("B1-rag", [])), agg(by[c].get("B2", []))
        if not (b1 and b2):
            continue
        tok_r = b1["tot_tok"] / b2["tot_tok"] if b2["tot_tok"] else float("inf")
        tool_r = b1["tools"] / b2["tools"] if b2["tools"] else float("inf")
        print(f"  {c:<17} F1 {b1['f1']:.2f}→{b2['f1']:.2f} (Δ{b2['f1'] - b1['f1']:+.2f}) | "
              f"B1 spends {tok_r:>4.1f}× B2 tokens, {tool_r:>4.1f}× tool-calls")

    # Pre-registered verdict (accuracy + efficiency), pooled over the hard categorical classes.
    def pool(arm):
        return [r for r in rows if r["arm"] == arm and klass(r) in HARD]
    b2h, b1h = pool("B2"), pool("B1-rag")
    if b2h and b1h:
        b2f1 = sum(r.get("score", 0) for r in b2h) / len(b2h)
        b1f1 = sum(r.get("score", 0) for r in b1h) / len(b1h)
        b2tok = _med([_agent_tok(r) + _nlq_tok(r) for r in b2h])
        b1tok = _med([_agent_tok(r) + _nlq_tok(r) for r in b1h])
        ratio = b1tok / b2tok if b2tok else float("inf")
        print("\nPRE-REGISTERED VERDICT (pooled set/negation/multi-hop):")
        acc = "PASS" if b2f1 >= 0.95 and b1f1 <= 0.6 else "WEAK/REVIEW"
        eff = "PASS" if b2tok <= 0.2 * b1tok else "REVIEW"
        print(f"  accuracy : B2 F1={b2f1:.2f} (≥0.95?), B1-rag F1={b1f1:.2f} (≤0.6 degraded?) → {acc}")
        print(f"  efficiency: B2 tot_tok={b2tok:.0f} vs B1-rag={b1tok:.0f} (B2 ≤20% of B1? {ratio:.1f}× tax) → {eff}")


if __name__ == "__main__":
    main()
