#!/usr/bin/env sh
# Replicate the beads Dolt database on `git push`.
#
# bd's own pre-push chain does not do this (verified 2026-08-05: a hook
# run left the remote's refs/dolt/data unchanged; an explicit push
# advanced it), so issue state otherwise only reaches the remote when
# someone remembers.
#
# Usage: bd-dolt-push.sh <git-remote-name> [remote-url]
#
# Never fails: shipping code must not depend on the issue tracker being
# reachable. It reports on failure, because a silent sync failure looks
# exactly like a working one.

set -u

remote="${1:-}"
# git's pre-push contract passes the destination URL as the second
# argument. It is authoritative — it already accounts for pushurl and
# insteadOf — so prefer it over looking the remote up again.
remote_url="${2:-}"
limit="${BEADS_HOOK_TIMEOUT:-300}"

# BEADS_HOOK_TIMEOUT is shared with bd-hook.sh, which hands it to GNU
# `timeout` — that accepts suffixes like `5m`, the arithmetic below does
# not. Left unvalidated, `5m` makes every comparison error out and the
# watchdog fires instantly, reporting a network timeout for what is
# really a config typo.
case "$limit" in
  '' | *[!0-9]*)
    echo >&2 "beads: BEADS_HOOK_TIMEOUT='${limit}' is not a whole number of seconds — using 300 for the dolt push"
    limit=300
    ;;
esac
[ "$limit" -gt 0 ] || limit=300

# Contributors clone without beads. Their pushes are not this hook's
# business, and a raw "command not found" is worse than silence.
command -v bd >/dev/null 2>&1 || exit 0

# `bd dolt push` targets `sync.remote`, which is fixed — it does not
# follow the remote `git push` was aimed at. Without a guard, `git push
# fork ...` publishes issue state to the canonical repo, and a fork
# contributor (whose `origin` is their own fork) gets an auth failure on
# every push. Comparing destinations rather than the remote's *name*
# covers both: `origin` is the fork's name in a fresh clone of a fork, so
# a name check alone would miss the case it most needs to catch.
#
# Ask bd for the value rather than parsing .beads/config.yaml: bd resolves
# the repo itself (so this works from any directory) and the answer
# survives quoting styles, comments, and any move to a nested key or to
# database-backed storage.
if ! sync_remote="$(bd config get sync.remote 2>/dev/null)"; then
  # Distinguished from an unset key: a bd whose CLI surface has moved would
  # otherwise be reported as a configuration problem, sending the reader to
  # a config file that is perfectly correct.
  echo >&2 "beads: 'bd config get' failed — skipping dolt push"
  exit 0
fi

# A missing key is printed on stdout with exit 0, in more than one wording
# (`<key> (not set)`, `<key> (not set in config.yaml)`), so match the shape
# rather than one phrasing — an unmatched sentinel would be compared as if
# it were a URL and reported as a remote mismatch.
case "$sync_remote" in
  '' | *'(not set'*) sync_remote='' ;;
esac

if [ -z "$sync_remote" ]; then
  echo >&2 "beads: no sync.remote configured — skipping dolt push"
  exit 0
fi

if [ -z "$remote_url" ]; then
  remote_url="$(git remote get-url --push "$remote" 2>/dev/null || true)"
fi

if [ -z "$remote_url" ]; then
  echo >&2 "beads: no URL for remote '${remote}' — skipping dolt push"
  exit 0
fi

