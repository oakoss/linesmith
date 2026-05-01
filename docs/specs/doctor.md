# Doctor

- Status: draft
- Version: 0.1
- Last updated: 2026-04-29
- Driving ADRs: [ADR-0001](../adrs/0001-use-rust-for-runtime.md), [ADR-0010](../adrs/0010-data-fetching-architecture.md), [ADR-0011](../adrs/0011-rate-limit-data-source.md)

## Overview

`linesmith doctor` is the self-diagnostic subcommand. It inspects the user's environment, config, Claude Code integration, credentials, cache state, plugins, and segment/theme registration — reporting each check as PASS / WARN / FAIL with a one-line remediation hint when something's off. The exit code reflects whether any check failed, so CI pipelines and install scripts can gate on it.

Every spec in the data-fetching lane ([data-fetching.md](data-fetching.md), [credentials.md](credentials.md), [rate-limit-segments.md](rate-limit-segments.md), [plugin-api.md](plugin-api.md), [git-segments.md](git-segments.md)) has "linesmith doctor can inspect this" hooks scattered through it. This spec collects those hooks into a single ordered check catalog with explicit criteria. The catalog inspects cache files and exercises every memoized resolver (credentials, OAuth usage, git, etc.) end-to-end — there's no dedicated "memoized value" row because the source checks themselves populate and validate the memo on first access.

Out of scope: remote telemetry; `doctor fix` auto-remediation; `doctor --json` machine-readable output (deferred to v0.2+ unless it falls out of the implementation trivially).

## Requirements

### Functional

- `linesmith doctor` runs a fixed catalog of checks and reports each as PASS / WARN / FAIL
- Each check has an explicit pass/warn/fail criterion documented in §Check catalog
- Each non-PASS result prints a one-line remediation hint
- Exit code contract: any FAIL → exit 1; WARN-only or all-PASS → exit 0
- Default output is a grouped tree with colors and Unicode status glyphs
- `--plain` flag disables colors and Unicode glyphs (ASCII only) for CI / log capture
- All checks — including network probes (endpoint reachability, update check) — run by default; there is no opt-in flag for slow checks
- Running `linesmith init` ends with an automatic `linesmith doctor` invocation; users can opt out with `linesmith init --no-doctor`
- Every failure mode in the data-fetching lane that the ADRs say should "surface via linesmith doctor" is covered by a check here

### Non-functional

- Local-only checks complete in <500ms; total runtime including network probes (one endpoint reachability call + one update check) is bounded at ~5s by per-call timeouts. Doctor is run interactively when something's wrong, not in tight loops, so the network cost is acceptable as a default.
- Fails gracefully when Claude Code isn't installed — reports that as a FAIL, doesn't crash
- Fails gracefully on permission errors — reports the specific path and OS error
- Cross-platform: no Unix-only assumptions (no `security` calls on Linux, no `cmd.exe` calls on macOS)
- Binary-size neutral: doctor code reuses the per-source probing helpers from the data-fetching layer rather than duplicating logic

## Interface / Contract

### CLI surface

```text
linesmith doctor [--plain]

Options:
  --plain      ASCII output without colors or Unicode glyphs (for CI, logs)
  -h, --help   Show this help
```

No positional arguments. Subcommand does one thing; no variants (`doctor check foo`, etc.). All checks run unconditionally — there is no scope-narrowing flag. If users later report friction with the network probes (corporate air-gapped, ultra-tight loops), an `--offline` opt-out is on the table; an opt-in `--full` would force users to remember the flag to get the diagnosis they came for, which inverts the contract.

### Severity levels

| Severity | Glyph | Plain | Color  | Meaning                                                                     |
| -------- | ----- | ----- | ------ | --------------------------------------------------------------------------- |
| PASS     | `✓`   | `OK`  | green  | Check verified; no action required                                          |
| WARN     | `⚠`   | `!!`  | yellow | Degraded but functional; linesmith works with reduced features              |
| FAIL     | `✗`   | `XX`  | red    | Broken; linesmith won't render correctly or at all for the affected surface |
| SKIP     | `·`   | `--`  | gray   | Not applicable in this environment (e.g., Keychain check on Linux)          |

`SKIP` does not affect the exit code.

### Exit code contract

```text
0  all checks PASS or WARN or SKIP (no failures)
1  at least one check FAIL
2  usage error (unrecognized flag, bad invocation)
```

