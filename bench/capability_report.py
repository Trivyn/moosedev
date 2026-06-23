"""Capability benchmark report: B2 (graph) vs B1-rag (flat RAG) vs B0 (cold), trivyn-temporal.

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

CORPUS = "trivyn-temporal"
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


def load() -> list[dict]:
    d = config.corpus_runs_path(CORPUS)
    p = d / "runs_regraded.jsonl"
    if not p.exists():
        p = d / "runs.jsonl"
    rows = [json.loads(l) for l in p.read_text().splitlines() if l.strip()] if p.exists() else []
    return [r for r in rows if r.get("task_type") == "capability_qa"]


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


def main() -> None:
    global CORPUS
    import argparse
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="trivyn-temporal")
    CORPUS = ap.parse_args().corpus
    rows = load()
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