# Reduces the URL forms git and beads each accept for the same repo to one
# string. Imperfect normalization is safe in one direction only: a missed
# equivalence skips a push that could have run, while a false match would
# publish issue state to the wrong repo. Every rule here is written to fail
# toward the skip.
#
# Host and path are handled separately and deliberately. Only the host is
# case-folded: DNS is case-insensitive, but repository paths are not on
# Gitea, GitLab, or any self-hosted forge, so folding the whole URL would
# make `/Team/Repo` and `/team/repo` — two different repositories — compare
# equal. Any port is likewise kept, since the same host on two ports can be
# two different servers.
normalize_remote() {
  # A scheme distinguishes `host:port/path` from scp-style `host:path`;
  # without one, the first colon is a path separator, not a port.
  case "$1" in
    *://*) normalize_scp=0 ;;
    *) normalize_scp=1 ;;
  esac

  normalize_body="$(
    printf '%s' "$1" |
      sed -e 's|^[a-zA-Z][a-zA-Z0-9+.-]*://||' -e 's|^[^/]*@||'
  )"
  [ "$normalize_scp" -eq 1 ] &&
    normalize_body="$(printf '%s' "$normalize_body" | sed -e 's|:|/|')"

  normalize_host="$(printf '%s' "${normalize_body%%/*}" | tr '[:upper:]' '[:lower:]')"
  case "$normalize_body" in
    */*) normalize_path="/${normalize_body#*/}" ;;
    *) normalize_path='' ;;
  esac
  normalize_path="$(
    printf '%s' "$normalize_path" | sed -e 's|/*$||' -e 's|\.git$||' -e 's|/*$||'
  )"

  printf '%s%s' "$normalize_host" "$normalize_path"
}

sync_key="$(normalize_remote "$sync_remote")"
push_key="$(normalize_remote "$remote_url")"

# An empty key means the URL reduced to nothing recognisable. Two such URLs
# would compare equal and authorise a push on the strength of having both
# failed to parse.
if [ -z "$sync_key" ] || [ -z "$push_key" ]; then
  echo >&2 "beads: could not compare '${remote}' against the sync remote — skipping dolt push"
  exit 0
fi

if [ "$sync_key" != "$push_key" ]; then
  echo >&2 "beads: skipping dolt push ('${remote}' is not the beads sync remote)"
  exit 0
fi

# Used where the command could not be placed in its own process group; the
# group signal is preferred because it is atomic and needs no external tool.
# dash creates no new group for a background job (it declines job control
# without a controlling terminal), so on any dash-as-/bin/sh system — Ubuntu
# included — this is the live path, not a rarity.
#
# Addresses the target only through "$1"/"$2": POSIX sh has no `local`, so a
# named variable would be clobbered by the recursive call and the signal
# would land on the last child instead of the target.
#
# Descendants are signalled before the target, and that order is load-bearing.
# Killing the target first ends the `wait` in the caller, which then reaps this
# watchdog — mid-walk, leaving the descendants alive. The group-signal path
# has no such race because one signal reaches the whole group at once.
#
# Without `pgrep` this narrows to signalling the target alone, which is why
# the caller warns rather than claiming a clean timeout.
kill_tree() {
  if command -v pgrep >/dev/null 2>&1; then
    kill_tree_children="$(pgrep -P "$1" 2>/dev/null)"
  else
    kill_tree_children=""
  fi
  # The word list is expanded before the loop body runs, so the recursive
  # call reassigning kill_tree_children cannot disturb this iteration.
  for kill_tree_child in $kill_tree_children; do
    kill_tree "$kill_tree_child" "$2"
  done
  kill -"$2" "$1" 2>/dev/null || true
}

