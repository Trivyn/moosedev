#!/usr/bin/env bash
# Fast rig verification. Run this before committing hours to a campaign.
#
#   ./smoke.sh [corpus]                  # full check, ~3-5 min with a warm daemon
#   SMOKE_SKIP_BUILD=1 ./smoke.sh        # skip the compile checks
#   SMOKE_CANARY_MODEL=... ./smoke.sh    # override the control model
#
# Every check here exists because its absence once cost real hours, and each failure it
# catches presented as a MEASUREMENT rather than a fault: a dead MCP read as a weak
# tool-caller (Lesson 0a890ea9); an unloaded NLQ model left moosedev_query dead for a
# whole campaign; a store without its substrate index returned empty dossiers that looked
# like a clean negative result; a 401 from a key prefix arrived as score=0.0; committed
# code that did not compile under --features harness (Lesson bb9d2dac). A zero that the
# rig manufactured is worse than no data, because it gets believed.
#
# The split matters: this verifies the RIG using a known-good control model. It does NOT
# judge the model under test — a model that fails is the experiment, not a smoke failure.
# Use run_capability_rung.sh's probe for that.
set -uo pipefail
cd "$(dirname "$0")"

CORPUS="${1:-trivyn-cap-2026-09}"
DD="$HOME/.moosedev-stores/$CORPUS"
BIN="$HOME/code/moosedev/target/release/moosedev"
ONTO="$HOME/code/moosedev/ontologies"
NLQ_URL="${MOOSEDEV_LLM_BASE_URL:-http://yavin:1234/v1}"
NLQ_MODEL="${MOOSEDEV_LLM_MODEL:-google/gemma-4-26b-a4b-qat}"
CANARY_MODEL="${SMOKE_CANARY_MODEL:-openrouter/openai/gpt-5.4-mini}"
CANARY_TASK="${SMOKE_CANARY_TASK:-}"
PASS=0; FAIL=0
ok()   { printf '  \033[32mPASS\033[0m  %s\n' "$1"; PASS=$((PASS+1)); }
bad()  { printf '  \033[31mFAIL\033[0m  %s\n' "$1"; FAIL=$((FAIL+1)); }
warn() { printf '  \033[33mWARN\033[0m  %s\n' "$1"; }
note() { printf '        %s\n' "$1"; }
step() { printf '\n%s\n' "$1"; }

printf '=== rig smoke: %s — %s ===\n' "$CORPUS" "$(date)"

# --- 1. the code compiles, including behind the feature flag ------------------
# `harness` is NOT a default feature, so a plain `cargo build` compiles none of the
# runner, session or executor. Twelve errors once reached a commit that way.
step "1. build"
if [ -n "${SMOKE_SKIP_BUILD:-}" ]; then
  note "skipped (SMOKE_SKIP_BUILD)"
else
  if cargo check --quiet --all-targets 2>/dev/null; then ok "cargo check --all-targets"
  else bad "cargo check --all-targets"; fi
  if cargo check --quiet --all-targets --features harness 2>/dev/null; then
    ok "cargo check --features harness"
  else bad "cargo check --features harness  <- the feature the study runs on"; fi
  if cargo test --quiet --lib sparql 2>/dev/null | grep -q "test result: ok"; then
    ok "empty-result diagnostic tests"
  else bad "empty-result diagnostic tests (src/sparql.rs)"; fi
fi

# --- 2. the release binary the bench actually launches is current -------------
# The bench runs target/release/moosedev, not the debug build the tests exercised.
# A stale binary silently benches yesterday's server against today's conclusions.
step "2. release binary"
if [ ! -x "$BIN" ]; then
  bad "no release binary at $BIN — run: cargo build --release --features harness"
else
  NEWER=$(find ../src ../Cargo.toml -newer "$BIN" -type f 2>/dev/null | head -3)
  if [ -n "$NEWER" ]; then
    bad "release binary is OLDER than source — rebuild before running"
    note "$(echo "$NEWER" | head -3 | tr '\n' ' ')"
  else
    ok "release binary newer than src/ ($("$BIN" --version 2>/dev/null | head -1))"
  fi
fi

# --- 3. corpus: real, not a live trial, and complete --------------------------
step "3. corpus"
if .venv/bin/python - "$CORPUS" <<'PYC'
import sys, config, pathlib
name = sys.argv[1]
c = config.CORPORA.get(name)
if c is None:
    print(f"        corpus {name} is not registered in config.py"); sys.exit(1)