I/O failures while writing the report (broken pipe, ENOSPC, etc.) are surfaced to stderr but do not change the exit code; truncated or missing stdout is the user-visible signal that the writer broke, not the report. This matches the `cli_main` convention used by `themes-list`, `presets-list`, and similar meta-commands.

CI pipelines gate on exit-code 0; install scripts can treat 1 as "setup incomplete, prompt the user to read doctor output."

The linesmith release profile uses `panic = "abort"` per [ADR-0007](../adrs/0007-cargo-dist-distribution.md) for size and predictability. A panicking check therefore terminates the whole `doctor` run via the abort signal — no per-check `catch_unwind` recovery is possible, and the process dies before reaching these exit codes. `doctor` is not meant to be robust against its own bugs; a panic there is a bug report, not a graceful-degradation opportunity. Prior drafts promised `catch_unwind` safety; that promise is withdrawn in favor of keeping the build profile uniform across the binary.

### Output format — default

Grouped by category, tree-structured. Color + Unicode:

```text
linesmith doctor (v0.1.0)

Environment
  ✓ Terminal is a tty (stdout fd 1)
  ✓ Terminal width detected: 120 cells
  ✓ TERM=xterm-256color (256-color capable)
  ✓ NO_COLOR is set — colors disabled per user preference

Config
  ✓ Config file: ~/.config/linesmith/config.toml
  ✓ Parses without errors
  ✓ All 8 enabled segments are registered
  ✓ Theme "catppuccin-mocha" is installed

Claude Code
  ✓ claude binary: /opt/homebrew/bin/claude (v2.1.114)
  ✓ ~/.claude/ exists (mode 755)
  ✓ ~/.claude.json parses (oauthAccount present)
  ✓ 3 recent sessions in ~/.claude/sessions/

Credentials
  ✓ OAuth token found via macOS Keychain ("Claude Code-credentials")
  ✓ Token has required scopes: user:inference, user:sessions:claude_code

Cache
  ✓ ~/.cache/linesmith/ exists (will be created on first fetch)
  · usage.json not yet written (no rate-limit segment enabled)

Plugins
  ✓ 2 plugins discovered in ~/.config/linesmith/segments/
  ✓ my-plugin.rhai parses, declares ["status", "usage"]
  ✓ another.rhai parses, default deps
  · No @data_deps collisions detected

Git
  ✓ Repo detected: /Users/jace/code/linesmith (Main checkout)
  ✓ HEAD: main
  ✓ Upstream: origin/main (0 ahead, 0 behind)

Summary: 16 PASS · 0 WARN · 0 FAIL · 2 SKIP
Exit: 0
```

Failing checks render the remediation hint as an indented second line:

```text
  ✗ ~/.claude.json parse failed: expected string at line 42, column 7
    → Fix: run `claude --version` to repair config, or back up and delete the file
```

### Output format — `--plain`

Same content, no colors, ASCII glyphs:

```text
linesmith doctor (v0.1.0)

Environment
  OK Terminal is a tty (stdout fd 1)
  OK Terminal width detected: 120 cells
  OK TERM=xterm-256color (256-color capable)
  OK NO_COLOR is set -- colors disabled per user preference
...
Summary: 16 PASS / 0 WARN / 0 FAIL / 2 SKIP
Exit: 0
```

**Plain-mode passthrough caveat.** The renderer guarantees no Unicode bytes in the strings it emits (glyphs, separators, fixed labels, summary line). User-supplied label and hint content (paths like `~/café/config`, gix branch names, parser error spans, environment values like `LANG=zh_CN.UTF-8`) passes through verbatim. CI scripts that need byte-clean ASCII should ASCII-fold their environment before invoking doctor, not rely on `--plain` to do it.

### `linesmith init` integration

After `linesmith init` writes the initial config, it prints:

```text
Wrote default config to ~/.config/linesmith/config.toml.

Running doctor to verify your setup...
```

Then invokes `linesmith doctor` (inline, same process) and reports its output. If doctor exits non-zero, init's own exit code propagates it. Users bypass the post-init doctor with `linesmith init --no-doctor`.

## Check catalog

Checks are grouped into eight categories. Within each category, checks run top-to-bottom. A panicking check terminates the whole `doctor` run because the binary is built with `panic = "abort"` (see §Exit code contract for the rationale); a panic is a doctor bug to report, not a user-facing failure mode.

### Environment

