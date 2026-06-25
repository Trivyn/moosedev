#!/usr/bin/env bash
# Overnight driver: Instrument B (hybrid A/B) then the summaries. One unattended job. Launch via `!`.
set -uo pipefail
cd "$(dirname "$0")"
CORPUS="${CORPUS:-codegraph}"
RUNS="$HOME/code/moosedev_benches/$CORPUS/runs/runs.jsonl"
OUT="$HOME/code/moosedev_benches/$CORPUS/runs"
# Bound per-cell wall-clock so one stuck codex cell can't stall the whole overnight run (these are
# short Q&A cells, no code tree). run.py treats a timeout as a graded result, not a crash.
export BENCH_CELL_TIMEOUT="${BENCH_CELL_TIMEOUT:-300}"

# Keep _work free of stale code checkouts. A materialize_tree:false (pure-memory) agent that finds
# leftover source in a sibling _work dir will read it and BOTH confound the memory test and
# balloon tokens ~10x (observed). Cells auto-clean, but quarantine any straggler from a prior crash.
WORK="$HOME/code/moosedev_benches/_work"; mkdir -p "$WORK"
if [ -n "$(ls -A "$WORK" 2>/dev/null)" ]; then
  q="$HOME/code/moosedev_benches/_work_stragglers_$(date +%s)"; mkdir -p "$q"; mv "$WORK"/* "$q"/ 2>/dev/null
  echo "quarantined _work stragglers -> $q"
fi

echo "=== OVERNIGHT START $(date) ==="

echo; echo "############## INSTRUMENT B: hybrid(0.5) vs BM25F(0.99) ##############"
bash run_hybrid_ab.sh || echo "(Instrument B exited non-zero)"

echo; echo "############## SUMMARIES ##############"
.venv/bin/python regrade.py --md || true
echo; echo "--- Instrument B report (re-print) ---"
.venv/bin/python hybrid_ab_report.py || true

echo "=== OVERNIGHT DONE $(date) ==="
