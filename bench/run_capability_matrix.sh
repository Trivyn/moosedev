#!/usr/bin/env bash
# Capability matrix — does the harness's premise hold for small models? (task #42)
#
# The premise (from the NeSy findings): small models benefit from MOOSEDev not through base
# retrieval but through CURRENCY, COMPLETENESS, NEGATION and SUPERSESSION — and the harness exists
# because they would not reliably CALL the MCP tooling to get them. Two things follow, and this
# matrix separates them:
#
#   tooluse vs oracle   — oracle pushes the same knowledge into the prompt, so a model that fails
#                         tooluse but passes oracle failed to FETCH, not to USE. That is the
#                         harness's whole justification, measured rather than assumed.
#   B1-rag vs B2        — free text vs typed structure at matched delivery. Push is retrieval; an
#                         exhaustive set is a symbolic query. Pre-registered expectation: push
#                         (either representation) cannot close completeness or negation, and B2
#                         tooluse can, IF the model will drive it.
#
# B0 is tooluse-only by construction: with no memory there is nothing to push (run.py skips the
# injection for B0), so a B0 oracle cell would duplicate B0 tooluse.
#
# Reps are the OUTER loop so an interrupted campaign still leaves a complete (noisier) matrix for
# both models rather than one model finished and the other untouched.
set -uo pipefail
cd "$(dirname "$0")"

CORPUS="${CORPUS:-trivyn-cap-2026-09}"
DD="$HOME/.moosedev-stores/$CORPUS"
BIN="$HOME/code/moosedev/target/release/moosedev"
ONTO="$HOME/code/moosedev/ontologies"
export BENCH_WORK_ROOT="${BENCH_WORK_ROOT:-/tmp/capability_matrix_work}"; mkdir -p "$BENCH_WORK_ROOT"

read -ra MODELS <<< "${MODELS:-lmstudio/qwen/qwen3.5-9b lmstudio/qwen/qwen3.8-27b}"
# arm:mode pairs
read -ra CELLS  <<< "${CELLS:-B0:tooluse B1-rag:tooluse B1-rag:oracle B2:tooluse B2:oracle}"
N=${N:-3}

if [ -n "${TASKS:-}" ]; then read -ra TASKS <<< "$TASKS"; else
  TDIR=$(.venv/bin/python -c "import config;print(config.corpus_tasks_path('$CORPUS'))")
  # mh_* excluded: both multi-hop questions resolve to a single record on this graph, so they are
  # lookups, not multi-hop capability tests. Reported as non-discriminating; not run as a null.
  read -ra TASKS <<< "$(ls "$TDIR"/*.json | xargs -n1 basename | sed 's/\.json$//' \
                        | grep -E '^(set_|neg_|sup_)' | tr '\n' ' ')"
fi

# A live trial store must never be read or written by a bench campaign. Check the CONFIG, not a
# content hash: a live store's hash changes under the trial's own writes, so hash equality proves
# nothing and hash drift is not evidence of tampering (Lesson d71099a1).
.venv/bin/python - "$CORPUS" <<'PY' || exit 1
import sys, config
c = config.CORPORA[sys.argv[1]]
if "-trial" in str(c["data_dir"]):
    sys.exit(f"REFUSING: corpus {sys.argv[1]} resolves to a live trial store {c['data_dir']}")
print(f"corpus {sys.argv[1]} -> {c['data_dir']} (not a trial store)")
PY

# LM Studio here has JIT loading DISABLED: an unloaded model answers 400, run.py still writes a
# row, and that row scores 0.0 — indistinguishable in the table from a model that tried and failed.
# One unloaded model would hand back a whole arm of confident zeros, so every model is verified
# loaded BEFORE the first cell rather than discovered at the end.
.venv/bin/python - "${MODELS[@]}" <<'PREFLIGHT' || exit 1
import json, sys, urllib.request
wanted = {m.split("/", 1)[1] for m in sys.argv[1:] if "/" in m}
served = urllib.request.urlopen("http://localhost:1234/api/v1/models", timeout=10).read()
loaded = {m["key"] for m in json.loads(served)["models"] if m.get("loaded_instances")}
missing = sorted(wanted - loaded)
if missing:
    sys.exit("REFUSING: not loaded in LM Studio (JIT is off): " + ", ".join(missing)
             + "\n  load with: lms load <key> -y")
