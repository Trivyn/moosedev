#!/bin/bash
# MOOSEDev Stop-hook capture (v2.2 active-agency adapter for Claude Code).
#
# At a session checkpoint (the Stop event), reports the uncommitted changed
# files to the daemon's grounded-capture surface, which mints a PROPOSED
# record for human ratification — never auto-accepted, never interrogated
# from the LLM. Debounced: skips unless 10 minutes have passed since the last
# capture AND the change set differs from the one already captured. Zero
# policy lives here; any failure fails OPEN (exit 0, no output).
set -uo pipefail

command -v jq >/dev/null 2>&1 || exit 0
command -v curl >/dev/null 2>&1 || exit 0
command -v git >/dev/null 2>&1 || exit 0

DIR="${CLAUDE_PROJECT_DIR:-$PWD}"
ADDR_FILE="$DIR/.moosedev/http.addr"
[ -f "$ADDR_FILE" ] || exit 0
ADDR=$(cat "$ADDR_FILE" 2>/dev/null) || exit 0
[ -n "$ADDR" ] || exit 0

FILES=$(git -C "$DIR" diff --name-only HEAD 2>/dev/null | head -20)
[ -n "$FILES" ] || exit 0

# Debounce: one capture per change-set per 10 minutes.
STAMP="$DIR/.moosedev/capture.stamp"
HASH=$(printf '%s' "$FILES" | cksum | cut -d' ' -f1)
NOW=$(date +%s)
if [ -f "$STAMP" ]; then
  read -r LAST_HASH LAST_TS <"$STAMP" || true
  if [ "${LAST_HASH:-}" = "$HASH" ]; then exit 0; fi
  if [ $((NOW - ${LAST_TS:-0})) -lt 600 ]; then exit 0; fi
fi

BODY=$(printf '%s\n' "$FILES" | jq -cRn \
  '{host: "claude-code", files: [inputs | select(length > 0)]}')
curl -sS --max-time 10 -H 'Content-Type: application/json' \
  -d "$BODY" "http://$ADDR/api/v1/capture" >/dev/null 2>&1 || exit 0

printf '%s %s\n' "$HASH" "$NOW" >"$STAMP"
exit 0
