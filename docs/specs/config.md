# Config

- Status: draft
- Version: 0.1.4
- Last updated: 2026-04-29
- Driving ADRs: [ADR-0003](../adrs/0003-segment-widget-system.md), [ADR-0005](../adrs/0005-role-based-themes.md), [ADR-0006](../adrs/0006-tool-agnostic-json-schema.md), [ADR-0010](../adrs/0010-data-fetching-architecture.md), [ADR-0011](../adrs/0011-rate-limit-data-source.md)

## Overview

linesmith's configuration ties together segments, themes, layout, and plugins. This spec defines:

1. Config file location (XDG-compliant)
2. TOML structure and every top-level section
3. Validation rules (required fields, type checks, referential integrity)
4. IntelliSense / schema publication (so editors auto-complete)
5. Env var and CLI-flag overrides
6. Multi-line layout declaration

Config is the user-facing API. Stability here matters; breaking changes require a migration tool or clear deprecation.

## Requirements

### Functional

- Single config file: `$XDG_CONFIG_HOME/linesmith/config.toml` (falls back to `~/.config/linesmith/config.toml`)
- `--config <path>` CLI flag overrides the default path
- Env var `LINESMITH_CONFIG` overrides the default path (lower priority than `--config`)
- Config declares: which theme, which segments, per-segment overrides, plugin directories, layout mode (single-line vs. multi-line)
- A JSON Schema is published at a stable URL and embedded in the binary for offline IntelliSense via `taplo`, `zed`, or VS Code
- Validation surfaces clear errors with file path, line, and column
- Missing config file: use defaults (don't fail); emit hint suggesting `linesmith init`
- `linesmith init` writes a starter config based on a preset
- `linesmith presets list` / `linesmith presets apply <name>` switch to a canned preset without user edits

### Non-functional

- Parse <5ms for a typical config (config parse is not a hot path; it's once per invocation)
- No allocations during config-driven segment lookup on the hot render path (resolved once at startup)
- Forward-compatible: unknown keys are warnings, not errors (allows config to roll forward across versions)
- Deterministic: same config always produces same rendered output

## Interface / Contract

### File location precedence

First match wins:

1. `--config <path>` CLI flag
2. `LINESMITH_CONFIG` env var
3. `$XDG_CONFIG_HOME/linesmith/config.toml`
4. `~/.config/linesmith/config.toml` (XDG default)
5. `~/Library/Application Support/linesmith/config.toml` (macOS, if `XDG_CONFIG_HOME` unset and we're on macOS)

If no file exists at any resolved path, linesmith runs with built-in defaults (one `default` theme, a minimal segment set).

### Top-level schema

```toml
# Schema hint for editor auto-complete. URL stable across linesmith versions.
"$schema" = "https://linesmith.sh/config.schema.json"

# Theme selection. Names match built-in themes or user themes in
# ~/.config/linesmith/themes/. Unknown theme falls back to "default".
theme = "catppuccin-mocha"

# Preset this config was last generated from (metadata; not enforced).
# `linesmith presets apply <name>` rewrites the file and updates this.
preset = "developer"

# Layout mode. "single-line" renders one line; "multi-line" renders each
# [line.N] section independently.
layout = "single-line"

# Tool detection override. If unset, uses env var then heuristic.
# See specs/input-schema.md for precedence.
# tool = "claude-code" | "qwen-code" | "codex-cli" | "copilot-cli"

# Plugin directories. `plugin_dirs` entries are scanned first (in list
# order) and the default `~/.config/linesmith/segments/` is scanned last,
# so project-local plugins here override user-level plugins with the same
# id. See specs/plugin-api.md "Plugin file location" for full discovery rules.
# plugin_dirs = ["./vendor/linesmith-segments"]

[line]
# Single-line segment order. Overridden by [line.1], [line.2], ... when
# layout = "multi-line".
segments = [
  "model",
  "workspace",
  "git_branch",
  "context_window",
  "rate_limit_5h",
  "cost",
]

# Multi-line mode:
# [line.1]
# segments = ["model", "context_window", "rate_limit_5h"]
# [line.2]
# segments = ["workspace", "git_branch", "cost", "effort"]

[layout_options]
# Separator: a reserved keyword OR an arbitrary literal string emitted
# verbatim between segments.
#   "space"     - single space (default)
#   "powerline" - Nerd Font right-arrow chevron (U+E0B0); see
#                 powerline_width below
#   "capsule"   - reserved for v0.2+; warns and renders as space
#   "flex"      - reserved for v0.2+; warns and renders as space
#   ""          - no separator (truly empty string)
#   <anything>  - rendered verbatim (e.g. " | ", " · ", "->", "   ").
#                 Whitespace-only literals are preserved as-is; only
#                 the truly-empty string suppresses the separator.
# Mixed per-segment separators (e.g., powerline at some boundaries,
# space at others) require the separator-as-item refactor in a later
# release.
separator = "space"

# Cell-count of the powerline chevron. Most modern Nerd Fonts render
# U+E0B0 as 1 cell at standard sizes; some fonts/sizes render as 2 cells
# and need this knob to keep `total_width` math correct. Values other
# than 1 or 2 warn and fall back to 1.
powerline_width = 1

# Padding added by Claude Code itself (see settings.json statusLine.padding).
# Duplicated here so linesmith can factor it into width calculations.
claude_padding = 0

# Force-color override. "auto" honors NO_COLOR/FORCE_COLOR; "always" forces
# color emission; "never" strips all color.
color = "auto"

# Global knobs for the OAuth /api/oauth/usage endpoint used by the
# rate-limit segments. The cache-tier contract (memory OnceCell +
# disk data JSON + disk lock) is defined by
# [specs/data-fetching.md](data-fetching.md) §OAuth usage cache stack;
# the three keys below are the only user-tunable parts of that stack.
# Per-segment display knobs (format, invert, progress_width, label,
# stale_marker, etc.) live under [segments.rate_limit_*]. See
# [specs/rate-limit-segments.md](rate-limit-segments.md) §Config schema.
[usage]
# Disk cache TTL in seconds. Shorter = fresher data + more endpoint
# traffic; longer = staler data + lower endpoint load. The endpoint
# is itself rate-limited, so values below ~60s risk 429s.
cache_duration = 180

# Endpoint base URL. Change only for self-hosters or proxies; the
# default is Anthropic's production endpoint. Scheme + host (+ port)
# only; the `/api/oauth/usage` path is fixed.
api_base_url = "https://api.anthropic.com"

# Request timeout in seconds per endpoint call. On timeout linesmith
# falls through to stale cache or JSONL aggregation per ADR-0011
# §Fallback cascade. Raising this only helps users on very slow
# networks and delays every cache-miss render.
timeout = 2

# Per-segment overrides. All sections optional. Keys match a segment's `id`.
# Any field set here overrides the segment's declared defaults. The
# top-level `width = { min, max }` table is reserved for layout bounds;
# segments that need an integer "cells" knob (context_bar) expose it
# under their own key (e.g. `cells = 10`).
[segments.context_window]
style = "role:primary bold"
format = "{pct}% · {size}" # segment-specific format string
priority = 32             # higher = more likely to be dropped (255 first; 0 never drops)

[segments.context_bar]
cells = 10                # bar width in terminal cells (default 10)
[segments.context_bar.thresholds]
green = 50                # pct < green renders Role::Success
yellow = 80               # green <= pct < yellow renders Role::Warning

[segments.workspace]
# Shorthand: hide the segment if this evaluates to false.
# `()` is rhai's unit/null sentinel (matches plugin-api.md's ctx nullability).
visible_if = "ctx.workspace.git_worktree != ()"

[segments.rate_limit_5h]
# Show the countdown only when usage > threshold.
# `ctx.usage` is a tagged result map — `#{ kind: "ok", data: {...} }`
# on success, `#{ kind: "error", error: "..." }` on failure — so the
# success payload lives under `ctx.usage.data`. See
# [specs/plugin-api.md](plugin-api.md) §ctx shape and the `UsageData`
# contract in [specs/data-fetching.md](data-fetching.md).
visible_if = "ctx.usage.kind == \"ok\" && ctx.usage.data.five_hour.utilization > 50"

[segments.git_branch]
# Sub-segments of a sub-composed segment can also be overridden.
show_ahead_behind = true
show_dirty = true

# Plugin-specific config lives under [plugins.<plugin-id>].
# Schema is defined by the plugin.
# [plugins.my_segment]
# foo = "bar"
```

### Validation rules

At parse time:

1. **Schema conformance**: warn on unknown top-level keys; error on type mismatches (e.g. `theme = 42`)
2. **Theme existence**: `theme` value resolves to a known theme at startup; unknown falls back to `default` with warning
3. **Segment ID validity**: every ID in `line.segments` / `line.N.segments` matches a registered segment (built-in or plugin); unknown IDs warn and skip
4. **Duplicate IDs**: same ID listed twice in a line warns; first occurrence wins
5. **Cross-line duplicates**: same segment in multiple lines allowed (rare but legal)
6. **Per-segment override keys**: each segment declares its accepted override keys; unknown keys in `[segments.<id>]` warn and are ignored
7. **`visible_if` expression syntax**: parse as rhai expression; invalid expressions warn and are treated as always-visible

### CLI flags that influence config

```text
linesmith [OPTIONS] [-- STATUSLINE_JSON]

Options:
  --config <path>       Config file path (overrides default resolution)
  --theme <name>        Theme override for this run (wins over config)
  --tool <name>         Tool detection override
  --no-color            Equivalent to NO_COLOR=1
  --force-color         Equivalent to FORCE_COLOR=1
  --preset <name>       One-off preset override (does not modify file)
  --debug               Print config as resolved, segment list, render trace
  --check-config        Validate config and exit (used by editor tooling)
```

### Env vars

| Var                   | Effect                                               |
| --------------------- | ---------------------------------------------------- |
| `LINESMITH_CONFIG`    | Config file path override                            |
| `LINESMITH_THEME`     | Theme override                                       |
| `LINESMITH_TOOL`      | Tool detection override                              |
| `LINESMITH_CACHE_DIR` | Override cache directory                             |
| `NO_COLOR`            | Strip all color (per no-color.org)                   |
| `FORCE_COLOR`         | Emit color even in non-TTY output                    |
| `BEADS_HOOK_TIMEOUT`  | (not linesmith) passed through for the lefthook shim |

CLI flags win over env vars. Env vars win over config file. Config file wins over built-in defaults.

### Presets

Presets are pre-authored `config.toml` bodies shipped with the binary. `linesmith presets list` prints available names. `linesmith presets apply <name>` writes the preset's body to the resolved config path (asking for confirmation if the file already exists).

v0.1 presets (per the matrix):

- `minimal`: model + context %
- `developer`: model + workspace + git + context + cost + rate limits
- `power-user`: everything v0.1 supports, multi-line
- `cost-focused`: model + context + cost + rate limits (5h+7d) + burn-rate placeholder
- `worktree-heavy`: model + workspace (worktree-styled) + git + context

### JSON Schema

Published as `config.schema.json` on the linesmith docs site and embedded via `include_str!` for offline editor integrations (taplo picks it up via the `$schema` key in the user's config).

The schema is generated from Rust types via `schemars` at build time (single source of truth). CI fails if the committed JSON schema is stale relative to the types.

## Behavior

### Startup resolution

```text
CLI parse
    ↓
Resolve config path (flag → env → XDG → default)
    ↓
Parse config (or use built-in defaults if file absent)
    ↓
Validate
    ↓
Resolve theme (registry lookup; fall back to "default" on miss)
    ↓
Resolve segment list (registry lookup; skip unknown)
    ↓
Apply per-segment overrides
    ↓
Build immutable RuntimeConfig passed to render pipeline
```

### `linesmith init`

Steps:

- Prompt (dialoguer) for preset and theme. The "tool" prompt listed in earlier drafts is deferred until `Config` grows a `tool` field and qwen-code support has a real install path; today only Claude Code ships.
- Write config to resolved path; create directories as needed (0755)
- Emit the Claude Code `settings.json` snippet for copy-paste (see below)
- Offer to run `linesmith doctor` for an environment health check (currently prints `linesmith doctor coming soon.` placeholder; offer wires up when the doctor subcommand lands)

Snippet emitted (default — XDG-resolved path, plain `linesmith` finds it):

```json
"statusLine": {
  "type": "command",
  "command": "linesmith",
  "padding": 0
}
```

When the user resolved an explicit path via `--config <PATH>` or `LINESMITH_CONFIG=<PATH>`, the snippet's `command` field includes the flag so Claude Code reads the file `init` just wrote rather than re-resolving to the XDG default:

```json
"statusLine": {
  "type": "command",
  "command": "linesmith --config /path/to/config.toml",
  "padding": 0
}
```

Path-content warnings (stderr, non-fatal): paths containing shell metacharacters or whitespace, paths starting with `-`, and paths with non-UTF-8 bytes all produce a hand-edit warning before the snippet is printed. The snippet still goes to stdout so users redirecting output get a clean copy.

### `linesmith check-config`

Parse and validate; print errors/warnings to stderr; exit non-zero on errors. Used by editor integrations and CI.

### Hot reload

**Deferred to v0.2+**: v0.1 reads config once at startup. Each invocation re-parses (the statusline process is short-lived anyway, so "hot reload" equals "next prompt").

## Edge cases

| Case                                                              | Handling                                                                                |
| ----------------------------------------------------------------- | --------------------------------------------------------------------------------------- |
| Config file missing entirely                                      | Use built-in defaults; log hint `run 'linesmith init' to customize`                     |
| Config file unreadable (permissions)                              | Log error, fall back to defaults                                                        |
| Config file has BOM                                               | Strip BOM before parse                                                                  |
| Unknown top-level key (e.g. `foo = 1`)                            | Warn, ignore                                                                            |
| Theme missing                                                     | Fall back to `default`, warn                                                            |
| Segment listed but not registered                                 | Skip, warn once                                                                         |
| `visible_if` expression invalid                                   | Treat as always-visible, warn                                                           |
| `layout = "multi-line"` but no `[line.N]` sections                | Treat as single-line, warn                                                              |
| `layout = "single-line"` but `[line.N]` sections exist            | Warn; ignore numbered tables and render `[line].segments`                               |
| `layout` omitted, `[line.N]` present, `[line].segments` empty     | Auto-promote to multi-line with hint to add `layout = "multi-line"`                     |
| Unknown scalar key under `[line]` (e.g. `[line] segmnts = [...]`) | Warn, drop the key (forward-compat: same rule as top-level unknown keys)                |
| `[line.foo]` (non-positive-integer key)                           | Warn, drop                                                                              |
| Preset name unknown for `--preset`                                | Fail with list of valid preset names                                                    |
| `linesmith presets apply <name>` on existing config               | Confirm overwrite; `--force` flag skips confirmation; backup saved as `config.toml.bak` |
| XDG vars unset AND home dir not detectable                        | Fail fast with clear error (can't locate a writable config path)                        |

## Testing strategy

### Unit tests

- Parse minimal valid config
- Parse with all optional sections populated
- Missing / unknown / malformed keys all produce expected warnings or errors
- CLI / env / file precedence: exhaustive table test
- `visible_if` expression parse and evaluate (rhai-based)

### Integration tests

- `linesmith init` creates a valid config; round-trips parse without errors
- Each preset parses and produces the expected segment list
- `linesmith --check-config` fixture tests for all edge cases
- User override of built-in theme works end-to-end

### Schema tests

- `schemars`-generated JSON schema committed; CI fails on drift
- taplo validates test-fixture configs against the committed schema
- VS Code / Zed pick up `$schema` key (manual smoke test on releases)

## Open questions

- **`visible_if` expression sandbox** — rhai expressions for visibility sound powerful but add complexity. Alternative: a small predicate DSL (`"ctx.usage.data.five_hour.utilization > 50"`). Decision deferred; v0.1 ships with a minimal predicate set, full rhai in v0.2+ if demand exists.
- **Preset storage** — in-binary (embedded TOML) vs. generated on demand from defaults. Current design: embedded. Adding new presets costs rebuilds. Deferred until we have 10+ presets.
- **Config versioning** — should we add a `version = 1` key to enable migration? Current design: no; forward-compat via "unknown keys are warnings" is enough until we have a breaking change.
- **Plugin discovery scope resolution** — both `plugin_dirs` and the default XDG directory are always scanned. On duplicate ids, `plugin_dirs` entries win over the XDG default (project-local / config-specified plugins override user-level ones), matching `specs/plugin-api.md`'s discovery order. Revisit if users want "replace, don't append" semantics.
- **Preset application UX** — should `linesmith presets apply` diff-merge or overwrite? Current design: overwrite with backup. Simpler; users wanting merge should hand-edit.
- **Config file format beyond TOML** — JSON / YAML support? Current design: TOML only. Additional formats increase validation surface for no user-facing win. Skip unless strongly requested.

## Change log

- 2026-04-17: initial draft (v0.1)
- 2026-04-20: v0.1.1 additive update. Adds `[usage]` top-level section (`cache_duration`, `api_base_url`, `timeout`) for the OAuth rate-limit endpoint knobs referenced by [data-fetching.md](data-fetching.md) §OAuth usage cache stack and [rate-limit-segments.md](rate-limit-segments.md) §Staleness bounds. Updates the `[segments.rate_limit_5h]` `visible_if` example from the stdin-payload `ctx.rate_limits` shape (dropped in lsm-7po) to the `ctx.usage` tagged-map shape matching ADR-0011's `UsageData` contract. Driven by ADR-0010 + ADR-0011. No breaking changes.
- 2026-04-28: v0.1.2 update for `linesmith init` (lsm-5yd) shipping. Clarifies the `linesmith init` section: drops the deferred "tool" prompt, documents the doctor-offer placeholder, adds the `--config`/`LINESMITH_CONFIG` snippet propagation, and notes the path-content warnings (shell metacharacters, leading dash, non-UTF-8). No config-shape changes.
- 2026-04-29: v0.1.3 update for multi-line layout (lsm-bvu) shipping. Top-level `layout = "single-line" | "multi-line"` selector and `[line.N]` numbered sub-tables are now implemented end-to-end. The `power-user` preset is now a two-line layout (line 1: model + context_window + workspace; line 2: rate limits + cost + effort + tokens_total). Edge-case warnings — missing `[line.N]` under multi-line, non-numeric numbered keys, single-line with `[line.N]` present — surface through both render and `--check-config`. Numbered tables sort by parsed integer key (so `[line.10]` follows `[line.2]`, not lexicographic). No breaking changes.
- 2026-04-29: v0.1.4 follow-up to v0.1.3 multi-line, addressing two review-found regressions. Forward-compat: typo'd or future scalar keys under `[line]` (e.g. `[line] segmnts = [...]`) now warn and drop, matching the spec's existing "unknown keys are warnings" rule for top-level keys, instead of failing the config load. Auto-promote: when `[line.N]` sub-tables are present but `layout` is omitted AND `[line].segments` is empty, the builder treats the config as multi-line with a hint pointing at the missing `layout = "multi-line"` key, instead of silently rendering blank. Both behaviors are documented in §Edge cases. No schema changes.