dd = pathlib.Path(c["data_dir"])
# A live trial store must never be read or written by a bench campaign, and the check is
# on the CONFIG: a live store's content hash changes under the trial's own writes, so hash
# equality proves nothing (Lesson d71099a1).
if "-trial" in str(dd):
    print(f"        {name} resolves to a LIVE TRIAL store {dd}"); sys.exit(1)
print(f"        corpus -> {dd} (not a trial store)")
broken = False
tasks = pathlib.Path(config.corpus_tasks_path(name))
n = len(list(tasks.glob("*.json"))) if tasks.exists() else 0
print(f"        {n} task files under {tasks}")
broken |= n == 0
# A store with no substrate index returns EMPTY file dossiers from file_entity_iris,
# which reads as "the harness delivered nothing" rather than as a broken corpus — that
# artifact once inverted a reported finding. It only affects surfaces that resolve code
# entities (harness push, linked-evidence walk, policy push); a tooluse rung asks the
# memory tools directly and never touches them. So it is a warning with a named blast
# radius, not a blocker: crying wolf on a tooluse run would train us to ignore it.
sub = dd / "substrate"
print(f"        substrate index: {'present' if sub.exists() else 'MISSING (exit 2)'}")
sys.exit(1 if broken else (0 if sub.exists() else 2))
PYC
then ok "corpus registered, not a trial store, tasks + substrate present"
else
  case $? in
    2) warn "no substrate index: walk/harness/policy cells would get EMPTY dossiers"
       note "harmless for a tooluse rung; rebuild the index before any walk arm" ;;
    *) bad "corpus unusable (detail above)" ;;
  esac
fi

# --- 4. daemon: up, and the store actually answers ---------------------------
# By pidfile, never by pattern: a broad `pkill -f "moosedev --serve"` also kills the
# daemon serving this repo's own dogfooding graph. Learned twice.
step "4. daemon + store"
# A running daemon is only reusable if it is running CURRENT code. One started before the
# last build serves yesterday's server to today's conclusions, and nothing in the scores
# would ever show it — the same stale-artifact trap as check 2, one layer out.
STALE_PID=""
if [ -S "$DD/moosedev.sock" ] && [ -f "$DD/moosedev-serve.pid" ]; then
  DPID=$(cat "$DD/moosedev-serve.pid" 2>/dev/null)
  if [ -n "$DPID" ] && kill -0 "$DPID" 2>/dev/null; then
    # No /proc on macOS: compare the binary's mtime against the process start time.
    STARTED=$(ps -o lstart= -p "$DPID" 2>/dev/null)
    BIN_EPOCH=$(stat -f %m "$BIN" 2>/dev/null || echo 0)
    PROC_EPOCH=$(date -j -f "%a %b %d %T %Y" "$STARTED" +%s 2>/dev/null || echo 0)
    if [ "$PROC_EPOCH" -gt 0 ] && [ "$BIN_EPOCH" -gt "$PROC_EPOCH" ]; then STALE_PID="$DPID"; fi
  fi
fi
if [ -n "$STALE_PID" ]; then
  note "daemon $STALE_PID predates the current binary — restarting it"
  kill "$STALE_PID" 2>/dev/null
  for _ in $(seq 1 15); do kill -0 "$STALE_PID" 2>/dev/null || break; sleep 1; done
  kill -0 "$STALE_PID" 2>/dev/null && kill -9 "$STALE_PID" 2>/dev/null
  rm -f "$DD/moosedev.sock" "$DD/moosedev-serve.pid"
fi
if [ -S "$DD/moosedev.sock" ]; then
  ok "daemon socket up and current (reusing)"
else
  note "starting daemon (a cold store can take several minutes to hydrate)…"
  MOOSEDEV_DATA_DIR="$DD" MOOSEDEV_ONTOLOGY_DIR="$ONTO" \
    MOOSEDEV_LLM_BASE_URL="$NLQ_URL" MOOSEDEV_LLM_API_KEY=lmstudio \
    MOOSEDEV_LLM_MODEL="$NLQ_MODEL" \
    nohup "$BIN" --serve > "/tmp/smoke_serve_${CORPUS}.log" 2>&1 &
  for _ in $(seq 1 600); do [ -S "$DD/moosedev.sock" ] && break; sleep 1; done
  if [ -S "$DD/moosedev.sock" ]; then ok "daemon came up"
  else bad "daemon never came up"; tail -3 "/tmp/smoke_serve_${CORPUS}.log"; fi
fi

