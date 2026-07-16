#!/usr/bin/env bash
# In-anger trial monthly checkpoint (AD 07415633). For each project corpus: ensure a serve is up on its
# trial store (B2 uses MOOSEDEV_NO_AUTOSPAWN=1, so the harness must own the backend), run the frozen probes
# on arms B0 (cold, current HEAD tree) + B2 (current graph), blind-judge each answer vs its frozen gold,
# then print the dashboard (gap + fire/miss tally + kill-condition). Appends to runs.jsonl /
# runs_judged.jsonl — safe to re-run. Missing probes/tasks are skipped with a notice.
#
#   bench/run_trial_month.sh [YYYY-MM]                                   # default month = current UTC month
#   TRIAL_CORPORA="trivyn-trial" bench/run_trial_month.sh 2026-06
#   TRIAL_PROBES="why_growl_over_reasonable" TRIAL_CORPORA="trivyn-trial" bench/run_trial_month.sh  # 1-probe canary
#   TRIAL_BACKEND=codex TRIAL_MODEL=gpt-5.5 bench/run_trial_month.sh
set -uo pipefail
cd "$(dirname "$0")" || exit 1
PY="${PY:-.venv/bin/python}"
MONTH="${1:-$(date -u +%Y-%m)}"
BACKEND="${TRIAL_BACKEND:-codex}"          # frontier agent = the strict test (recovers 'why' cold)
MODEL="${TRIAL_MODEL:-gpt-5.5}"
CORPORA="${TRIAL_CORPORA:-trivyn-trial moose-trial}"

# Serve params from config.py (respects the qat NLQ model + endpoint).
BIN=$($PY -c "import config; print(config.MOOSEDEV_BIN)")
ONT=$($PY -c "import config; print(config.ONTOLOGY_DIR)")
LURL=$($PY -c "import config; print(config.LLM_BASE_URL)")
LKEY=$($PY -c "import config; print(config.LLM_API_KEY)")
LMODEL=$($PY -c "import config; print(config.NLQ_MODEL)")

STARTED_PID=""
STARTED_BINARY_SHA256=""
stop_owned_serve() {
  if [ -n "$STARTED_PID" ]; then
    echo "  (stopping harness-owned serve pid $STARTED_PID)"
    kill "$STARTED_PID" 2>/dev/null || true
    wait "$STARTED_PID" 2>/dev/null || true
    STARTED_PID=""
    STARTED_BINARY_SHA256=""
  fi
}
trap stop_owned_serve EXIT
trap 'stop_owned_serve; exit 130' INT TERM

ensure_serve() {  # $1=data_dir; starts a daemon owned by this harness and sets STARTED_PID
  local dd="$1"
  STARTED_PID=""
  if [ -S "$dd/moosedev.sock" ]; then
    if ! command -v lsof >/dev/null 2>&1; then
      echo "  !! socket already exists at $dd/moosedev.sock and lsof is unavailable; refusing unsafe reuse"
      return 1
    fi
    if lsof "$dd/moosedev.sock" >/dev/null 2>&1; then
      echo "  !! live serve already owns $dd; refusing to reuse or stop a daemon this harness did not start"
      return 1
    fi
  fi
  rm -f "$dd/moosedev.sock"
  STARTED_BINARY_SHA256=$($PY -c \
    'import hashlib, sys; print(hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest())' \
    "$BIN") || return 1
  MOOSEDEV_DATA_DIR="$dd" MOOSEDEV_ONTOLOGY_DIR="$ONT" \
    MOOSEDEV_LLM_BASE_URL="$LURL" MOOSEDEV_LLM_API_KEY="$LKEY" MOOSEDEV_LLM_MODEL="$LMODEL" \
    "$BIN" --serve >"/tmp/trial-serve-$(basename "$dd").log" 2>&1 &
  STARTED_PID=$!
  for _ in $(seq 1 120); do
    if ! kill -0 "$STARTED_PID" 2>/dev/null; then
      echo "  !! harness-owned serve pid $STARTED_PID exited during startup (see /tmp/trial-serve-$(basename "$dd").log)"
      wait "$STARTED_PID" 2>/dev/null || true
      STARTED_PID=""
      STARTED_BINARY_SHA256=""
      return 1
    fi
    [ -S "$dd/moosedev.sock" ] && { echo "  (started harness-owned serve pid $STARTED_PID on $dd)"; return 0; }
    sleep 0.5
  done
  echo "  !! serve failed to come up on $dd (see /tmp/trial-serve-$(basename "$dd").log)"
  stop_owned_serve
  return 1
}

echo "### in-anger trial  month=$MONTH  backend=$BACKEND  model=$MODEL  corpora: $CORPORA"
for corpus in $CORPORA; do
  tasks_dir=$($PY -c "import config; print(config.corpus_tasks_path('$corpus'))" 2>/dev/null)
  data_dir=$($PY -c "import config; print(config.CORPORA['$corpus']['data_dir'])" 2>/dev/null)
  if [ -z "$tasks_dir" ] || [ ! -d "$tasks_dir" ]; then
    echo "!! $corpus: no tasks dir ($tasks_dir) — author + freeze probes first. Skipping."; continue
  fi
  if [ -n "${TRIAL_PROBES:-}" ]; then probes="$TRIAL_PROBES"
  else probes=$(cd "$tasks_dir" && ls ./*.json 2>/dev/null | sed 's#.*/##; s/\.json$//'); fi
  [ -z "$probes" ] && { echo "!! $corpus: no probe JSONs in $tasks_dir. Skipping."; continue; }
  echo "=== $corpus  probes: $(echo "$probes" | tr '\n' ' ')"
  ensure_serve "$data_dir" || { echo "  skipping $corpus (no serve)"; continue; }
  if ! IDENTITY=$($PY run.py --print-trial-identity "$data_dir" \
      --trial-daemon-pid "$STARTED_PID" \
      --expected-binary-sha256 "$STARTED_BINARY_SHA256"); then
    echo "  !! could not fingerprint the running MOOSEDev backend; skipping $corpus"
    stop_owned_serve
    continue
  fi
  if ! kill -0 "$STARTED_PID" 2>/dev/null; then
    echo "  !! harness-owned serve exited while it was being fingerprinted; skipping $corpus"
    STARTED_PID=""
    continue
  fi
  IFS=$'\t' read -r TRIAL_EPOCH MOOSEDEV_VERSION MOOSEDEV_BINARY_SHA256 <<< "$IDENTITY"
  echo "  trial epoch=$TRIAL_EPOCH  version=$MOOSEDEV_VERSION  binary_sha256=$MOOSEDEV_BINARY_SHA256"
  for probe in $probes; do
    for arm in B0 B2; do
      echo "--- $corpus / $probe / $arm (month=$MONTH) ---"
      $PY run.py --corpus "$corpus" --task "$probe" --arm "$arm" \
        --backend "$BACKEND" --model "$MODEL" --month "$MONTH" \
        --trial-epoch "$TRIAL_EPOCH" --moosedev-version "$MOOSEDEV_VERSION" \
        --moosedev-binary-sha256 "$MOOSEDEV_BINARY_SHA256" || echo "  (cell failed; continuing)"
    done
    $PY judge_recovery.py --corpus "$corpus" --task "$probe" --arms B0 B2 --month "$MONTH" \
      --trial-epoch "$TRIAL_EPOCH" \
      || echo "  (judge failed for $probe)"
  done
  stop_owned_serve
  $PY trial_report.py --corpus "$corpus" || true
done
echo "### done: month=$MONTH"
