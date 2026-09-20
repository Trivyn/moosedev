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
# Only lmstudio/* models are served from this box. A hosted model (openrouter/*) has no
# local presence to verify and no weights to hold, so it is exempt from this check and
# from hold_only -- which, since the load-failure fix, would otherwise abort the campaign
# trying to `lms load` a model that lives on someone else's GPU.
wanted = {m.split("/", 1)[1] for m in sys.argv[1:] if m.startswith("lmstudio/")}
# The daemon's internal NLQ model too: when it is absent every moosedev_query
# call 400s and B2 runs with its symbolic-answer path dead, which is invisible
# in the scores and cost a whole campaign once.
import os
wanted.add(os.environ.get("MOOSEDEV_LLM_MODEL", "google/gemma-4-26b-a4b-qat"))
served = urllib.request.urlopen("http://localhost:1234/api/v1/models", timeout=10).read()
present = {m["key"] for m in json.loads(served)["models"]}
missing = sorted(wanted - present)
if missing:
    sys.exit("REFUSING: not present in LM Studio: " + ", ".join(missing))
print("models present:", ", ".join(sorted(wanted)))
PREFLIGHT

SERVE=""
serve_up()   { rm -f "$DD/moosedev.sock"
  MOOSEDEV_DATA_DIR="$DD" MOOSEDEV_ONTOLOGY_DIR="$ONTO" \
    MOOSEDEV_LLM_BASE_URL="${MOOSEDEV_LLM_BASE_URL:-http://yavin:1234/v1}" \
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

# Hold exactly one agent model in memory at a time. Keeping every model resident is what
# starved the machine and killed the per-cell `moosedev --connect` children mid-campaign,
# silently voiding 14 cells; a cell only ever needs its own model.
#
# Eviction covers EVERY loaded instance, not only the models this invocation names. On
# 2026-09-18 two back-to-back invocations stacked a4b, a duplicate a4b instance and the 27B to
# 56.9 GB and the OS killed the run: each invocation could see only its own MODELS array. And a
# second `lms load` of a model that is already loaded does not reuse it — it adds a `:2`
# instance — so the target is evicted too and then loaded exactly once. The daemon's NLQ model
# is spared only when it is served from this machine.
nlq_local_key() {
  case "${MOOSEDEV_LLM_BASE_URL:-http://yavin:1234/v1}" in
    *localhost*|*127.0.0.1*) echo "${MOOSEDEV_LLM_MODEL:-google/gemma-4-26b-a4b-qat}" ;;
  esac
}
instances_of() {  # $1: model key, or empty for every loaded instance; prints identifiers
  lms ps --json 2>/dev/null | .venv/bin/python -c '
import json, sys
want = sys.argv[1]
for inst in json.load(sys.stdin):
    if not want or inst.get("modelKey") == want:
        print(inst["identifier"], inst.get("modelKey"))' "$1"
}
resident=""
hold_only() {
  [ "$resident" = "$1" ] && return 0
  keep=$(nlq_local_key)
  instances_of "" | while read -r id key; do
    [ -n "$keep" ] && [ "$key" = "$keep" ] && continue
    lms unload "$id" >/dev/null 2>&1
  done
  lms load "$1" -y --context-length "${BENCH_LOCAL_CONTEXT:-65536}" >/dev/null 2>&1 \
    || { echo "!!! could not load $1 — its cells will fail"; return 1; }
  # Verify the WHOLE resident set, not just the target's own count: an unrelated model whose
  # unload silently failed leaves the target at exactly 1 and the box still over-committed,
  # which is the state that OOM-killed the 2026-09-18 run.
  allowed="$1"; [ -n "$keep" ] && allowed="$allowed $keep"
  actual=$(instances_of "" | awk '{print $2}' | sort | tr '\n' ' ')
  want=$(printf '%s\n' $allowed | sort | tr '\n' ' ')
  if [ "$actual" != "$want" ]; then
    echo "!!! resident set is [$actual], expected [$want] — an unload failed or something else"
    echo "!!! is loading models; continuing is how the box OOMs. Stopping."
    exit 1
  fi
  # The quantisation cannot be pinned from the CLI -- `lms load` has no variant flag, and with
  # two variants present `-y` documents itself as loading "the first matching model". The GUI's
  # selected_variant governs, which is ambient state no run row used to record. So the campaign
  # DECLARES what it expects and we verify after loading: a 5-bit and an 8-bit run of
  # qwen/qwen3.8-27b are otherwise indistinguishable, since both answer to the same model key.
  if [ -n "${EXPECT_QUANT:-}" ]; then
    got=$(.venv/bin/python - "$1" <<'PYQ'
import json, sys, urllib.request
key = sys.argv[1]
try:
    d = json.loads(urllib.request.urlopen("http://localhost:1234/api/v1/models", timeout=10).read())
    for m in d["models"]:
        if m.get("key") == key:
            print((m.get("quantization") or {}).get("name") or "?"); break
    else:
        print("?")
except Exception:
    print("?")
PYQ
)
    if [ "$got" != "$EXPECT_QUANT" ]; then
      echo "!!! $1 is served at '$got' but this campaign expects '$EXPECT_QUANT'."
      echo "!!! Select the right variant in LM Studio (the CLI cannot pin it) and re-run. Stopping"
      echo "!!! rather than recording a campaign whose rows would be indistinguishable from the other."
      exit 1
    fi
    note_quant="$got"
  fi
  resident="$1"
}