| Check                   | PASS                                 | WARN                                          | FAIL           | Hint on non-PASS                                             |
| ----------------------- | ------------------------------------ | --------------------------------------------- | -------------- | ------------------------------------------------------------ |
| Stdout is a tty         | `isatty(1) == true`                  | `isatty(1) == false` (piped / CI)             | —              | `stdout is not a tty; use --plain for CI or log capture`     |
| Terminal width detected | `terminal_size::size()` returns Some | returns `None`, OR `Some((W, _))` with W < 40 | —              | `set $COLUMNS or use --plain; narrow widths may wrap output` |
| TERM is set             | `$TERM` non-empty                    | `$TERM` is `dumb` OR unset                    | —              | `set TERM=xterm-256color, or accept plain-mode fallback`     |
| NO_COLOR respected      | Either unset, or set and honored     | —                                             | —              | `NO_COLOR` is a user preference, never a warning             |
| $HOME resolves          | `dirs::home_dir()` returns Some      | —                                             | returns `None` | `set $HOME to your user directory`                           |

### Config

| Check                         | PASS                                                            | WARN                                             | FAIL                                    | Hint on non-PASS                                                   |
| ----------------------------- | --------------------------------------------------------------- | ------------------------------------------------ | --------------------------------------- | ------------------------------------------------------------------ |
| Config file discovered        | Found per config.md cascade                                     | None found, using built-in defaults              | —                                       | `run linesmith init to create a config`                            |
| Config parses                 | TOML parse succeeds                                             | —                                                | TOML parse error                        | `see line/column in the error for the invalid key`                 |
| All referenced segments exist | Every id in `[line.segments]` is registered                     | Unknown segment id (config ignored)              | —                                       | `remove the unknown id, or install the plugin that provides it`    |
| Theme is installed            | `[theme] name` maps to a known theme                            | Theme unknown, falling back to default           | —                                       | `linesmith themes list shows available names`                      |
| Plugin dirs are readable      | Every explicitly configured `plugin_dir` exists and is readable | An explicitly configured `plugin_dir` is missing | Permission denied on any referenced dir | `mkdir -p <path> or remove the entry from config.toml plugin_dirs` |

### Claude Code

| Check                    | PASS                                       | WARN                                      | FAIL                                    | Hint on non-PASS                                  |
| ------------------------ | ------------------------------------------ | ----------------------------------------- | --------------------------------------- | ------------------------------------------------- |
| `claude` binary found    | `which claude` succeeds                    | Not found, but `~/.claude/` exists anyway | Neither binary nor `~/.claude/` present | `install Claude Code from https://claude.ai/code` |
| `~/.claude/` directory   | Exists and readable                        | Exists with permission issues             | Missing                                 | `launch Claude Code at least once to create it`   |
| `~/.claude.json` parses  | Valid JSON, `oauthAccount` block present   | JSON valid but `oauthAccount` missing     | Parse error                             | delete + recreate, or run `claude` to regenerate  |
| Recent sessions recorded | At least one file in `~/.claude/sessions/` | Directory exists but empty                | Directory missing                       | `open a new Claude Code session to populate`      |

### Credentials

| Check                   | PASS                                              | WARN                                        | FAIL                                          | Hint on non-PASS                                     |
| ----------------------- | ------------------------------------------------- | ------------------------------------------- | --------------------------------------------- | ---------------------------------------------------- |
| OAuth token resolvable  | `resolve_credentials()` returns Ok                | —                                           | Returns `NoCredentials` or `SubprocessFailed` | `log in to Claude Code to provision a fresh token`   |
| Token source attested   | `CredentialSource` identifiable (keychain / file) | —                                           | Indeterminate source                          | `rm the stale credentials file and log in again`     |
| Token shape valid       | JSON parse succeeds, `accessToken` non-empty      | —                                           | `ParseError` / `MissingField` / `EmptyToken`  | `rerun claude login to rewrite the credentials file` |
| Required scopes present | Scopes include `user:inference`                   | Scopes present but include deprecated names | Required scope absent                         | `log in again to refresh scopes`                     |

The token value itself is NEVER printed in doctor output — only source, scope list, and shape validity. See credentials.md §Behavior for the redaction contract.

### Cache

