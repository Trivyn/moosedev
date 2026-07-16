#!/bin/bash
# MOOSEDev PostToolUse push (v2.2 active-agency adapter for Claude Code).
#
# Reports the touched file to the daemon's policy engine; when the verdict is
# Inject, adds the returned dossier markdown as additionalContext — the same
# bytes the editor hover shows, by construction. Zero policy lives here; any
# failure fails OPEN (exit 0, no output).
set -uo pipefail

INPUT=$(cat 2>/dev/null) || exit 0
command -v jq >/dev/null 2>&1 || exit 0
command -v curl >/dev/null 2>&1 || exit 0

DIR="${CLAUDE_PROJECT_DIR:-$PWD}"
ADDR_FILE="$DIR/.moosedev/http.addr"
[ -f "$ADDR_FILE" ] || exit 0
ADDR=$(cat "$ADDR_FILE" 2>/dev/null) || exit 0
[ -n "$ADDR" ] || exit 0

FILE=$(jq -r '.tool_input.file_path // empty' <<<"$INPUT" 2>/dev/null) || exit 0
[ -n "$FILE" ] || exit 0
REL="${FILE#"$DIR"/}"

# Repeat suppression: pushing the same file's dossier on every touch is noise,
# not knowledge — one push per file per 10 minutes.
STAMP_DIR="$DIR/.moosedev/push-stamps"
mkdir -p "$STAMP_DIR" 2>/dev/null || exit 0
STAMP="$STAMP_DIR/$(printf '%s' "$REL" | cksum | cut -d' ' -f1)"
NOW=$(date +%s)
if [ -f "$STAMP" ]; then
  read -r LAST_TS <"$STAMP" || true
  if [ $((NOW - ${LAST_TS:-0})) -lt 600 ]; then exit 0; fi
fi

BODY=$(jq -cn --arg file "$REL" \
  '{host: "claude-code", kind: "entity_touched", file: $file}')
VERDICT=$(curl -sS --max-time 5 -H 'Content-Type: application/json' \
  -d "$BODY" "http://$ADDR/api/v1/policy" 2>/dev/null) || exit 0

DECISION=$(jq -r '.decision // empty' <<<"$VERDICT" 2>/dev/null) || exit 0
[ "$DECISION" = "inject" ] || exit 0
# Cap the payload: keep the entity-exact head, drop the long component tail.
DOSSIER=$(jq -r '.dossier_markdown // empty' <<<"$VERDICT" | head -c 6000)
[ -n "$DOSSIER" ] || exit 0

printf '%s\n' "$NOW" >"$STAMP"
jq -n --arg context "$DOSSIER" '{
  hookSpecificOutput: {
    hookEventName: "PostToolUse",
    additionalContext: ("Recorded project knowledge for this code (MOOSEDev):\n" + $context)
  }
}'
exit 0
