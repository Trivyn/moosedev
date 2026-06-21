#!/usr/bin/env bash
# Instrument B — end-to-end B2 hybrid (floor 0.5) vs pure-BM25F (floor 0.99) on trivyn-temporal,
# codex/gpt-5.4-mini. Same binary, same graph, same live serve; the ONLY variable is
# MOOSEDEV_DENSE_FLOOR on the serve. Drift tasks (vocabulary-drift, the signal) at N=NDRIFT in BOTH
# oracle (harness pushes get_relevant_context(memory_topic,k=6) — isolates retrieval→answer) and
# tooluse (agent forms its own query). The 5 existing currency tasks ride along as a tooluse
# regression floor (predicted ~null — self-announcing records, Lesson 9e7ebeb6). Launch via `!`.
set -uo pipefail
cd "$(dirname "$0")"

CORPUS=trivyn-temporal
DD="$HOME/.moosedev-stores/$CORPUS"
BIN="$HOME/code/moosedev/target/release/moosedev"
ONTO="$HOME/code/moosedev/ontologies"
RUNS="$HOME/code/moosedev_benches/$CORPUS/runs/runs.jsonl"
OUT="$HOME/code/moosedev_benches/$CORPUS/runs"
# Isolate this run's agent workdirs to a private root so a pure-memory agent's `find ..` can't read
# a CONCURRENT run's materialized Trivyn source (the dominant escape vector). /tmp is barren of code.
export BENCH_WORK_ROOT="${BENCH_WORK_ROOT:-/tmp/hybrid_ab_work}"; mkdir -p "$BENCH_WORK_ROOT"

read -ra DRIFT <<< "${DRIFT:-merge_ns_currency objprop_validate_currency nlq_deadline_currency align_confidence_currency}"
read -ra CURR  <<< "${CURR:-local_embeddings_currency sparql_editor_currency owl_reasoner_currency nlq_filter_currency geospatial_currency}"
read -ra MODES <<< "${MODES:-oracle tooluse}"
MODEL="${MODEL:-gpt-5.4-mini}"
NDRIFT=${NDRIFT:-5}
NCURR=${NCURR:-3}
SERVE=""

serve_up()   { rm -f "$DD/moosedev.sock"
  MOOSEDEV_DENSE_FLOOR="$1" MOOSEDEV_DATA_DIR="$DD" MOOSEDEV_ONTOLOGY_DIR="$ONTO" \
    nohup "$BIN" --serve > "/tmp/hybridab_serve.log" 2>&1 &
  SERVE=$!; for _ in $(seq 1 240); do [ -S "$DD/moosedev.sock" ] && break; python3 -c 'import time;time.sleep(0.5)'; done; }
serve_down() { [ -n "$SERVE" ] && kill "$SERVE" 2>/dev/null
  for _ in 1 2 3 4 5 6; do [ -S "$DD/moosedev.sock" ] || break; python3 -c 'import time;time.sleep(0.5)'; done
  [ -n "$SERVE" ] && kill -9 "$SERVE" 2>/dev/null; rm -f "$DD/moosedev.sock"; SERVE=""; }

cell() { echo "[$1] $2 $3 #$4"; .venv/bin/python run.py --corpus "$CORPUS" --task "$3" --arm B2 \
           --mode "$2" --backend codex --model "$MODEL" || echo "  (cell failed)"; }

run_condition() { # $1 floor  $2 label
  local floor=$1 label=$2
  serve_up "$floor"; [ -S "$DD/moosedev.sock" ] || { echo "serve($label) failed"; tail -8 /tmp/hybridab_serve.log; return 1; }
  echo "=== condition $label @ floor=$floor (serve pid=$SERVE) ==="
  local before; before=$(wc -l < "$RUNS" 2>/dev/null || echo 0)
  for mode in "${MODES[@]}"; do for task in "${DRIFT[@]}"; do
    for i in $(seq 1 "$NDRIFT"); do cell "$label" "$mode" "$task" "$i"; done
  done; done
  # bash 3.2 (Apple /usr/bin/env bash) throws "unbound variable" on "${CURR[@]}" when CURR is an
  # empty array under `set -u`; the ${CURR[@]+...} form expands to nothing safely (so CURR= skips).
  for task in ${CURR[@]+"${CURR[@]}"}; do for i in $(seq 1 "$NCURR"); do cell "$label" tooluse "$task" "$i"; done; done
  serve_down
  local after; after=$(wc -l < "$RUNS")
  tail -n +$((before+1)) "$RUNS" | DENSE_FLOOR="$floor" COND="$label" .venv/bin/python -c "
import sys, json, os
fl = float(os.environ['DENSE_FLOOR']); cond = os.environ['COND']
for l in sys.stdin:
    if not l.strip(): continue
    r = json.loads(l); r['dense_floor'] = fl; r['condition'] = cond
    print(json.dumps(r))" >> "$OUT/runs_${label}.jsonl"
  echo "=== [$label] tagged $((after-before)) rows -> runs_${label}.jsonl ==="
}

: > "$OUT/runs_hybrid.jsonl"; : > "$OUT/runs_bm25f.jsonl"
trap serve_down EXIT
run_condition 0.99 bm25f  || exit 1
run_condition 0.50 hybrid || exit 1
.venv/bin/python hybrid_ab_report.py
