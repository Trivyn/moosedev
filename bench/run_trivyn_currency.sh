#!/usr/bin/env bash
# Trivyn ECOLOGICAL currency/why run (AD b3205dcb): B1-notes (agent greps the team's REAL 287KB of
# lessons.md/docs) vs B2 (MOOSEDev graph) vs B0 (cold). Starts one serve on trivyn-temporal (B2 needs
# it; B1-notes/B0 ignore it). Launch via `!` (run.py --backend codex spawns codex agents).
# Defaults to the ecological tooluse run; env-overridable, e.g. ARMS="B1-export B2" MODES=oracle ...
set -uo pipefail
cd "$(dirname "$0")"

CORPUS=trivyn-temporal
DD="$HOME/.moosedev-stores/$CORPUS"
BIN="$HOME/code/moosedev/target/release/moosedev"
read -ra TASKS  <<< "${TASKS:-local_embeddings_currency sparql_editor_currency owl_reasoner_currency nlq_filter_currency geospatial_currency}"
read -ra ARMS   <<< "${ARMS:-B0 B1-notes B2}"      # ecological: cold vs real-notes-grep vs graph
read -ra MODES  <<< "${MODES:-tooluse}"            # B1-notes is tooluse-only (greps real docs)
read -ra MODELS <<< "${MODELS:-gpt-5.4-mini}"
N=${N:-3}

MOOSEDEV_DATA_DIR="$DD" MOOSEDEV_ONTOLOGY_DIR="$HOME/code/moosedev/ontologies" \
  nohup "$BIN" --serve > /tmp/trivyn_currency_serve.log 2>&1 &
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
echo "trivyn ecological currency run done ($total cells)"
