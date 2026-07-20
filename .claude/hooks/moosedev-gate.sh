#!/bin/bash
# MOOSEDev PreToolUse gate (v2.2 active-agency adapter for Claude Code).
#
# Zero policy lives here: the hook reports the proposed edit to the MOOSEDev
# daemon's policy engine over HTTP and translates the typed verdict into the
# Claude Code permission contract:
#   Gate::Deny                 -> permissionDecision "deny"
#   Gate::RequireRatification  -> permissionDecision "ask"
#   Allow / anything else      -> exit 0 with no output (no opinion; the
#                                 normal permission flow proceeds)
# Any failure (daemon down, jq missing, malformed input) fails OPEN — a
# memory sidecar must never brick the host.
set -uo pipefail

INPUT=$(cat 2>/dev/null) || exit 0
command -v jq >/dev/null 2>&1 || exit 0
command -v curl >/dev/null 2>&1 || exit 0

DIR="${CLAUDE_PROJECT_DIR:-$PWD}"
ADDR_FILE="$DIR/.moosedev/http.addr"
[ -f "$ADDR_FILE" ] || exit 0
ADDR=$(cat "$ADDR_FILE" 2>/dev/null) || exit 0
[ -n "$ADDR" ] || exit 0

# Identity preflight: http.addr can be crash-stale and the ephemeral port
# since reclaimed by an unrelated local process — never send project payloads
# without confirming a MOOSEDev backend serving THIS data dir answers there.
EXPECT=$(cd "$DIR/.moosedev" 2>/dev/null && pwd -P) || exit 0
SERVED=$(curl -fsS --max-time 2 "http://$ADDR/api/v1/health" 2>/dev/null \
  | jq -r 'select(.status == "ok" and .project_graph == "https://moosedev.dev/kg/project") | .data_dir // empty' 2>/dev/null) || exit 0
[ -n "$SERVED" ] && [ "$SERVED" = "$EXPECT" ] || exit 0

FILE=$(jq -r '.tool_input.file_path // empty' <<<"$INPUT" 2>/dev/null) || exit 0
[ -n "$FILE" ] || exit 0
REL="${FILE#"$DIR"/}"

# The edit's own text anchors the gate to the definitions it overlaps.
BODY=$(jq -c --arg file "$REL" '
  {host: "claude-code", kind: "edit_proposed", file: $file}
  + (if (.tool_input.old_string // "") != "" then {anchor: .tool_input.old_string} else {} end)
' <<<"$INPUT" 2>/dev/null) || exit 0

VERDICT=$(curl -sS --max-time 5 -H 'Content-Type: application/json' \
  -d "$BODY" "http://$ADDR/api/v1/policy" 2>/dev/null) || exit 0

DECISION=$(jq -r '.decision // empty' <<<"$VERDICT" 2>/dev/null) || exit 0
[ "$DECISION" = "gate" ] || exit 0

DISPOSITION=$(jq -r '.disposition // empty' <<<"$VERDICT")
REASON=$(jq -r '.reason // "a recorded constraint governs this code"' <<<"$VERDICT")
case "$DISPOSITION" in
  deny) PERM="deny" ;;
  *) PERM="ask" ;;
esac

jq -n --arg perm "$PERM" --arg reason "MOOSEDev gate: $REASON" '{
  hookSpecificOutput: {
    hookEventName: "PreToolUse",
    permissionDecision: $perm,
    permissionDecisionReason: $reason
  }
}'
exit 0