| Check                         | PASS                                                                                                                           | WARN                                                                                                                                                     | FAIL | Hint on non-PASS                                                                                                                                                                                                            |
| ----------------------------- | ------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------- | ---- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Cache dir exists or creatable | `~/.cache/linesmith/` exists, OR doesn't exist yet AND first existing ancestor is writable (runtime creates it on first fetch) | (a) Doesn't exist AND first existing ancestor is read-only; OR (b) path exists but is not a directory; OR (c) `metadata()` returned a non-NotFound error | —    | (a) `point $XDG_CACHE_HOME (or $HOME) at a writable location`; (b) `remove or rename the file at the cache path so linesmith can create the cache dir`; (c) `check filesystem permissions on the cache path or its parents` |
| `usage.json` shape current    | `schema_version` matches                                                                                                       | Present but stale schema (will be overwritten)                                                                                                           | —    | `safe to ignore; next fetch rewrites`                                                                                                                                                                                       |
| Lock file is fresh            | Absent, or present with `blocked_until > now`                                                                                  | Present, past `blocked_until` (stale lock)                                                                                                               | —    | `rm ~/.cache/linesmith/usage.lock to clear`                                                                                                                                                                                 |

### Rate-limit endpoint

| Check                           | PASS                                          | WARN                                                                           | FAIL                                       | Hint on non-PASS                                      |
| ------------------------------- | --------------------------------------------- | ------------------------------------------------------------------------------ | ------------------------------------------ | ----------------------------------------------------- |
| Endpoint reachable              | `GET /api/oauth/usage` returns 200            | Response 2s-5s (slow), OR transport-level error (DNS / connect / read timeout) | 4xx other than 429 (definitive bad answer) | `check internet, or Anthropic status page`            |
| Endpoint returns expected shape | Response deserializes into `UsageApiResponse` | Truly-new unknown fields present (forward-compat)                              | Missing required fields                    | `report a linesmith issue; Anthropic changed the API` |
| Rate-limit headers sane         | No 429 returned                               | 429 with a reasonable `Retry-After`                                            | 429 with an abusive `Retry-After` (>1h)    | `slow down: you're hitting the rate limit`            |

Transport-level errors (no network, captive portal, corporate proxy refusal) are WARN, not FAIL: the user's _setup_ isn't broken, their _network_ is. FAIL is reserved for "we reached the endpoint and it gave us a definitive bad answer" (401 revoked token, 4xx server contract violation). This keeps offline users — including CI runs without network egress — at exit 0 unless their credentials are actually broken.

The "Endpoint returns expected shape" WARN row excludes codenamed forward-compat buckets that the live endpoint already ships on every response (`iguana_*`, `omelette_*`, `seven_day_cowork`, `seven_day_omelette`, `tangelo`, etc. — see `docs/research/claude-data-files.md` §Raw data). Those buckets are documented research baseline, not new keys; gating the WARN on the _truly-new_ set (anything outside `KNOWN_BUCKETS ∪ RESEARCH_DOCUMENTED_BUCKETS` in `data_context::usage`) keeps healthy doctor runs quiet while still alerting maintainers to genuinely-unrecognized keys. Refresh `RESEARCH_DOCUMENTED_BUCKETS` whenever the research capture is updated.

### Plugins

