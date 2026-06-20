#!/usr/bin/env bash
# Burrow currency A/B matrix. Starts one persistent serve on the burrow-temporal store (B2 needs it
# for the oracle-context fetch AND tooluse MCP; the per-run env sets NO_AUTOSPAWN), runs the cells,
# then stops the serve. Launch via `!` (run.py --backend codex spawns codex agents). Adjust the
# arrays below to widen/narrow scope. Results append to runs/runs.jsonl; grade with regrade.py.
set -uo pipefail
cd "$(dirname "$0")"

CORPUS=burrow-temporal
DD="$HOME/.moosedev-stores/$CORPUS"
BIN="$HOME/code/moosedev/target/release/moosedev"
# Env-overridable for a smoke first, e.g.:  N=1 MODES=oracle MODELS=gpt-5.4-mini bash run_burrow_currency.sh
read -ra TASKS  <<< "${TASKS:-rest_rss_currency quickstart_currency report_voice_currency}"
read -ra ARMS   <<< "${ARMS:-B1-rag B2}"            # currency comparison: currency-blind BM25 vs current-only
read -ra MODES  <<< "${MODES:-oracle tooluse}"      # push vs pull
read -ra MODELS <<< "${MODELS:-gpt-5.4-mini gpt-5.3-codex-spark}"
N=${N:-5}

MOOSEDEV_DATA_DIR="$DD" MOOSEDEV_ONTOLOGY_DIR="$HOME/code/moosedev/ontologies" \
  nohup "$BIN" --serve > /tmp/burrow_currency_serve.log 2>&1 &
SERVE=$!
trap 'kill $SERVE 2>/dev/null' EXIT
for _ in $(seq 1 120); do [ -S "$DD/moosedev.sock" ] && break; sleep 0.5; done
echo "serve PID $SERVE up on $DD"

total=0
for task in "${TASKS[@]}"; do
  for model in "${MODELS[@]}"; do
    for mode in "${MODES[@]}"; do
      for arm in "${ARMS[@]}"; do
        for i in $(seq 1 "$N"); do
          total=$((total + 1))
          echo "[$total] $task $arm $mode $model #$i"
          .venv/bin/python run.py --corpus "$CORPUS" --task "$task" --arm "$arm" \
            --mode "$mode" --backend codex --model "$model" || echo "  (cell failed)"
        done
      done
    done
  done
done
echo "burrow currency matrix done ($total cells)"
