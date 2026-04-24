#!/usr/bin/env bash
# Tee Claude Code statusline stdin to a JSONL log, then pass stdin through to
# a downstream statusline command.
#
# Usage (in ~/.claude/settings.json):
#   "statusLine": {
#     "type": "command",
#     "command": "/abs/path/to/scripts/statusline-tee.sh -- /abs/path/to/linesmith"
#   }
#
# Label a capture window by exporting LINESMITH_CAPTURE_SCENARIO before
# triggering statusline refreshes:
#   export LINESMITH_CAPTURE_SCENARIO="post-compact"
#
# Each invocation appends one record to
# $LINESMITH_CAPTURE_DIR/stdin.jsonl (default: $HOME/.linesmith-captures/stdin.jsonl):
#   {"captured_at":"2026-04-24T14:32:01Z","scenario":"...","payload":<raw stdin>}
#
# Capture needs jq; the passthrough runs either way.

set -u

CAPTURE_DIR="${LINESMITH_CAPTURE_DIR:-$HOME/.linesmith-captures}"
CAPTURE_FILE="$CAPTURE_DIR/stdin.jsonl"
SCENARIO="${LINESMITH_CAPTURE_SCENARIO:-}"

# Command substitution strips trailing newlines; the `printf x` sentinel plus
# %x-suffix strip keeps the buffer byte-for-byte identical to stdin.
stdin_buffer="$(cat; printf x)"
stdin_buffer="${stdin_buffer%x}"

if command -v jq >/dev/null 2>&1; then
  mkdir -p "$CAPTURE_DIR" 2>/dev/null || true
  ts="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  # Fall back to --arg raw if stdin_buffer isn't valid JSON.
  {
    jq -cn --arg ts "$ts" --arg scenario "$SCENARIO" --argjson payload "$stdin_buffer" \
      '{captured_at:$ts, scenario:$scenario, payload:$payload}' 2>/dev/null \
    || jq -cn --arg ts "$ts" --arg scenario "$SCENARIO" --arg raw "$stdin_buffer" \
      '{captured_at:$ts, scenario:$scenario, raw:$raw}' 2>/dev/null
  } >> "$CAPTURE_FILE" 2>/dev/null || true
fi

if [ $# -gt 0 ] && [ "$1" = "--" ]; then
  shift
fi

if [ $# -eq 0 ]; then
  exit 0
fi

printf '%s' "$stdin_buffer" | "$@"