| Check                  | PASS                                                    | WARN | FAIL                                | Hint on non-PASS                                         |
| ---------------------- | ------------------------------------------------------- | ---- | ----------------------------------- | -------------------------------------------------------- |
| All plugins compile    | Every discovered `.rhai` parses                         | —    | One or more plugins fail to compile | `the reported plugin file has a syntax error at line N`  |
| All `@data_deps` valid | Every declared name maps to a plugin-accessible DataDep | —    | Any unknown or reserved dep name    | `remove the reserved dep, or fix the typo in @data_deps` |
| No id collisions       | Every plugin's `ID` is unique                           | —    | Two plugins share an id             | `rename one of the plugin files' ID const`               |
| No built-in collisions | No plugin id matches a built-in segment                 | —    | Plugin id matches a built-in        | `rename the plugin (built-in wins)`                      |

### Git

| Check             | PASS                                     | WARN                                               | FAIL                                      | Hint on non-PASS                                                   |
| ----------------- | ---------------------------------------- | -------------------------------------------------- | ----------------------------------------- | ------------------------------------------------------------------ |
| cwd git status    | `gix::discover(cwd)` finds a repo        | Not in a repo (SKIP if no git\_\* segment enabled) | Repo found but `gix::open` fails          | `repair the repo; gix reported <cause>`                            |
| HEAD resolves     | HEAD is `Branch` / `Detached` / `Unborn` | —                                                  | HEAD unresolvable (`.git/HEAD` corrupt)   | `edit .git/HEAD manually or re-clone`                              |
| RepoKind detected | Main / LinkedWorktree / Bare reported    | —                                                  | gix error during repo-kind classification | file a linesmith bug with the output of `linesmith doctor --plain` |

### Self

| Check            | PASS                                         | WARN                                                                                         | FAIL | Hint on non-PASS                             |
| ---------------- | -------------------------------------------- | -------------------------------------------------------------------------------------------- | ---- | -------------------------------------------- |
| Binary path      | `std::env::current_exe()` returns Ok         | `current_exe()` failed (sandbox / permissions / deleted exe — error message included)        | —    | check sandbox / permissions or reinstall     |
| Update available | Running on latest release                    | Newer release available on GitHub, OR transport-level error reaching the GitHub releases API | —    | `run brew upgrade linesmith (or equivalent)` |
| Binary integrity | `linesmith --version` matches build metadata | Build metadata missing (unusual)                                                             | —    | `reinstall from a canonical source`          |

**Binary path vs. Binary integrity.** The first check answers "do we know where this binary lives" — a precondition for any reinstall guidance and for diagnostic context the user copies into bug reports. The second answers "does our advertised version match what was baked in" — a tamper / corruption check that requires build metadata (vergen / built / option_env\!("LINESMITH_BUILD_SHA")). They're independent and can land in separate slices.

## Behavior

### Check ordering

Categories run in the order shown above (Environment → Config → Claude Code → Credentials → Cache → Rate-limit endpoint → Plugins → Git → Self). Within a category, checks run top-to-bottom. Ordering matters when a later check depends on an earlier one: a FAIL on "Config file discovered" short-circuits the rest of the Config category (all subsequent Config checks render as SKIP with reason "config not loaded").

### Cross-category short-circuits

| If this check FAILs  | Downstream checks SKIP with reason                                                                                                           |
| -------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| `$HOME resolves`     | Claude Code, Credentials, Cache (all path-from-`$HOME`); Config only when no `--config` / `$LINESMITH_CONFIG` / XDG override resolved a path |
| `~/.claude/` missing | Recent sessions, Credentials (file path), Claude-state cache                                                                                 |
| Config parse fails   | Segment / Theme / Plugin-dirs checks                                                                                                         |
| OAuth token missing  | Rate-limit endpoint checks (can't probe without token)                                                                                       |

The Config carve-out matters because Config has explicit override sources (`--config`, `$LINESMITH_CONFIG`, `$XDG_CONFIG_HOME`) that bypass the `$HOME`-anchored cascade. When the cascade resolves a path, the Config category runs against that path regardless of `$HOME`'s state — gating it on `$HOME` would silently SKIP a config the user explicitly named, hiding real parse / theme / plugin failures.

### Timing

Local-only checks complete in <500ms total on a warm filesystem. Each check's budget:

- Environment: sub-ms per check (all in-process)
- Config: <5ms (one file parse)
- Claude Code: <10ms (stat + parse `~/.claude.json`)
- Credentials: <200ms on macOS (one `security` subprocess on first call, memoized); <1ms on Linux/Windows (file read)
- Cache: <1ms (stat only)
- Plugins: <3ms per plugin (rhai parse)
- Git: <15ms (gix discover + HEAD resolve)
- Rate-limit endpoint: ~2s worst case (2s timeout per rate-limit-segments.md)
- Self: <1ms for local checks; the GitHub update check adds up to 2s
- Total worst case including network: ~5s

### Color / glyph selection

Default (`--plain` absent):

- Terminal has 16-color support or better (TERM is set, not `dumb`): colors + Unicode glyphs
- `NO_COLOR` set: colors disabled, Unicode glyphs still rendered (Unicode support is orthogonal to color support)
- TERM unset or `dumb`: colors disabled and Unicode glyphs replaced with ASCII (the terminal can't render either reliably)

`--plain` always uses ASCII + no colors, regardless of terminal capability.

### Panic behavior

Per [ADR-0007](../adrs/0007-cargo-dist-distribution.md), linesmith's release profile uses `panic = "abort"`, so a panicking check terminates the whole `doctor` run immediately — no `catch_unwind` recovery, no partial output. The checks themselves are written defensively (stat before read, match on Option/Result, no unwraps on user input) so panics indicate a doctor bug, not a user-environment problem. Treat any panic-terminated run as a bug report and include the panic message.

### JSON output (deferred)

`--json` is reserved for v0.2. The v0.1 implementation populates each check's `id` field today (so adding `--json` later is purely a serializer + flag, not a refactor). When implemented, the shape will be:

```json
{
  "linesmith_version": "0.1.0",
  "exit_code": 0,
  "summary": { "pass": 15, "warn": 1, "fail": 0, "skip": 2 },
  "categories": [
    {
      "name": "Environment",
      "checks": [
        {
          "id": "env.stdout_tty",
          "label": "Stdout is a tty",
          "severity": "pass",
          "detail": "isatty(1) == true",
          "hint": null
        }
      ]
    }
  ]
}
```

The `id` field is stable across versions for CI-script consumers. Convention: `<category>.<check_name>` in snake_case (e.g. `env.stdout_tty`, `config.parses`, `creds.token_resolvable`, `cache.usage_json_present`, `endpoint.reachable`, `plugins.compile`, `git.repo_detected`, `self.version`). Each category prefix matches its §Check catalog section name lowercased; the suffix names what the check verifies, not what it does (i.e., `env.stdout_tty` not `env.check_stdout_is_tty`). Once a slice publishes an `id`, it's locked — renames are a breaking change for consumers.

## Edge cases

| Case                                                                 | Handling                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| -------------------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `linesmith doctor` run inside a CI container                         | Terminal width unknown, `$TERM=dumb` → WARN each; many checks still PASS                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `~/.claude.json` is a 500MB file (pathological)                      | Cap read at 2MB; FAIL with "file too large — likely corrupt"                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| Keychain prompt appears on macOS during Credentials check            | First-run expected; if user denies, report as FAIL with `SubprocessFailed` hint                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| Config references a plugin that exists but fails to compile          | Config check PASSes (reference is legal); Plugins check FAILs (compile error)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| Stale `usage.lock` (mtime > 30s)                                     | WARN with path + rm hint; doesn't block                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| Anthropic endpoint returns 401                                       | FAIL (token expired/revoked); hint: `log in to Claude Code to refresh`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| No internet connection                                               | Endpoint reachability → WARN with "no network" (transport error, not a setup problem); update check → WARN; other checks unaffected; exit 0                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| Multiple doctor runs in rapid succession                             | Each is independent; no cross-run state                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| User runs `doctor` from a non-git directory                          | Git category's first check reports "not in a repo"; remaining Git checks SKIP. No error.                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Default plugin directory doesn't exist (no `plugin_dirs` configured) | SKIP the Plugins category with reason "no plugins configured"; not a failure                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                            |
| `~/.claude/sessions/{pid}.json` with a PID from a dead process       | Neutral — the stale entry doesn't affect doctor's checks                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| Terminal width < 80 cells                                            | Tree output wraps; no horizontal-scroll guarantees                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `doctor` invoked with unrecognized flag                              | Prints usage; exits 2 (the usage-error code — distinct from FAIL's exit 1)                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| Cache dir is a symlink to a read-only filesystem                     | `metadata()` follows symlinks: a symlink resolving to a non-directory lands in `NotADirectory`; a broken symlink (target missing) resolves to NotFound and lands in `Absent` (or `AbsentParentReadOnly` if the parent walk finds a read-only ancestor); a symlink to a directory whose `permissions().readonly()` returns true catches the read-only state and WARNs as `AbsentParentReadOnly`. Cross-user ownership, POSIX ACLs, and mount-level read-only states that don't surface as the `readonly()` bit still produce a false PASS; the user sees the `lsm_error!` from the runtime's `atomic_write_json` failure on first fetch (visible only with stderr open). |
| A check panics                                                       | Whole `doctor` run aborts (per §Panic behavior — `panic = "abort"` forecloses on `catch_unwind`); treat as a doctor bug, not a degradation surface                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |

## Testing strategy

Follows `AGENTS.md`: inline `#[cfg(test)] mod tests` for unit tests, `tests/doctor_integration.rs` for integration, `insta` for snapshots.

