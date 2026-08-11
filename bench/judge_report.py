"""Characterize the LLM-judge instrument from persisted verdicts: ranges across repeat draws.

The judge is stochastic: identical (row, prompt, slug) calls credit different borderline items,
and the model behind an OpenRouter slug is not a pinned snapshot. A single judged pass is
therefore ONE DRAW, and quoting its decimals as THE number orphans them (nobody can regenerate a
draw). This report deliberately does NOT print a rival point table for any previously published
draw. It prints, per capability class and arm:

  mean [min-max] over k draws, micro-averaged over expected ITEMS within the class

Micro-averaging pools items across the class's tasks, so a degenerate 1-item task contributes one
item, not one-third of the class, and large sets damp per-item judge flips. The envelope is
computed per cell (min/max covered count per draw) then pooled -- a conservative bound.

VALIDATION (Lesson a6529240): the judge must roughly reproduce the deterministic strict grader on
the structured B2 arm -- in its WORST draw, not just on average. A judge whose worst draw
under-reads B2 is unstable enough that the competitor columns should not be trusted either.

  python3 judge_report.py                 # codegraph, verdicts beside the runs file
  python3 judge_report.py --cells         # add per-cell draw detail
"""
import argparse
import collections
import functools
import json
from pathlib import Path

import config

CLASSES = ["set_completeness", "negation", "supersession", "multi_hop", "relevance", "currency"]
ARMS = ["B2", "B1-rag", "B1-mem0", "B1-notes", "B0"]
VALIDATION_ARM = "B2"
VALIDATION_TOL = 0.10       # worst-draw B2 judge micro may trail strict micro by this much


@functools.lru_cache(maxsize=None)
def _class_of(corpus: str, task_id: str) -> str:
    try:
        return json.loads((config.corpus_tasks_path(corpus) / f"{task_id}.json").read_text()
                          ).get("capability_class", "?")
    except (FileNotFoundError, json.JSONDecodeError):
        return "?"


def load(path: Path, corpus: str):
    """All verdicts for `corpus`, grouped into draws per (task_id, arm).

    Every verdict is a DRAW and all draws are kept -- repeat judging is the point, so there is no
    last-wins dedup here. Corpus filtering is load-bearing: public corpora share bench/runs, so
    one verdict log holds several corpora and locating the file is not selecting a corpus.
    """
    rows = [json.loads(l) for l in path.read_text().splitlines() if l.strip()]
    present = {r.get("corpus") for r in rows}
    cells = collections.defaultdict(list)
    for r in rows:
        if r.get("corpus") == corpus:
            cells[(r["task_id"], r["arm"])].append(r)
    return cells, present


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", default="codegraph")
    ap.add_argument("--in", dest="src", help="verdict log (default: judge_verdicts.jsonl beside the runs file)")
    ap.add_argument("--cells", action="store_true", help="print per-cell draw detail")
    a = ap.parse_args()

    src = Path(a.src) if a.src else config.corpus_runs_path(a.corpus) / "judge_verdicts.jsonl"
    if not src.exists():
        raise SystemExit(f"no verdict log at {src} -- run regrade_judge.py first (it appends there).")
    cells, present = load(src, a.corpus)
    if not cells:
        raise SystemExit(f"{src} holds no verdicts for corpus '{a.corpus}' "
                         f"(corpora present: {', '.join(sorted(str(p) for p in present)) or 'none'}).")

    models = sorted({r.get("judge_model", "?") for ds in cells.values() for r in ds})
    n_draws = sum(len(ds) for ds in cells.values())

    # class -> arm -> list of per-cell stats
    by = collections.defaultdict(lambda: collections.defaultdict(list))
    for (task_id, arm), draws in cells.items():
        covs = [len(d["covered_idx"]) for d in draws]
        stricts = {round(d["strict_recall"], 4) for d in draws}
        n_exp = draws[0]["n_expected"]
        if len(stricts) > 1:
            print(f"WARNING: {task_id}/{arm}: strict recall varies across draws {sorted(stricts)} "
                  f"-- the judged row changed between draws; envelope mixes rows.")
        by[_class_of(a.corpus, task_id)][arm].append({
            "task": task_id, "n_exp": n_exp, "covs": covs,
            "strict_cov": draws[0]["strict_recall"] * n_exp,
        })
    classes = [c for c in CLASSES if c in by] + sorted(c for c in by if c not in CLASSES)
    arms = [x for x in ARMS if any(x in by[c] for c in classes)]
    arms += sorted({x for c in classes for x in by[c] if x not in arms})

    print(f"\n=== Judge-instrument characterization: paraphrase-fair recall ({a.corpus}) ===")
    print(f"source: {src}  |  {n_draws} draws over {len(cells)} (task,arm) cells  |  judge: {', '.join(models)}")
    if len(models) > 1:
        print("WARNING: draws span MORE THAN ONE judge model; the envelope mixes instruments.")
    print("Per class x arm: mean [min-max] micro recall over k draws/cell. Ranges are the instrument's")
    print("spread, not uncertainty in the underlying answer -- the transcripts are frozen.\n")

    head = f"{'class':<18}" + "".join(f"{x:>22}" for x in arms)
    print(head)
    print("-" * len(head))
    for c in classes:
        line = f"{c:<18}"
        for x in arms:
            cs = by[c].get(x)
            if not cs:
                line += f"{'-':>22}"
                continue
            tot = sum(s["n_exp"] for s in cs)
            mean = sum(sum(s["covs"]) / len(s["covs"]) for s in cs) / tot
            lo = sum(min(s["covs"]) for s in cs) / tot
            hi = sum(max(s["covs"]) for s in cs) / tot
            ks = {len(s["covs"]) for s in cs}
            k = str(min(ks)) if len(ks) == 1 else f"{min(ks)}-{max(ks)}"
            line += f"{f'{mean:.2f} [{lo:.2f}-{hi:.2f}] k={k}':>22}"
        print(line)

    if a.cells:
        print("\nPer-cell draws (covered / expected per draw):")
        for c in classes:
            for x in arms:
                for s in by[c].get(x, []):
                    draws = " ".join(str(v) for v in s["covs"])
                    print(f"  {c:<18} {s['task']:<30} {x:<9} n={s['n_exp']:<4} draws: {draws}")

    print(f"\nVALIDATION -- {VALIDATION_ARM} worst draw vs deterministic strict grader (micro):")
    suspect = []
    for c in classes:
        cs = by[c].get(VALIDATION_ARM)
        if not cs:
            continue
        tot = sum(s["n_exp"] for s in cs)
        strict = sum(s["strict_cov"] for s in cs) / tot
        worst = sum(min(s["covs"]) for s in cs) / tot
        flag = ""
        if strict - worst > VALIDATION_TOL:
            flag = "  <-- SUSPECT: judge's worst draw under-reads the structured arm"
            suspect.append(c)
        print(f"  {c:<18} strict {strict:.2f}  worst-draw {worst:.2f}  (delta {worst - strict:+.2f}){flag}")
    print("\nVERDICT:", "REVIEW THE JUDGE -- " + ", ".join(suspect) if suspect
          else "judge tracks the strict grader on the structured arm in every draw; "
               "baseline ranges are a fair read of the instrument.")


if __name__ == "__main__":
    main()