for i in $(seq 1 "$N"); do for model in "${MODELS[@]}"; do
  case "$model" in lmstudio/*) hold_only "${model#lmstudio/}" || {
    echo "!!! could not hold ${model#lmstudio/} — refusing to run its cells against an unloaded"
    echo "!!! model: LM Studio has JIT off, so every cell would 400 and score a plausible 0.0"
    exit 1
  } ;; *) echo "  (hosted model — no local weights to hold)" ;; esac
  for cell in "${CELLS[@]}"; do
  arm="${cell%%:*}"; mode="${cell##*:}"
  for task in "${TASKS[@]}"; do
    DONE=$((DONE+1)); EL=$(( $(date +%s) - START ))
    printf -- "--- [%d/%d %dm elapsed] %s %s/%s %s #%d ---\n" \
      "$DONE" "$TOTAL" "$((EL/60))" "${model##*/}" "$arm" "$mode" "$task" "$i"
    .venv/bin/python run.py --corpus "$CORPUS" --task "$task" --arm "$arm" \
      --mode "$mode" --backend opencode --model "$model" 2>&1 \
      | grep -Ev "moosedev::runtime" | grep -E "score=|Traceback|Error" || echo "  (cell produced no row)"
    # Per cell, not per campaign. On 2026-09-17 two Qwen3.5-9B cells wrote into this corpus
    # (a "Test query" Consequence; an AD superseding ITSELF) and the end-of-campaign check never
    # ran because the campaign was interrupted. The mutated export then became the "verified"
    # baseline for every later campaign. A cell that writes stops the run here, so the damage is
    # one cell and the culprit is named, rather than discovered two days later in a transcript.
    KG_NOW=$(shasum -a 256 "$KG" | cut -d' ' -f1)
    if [ "$KG_NOW" != "$KG_BEFORE" ]; then
      echo "!!! SNAPSHOT MUTATED by ${model##*/} $arm/$mode $task #$i: ${KG_BEFORE:0:16} -> ${KG_NOW:0:16} — stopping"
      exit 1
    fi
  done
done; done; done

serve_down
KG_AFTER=$(shasum -a 256 "$KG" | cut -d' ' -f1)
[ "$KG_BEFORE" = "$KG_AFTER" ] && echo "=== snapshot unchanged (${KG_BEFORE:0:16}) ===" \
  || echo "!!! SNAPSHOT MUTATED: $KG_BEFORE -> $KG_AFTER — a cell wrote to the evidence graph"
echo "=== report ==="
.venv/bin/python capability_report.py --corpus "$CORPUS"