### Unit tests (per check category)

- Severity-determination logic: given a stub source result, the check produces the documented severity
- Remediation hints: every non-PASS path prints the expected hint text (tested against inline fixtures)
- Short-circuit propagation: stubbed `$HOME` missing causes file-based checks to SKIP
- `--plain` rendering: no ANSI codes, no Unicode glyphs in linesmith-emitted strings (user-supplied labels/hints pass through verbatim — see §Output format — `--plain` caveat)

### Integration tests

Fixtures under `tests/fixtures/doctor/`:

- `all_pass/`: synthetic env with everything healthy → exit 0, snapshot output
- `no_claude/`: missing `~/.claude/` → specific FAILs, exit 1
- `stale_lock/`: aged lock file → WARN, exit 0
- `plugin_collision/`: two plugins with the same id → FAIL, exit 1
- `ci_mode/`: TERM=dumb + NO_COLOR set + piped stdout → WARN + SKIP, exit 0

Snapshot each scenario's default and `--plain` output. Assert exit codes match the contract.

### Benchmarks

- Local-only run on the `all_pass` fixture (network checks stubbed out) — criterion target: p95 <500ms

## Open questions

- **`--json` timeline.** The v0.1 scaffold's `CheckResult` already carries a stable `id` field (per §JSON output), so adding a `serde::Serialize` impl + a `--json` flag is purely additive. Land it whenever a real consumer asks; no spec change needed up front.
- **`--verbose` / `-v`.** Diagnostic detail level (e.g. show `$TERM` / `$COLORTERM` dumps, full keychain paths, gix HEAD sha, response headers from the endpoint probe) — orthogonal to `--plain`. Defer until at least one check category reveals data worth gating behind it; pair with `-v` short flag per CLI convention.
- **`--offline` opt-out.** If users in air-gapped or proxy-restricted environments report friction with the always-on network probes, add `--offline` to skip the rate-limit endpoint + GitHub update check. Opt-out for stated need is appropriate; opt-in for speculative need is not.
- **`doctor fix` subcommand.** Auto-remediation for the ten most common FAILs (regenerate config, clear stale lock, etc.) is tempting but opens a large blast-radius surface. Explicitly out of scope for v0.1; revisit once we have real user failure reports.
- **Per-segment probes.** Currently doctor checks segment _registration_, not whether each segment actually renders output for the current StatusContext. A deeper mode would `render()` each enabled segment and flag any that return an error. Defer — the overhead isn't justified until users report hard-to-diagnose rendering bugs.
- **Telemetry opt-in.** A "report this doctor output to Anthropic/linesmith maintainers" hint on fatal FAILs would help us debug installation issues. No PII concerns if we prompt + redact (token, email), but v0.1 sidesteps by making `doctor --plain` output easy to copy manually.
- **Interactive doctor (TUI).** ratatui-driven interactive explorer with expandable details. Nice but optional; v0.1 stays CLI-first.

