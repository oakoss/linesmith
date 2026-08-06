#!/usr/bin/env sh
# Contract tests for .lefthook/bd-dolt-push.sh.
#
# This script has now shipped the same bug class twice: a watchdog that
# stranded a process on every push, and a successor that stranded `bd`'s
# child on timeout. Neither was caught by reading the code or by manual
# runs that only checked exit codes — both needed a process-table
# assertion. That is what row "no strays" below exists for; the rest is
# cheap once the fixture exists.
#
# Assertions are on the observable contract — exit status, whether a
# stderr line appeared, whether `bd` ran at all, and stray count. Message
# wording and the internals of run_bounded are deliberately not pinned.
#
# Usage: test-bd-dolt-push.sh [path-to-sh]

set -u

SHELL_UNDER_TEST="${1:-/bin/sh}"
SCRIPT="$(cd "$(dirname "$0")/.." && pwd)/.lefthook/bd-dolt-push.sh"
CANON="git@github.com:oakoss/linesmith.git"
SYNC="git+ssh://git@github.com/oakoss/linesmith.git"

failures=0
tests=0

# Each case runs in a throwaway repo with a stub `bd` and a PATH scrubbed
# of timeout/gtimeout, so the hand-rolled watchdog is always the path
# under test — on Linux the real `timeout` would otherwise mask it.
sandbox=""
setup() {
  sandbox="$(mktemp -d)"
  mkdir -p "$sandbox/bin"
  git -C "$sandbox" init -q
  git -C "$sandbox" remote add origin "$CANON"
  git -C "$sandbox" remote add fork git@github.com:contributor/linesmith.git
  # PATH is built from individual symlinks rather than real bin directories,
  # for two reasons: the real `bd` lives next to `git` and would defeat the
  # bd-absent case, and `timeout` must be genuinely absent so the portable
  # watchdog is what gets exercised (on Linux it would otherwise be found in
  # /usr/bin and the watchdog would never run).
  for util in git sed tr mktemp rm sleep sh pgrep; do
    util_path="$(command -v "$util")" || {
      echo "test harness: required utility '$util' not found" >&2
      exit 1
    }
    ln -s "$util_path" "$sandbox/bin/$util"
  done
  stub_path="$sandbox/bin"
}

teardown() {
  [ -n "$sandbox" ] && rm -rf "$sandbox"
  sandbox=""
}

# The stub answers `bd config get sync.remote`; everything else is the
# caller-supplied body, so a test can make the push hang, fail, or record
# that it ran.
stub_bd_with_config() {
  cat > "$sandbox/bin/bd" <<STUB
#!/bin/sh
if [ "\$1" = "config" ] && [ "\$2" = "get" ]; then
  printf '%s\n' '${1}'
  exit 0
fi
: > "$sandbox/bd-ran"
echo \$\$ > "$sandbox/bd-pid"
${2}
STUB
  chmod +x "$sandbox/bin/bd"
}

run_script() {
  # shellcheck disable=SC2086 # deliberate: args are controlled by callers
  ( cd "$sandbox" && env PATH="$stub_path" "$SHELL_UNDER_TEST" "$SCRIPT" $1 ) \
    > "$sandbox/out" 2> "$sandbox/err"
}

report() {
  tests=$((tests + 1))
  if [ "$1" = pass ]; then
    printf '  ok   %s\n' "$2"
  else
    printf '  FAIL %s\n       %s\n' "$2" "$3"
    [ -s "${sandbox:-}/err" ] && sed 's/^/       stderr: /' "$sandbox/err"
    failures=$((failures + 1))
  fi
}

check() {
  # check <name> <condition-description> <actual> <expected>
  if [ "$3" = "$4" ]; then
    report pass "$1"
  else
    report fail "$1" "$2: expected '$4', got '$3'"
  fi
}

echo "bd-dolt-push contract tests (shell: $SHELL_UNDER_TEST)"

