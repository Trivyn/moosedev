#!/usr/bin/env bash
# Stage 1 — agentic CAPABILITY suite on trivyn-temporal (codex). B0/B1-rag/B2 × the capability_qa
# questions, tooluse. ONE warm serve at the DEFAULT (production) hybrid floor — B2's real config;
# B0/B1-rag never touch it. Graded by grade_set (set recall/precision/F1); both metric families
# reported. Pure-memory (materialize_tree:false) so no source tree to escape to. Launch via `!`.
#   ARMS="B1-rag B2" TASKS=set_all_constraints N=1 ./run_capability.sh   # smoke a subset
set -uo pipefail
cd "$(dirname "$0")"

CORPUS="${CORPUS:-trivyn-temporal}"
DD="$HOME/.moosedev-stores/$CORPUS"
BIN="$HOME/code/moosedev/target/release/moosedev"
ONTO="$HOME/code/moosedev/ontologies"
export BENCH_WORK_ROOT="${BENCH_WORK_ROOT:-/tmp/capability_work}"; mkdir -p "$BENCH_WORK_ROOT"

read -ra ARMS  <<< "${ARMS:-B0 B1-rag B2}"
if [ -n "${TASKS:-}" ]; then read -ra TASKS <<< "$TASKS"; else  # else auto-discover the corpus's capability tasks
  TDIR=$(.venv/bin/python -c "import config;print(config.corpus_tasks_path('$CORPUS'))")
  read -ra TASKS <<< "$(ls "$TDIR"/*.json 2>/dev/null | xargs -n1 basename | sed 's/\.json$//' | grep -E '^(set_|neg_|sup_|mh_)' | tr '\n' ' ')"
fi
MODEL="${MODEL:-gpt-5.4-mini}"
N=${N:-1}
SERVE=""

serve_up()   { rm -f "$DD/moosedev.sock"
  MOOSEDEV_DATA_DIR="$DD" MOOSEDEV_ONTOLOGY_DIR="$ONTO" \
    nohup "$BIN" --serve > "/tmp/capability_serve.log" 2>&1 &
  SERVE=$!; for _ in $(seq 1 240); do [ -S "$DD/moosedev.sock" ] && break; python3 -c 'import time;time.sleep(0.5)'; done; }
serve_down() { [ -n "$SERVE" ] && kill "$SERVE" 2>/dev/null
  for _ in 1 2 3 4 5 6; do [ -S "$DD/moosedev.sock" ] || break; python3 -c 'import time;time.sleep(0.5)'; done
  [ -n "$SERVE" ] && kill -9 "$SERVE" 2>/dev/null; rm -f "$DD/moosedev.sock"; SERVE=""; }

trap serve_down EXIT
serve_up; [ -S "$DD/moosedev.sock" ] || { echo "serve failed"; tail -8 /tmp/capability_serve.log; exit 1; }
echo "=== serve up (pid=$SERVE) — arms=[${ARMS[*]}] tasks=${#TASKS[@]} N=$N model=$MODEL ==="

for arm in "${ARMS[@]}"; do for task in "${TASKS[@]}"; do for i in $(seq 1 "$N"); do
  echo "--- [$arm] $task #$i ---"
  .venv/bin/python run.py --corpus "$CORPUS" --task "$task" --arm "$arm" \
     --mode tooluse --backend codex --model "$MODEL" || echo "  (cell failed)"
done; done; done

serve_down
echo "=== regrade + report ==="
.venv/bin/python regrade.py >/dev/null 2>&1 || true
.venv/bin/python capability_report.py --corpus "$CORPUS"
