#!/usr/bin/env sh
# Replicate the beads Dolt database on `git push`.
#
# bd's own pre-push chain does not do this (verified 2026-08-05: a hook
# run left the remote's refs/dolt/data unchanged; an explicit push
# advanced it), so issue state otherwise only reaches the remote when
# someone remembers.
#
# Usage: bd-dolt-push.sh <git-remote-name>
#
# Never fails: shipping code must not depend on the issue tracker being
# reachable. It reports on failure, because a silent sync failure looks
# exactly like a working one.

set -u

remote="${1:-}"
limit="${BEADS_HOOK_TIMEOUT:-300}"

# `bd dolt push` targets `sync.remote` from .beads/config.yaml, which is
# fixed — it does not follow the remote `git push` was aimed at. Without
# this guard, `git push fork ...` would publish issue state to the
# canonical repo. Skip rather than guess; an issue-only session needs the
# explicit command anyway.
if [ "$remote" != "origin" ]; then
  echo >&2 "beads: skipping dolt push (pushing to '${remote}', not origin)"
  exit 0
fi

# Bound the push so an unreachable remote can't hold `git push` open.
# Dolt's git subprocess disables interactive credential prompts but has no
# timeout of its own. Default macOS ships neither `timeout` nor
# `gtimeout`, so the portable watchdog is the path this repo's maintainer
# machine takes — it is the primary implementation, not a degraded
# fallback.
run_bounded() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "$limit" "$@"
    return $?
  fi
  if command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$limit" "$@"
    return $?
  fi

  "$@" &
  cmd_pid=$!
  # Polls rather than sleeping the whole budget in one go. A single
  # `sleep "$limit"` leaves an orphan: killing the watchdog subshell does
  # not reap the `sleep` it spawned, so every push would strand a process
  # for the full timeout and any parent waiting on its children would
  # hang. Polling exits as soon as the command does, and the longest-lived
  # stray is a 1s sleep.
  (
    waited=0
    while [ "$waited" -lt "$limit" ]; do
      kill -0 "$cmd_pid" 2>/dev/null || exit 0
      sleep 1
      waited=$((waited + 1))
    done
    kill -TERM "$cmd_pid" 2>/dev/null
  ) &
  watchdog_pid=$!

  wait "$cmd_pid" 2>/dev/null
  rc=$?

  kill -TERM "$watchdog_pid" 2>/dev/null
  wait "$watchdog_pid" 2>/dev/null

  # 128+SIGTERM: the watchdog fired. Report it as `timeout` would.
  if [ "$rc" -eq 143 ]; then
    return 124
  fi
  return "$rc"
}

run_bounded bd dolt push
exit_code=$?

if [ "$exit_code" -eq 124 ]; then
  echo >&2 "beads: dolt push timed out after ${limit}s — run 'bd dolt push' manually"
elif [ "$exit_code" -ne 0 ]; then
  echo >&2 "beads: dolt push failed (exit ${exit_code}) — run 'bd dolt push' manually"
fi

exit 0
