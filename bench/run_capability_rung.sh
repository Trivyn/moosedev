#!/usr/bin/env bash
# One capability rung for one local model: probe the runtime, then spend the hours.
#
#   ./run_capability_rung.sh <lms-model-key> [corpus]
#
# The probe is not optional and not a formality. Four MLX models — Llama-3.3-70B,
# Hermes-4-70B, Kimi-Dev-72B and by extension the 122B — were written off as
# incapable when the actual fault was that the runtime never parsed their tool
# calls: Llama-3.3 burned 15.2M input tokens over 2,728 identical retries with
# zero calls recorded. The same weights as GGUF emit a clean parsed call. So a
# rung costs one cheap request to find out whether the model can act at all,
# before committing hours of cells (Lesson f318bbed).
set -uo pipefail
cd "$(dirname "$0")"

KEY="${1:?usage: run_capability_rung.sh <lms-model-key> [corpus]}"
CORPUS="${2:-trivyn-cap-2026-09}"
DD="$HOME/.moosedev-stores/$CORPUS"
BIN="$HOME/code/moosedev/target/release/moosedev"
ONTO="$HOME/code/moosedev/ontologies"
# The daemon's internal NLQ runs on another host so local memory holds only the
# agent under test; a second resident model is what starved the box mid-campaign.
NLQ_URL="${MOOSEDEV_LLM_BASE_URL:-http://yavin:1234/v1}"
NLQ_MODEL="${MOOSEDEV_LLM_MODEL:-google/gemma-4-26b-a4b-qat}"
CTX="${BENCH_LOCAL_CONTEXT:-65536}"
export BENCH_CELL_TIMEOUT="${BENCH_CELL_TIMEOUT:-2400}"
export BENCH_WORK_ROOT="${BENCH_WORK_ROOT:-/tmp/rung_work_$$}"; mkdir -p "$BENCH_WORK_ROOT"
OUT="${RUNG_REPORT:-$HOME/.claude/jobs/rung-${KEY//\//_}.txt}"
say() { echo "$@" | tee -a "$OUT"; }
: > "$OUT"; say "=== rung: $KEY on $CORPUS — $(date) ==="

# --- one resident model ------------------------------------------------------
curl -s http://localhost:1234/api/v1/models \
  | python3 -c "
import json,sys
ms=json.load(sys.stdin).get('models',[])
if '$KEY' not in {m['key'] for m in ms}: sys.exit('REFUSING: $KEY is not present in LM Studio')
" || exit 1
for other in $(curl -s http://localhost:1234/api/v1/models \
    | python3 -c "
import json,sys
for m in json.load(sys.stdin).get('models',[]):
    if m.get('type')=='llm' and m['key']!='$KEY' and m.get('loaded_instances'): print(m['key'])"); do
  lms unload "$other" >/dev/null 2>&1
done
lms load "$KEY" -y --context-length "$CTX" >/dev/null 2>&1 || { say "FAILED to load $KEY"; exit 1; }
say "loaded (context $CTX)"

# --- the probe: can this model act at all? -----------------------------------
say ""; say "--- tool-call probe ---"
VERDICT=$(curl -s -m 300 http://localhost:1234/v1/chat/completions \
  -H 'Content-Type: application/json' -d "{
   \"model\":\"$KEY\",
   \"messages\":[{\"role\":\"user\",\"content\":\"List every Constraint in the project knowledge graph. Use the tool.\"}],
   \"tools\":[{\"type\":\"function\",\"function\":{\"name\":\"moosedev_sparql\",
     \"description\":\"Run a read-only SPARQL query over the project knowledge graph\",
     \"parameters\":{\"type\":\"object\",\"properties\":{\"query\":{\"type\":\"string\"}},\"required\":[\"query\"]}}}],
   \"max_tokens\":600,\"temperature\":0}" \
  | python3 -c "
import json,sys
d=json.load(sys.stdin); ch=d['choices'][0]; m=ch['message']; tc=m.get('tool_calls')
print(('PASS' if tc else 'FAIL'), '| finish=' + str(ch.get('finish_reason')),
      '| call=' + (json.dumps(tc[0]['function'])[:120] if tc else 'NONE'),
      '| content=' + repr((m.get('content') or '')[:70]))" 2>&1)
say "$VERDICT"
case "$VERDICT" in
  PASS*) say "-> the model can act; running the rung" ;;
  *) say "-> no parsed tool call: this model cannot act through this runtime."
     say "   Try the other quant format before concluding anything about capability."
     exit 0 ;;
esac

# --- daemon: by pidfile, never by pattern ------------------------------------
# A broad `pkill -f "moosedev --serve"` also kills the daemon serving this repo's
# own dogfooding graph, and a stale daemon holds the store's RocksDB LOCK so the
# next one cannot open it. Both were learned the hard way.
if [ -f "$DD/moosedev-serve.pid" ]; then
  OLD=$(cat "$DD/moosedev-serve.pid"); kill "$OLD" 2>/dev/null
  for _ in $(seq 1 12); do kill -0 "$OLD" 2>/dev/null || break; sleep 1; done
  kill -0 "$OLD" 2>/dev/null && kill -9 "$OLD" 2>/dev/null
fi
rm -f "$DD/moosedev.sock"
MOOSEDEV_DATA_DIR="$DD" MOOSEDEV_ONTOLOGY_DIR="$ONTO" \
  MOOSEDEV_LLM_BASE_URL="$NLQ_URL" MOOSEDEV_LLM_API_KEY=lmstudio \
  MOOSEDEV_LLM_MODEL="$NLQ_MODEL" \
  nohup "$BIN" --serve > "/tmp/rung_serve_${KEY//\//_}.log" 2>&1 &
for _ in $(seq 1 480); do [ -S "$DD/moosedev.sock" ] && break; sleep 1; done
[ -S "$DD/moosedev.sock" ] || { say "daemon never came up"; tail -3 "/tmp/rung_serve_${KEY//\//_}.log" | tee -a "$OUT"; exit 1; }
say "daemon up (NLQ $NLQ_MODEL @ $NLQ_URL)"

# --- cells: set_* first, so a rung that runs out of time still answers the
#     headline question (where the frontier scores 1.00 and a 9B scores 0.23)
say ""; say "--- B2 tooluse, 13 tasks ---"
TDIR=$(.venv/bin/python -c "import config;print(config.corpus_tasks_path('$CORPUS'))")
for t in $(ls "$TDIR"/*.json | xargs -n1 basename | sed 's/\.json$//' | grep -E '^set_') \
         $(ls "$TDIR"/*.json | xargs -n1 basename | sed 's/\.json$//' | grep -E '^(neg_|sup_)'); do
  line=$(.venv/bin/python run.py --corpus "$CORPUS" --task "$t" --arm B2 --mode tooluse \
           --backend opencode --model "lmstudio/$KEY" 2>&1 \
         | grep -oE "score=[0-9.]+ passed=[A-Za-z]+|wall=[0-9]+ms" | tr '\n' ' ')
  say "  $(printf '%-30s' "$t") ${line:-(no row)}"
done
say ""; say "=== rung complete $(date) ==="
