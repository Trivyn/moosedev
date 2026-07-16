"""In-anger trial dashboard (AD 07415633): monthly probe gap + graph-assist tally + kill-condition check.

Reads runs_judged.jsonl (blind-judge scores from judge_recovery.py) for a trial corpus, groups by month,
and reports per-probe and mean gap = judge(B2) - judge(B0) — the comprehension-debt signal (the cold B0
arm is expected to DECAY as rationale recedes from the evolving tree, while B2 holds). Reads the project
graph store via SPARQL for the [trial-fire]/[trial-miss] tally. Evaluates the frozen kill thresholds
(Constraint 75b0cf14): PRIMARY engagement (fires/month), SECONDARY signal (gap widening vs month-0).

  bench/.venv/bin/python bench/trial_report.py --corpus trivyn-trial
"""
import argparse
import asyncio
import collections
import json

import config
from mcp_client import call_tool

PROJECT_GRAPH = "https://moosedev.dev/kg/project"
RDFS_LABEL = "http://www.w3.org/2000/01/rdf-schema#label"
FIRE_THRESHOLD = 4  # PRIMARY kill: fewer than this many fires/project/month (sustained) by end of month 2
LEGACY_EPOCH = "legacy-unversioned"


def _epoch(row: dict) -> str:
    return row.get("trial_epoch") or LEGACY_EPOCH


def _sparql(data_dir: str, query: str) -> str:
    """One-shot read against the project store (autospawns a serve via --connect if none is running)."""
    env = {"MOOSEDEV_DATA_DIR": data_dir, "MOOSEDEV_ONTOLOGY_DIR": config.ONTOLOGY_DIR}
    return asyncio.run(call_tool(config.MOOSEDEV_BIN, ["--connect"], env, "sparql", {"query": query}))


def fire_tally(data_dir: str):
    """Count [trial-fire]/[trial-miss] InformationRecord events by month (label prefix + hasTimestamp)."""
    q = ('SELECT ?label ?ts WHERE { GRAPH <%s> { ?s <%s> ?label . '
         'OPTIONAL { ?s ?p ?ts FILTER(STRENDS(STR(?p),"hasTimestamp")) } '
         'FILTER(STRSTARTS(STR(?label),"[trial-")) } }' % (PROJECT_GRAPH, RDFS_LABEL))
    fires, misses = collections.Counter(), collections.Counter()
    try:
        data = json.loads(_sparql(data_dir, q))
    except Exception as e:
        print(f"  (fire tally unavailable: {e})")
        return fires, misses
    for b in data.get("results", {}).get("bindings", []):
        label = b.get("label", {}).get("value", "")
        month = (b.get("ts", {}).get("value", "") or "unknown")[:7]
        if label.startswith("[trial-fire]"):
            fires[month] += 1
        elif label.startswith("[trial-miss]"):
            misses[month] += 1
    return fires, misses


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", required=True)
    a = ap.parse_args()
    data_dir = config.CORPORA[a.corpus]["data_dir"]

    # 1. PROBE GAPS by month and implementation epoch. Never pair arms across builds.
    jp = config.corpus_runs_path(a.corpus) / "runs_judged.jsonl"
    judged = [json.loads(l) for l in open(jp) if l.strip()] if jp.exists() else []
    by = collections.defaultdict(
        lambda: collections.defaultdict(lambda: collections.defaultdict(dict)))
    epoch_meta = collections.defaultdict(set)
    for r in judged:
        epoch = _epoch(r)
        by[r["month"]][epoch][r["task_id"]][r["arm"]] = r["judge_score"]
        epoch_meta[(r["month"], epoch)].add(
            (r.get("moosedev_version"), r.get("moosedev_binary_sha256")))

    months = sorted(m for m in by if m)
    month_mean_gap = {}
    print(f"\n=== {a.corpus}: probe gap  judge(B2) - judge(B0)  by month ===")
    if not months:
        print("  (no judged probe data yet — author probes + run a month)")
    for m in months:
        epochs = sorted(by[m])
        if len(epochs) > 1:
            print(f"  !! {m} contains multiple trial epochs; results are shown separately and not pooled")
        only_epoch_gaps = None
        for epoch in epochs:
            meta = sorted(epoch_meta[(m, epoch)], key=lambda x: (x[0] or "", x[1] or ""))
            version, digest = meta[-1] if meta else (None, None)
            detail = "legacy rows without a recorded build fingerprint" if epoch == LEGACY_EPOCH else (
                f"version={version or 'unknown'} sha256={(digest or 'unknown')[:12]}")
            print(f"  --- {m} / epoch={epoch} ({detail}) ---")
            gaps = []
            for task, arms in sorted(by[m][epoch].items()):
                if "B0" in arms and "B2" in arms:
                    g = arms["B2"] - arms["B0"]
                    gaps.append(g)
                    print(f"  {m}  {task:<30} B0={arms['B0']:.2f}  "
                          f"B2={arms['B2']:.2f}  gap={g:+.2f}")
            mean = sum(gaps) / len(gaps) if gaps else None
            if mean is None:
                print(f"  {m}  >>> EPOCH MEAN GAP = (no paired probes)")
            else:
                print(f"  {m}  >>> EPOCH MEAN GAP = {mean:+.2f}  (n={len(gaps)} probes)")
            if len(epochs) == 1:
                only_epoch_gaps = gaps
        # Preserve one pre-registered longitudinal series. A mixed-epoch month is descriptive only;
        # silently pooling it would manufacture a comparison the frozen contract never specified.
        month_mean_gap[m] = (sum(only_epoch_gaps) / len(only_epoch_gaps)
                             if only_epoch_gaps else None)

    # 2. GRAPH-ASSIST TALLY (fires + misses) by month
    fires, misses = fire_tally(data_dir)
    print(f"\n=== {a.corpus}: graph-assist tally by month (fires AND misses — invariant #6) ===")
    tally_months = sorted(set(fires) | set(misses))
    if not tally_months:
        print("  (no [trial-fire]/[trial-miss] events recorded yet)")
    for m in tally_months:
        print(f"  {m}  fires={fires[m]}  misses={misses[m]}")

    # 3. KILL-CONDITION CHECK (Constraint 75b0cf14)
    print(f"\n=== kill-condition check (Constraint 75b0cf14) ===")
    base_month = months[0] if months else None
    base = month_mean_gap.get(base_month) if base_month else None
    print(f"  month-0 baseline ({base_month}) mean gap: {base:+.2f}" if base is not None
          else "  month-0 baseline: (no unambiguous paired data; it is not reset at a later epoch)")
    if fires:
        latest_fm = sorted(fires)[-1]
        flag = "ABANDON-RISK" if fires[latest_fm] < FIRE_THRESHOLD else "ok"
        print(f"  PRIMARY (engagement): latest-month fires={fires[latest_fm]} vs threshold {FIRE_THRESHOLD} -> {flag}")
    else:
        print(f"  PRIMARY (engagement): no fires recorded -> watch (threshold {FIRE_THRESHOLD}/month by end of month 2)")
    if len(months) >= 3 and base is not None and month_mean_gap.get(months[-1]) is not None:
        latest = month_mean_gap[months[-1]]
        flag = "ABANDON-RISK (not widening)" if latest <= base else "widening — ok"
        print(f"  SECONDARY (signal): month-0 gap {base:+.2f} -> latest {latest:+.2f} -> {flag}")
    else:
        print("  SECONDARY (signal): needs >=3 months of probe data to evaluate widening")
    print("  TRIPWIRE (cost/trust): qualitative — is the maintainer still reaching for the graph unprompted?")


if __name__ == "__main__":
    main()
