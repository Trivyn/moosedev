#!/usr/bin/env bash
# Instrument A — retrieval-slice seed-recall A/B for the hybrid dense channel (deterministic, $0).
# Stands up a floor-toggled serve on trivyn-temporal (0.99 = pure BM25F, 0.50 = hybrid), runs the
# seed-recall probe against each, then prints the recall@k/MRR comparison + the HYBRID-recovered
# chains (which seed the Instrument-B drift tasks). No codex; no agent spend. Launch via `!`.
set -uo pipefail
cd "$(dirname "$0")"

CORPUS="${CORPUS:-trivyn-temporal}"
DD="$HOME/.moosedev-stores/$CORPUS"
BIN="$HOME/code/moosedev/target/release/moosedev"
ONTO="$HOME/code/moosedev/ontologies"

run_floor() {
  local floor="$1" label="$2"
  rm -f "$DD/moosedev.sock"
  MOOSEDEV_DENSE_FLOOR="$floor" MOOSEDEV_DATA_DIR="$DD" MOOSEDEV_ONTOLOGY_DIR="$ONTO" \
    nohup "$BIN" --serve > "/tmp/seedrecall_serve_${label}.log" 2>&1 &
  local pid=$!
  # Warm restart: instance-vectors.db is pre-built, so the socket comes up in seconds (re-embeds 0).
  local up=0
  for _ in $(seq 1 240); do [ -S "$DD/moosedev.sock" ] && { up=1; break; }; sleep 0.5; done
  if [ "$up" != 1 ]; then echo "serve($label) failed to come up; log:"; tail -8 "/tmp/seedrecall_serve_${label}.log"; kill -9 "$pid" 2>/dev/null; return 1; fi
  echo "[serve $label up @ floor=$floor pid=$pid]"
  grep -qi "embedded 0 new" "/tmp/seedrecall_serve_${label}.log" && echo "  (warm: re-embedded 0 — durable index reused)"
  .venv/bin/python hybrid_seed_recall.py --corpus "$CORPUS" --floor-label "$label" --floor "$floor"
  kill "$pid" 2>/dev/null
  for _ in 1 2 3 4 5 6; do [ -S "$DD/moosedev.sock" ] || break; sleep 0.5; done
  kill -9 "$pid" 2>/dev/null; rm -f "$DD/moosedev.sock"
}

run_floor 0.99 bm25f  || exit 1
run_floor 0.50 hybrid || exit 1
.venv/bin/python hybrid_seed_recall.py --corpus "$CORPUS" --compare bm25f hybrid