## Change log

- 2026-04-30: §Cache "dir creatable" check is now read-only stat per `lsm-oqm6`. The previous implementation probe-wrote a temp file in the first existing ancestor directory to predict whether `create_dir_all` would succeed; that violated doctor's read-only contract on fresh-setup runs (e.g. `XDG_CACHE_HOME=/tmp/new-root`). New behavior, all read-only stat: `Exists` PASSes; `Absent` walks the parent chain and PASSes if the first existing ancestor is writable, otherwise WARNs as `AbsentParentReadOnly { parent }` so a setup like `XDG_CACHE_HOME=/proc/cache/...` gets a clear "point $XDG_CACHE_HOME at a writable location" hint instead of silently passing forever. The "no recovery on next run" framing matters here: a failed first fetch leaves the path Absent, so neither the `usage.json` nor `usage.lock` rows can pick up the signal — `AbsentParentReadOnly` is the only place doctor can surface this misconfig. The walk also biases toward WARN when any ancestor returns `PermissionDenied` (the unprivileged-user-with-`XDG_CACHE_HOME=/root/cache` case): a stat-error on `/root/cache` followed by a writable `/root` doesn't mean the runtime can create through it. `NotADirectory` (path exists as a regular file) and `Unreadable` (`metadata()` returned a non-NotFound error) both WARN. The `permissions().readonly()` check is weaker than a probe-write — cross-user ownership (mode 0755 owned by another user) is the dominant blind spot, and POSIX ACLs / BSD-macOS immutable flag / NFS root-squash can also flip a `false` reading to an unwritable runtime — but it catches the dominant misconfig class without violating read-only.
- 2026-04-30: §Self "Binary integrity" check landed (`lsm-l35.10`). Reads the build-time `LINESMITH_BUILD_SHA` env var via compile-time `option_env!` and surfaces a 7-char prefix in the PASS label so users can copy it into bug reports. `Some(sha)` → PASS ("Built from `<sha>` (linesmith `<version>`)"). `None` → WARN with the spec-stated "reinstall from a canonical source" hint — most commonly fired on a local `cargo build` (no release pipeline), occasionally on a binary patched after the fact. WARN, not FAIL, because the dev-build case is the dominant trigger and gating exit-1 there would be hostile to contributors. Long SHAs truncate to 7 chars; short SHAs (test pipelines) pass through untruncated.
- 2026-04-30: §Self "Update available" check landed (`lsm-l35.9`). Probes `https://api.github.com/repos/oakoss/linesmith/releases/latest` with a 2s timeout, `User-Agent: linesmith/<version>`, and a 256 KiB response cap (initial 32 KiB cap truncated the live ~41 KiB response mid-JSON; bumped after smoke testing surfaced the spurious `ParseError` WARN). Compares the upstream `tag_name` (after stripping a leading `v`) against `CARGO_PKG_VERSION` as a numeric `(major, minor, patch)` tuple. PASS when local ≥ upstream tag, WARN when upstream is strictly higher. Transport errors (DNS, timeout, non-2xx HTTP, connect refusal, body cap exhausted) and shape failures (missing `tag_name`, non-JSON body, mixed parseability where one side is semver and the other isn't) both surface as WARN per the always-run / no-FAIL spec rule, so an offline doctor run still exits 0. Comparison falls back to verbatim string equality only when **both** sides are non-semver (date-based or monorepo-prefixed releases on both); a mixed case (e.g. local `1.2.3` against remote `nightly-build`) WARNs with a "couldn't compare" diagnostic rather than guessing. Defensive sanitization: response `tag_name` is stripped of control bytes and clamped to 64 chars before any user-facing rendering, and parse-error / I/O-error diagnostics are clamped to one line + 200 chars to bound any body fragment a `serde_json::Error` could echo.
- 2026-04-30: three review-driven adjustments while landing `lsm-l35.5` (Credentials + Cache + Rate-limit endpoint). (a) §Cross-category short-circuits: Config no longer SKIPs when `$HOME` is unresolved if `--config` / `$LINESMITH_CONFIG` / `$XDG_CONFIG_HOME` already resolved a path — the carve-out keeps an explicitly-named config readable. (b) §Rate-limit endpoint: only `4xx other than 429` maps to FAIL; `5xx` and `3xx` (server outage, redirect-handling exhausted) join transport-level errors at WARN so an Anthropic incident doesn't gate CI exit-1. (c) §Rate-limit endpoint shape WARN now triggers on _truly-new_ unknown keys only — codenamed forward-compat buckets observed in the live research capture (`iguana_*`, `omelette_*`, `seven_day_cowork`, `seven_day_omelette`, `tangelo`) are baseline, not new, and gating on `KNOWN_BUCKETS ∪ RESEARCH_DOCUMENTED_BUCKETS` keeps healthy doctor runs quiet while preserving the maintainer signal.
- 2026-04-30: split §Self "Binary integrity" row into "Binary path" (current_exe resolution; ships now) + "Binary integrity" (version vs build metadata; future). The two answer different questions and the original row conflated them, leading the implementation to ship the cheaper precondition under the integrity-check label.
- 2026-04-29: drop `--full` from the v0.1 CLI surface; all checks (including network probes) run unconditionally. Demote transport-level errors on the rate-limit endpoint and GitHub update check to WARN so offline environments stay at exit 0. Add I/O-failure clause to §Exit code contract. Add plain-mode passthrough caveat (renderer guarantees ASCII for its own strings only; user-supplied labels/hints pass through verbatim). Drop the contradictory "Panic catching" unit-test bullet from §Testing strategy — `panic = "abort"` forecloses on `catch_unwind` and §Panic behavior already documents this. Move `--verbose` and `--offline` to §Open questions for future revisits.
- 2026-04-19: initial draft (v0.1). Defines the check catalog across eight categories (Environment, Config, Claude Code, Credentials, Cache, Rate-limit endpoint, Plugins, Git, Self), severity levels (PASS / WARN / FAIL / SKIP), exit-code contract (any FAIL → 1; WARN-only → 0), default tree-style output + `--plain` ASCII output, short-circuit propagation rules between categories, panic-safety wrapper, and testing strategy with fixture scenarios. `--json` and `doctor fix` flagged for v0.2+.