# --- bd absent: silent, exit 0, push never blocked ------------------------
# No stub `bd` is installed, and PATH holds only the sandbox bin.
setup
run_script "origin $CANON"
rc=$?
check "bd absent -> exit 0" "exit status" "$rc" "0"
check "bd absent -> silent" "stderr bytes" "$(wc -c < "$sandbox/err" | tr -d ' ')" "0"
teardown

# --- sync.remote unset: reports rather than silently disabling sync -------
setup
stub_bd_with_config "sync.remote (not set)" "exit 0"
run_script "origin $CANON"
check "sync.remote unset -> exit 0" "exit status" "$?" "0"
check "sync.remote unset -> warns" "stderr non-empty" \
  "$([ -s "$sandbox/err" ] && echo yes || echo no)" "yes"
check "sync.remote unset -> no push" "bd push invoked" \
  "$([ -f "$sandbox/bd-ran" ] && echo yes || echo no)" "no"
teardown

# --- bd itself failing must not be reported as a config problem -----------
setup
printf '#!/bin/sh\nexit 3\n' > "$sandbox/bin/bd"
chmod +x "$sandbox/bin/bd"
run_script "origin $CANON"
check "bd config get fails -> exit 0" "exit status" "$?" "0"
check "bd config get fails -> warns" "stderr non-empty" \
  "$([ -s "$sandbox/err" ] && echo yes || echo no)" "yes"
check "bd config get fails -> blames bd, not the config" "message mentions bd" \
  "$(grep -c "bd config get" "$sandbox/err" || true)" "1"
teardown

# --- the other "(not set)" wording bd emits when config.yaml is absent -----
# Matched by shape, not by exact phrasing: an unmatched sentinel would be
# normalized as if it were a URL and misreported as a remote mismatch.
setup
stub_bd_with_config "sync.remote (not set in config.yaml)" "exit 0"
run_script "origin $CANON"
check "alternate not-set wording -> no push" "bd push invoked" \
  "$([ -f "$sandbox/bd-ran" ] && echo yes || echo no)" "no"
check "alternate not-set wording -> names the config" "message is about config" \
  "$(grep -c "sync.remote configured" "$sandbox/err" || true)" "1"
teardown

# --- destination is not the sync remote: skip, and never invoke bd --------
setup
stub_bd_with_config "$SYNC" "exit 0"
run_script "fork git@github.com:contributor/linesmith.git"
check "fork remote -> exit 0" "exit status" "$?" "0"
check "fork remote -> warns" "stderr non-empty" \
  "$([ -s "$sandbox/err" ] && echo yes || echo no)" "yes"
check "fork remote -> no push" "bd push invoked" \
  "$([ -f "$sandbox/bd-ran" ] && echo yes || echo no)" "no"
teardown

# --- matching destination across differing URL spellings ------------------
# The config says git+ssh://…/ and git says git@…: — the asymmetry this
# guard exists to tolerate.
setup
stub_bd_with_config "$SYNC" "exit 0"
run_script "origin $CANON"
check "matching remote -> exit 0" "exit status" "$?" "0"
check "matching remote -> pushes" "bd push invoked" \
  "$([ -f "$sandbox/bd-ran" ] && echo yes || echo no)" "yes"
check "matching remote -> quiet" "stderr bytes" \
  "$(wc -c < "$sandbox/err" | tr -d ' ')" "0"
teardown

# --- pushurl: git's own URL argument must win over the remote name --------
# A remote whose fetch URL is canonical but which pushes to a fork must
# NOT sync; reading the fetch URL would wrongly match here.
setup
stub_bd_with_config "$SYNC" "exit 0"
git -C "$sandbox" remote add triangular "$CANON"
git -C "$sandbox" config remote.triangular.pushurl git@github.com:contributor/linesmith.git
run_script "triangular git@github.com:contributor/linesmith.git"
check "pushurl -> no push" "bd push invoked" \
  "$([ -f "$sandbox/bd-ran" ] && echo yes || echo no)" "no"
