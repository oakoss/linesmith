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

# `bd dolt push` targets `sync.remote` from .beads/config.yaml, which is
# fixed — it does not follow the remote `git push` was aimed at. Without
# this guard, `git push fork ...` would publish issue state to the
# canonical repo. Skip rather than guess; an issue-only session needs the
# explicit command anyway.
if [ "$remote" != "origin" ]; then
  echo >&2 "beads: skipping dolt push (pushing to '${remote}', not origin)"
  exit 0
fi

# Same shim as bd-hook.sh: `timeout` is absent on default macOS. Dolt's
# git subprocess disables interactive credential prompts but has no
# command timeout of its own, so an unreachable remote would otherwise
# hold `git push` open indefinitely.
if command -v timeout >/dev/null 2>&1; then
  timeout "${BEADS_HOOK_TIMEOUT:-300}" bd dolt push
else
  bd dolt push
fi
exit_code=$?

if [ "$exit_code" -eq 124 ]; then
  echo >&2 "beads: dolt push timed out — run 'bd dolt push' manually"
elif [ "$exit_code" -ne 0 ]; then
  echo >&2 "beads: dolt push failed (exit ${exit_code}) — run 'bd dolt push' manually"
fi

exit 0
