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
cd "$(dirname "$0")"
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

ensure_serve() {  # $1=data_dir; sets STARTED_PID ("" when an existing live serve is reused)
  local dd="$1"; STARTED_PID=""
  if [ -S "$dd/moosedev.sock" ] && lsof "$dd/moosedev.sock" >/dev/null 2>&1; then
    echo "  (reusing live serve on $dd)"; return 0
  fi
  rm -f "$dd/moosedev.sock"
  MOOSEDEV_DATA_DIR="$dd" MOOSEDEV_ONTOLOGY_DIR="$ONT" \
    MOOSEDEV_LLM_BASE_URL="$LURL" MOOSEDEV_LLM_API_KEY="$LKEY" MOOSEDEV_LLM_MODEL="$LMODEL" \
    "$BIN" --serve >"/tmp/trial-serve-$(basename "$dd").log" 2>&1 &
  STARTED_PID=$!
  for _ in $(seq 1 120); do [ -S "$dd/moosedev.sock" ] && { echo "  (started serve pid $STARTED_PID on $dd)"; return 0; }; sleep 0.5; done
  echo "  !! serve failed to come up on $dd (see /tmp/trial-serve-$(basename "$dd").log)"; return 1
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
  echo "=== $corpus  probes: $(echo $probes | tr '\n' ' ')"
  ensure_serve "$data_dir" || { echo "  skipping $corpus (no serve)"; continue; }
  for probe in $probes; do
    for arm in B0 B2; do
      echo "--- $corpus / $probe / $arm (month=$MONTH) ---"
      $PY run.py --corpus "$corpus" --task "$probe" --arm "$arm" \
        --backend "$BACKEND" --model "$MODEL" --month "$MONTH" || echo "  (cell failed; continuing)"
    done
    $PY judge_recovery.py --corpus "$corpus" --task "$probe" --arms B0 B2 --month "$MONTH" \
      || echo "  (judge failed for $probe)"
  done
  [ -n "$STARTED_PID" ] && { echo "  (stopping serve pid $STARTED_PID that we started)"; kill "$STARTED_PID" 2>/dev/null; }
  $PY trial_report.py --corpus "$corpus" || true
done
echo "### done: month=$MONTH"