# --- 5. endpoints: reachable, and serving the EXACT model ids we name --------
# An unloaded model answers 400, run.py still writes a row, and that row scores 0.0 —
# indistinguishable in the table from a model that tried and failed.
step "5. endpoints"
if .venv/bin/python - "$NLQ_URL" "$NLQ_MODEL" <<'PYN'
import json, sys, urllib.request
url, want = sys.argv[1].rstrip("/"), sys.argv[2]
try:
    served = urllib.request.urlopen(f"{url}/models", timeout=10).read()
    ids = {m.get("id") or m.get("key") for m in json.loads(served).get("data", [])}
    if want not in ids:
        print(f"        NLQ model '{want}' NOT served at {url}")
        print(f"        every moosedev_query would 400 and B2 would run with its symbolic")
        print(f"        answer path dead — invisible in the scores. served: "
              f"{sorted(i for i in ids if i)[:6]}")
        sys.exit(1)
except Exception as e:
    print(f"        NLQ endpoint {url} unreachable: {e}"); sys.exit(1)
PYN
then ok "NLQ model '$NLQ_MODEL' served at $NLQ_URL"
else bad "NLQ dead or misnamed — moosedev_query would fail silently all campaign"; fi

# --- 6. headroom -------------------------------------------------------------
# Four resident models and 2.9 GB free once had the OS kill the MCP children
# mid-campaign, voiding 14 cells.
step "6. memory"
FREE=$(vm_stat | awk '/page size/{ps=$8} /Pages free/{f=$3} /Pages inactive/{i=$3} \
        END{gsub(/\./,"",f); gsub(/\./,"",i); printf "%.0f",(f+i)*ps/1073741824}')
if [ "$FREE" -ge 8 ]; then ok "${FREE} GB free+inactive"
else bad "${FREE} GB free+inactive — the OS killed MCP children at 2.9 GB once"; fi

# --- 7. the canary: one real scored cell, end to end -------------------------
# The single check that exercises everything at once — daemon, MCP child launch, prompt
# assembly, tool calls, grading, and the no-progress abort. A control model is used on
# purpose: this asks "does the rig work", never "is the model good".
step "7. canary cell ($CANARY_MODEL)"
if [ -z "${OPENROUTER_API_KEY:-}" ] && [ -z "${SMOKE_CANARY_MODEL:-}" ]; then
  bad "OPENROUTER_API_KEY unset and no SMOKE_CANARY_MODEL — cannot verify the rig end to end"
  note "a smoke test that skips its most important check is worse than none"
else
  if [ -z "$CANARY_TASK" ]; then
    TDIR=$(.venv/bin/python -c "import config;print(config.corpus_tasks_path('$CORPUS'))")
    CANARY_TASK=$(ls "$TDIR"/*.json | xargs -n1 basename | sed 's/\.json$//' \
                  | grep -E '^set_' | head -1)
  fi
  note "task: $CANARY_TASK"
  export BENCH_WORK_ROOT="${BENCH_WORK_ROOT:-/tmp/smoke_work_$$}"; mkdir -p "$BENCH_WORK_ROOT"
  export BENCH_CELL_TIMEOUT="${BENCH_CELL_TIMEOUT:-600}"
  OUT=$(.venv/bin/python run.py --corpus "$CORPUS" --task "$CANARY_TASK" --arm B2 \
          --mode tooluse --backend opencode --model "$CANARY_MODEL" 2>&1)
  echo "$OUT" | grep -E "CELL FAILED|score=" | sed 's/^/        /'
  SCORE=$(echo "$OUT" | grep -oE "score=[0-9.]+" | head -1 | cut -d= -f2)
  TOOLS=$(echo "$OUT" | grep -oE "moosedev_[a-z_]+" | sort -u | tr '\n' ' ')
  if echo "$OUT" | grep -q "CELL FAILED"; then
    bad "the cell never reached the model (see CELL FAILED above)"
  elif [ -z "${SCORE:-}" ]; then
    bad "no score row — run.py did not complete the cell"
  elif awk "BEGIN{exit !($SCORE > 0)}"; then
    ok "canary scored $SCORE"
  else
    bad "canary scored $SCORE with a CONTROL model — the rig is wrong, not the model"
  fi
  if [ -n "$TOOLS" ]; then ok "memory server answered: $TOOLS"
  else bad "ZERO moosedev_* calls — a dead MCP reads exactly like a weak tool-caller"; fi
  if echo "$OUT" | grep -q "ABORTED"; then
    bad "the no-progress abort fired on a CONTROL cell — it is too aggressive"
  else ok "no-progress abort stayed quiet"; fi
fi

# --- verdict -----------------------------------------------------------------
printf '\n=== %d passed, %d failed ===\n' "$PASS" "$FAIL"
[ "$FAIL" -eq 0 ] && printf 'rig is good — safe to start the campaign\n' \
                  || printf '\033[31mDO NOT START THE CAMPAIGN\033[0m — fix the failures above\n'
exit "$FAIL"
