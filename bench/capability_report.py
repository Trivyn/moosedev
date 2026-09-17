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
    if model:  # never pool two agent models: model size is a variable under test, not noise
        rows = [r for r in rows if model in (r.get("agent_model") or "")]
    if mode:   # nor two delivery modes: tooluse vs oracle IS the question
        rows = [r for r in rows if r.get("mode") == mode]
    return rows


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
    ap.add_argument("--premise", action="store_true",
                    help="tooluse-vs-oracle summary per model: did it fail to fetch, or to use?")
    args = ap.parse_args()
    CORPUS = args.corpus
    if args.premise:
        premise(load())
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