print("models loaded:", ", ".join(sorted(wanted)))
PREFLIGHT

SERVE=""
serve_up()   { rm -f "$DD/moosedev.sock"
  MOOSEDEV_DATA_DIR="$DD" MOOSEDEV_ONTOLOGY_DIR="$ONTO" \
    MOOSEDEV_LLM_BASE_URL="${MOOSEDEV_LLM_BASE_URL:-http://localhost:1234/v1}" \
    MOOSEDEV_LLM_API_KEY="${MOOSEDEV_LLM_API_KEY:-lmstudio}" \
    MOOSEDEV_LLM_MODEL="${MOOSEDEV_LLM_MODEL:-google/gemma-4-26b-a4b-qat}" \
    nohup "$BIN" --serve > "/tmp/capability_matrix_serve.log" 2>&1 &
  SERVE=$!; for _ in $(seq 1 480); do [ -S "$DD/moosedev.sock" ] && break; sleep 1; done; }
serve_down() { [ -n "$SERVE" ] && kill "$SERVE" 2>/dev/null
  for _ in $(seq 1 10); do [ -S "$DD/moosedev.sock" ] || break; sleep 1; done
  [ -n "$SERVE" ] && kill -9 "$SERVE" 2>/dev/null; rm -f "$DD/moosedev.sock"; SERVE=""; }
trap serve_down EXIT

# The snapshot is read-only evidence: a tooluse arm that captured into it would change the very
# graph the later reps are measured against. Verified, not assumed.
KG="$DD/kg.nq"; KG_BEFORE=$(shasum -a 256 "$KG" | cut -d' ' -f1)

serve_up; [ -S "$DD/moosedev.sock" ] || { echo "serve failed"; tail -8 /tmp/capability_matrix_serve.log; exit 1; }
TOTAL=$(( ${#MODELS[@]} * ${#CELLS[@]} * ${#TASKS[@]} * N )); DONE=0
echo "=== capability matrix: ${#MODELS[@]} models x ${#CELLS[@]} cells x ${#TASKS[@]} tasks x N=$N = $TOTAL runs ==="
START=$(date +%s)

for i in $(seq 1 "$N"); do for model in "${MODELS[@]}"; do for cell in "${CELLS[@]}"; do
  arm="${cell%%:*}"; mode="${cell##*:}"
  for task in "${TASKS[@]}"; do
    DONE=$((DONE+1)); EL=$(( $(date +%s) - START ))
    printf -- "--- [%d/%d %dm elapsed] %s %s/%s %s #%d ---\n" \
      "$DONE" "$TOTAL" "$((EL/60))" "${model##*/}" "$arm" "$mode" "$task" "$i"
    .venv/bin/python run.py --corpus "$CORPUS" --task "$task" --arm "$arm" \
      --mode "$mode" --backend opencode --model "$model" 2>&1 \
      | grep -Ev "moosedev::runtime" | grep -E "score=|Traceback|Error" || echo "  (cell produced no row)"
  done
done; done; done

serve_down
KG_AFTER=$(shasum -a 256 "$KG" | cut -d' ' -f1)
[ "$KG_BEFORE" = "$KG_AFTER" ] && echo "=== snapshot unchanged (${KG_BEFORE:0:16}) ===" \
  || echo "!!! SNAPSHOT MUTATED: $KG_BEFORE -> $KG_AFTER — a cell wrote to the evidence graph"
echo "=== report ==="
.venv/bin/python capability_report.py --corpus "$CORPUS"