teardown

# --- push fails: reported, but the git push is not blocked ----------------
setup
stub_bd_with_config "$SYNC" "exit 7"
run_script "origin $CANON"
check "push fails -> still exit 0" "exit status" "$?" "0"
check "push fails -> reports" "stderr non-empty" \
  "$([ -s "$sandbox/err" ] && echo yes || echo no)" "yes"
teardown

# --- invalid timeout: rejected, not treated as "kill immediately" ---------
setup
stub_bd_with_config "$SYNC" "exit 0"
( cd "$sandbox" && env PATH="$stub_path" BEADS_HOOK_TIMEOUT=5m \
  "$SHELL_UNDER_TEST" "$SCRIPT" origin "$CANON" ) >/dev/null 2>"$sandbox/err"
check "bad timeout -> still pushes" "bd push invoked" \
  "$([ -f "$sandbox/bd-ran" ] && echo yes || echo no)" "yes"
teardown

# --- timeout fires, and leaves nothing behind -----------------------------
# The load-bearing case. The stub spawns a grandchild that does NOT exec,
# so a watchdog that signals only its direct child strands the sleep.
setup
# The marker is the sleep *duration*, not a comment: `sh -c 'sleep N #tag'`
# is a single simple command, so the shell execs sleep and the tag never
# reaches the process table — a probe matching on it silently finds nothing
# and the case passes no matter what the watchdog does.
# Durations double as process markers and are derived from this run's PID:
# a fixed value would collide with a stray left by an earlier failed run or
# by a concurrent invocation, turning someone else's leak into this run's
# failure (or masking a real one).
child_sleep=$((9000 + $$ % 500))
parent_sleep=$((child_sleep + 500))
marker="sleep $child_sleep"
# The stub outlives its child (a longer sleep, not `wait`) so a tree-kill
# that reaches the grandchild but not the parent is still caught. With
# `& wait` the parent exits on its own the moment the child dies, masking
# exactly that bug.
stub_bd_with_config "$SYNC" "sh -c 'sleep $child_sleep' & sleep $parent_sleep"
start="$(date +%s)"
( cd "$sandbox" && env PATH="$stub_path" BEADS_HOOK_TIMEOUT=2 \
  "$SHELL_UNDER_TEST" "$SCRIPT" origin "$CANON" ) >/dev/null 2>"$sandbox/err"
rc=$?
elapsed=$(( $(date +%s) - start ))
check "timeout -> exit 0" "exit status" "$rc" "0"
# Asserts the timeout is named as such, not merely that something was
# printed: a flag file deleted early (an inherited trap, a lost race) still
# reports — as a generic push failure — and a non-empty check cannot tell
# the two apart.
check "timeout -> reported as a timeout" "message names the timeout" \
  "$(grep -c 'timed out' "$sandbox/err" || true)" "1"
# Both bounds matter. The upper one is the actual contract; the lower one
# catches a fixture that never reached the timeout at all — a stub that
# dies immediately makes every assertion below it vacuous, including the
# stray-process check this suite exists for.
if [ "$elapsed" -ge 2 ] && [ "$elapsed" -le 12 ]; then
  report pass "timeout -> bounded (${elapsed}s)"
elif [ "$elapsed" -lt 2 ]; then
  report fail "timeout -> bounded" \
    "returned in ${elapsed}s — the stub died before the timeout could fire, so this case proved nothing"
else
  report fail "timeout -> bounded" "took ${elapsed}s, expected <= 12s"
fi
sleep 2
strays="$(pgrep -f "$marker" 2>/dev/null | wc -l | tr -d ' ')"
# Asserts on the command's OWN pid, recorded by the stub. Probing its child
# sleep instead would pass whenever that sleep dies, since the stub then
# exits on its own — which is exactly the bug being tested for.
cmd_pid_under_test="$(cat "$sandbox/bd-pid" 2>/dev/null || echo 0)"
if [ "$cmd_pid_under_test" -gt 0 ] && kill -0 "$cmd_pid_under_test" 2>/dev/null; then
  parent_strays=1
