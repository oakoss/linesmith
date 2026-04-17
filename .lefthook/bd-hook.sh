#!/usr/bin/env sh
# Invoke a beads hook with the same safety shim beads uses in .beads/hooks/*.
# Swallow exit 3 (db uninitialized) and exit 124 (timeout) so a broken beads
# setup never blocks a git operation. Honors BEADS_HOOK_TIMEOUT (default 300s)
# when the `timeout` binary is available (not on default macOS; present on
# Linux and on macOS via coreutils).
#
# Usage: bd-hook.sh <event> [args...]

set -u

event="$1"
shift

if command -v timeout >/dev/null 2>&1; then
  timeout "${BEADS_HOOK_TIMEOUT:-300}" bd hooks run "$event" "$@"
else
  bd hooks run "$event" "$@"
fi
exit_code=$?

case "$exit_code" in
  3)
    echo >&2 "beads: database not initialized — skipping hook '$event'"
    exit 0
    ;;
  124)
    echo >&2 "beads: hook '$event' timed out — continuing without beads"
    exit 0
    ;;
  *)
    exit "$exit_code"
    ;;
esac