# Bound the push so an unreachable remote can't hold `git push` open.
# Dolt's git subprocess disables interactive credential prompts but has no
# timeout of its own. Default macOS ships neither `timeout` nor `gtimeout`,
# so the portable watchdog is the path this repo's maintainer machine takes
# — it is the primary implementation, not a degraded fallback.
#
# Returns 124 for a timeout, 125 when it could not run the command at all.
run_bounded() {
  if command -v timeout >/dev/null 2>&1; then
    timeout "$limit" "$@"
    return $?
  fi
  if command -v gtimeout >/dev/null 2>&1; then
    gtimeout "$limit" "$@"
    return $?
  fi

  # Distinct from a push failure: reporting "dolt push failed (exit 1)" here
  # would send the reader after a push that never ran.
  timed_out="$(mktemp 2>/dev/null)" || return 125
  trap 'rm -f "${timed_out:-}"' EXIT HUP INT TERM

  # Job control puts the command in its own process group, so the watchdog
  # can signal the git subprocess `bd dolt push` spawns rather than only bd
  # itself — an orphaned child would hold the Dolt lock and surface later as
  # an unrelated-looking sync failure. Where the shell declines job control,
  # kill_tree walks the tree instead.
  set -m 2>/dev/null || true
  "$@" &
  cmd_pid=$!
  set +m 2>/dev/null || true

  # Polls rather than sleeping the whole budget in one go. A single
  # `sleep "$limit"` leaves an orphan: killing the watchdog subshell does
  # not reap the `sleep` it spawned, so every push would strand a process
  # for the full timeout and any parent waiting on its children would
  # hang. Polling exits as soon as the command does, and the longest-lived
  # stray is a 1s sleep.
  #
  # Both loops also exit if the main script is gone. Ctrl-C on a slow push
  # kills the foreground group but leaves this watchdog — background jobs
  # started without job control ignore SIGINT — polling for up to limit+5
  # seconds against a PID it no longer owns. If that PID is recycled into a
  # group leader, the escalation below would signal an unrelated group.
  main_pid=$$
  (
    waited=0
    while [ "$waited" -lt "$limit" ]; do
      kill -0 "$main_pid" 2>/dev/null || exit 0
      kill -0 "$cmd_pid" 2>/dev/null || exit 0
      sleep 1
      waited=$((waited + 1))
    done
    # Re-check before claiming the timeout: the command may have finished
    # during the final poll, and reporting a successful push as timed out
    # would have the reader re-run work that already landed.
    kill -0 "$cmd_pid" 2>/dev/null || exit 0
    echo 1 > "$timed_out"
    kill -TERM -"$cmd_pid" 2>/dev/null || kill_tree "$cmd_pid" TERM
    # SIGTERM is a request. Escalate so a wedged process can't outlive the
    # bound the timeout is supposed to enforce. Polled, not slept, for the
    # same reason as above.
    grace=0
    while [ "$grace" -lt 5 ]; do
      kill -0 "$main_pid" 2>/dev/null || exit 0
      kill -0 "$cmd_pid" 2>/dev/null || exit 0
      sleep 1
      grace=$((grace + 1))
    done
    kill -KILL -"$cmd_pid" 2>/dev/null || kill_tree "$cmd_pid" KILL
    # A survivor holds the Dolt lock, and the caller is about to report a
    # tidy timeout. Say so, or the manual retry fails on the lock and looks
    # like an unrelated fault.
    if [ -n "$(pgrep -P "$cmd_pid" 2>/dev/null)" ] || kill -0 "$cmd_pid" 2>/dev/null; then
      echo >&2 "beads: dolt subprocesses survived the timeout — check for a stale lock before retrying"
    fi
  ) &
  watchdog_pid=$!

  wait "$cmd_pid" 2>/dev/null
  rc=$?

  kill -TERM "$watchdog_pid" 2>/dev/null
  wait "$watchdog_pid" 2>/dev/null

  # Read the flag rather than inferring from exit 143: a command TERMed by
  # anything else (Ctrl-C on the push, the OOM killer) exits 143 too, and
  # blaming a timeout would send the reader after the wrong problem.
  if [ -s "$timed_out" ]; then
    return 124
  fi
  return "$rc"
}

run_bounded bd dolt push
exit_code=$?

if [ "$exit_code" -eq 124 ]; then
  echo >&2 "beads: dolt push timed out after ${limit}s — run 'bd dolt push' manually"
elif [ "$exit_code" -eq 125 ]; then
  echo >&2 "beads: no writable temp dir — skipped dolt push; run 'bd dolt push' manually"
elif [ "$exit_code" -ne 0 ]; then
  echo >&2 "beads: dolt push failed (exit ${exit_code}) — run 'bd dolt push' manually"
fi

exit 0