else
  parent_strays=0
fi
if [ "$strays" -eq 0 ]; then
  report pass "timeout -> no stray descendants"
else
  report fail "timeout -> no stray descendants" "$strays process(es) survived the timeout"
  pkill -f "$marker" 2>/dev/null
fi
if [ "$parent_strays" -eq 0 ]; then
  report pass "timeout -> command itself reaped"
else
  report fail "timeout -> command itself reaped" \
    "the timed-out command is still running — a tree walk that recurses before signalling its own target hits the children and leaves the parent"
  pkill -f "sleep $parent_sleep" 2>/dev/null
fi
teardown

# --- no writable temp dir: skipped, not reported as a failed push ---------
# `bd dolt push` never runs here, so "dolt push failed" would send the reader
# after a push that was never attempted.
setup
stub_bd_with_config "$SYNC" "exit 0"
rm -f "$sandbox/bin/mktemp"
( cd "$sandbox" && env PATH="$stub_path" \
  "$SHELL_UNDER_TEST" "$SCRIPT" origin "$CANON" ) >/dev/null 2>"$sandbox/err"
check "no mktemp -> exit 0" "exit status" "$?" "0"
check "no mktemp -> names the temp dir, not a push failure" "message" \
  "$(grep -c "temp dir" "$sandbox/err" || true)" "1"
teardown

# --- pgrep absent: still bounded, still exits 0 ---------------------------
# The tree walk narrows to the target alone without pgrep; the contract that
# the push is never blocked must hold regardless.
setup
stub_bd_with_config "$SYNC" "sleep 30"
rm -f "$sandbox/bin/pgrep"
start="$(date +%s)"
( cd "$sandbox" && env PATH="$stub_path" BEADS_HOOK_TIMEOUT=2 \
  "$SHELL_UNDER_TEST" "$SCRIPT" origin "$CANON" ) >/dev/null 2>"$sandbox/err"
rc=$?
elapsed=$(( $(date +%s) - start ))
check "no pgrep -> exit 0" "exit status" "$rc" "0"
if [ "$elapsed" -ge 2 ] && [ "$elapsed" -le 12 ]; then
  report pass "no pgrep -> still bounded (${elapsed}s)"
else
  report fail "no pgrep -> still bounded" "took ${elapsed}s"
fi
teardown

# --- success path leaves no watchdog behind -------------------------------
# A watchdog outliving its command would keep polling in a sleep loop
# reparented to init.
#
# Compares PID *sets*, not counts. A count delta treats any unrelated sleep
# starting or ending inside the window as this script's leak, which flaked
# roughly one run in four. `-x` matches the executable name so that an
# unrelated process merely mentioning "sleep 1" in a long command line is
# not counted either.
setup
stub_bd_with_config "$SYNC" "exit 0"
before="$(pgrep -P 1 -x sleep 2>/dev/null | sort)"
( cd "$sandbox" && env PATH="$stub_path" BEADS_HOOK_TIMEOUT=90 \
  "$SHELL_UNDER_TEST" "$SCRIPT" origin "$CANON" ) >/dev/null 2>&1
sleep 2
after="$(pgrep -P 1 -x sleep 2>/dev/null | sort)"
# Only PIDs present afterwards but not before can be ours.
new_orphans="$(printf '%s\n' "$before" "$before" "$after" | sort | uniq -u | grep -c '[0-9]' || true)"
check "success -> watchdog reaped" "newly orphaned pollers" "$new_orphans" "0"
teardown

echo
if [ "$failures" -eq 0 ]; then
  echo "all $tests checks passed"
  exit 0
fi
echo "$failures of $tests checks failed"
exit 1
