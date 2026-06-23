#!/usr/bin/env bash
# Scale-degradation sweep: build per-N stores, then per N probe B1-rag (BM25 over the parity export)
# and B2 get_relevant_context at floor 0.99 (BM25F) and 0.50 (hybrid) on a warm floor-toggled serve,
# then the strict-verdict report. Deterministic, $0 (no codex). Launch via `!`.
#   CORPUS=rust-rfcs NS=50,100,200,400,634 SEEDS=0 bash run_scale_degradation.sh
set -uo pipefail
cd "$(dirname "$0")"

CORPUS="${CORPUS:-rust-rfcs}"
NS="${NS:-50,100,200,400,634}"
SEEDS="${SEEDS:-0}"
BIN="$HOME/code/moosedev/target/release/moosedev"
ONTO="$HOME/code/moosedev/ontologies"
STORES="$HOME/code/moosedev_benches/scale/stores/$CORPUS"
EXP="scale/exports"
PROBE="scale/probe_$CORPUS.jsonl"
PY=".venv/bin/python"
SPID=""

serve_up() {  # $1 store  $2 floor
  rm -f "$1/moosedev.sock"
  MOOSEDEV_DENSE_FLOOR="$2" MOOSEDEV_EXPAND_HOPS=0 MOOSEDEV_DATA_DIR="$1" MOOSEDEV_ONTOLOGY_DIR="$ONTO" \
    nohup "$BIN" --serve > /tmp/scale_serve.log 2>&1 &
  SPID=$!
  for _ in $(seq 1 240); do [ -S "$1/moosedev.sock" ] && break; sleep 0.5; done
}
serve_down() {  # $1 store
  [ -n "$SPID" ] && kill "$SPID" 2>/dev/null
  for _ in 1 2 3 4 5 6; do [ -S "$1/moosedev.sock" ] || break; sleep 0.5; done
  [ -n "$SPID" ] && kill -9 "$SPID" 2>/dev/null; rm -f "$1/moosedev.sock"; SPID=""
}
trap '[ -n "$SPID" ] && kill -9 "$SPID" 2>/dev/null' EXIT

echo "=== build N-stores ($CORPUS, NS=$NS, SEEDS=$SEEDS) ==="
$PY scale_build.py --corpus "$CORPUS" --ns "$NS" --seeds "$SEEDS" || { echo "build failed"; exit 1; }

: > "$PROBE"
IFS=, read -ra NLIST <<< "$NS"
IFS=, read -ra SLIST <<< "$SEEDS"
for seed in "${SLIST[@]}"; do
  for n in "${NLIST[@]}"; do
    store="$STORES/N${n}_s${seed}/.moosedev"
    echo "=== probe N=$n seed=$seed ==="
    $PY scale_probe.py --corpus "$CORPUS" --arm b1rag \
      --export "$EXP/${CORPUS}_N${n}_s${seed}.json" --n "$n" --seed "$seed" --out "$PROBE" || echo "  (b1rag failed)"
    for floor in 0.99 0.50; do
      serve_up "$store" "$floor"
      if [ ! -S "$store/moosedev.sock" ]; then echo "  serve($floor) failed"; tail -5 /tmp/scale_serve.log; continue; fi
      $PY scale_probe.py --corpus "$CORPUS" --arm b2 --store "$store" --floor "$floor" \
        --target-map "$EXP/${CORPUS}_N${n}_s${seed}.targets.json" --n "$n" --seed "$seed" --out "$PROBE" || echo "  (b2@$floor failed)"
      serve_down "$store"
    done
  done
done

echo "=== report ==="
$PY scale_report.py --corpus "$CORPUS" --probe "$PROBE"
