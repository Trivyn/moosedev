"""Re-grade logged benchmark rows from their stored artifacts with the CURRENT grader.

grade.py is the single source of grading truth. When it changes (e.g. the negation-aware
`stale` fix), the recorded score/metrics must be recomputable from the immutable transcripts
WITHOUT re-running any agent — every row logs its `final_text` (context_qa) and code tasks
save a `{run_id}.patch` artifact next to the runs file. This script reads each tracked
runs.jsonl, recomputes score/passed/metrics, and writes a `runs_regraded.jsonl` sibling.

Non-destructive: the append-only runs.jsonl (raw telemetry) is never modified.

Usage:
  python regrade.py            # regrade all corpora, write *_regraded.jsonl, print currency summary
  python regrade.py --md       # also emit the currency table as Markdown (for RESULTS.md)
"""
import argparse
import collections
import json
from pathlib import Path

import config
from grade import grade
from grade_set import grade_set
from grade_code import grade_patch

CODE_TASK_TYPES = {"constraint_code"}  # graded on the saved patch, not final_text (mirrors run.py)


def _ground_truth(corpus: str, task_id: str) -> dict:
    return json.loads((config.corpus_tasks_path(corpus) / f"{task_id}.json").read_text())["ground_truth"]


def regrade_row(row: dict, runs_dir: Path) -> dict | None:
    """Recompute a row's grade from its stored artifact. Returns a NEW row, or None if the
    artifact needed to regrade (a code task's patch) is missing."""
    gt = _ground_truth(row["corpus"], row["task_id"])
    if row["task_type"] in CODE_TASK_TYPES:
        patch_path = runs_dir / f"{row['run_id']}.patch"
        if not patch_path.exists():
            return None
        patch = patch_path.read_text()
        g = grade_patch(patch, gt)
        g["patch_len"] = len(patch)
    elif row["task_type"] == "capability_qa":
        g = grade_set(row.get("final_text", ""), gt)
    else:
        g = grade(row.get("final_text", ""), gt)
    new = dict(row)
    new["score"] = g["score"]
    new["passed"] = g["passed"]
    new["metrics"] = {k: v for k, v in g.items() if k not in ("score", "passed")}
    return new


def runs_files() -> list[Path]:
    """Every runs.jsonl across configured corpora (public under bench/runs, private under
    BENCH_HOME). De-duplicated; only those that exist."""
    paths = {config.RUNS_DIR / "runs.jsonl"}
    for corpus in config.CORPORA:
        paths.add(config.corpus_runs_path(corpus) / "runs.jsonl")
    return sorted(p for p in paths if p.exists())


def regrade_all() -> list[dict]:
    """Regrade every runs file in place (writing a sibling *_regraded.jsonl) and return all
    regraded rows."""
    all_rows, changed, skipped = [], 0, 0
    for path in runs_files():
        rows = [json.loads(l) for l in path.read_text().splitlines() if l.strip()]
        out = []
        for row in rows:
            new = regrade_row(row, path.parent)
            if new is None:
                skipped += 1
                out.append(row)
                continue
            if (new["score"], new["passed"], new["metrics"]) != (
                    row["score"], row["passed"], row.get("metrics")):
                changed += 1
            out.append(new)
        dest = path.with_name("runs_regraded.jsonl")
        dest.write_text("".join(json.dumps(r) + "\n" for r in out))
        print(f"  {path}  ->  {dest.name}  ({len(out)} rows)")
        all_rows.extend(out)
    print(f"regraded {len(all_rows)} rows | {changed} changed vs stored | {skipped} skipped (no patch artifact)")
    return all_rows


def currency_summary(rows: list[dict], as_md: bool = False) -> None:
    """Currency (STRICT) = the answer asserts the CURRENT state (full must_include_any coverage)
    AND does NOT assert the superseded state (no negation-aware `stale` marker). The AND-not-stale
    guard is essential: coverage alone is fooled by a NEGATED current marker ("not routing through
    MOOSE") and by binary-choice prompt echoes — the trivyn local_embeddings B1-notes case scored
    coverage 1.0 while actually serving the stale Candle answer (Lesson 046bc2d8/9fc21dc0)."""
    cur = [r for r in rows if r["task_id"].endswith("currency")]
    by = collections.defaultdict(lambda: collections.defaultdict(list))
    for r in cur:
        by[r["task_id"]][r["arm"]].append(r)
    if as_md:
        print("\n| task | arm | n | current % | mean score | stale-flag (hint) |")
        print("|---|---|---|---|---|---|")
    for task in sorted(by):
        if not as_md:
            print(f"\n{task}")
        for arm in sorted(by[task]):
            rs = by[task][arm]
            n = len(rs)
            current = sum(1 for r in rs if (r["metrics"] or {}).get("coverage") == 1.0
                           and not (r["metrics"] or {}).get("stale"))  # STRICT: current AND not-stale
            mean = sum(r["score"] for r in rs) / n
            staleflag = sum(1 for r in rs if (r["metrics"] or {}).get("stale"))
            pct = round(100 * current / n)
            if as_md:
                print(f"| {task} | {arm} | {n} | {pct}% ({current}/{n}) | {mean:.2f} | {staleflag}/{n} |")
            else:
                print(f"    {arm:8s} n={n:2d}  current={pct:3d}% ({current}/{n})  "
                      f"mean_score={mean:.2f}  stale-flag={staleflag}/{n}")
    models = sorted({r["agent_model"] for r in cur if r.get("agent_model")})
    print(f"\nmodels in currency runs: {', '.join(models)}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--md", action="store_true", help="emit the currency table as Markdown")
    args = ap.parse_args()
    rows = regrade_all()
    currency_summary(rows, as_md=args.md)


if __name__ == "__main__":
    main()
