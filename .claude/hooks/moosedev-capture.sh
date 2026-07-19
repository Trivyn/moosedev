#!/bin/bash
# MOOSEDev Stop-hook journal (v2.2 active-agency adapter for Claude Code).
#
# At a session checkpoint (the Stop event), reports the uncommitted changed
# files PLUS the host's own summary — the assistant's final message from the
# transcript (extract, never interrogate) — to the daemon's session-journal
# surface. The daemon appends ONE fire-telemetry line and NEVER writes the
# graph: a session's final message is a status report, not a decision
# (Lesson 641c1811, AD 007dce15); records are minted only by deliberate
# calls. No extractable summary → nothing to journal. Debounced: skips
# unless 10 minutes have passed since the last journal entry AND the change
# set differs from the one already journaled. Zero policy lives here; any
# failure fails OPEN (exit 0, no output).
set -uo pipefail

command -v jq >/dev/null 2>&1 || exit 0
command -v curl >/dev/null 2>&1 || exit 0
command -v git >/dev/null 2>&1 || exit 0

# Stop-hook payload arrives on stdin. Prefer the host's explicit final-message
# field; older Claude Code payloads fall back to transcript extraction.
PAYLOAD=$(cat 2>/dev/null || true)
TRANSCRIPT=$(printf '%s' "$PAYLOAD" | jq -r '.transcript_path // empty' 2>/dev/null)

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

# Tracked changes AND untracked files: a session of purely new files is still
# a checkpoint worth journaling. .moosedev state (kg.nq re-exports) is not work.
FILES=$({ git -C "$DIR" diff --name-only HEAD -- . ':(top,exclude).moosedev/**' 2>/dev/null; \
          git -C "$DIR" ls-files --others --exclude-standard -- . ':(top,exclude).moosedev/**' 2>/dev/null; } | sort -u | head -20)
[ -n "$FILES" ] || exit 0

# The host's own summary: the last assistant text in the transcript, with
# useful multiline structure retained but capped. Nothing extractable → skip.
SUMMARY=$(printf '%s' "$PAYLOAD" | jq -r \
  'select((.last_assistant_message // null) | type == "string") | .last_assistant_message' \
  2>/dev/null || true)
if [ -z "$SUMMARY" ] && [ -n "$TRANSCRIPT" ] && [ -f "$TRANSCRIPT" ]; then
  SUMMARY=$(tail -n 300 "$TRANSCRIPT" 2>/dev/null | jq -rs '
    [ .[]
      | select(.type == "assistant")
      | .message.content
      | if type == "array"
        then ([.[] | select(.type == "text") | .text] | join("\n"))
        else empty end
      | select(length > 0)
    ] | last // empty' 2>/dev/null)
fi
# Remove leading/trailing blank lines, cap each line to keep pathological host
# output bounded, then cap the whole summary without flattening bullets or
# paragraphs. 2 KiB is enough for a useful journal line.
SUMMARY=$(printf '%s\n' "$SUMMARY" | awk '
  BEGIN { started = 0; bytes = 0; cap = 2048 }
  {
    sub(/\r$/, "")
    if (!started && $0 ~ /^[[:space:]]*$/) next
    started = 1
    line = substr($0, 1, 500)
    pending[++n] = line
  }
  END {
    while (n > 0 && pending[n] ~ /^[[:space:]]*$/) n--
    for (i = 1; i <= n; i++) {
      sep = (i == 1 ? "" : "\n")
      remaining = cap - bytes - length(sep)
      if (remaining <= 0) break
      text = substr(pending[i], 1, remaining)
      printf "%s%s", sep, text
      bytes += length(sep) + length(text)
      if (length(text) < length(pending[i])) break
    }
  }')
[ -n "$SUMMARY" ] || exit 0

# Debounce: one journal entry per change-set per 10 minutes.
STAMP="$DIR/.moosedev/capture.stamp"
HASH=$(printf '%s' "$FILES" | cksum | cut -d' ' -f1)
NOW=$(date +%s)
if [ -f "$STAMP" ]; then
  read -r LAST_HASH LAST_TS <"$STAMP" || true
  if [ "${LAST_HASH:-}" = "$HASH" ]; then exit 0; fi
  case "${LAST_TS:-}" in
    ''|*[!0-9]*) ;;
    *) if [ $((NOW - LAST_TS)) -lt 600 ]; then exit 0; fi ;;
  esac
fi

BODY=$(printf '%s\n' "$FILES" | jq -cRn --arg summary "$SUMMARY" \
  '{host: "claude-code", summary: $summary, files: [inputs | select(length > 0)]}')
# --fail: only a confirmed 2xx earns the debounce stamp — stamping an HTTP
# error would suppress retries of this change set indefinitely.
curl -sS --fail --max-time 10 -H 'Content-Type: application/json' \
  -d "$BODY" "http://$ADDR/api/v1/capture" >/dev/null 2>&1 || exit 0

printf '%s %s\n' "$HASH" "$NOW" >"$STAMP"
exit 0
