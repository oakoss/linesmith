#!/usr/bin/env sh
# Invoke a beads hook with the same safety shim beads uses in .beads/hooks/*.
#
# Swallows a missing `bd`, an uninitialized database (exit 3), a timeout
# (124), and a rejected timeout interval (125). Every other non-zero status
# still fails the hook — unlike bd-dolt-push.sh, which never does — because a
# beads hook that errors mid-write has touched the database and silently
# continuing would leave it inconsistent.
#
# Honors BEADS_HOOK_TIMEOUT (default 300s) when the `timeout` binary is
# available (not on default macOS; present on Linux and on macOS via
# coreutils).
#
# Usage: bd-hook.sh <event> [args...]

set -u

event="$1"
shift

# Contributors clone without beads. Without this, `bd hooks run` exits 127,
# which the case below propagates — failing the hook and blocking their git
# operation outright, for a tool they never opted into.
command -v bd >/dev/null 2>&1 || exit 0

# Validated here as well as in bd-dolt-push.sh, because this script runs
# first in the pre-push chain: GNU `timeout` rejects a malformed interval
# with exit 125, which the `*)` arm below would propagate and block the
# push before the friendlier message downstream could ever print.
limit="${BEADS_HOOK_TIMEOUT:-300}"
case "$limit" in
  '' | *[!0-9]*)
    echo >&2 "beads: BEADS_HOOK_TIMEOUT='${limit}' is not a whole number of seconds — using 300 for this hook"
    limit=300
    ;;
esac
[ "$limit" -gt 0 ] || limit=300

if command -v timeout >/dev/null 2>&1; then
  timeout "$limit" bd hooks run "$event" "$@"
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
  125)
    echo >&2 "beads: 'timeout' rejected the interval for hook '$event' — continuing without beads"
    exit 0
    ;;
  *)
    exit "$exit_code"
    ;;
esac
