# Plugin API

- Status: draft
- Version: 0.2
- Last updated: 2026-04-19
- Driving ADRs: [ADR-0003](../adrs/0003-segment-widget-system.md), [ADR-0004](../adrs/0004-rhai-for-plugins.md), [ADR-0008](../adrs/0008-canonical-type-refinements.md), [ADR-0010](../adrs/0010-data-fetching-architecture.md)

## Overview

linesmith lets users define custom segments without recompiling the binary. Plugins are [rhai](https://rhai.rs/) scripts that implement the same `Segment` trait surface as built-ins (wrapped by a `RhaiSegment` adapter). This spec defines:

1. Plugin file discovery and loading
2. The rhai runtime host configuration (sandboxing, registered APIs, cost ceilings)
3. The plugin script contract (what functions a script must export, what objects it sees)
4. Lifecycle: when scripts are parsed, compiled, cached, and invoked
5. Error handling: script syntax errors, runtime errors, timeouts, resource limits
6. Testing strategy and the plugin compatibility guarantee

Built-in segments and plugin segments share the `Segment` trait (see [`segment-system.md`](segment-system.md)). A plugin script is the smallest unit of extension; there is no heavier plugin concept in v0.1 (no themes, no tool normalizers via plugin; those come later if demand warrants).

## Requirements

### Functional

- A plugin is a single `.rhai` file on disk
- Plugins are auto-discovered from `$XDG_CONFIG_HOME/linesmith/segments/*.rhai` at startup
- Additional discovery paths declarable via `config.toml` → `plugin_dirs`
- Each plugin script exports: an `id` constant, a `defaults()` function, a `render(ctx)` function, and optionally a `visible_if(ctx)` predicate
- Plugins receive a `ctx` object mirroring `DataContext` ([data-fetching.md](data-fetching.md) §DataContext). `ctx.status` exposes the parsed stdin payload (`StatusContext`) for fields like `ctx.status.model.display_name`; non-stdin sources (`ctx.usage`, `ctx.claude_json`, `ctx.settings`, `ctx.sessions`, `ctx.git`) are populated only when the plugin declares the matching `@data_deps`. Some Rust-side sources (`credentials`, `jsonl`) are intentionally not plugin-accessible — see §`@data_deps` header syntax for the reserved list.
- Plugins declare their data dependencies via a `// @data_deps = [...]` header comment parsed at script load time. Missing declarations mean the runtime's prefetch skips those sources and the corresponding `ctx` accessors return `()`.
- Plugins return a `RenderedSegment`-shaped value from `render(ctx)` (or `()` to hide)
- Plugins cannot exec shell, read arbitrary files, open network sockets, or import other rhai files (sandbox)
- Plugins have bounded CPU and memory per invocation (configurable ceiling; default tight)
- Syntax errors at load time surface clearly (`linesmith doctor` reports which plugin failed)
- Runtime errors during `render(ctx)` drop the plugin for that invocation only, log once, and continue rendering the rest of the line
- The plugin API is versioned; future incompatible changes ship with a version bump and a migration note

### Non-functional

- Plugin compile (rhai AST parse) <3ms per script (one-time, cached per file hash)
- Plugin invocation (AST already compiled) <2ms per render
- Plugin sandbox cost negligible (rhai's built-in `Engine::register_*` filtering, no process isolation)
- Zero-binary-impact when no plugins are present (`rhai` crate is compiled in but its engine isn't initialized until a plugin is discovered)

## Interface / Contract

### Plugin file location

Discovery order, first match wins per plugin id:

1. Paths from `config.toml` → `plugin_dirs` (in list order)
2. `$XDG_CONFIG_HOME/linesmith/segments/` (falls back to `~/.config/linesmith/segments/`)

All `.rhai` files in each directory are loaded (non-recursive). Duplicate ids are a startup error; the config-file path's plugin wins in the `linesmith doctor` report.

### Plugin script contract

A plugin script must export (at minimum):

```rhai
// OPTIONAL: data-dependency declaration. Parsed from the first block of
// line comments at the top of the file. Plugin-accessible values:
// "status" (default, always available), "settings", "claude_json",
// "usage", "sessions", "git". "credentials" and "jsonl" are reserved —
// see §@data_deps header syntax below. Unknown names are startup errors.
// @data_deps = ["usage", "git"]

// REQUIRED: stable id for this segment. Lowercase-kebab-case. Used in
// config as `[segments.<id>]`. Must not collide with a built-in.
const ID = "my_segment";

// REQUIRED: the renderer. Returns a map shaped like RenderedSegment,
// or () to hide the segment.
fn render(ctx) {
    let model_name = ctx.status.model.display_name;
    #{ runs: [ #{ text: `model: ${model_name}`, role: "primary", bold: true } ],
       width: model_name.len() + 7 }
}

// OPTIONAL: layout defaults. If missing, defaults to priority 128, no
// width bounds, separator "space".
fn defaults() {
    #{ priority: 128, separator: "space" }
}

// OPTIONAL: conditional visibility predicate. If present and returns false,
// `render` is not called and the segment is hidden.
fn visible_if(ctx) {
    ctx.status.cost != ()
}
```

### `@data_deps` header syntax

The data-dependency declaration is an optional line comment of exactly the form:

```rhai
// @data_deps = ["status", "usage", "git"]
```

Parser rules:

- Must appear in the first contiguous block of `//`-style line comments at the top of the file. A blank line or any non-`//` token (including `/* */` block comments or rhai statements) ends the block; the parser stops scanning there
- The RHS is a JSON-style array of bare-string dep names
- Unknown dep names fail at startup with `PluginError::UnknownDataDep { plugin_id, name }`; the plugin is rejected and reported via `linesmith doctor`
- Omitted or empty (`@data_deps = []`) declarations default to `["status"]` (stdin-only behavior; matches v0.1 plugins verbatim)
- `["status"]` doesn't need to be listed explicitly — it's always implicit
- Trailing commas, comments inside the array, and multi-line forms are all accepted
- Block-comment syntax (`/* @data_deps = [...] */`) is NOT scanned — line comments only, to keep the parser simple

Accepted dep names are a deliberately narrower subset of the `DataDep` enum from [data-fetching.md](data-fetching.md) §Segment dependency declaration — some Rust-side deps are reserved from plugin access for security or schema-stability reasons.

| Dep name      | DataDep variant | Source                                                      |
| ------------- | --------------- | ----------------------------------------------------------- |
| `status`      | `Status`        | Stdin payload (always available; listing is optional)       |
| `settings`    | `Settings`      | `~/.claude/settings.json` + overlays                        |
| `claude_json` | `ClaudeJson`    | `~/.claude.json` per-user state                             |
| `usage`       | `Usage`         | OAuth `/api/oauth/usage` endpoint + JSONL fallback          |
| `sessions`    | `Sessions`      | `~/.claude/sessions/{pid}.json` live process directory      |
| `git`         | `Git`           | Git repo state via gix ([git-segments.md](git-segments.md)) |

**Reserved for future use** (rejected at startup with `UnknownDataDep` today):

- `credentials` — the `Credentials` struct contains a `SecretString` OAuth token ([credentials.md](credentials.md)). Exposing it to rhai plugins, even through a tagged-map wrapper, creates a leak path where a third-party or buggy plugin could log the bearer token via `log()` or embed it in rendered output. Plugins needing rate-limit state use `usage` instead; `usage` pulls credentials internally without exposing them. A future `credentials_meta` dep may surface non-secret metadata (scopes, source kind) if a real product need emerges.
- `jsonl` — `data-fetching.md` currently defines `JsonlAggregate` as an opaque placeholder with its concrete shape deferred. Publishing `ctx.jsonl` now would either lock in an accidental shape or break plugins when the real aggregation spec lands. Plugins that need JSONL data must wait for the dedicated `jsonl-aggregation` spec (tracked under lsm-y6m).

### `ctx` shape exposed to rhai

`ctx` is an **immutable** rhai `Map` that mirrors `DataContext` from [data-fetching.md](data-fetching.md). The Rust-side types use typed enums and newtypes; the rhai mirror flattens enums into tagged maps and unwraps newtypes to their underlying primitive.

**Top-level shape:**

```rhai
ctx.status          // always available; mirrors StatusContext (parsed stdin)
ctx.config          // per-plugin config from [segments.<id>] TOML table
ctx.env             // whitelisted env var snapshot (see §ctx.env below)
ctx.settings        // present iff @data_deps includes "settings"
ctx.claude_json     // present iff @data_deps includes "claude_json"
ctx.usage           // present iff @data_deps includes "usage"
ctx.sessions        // present iff @data_deps includes "sessions"
ctx.git             // present iff @data_deps includes "git"
// ctx.credentials and ctx.jsonl are reserved and not plugin-accessible in v0.2;
// see §@data_deps header syntax for the rationale
```

Accessing a non-declared source's accessor (e.g. `ctx.usage` without `@data_deps = ["usage"]`) returns `()`. This is not an error — it's the plugin's responsibility to declare every source it reads.

**Result-shaped accessors.** Non-stdin sources expose `Arc<Result<T, E>>` on the Rust side. In rhai, each is a tagged map:

```rhai
// Success:
ctx.usage = #{
    kind: "ok",
    data: #{
        five_hour: #{ utilization: 22.0, resets_at: "2026-04-19T05:00:00Z" },
        seven_day: #{ utilization: 33.0, resets_at: "..." },
        seven_day_sonnet: ...,
        extra_usage: ...,
    },
};

// Failure:
ctx.usage = #{
    kind: "error",
    error: "NoCredentials",   // short error code string
};
```

Plugins check `kind` before accessing `data` or `error`:

```rhai
switch ctx.usage.kind {
    "ok" => {
        if ctx.usage.data.kind == "endpoint" && ctx.usage.data.five_hour != () {
            let pct = ctx.usage.data.five_hour.utilization;
            ...
        }
        // See the §ctx.usage shape block below for the full jsonl branch.
    }
    "error" => {
        // ctx.usage.error is one of:
        //   "NoCredentials" | "SubprocessFailed" | "IoError" | "ParseError" |
        //   "MissingField" | "EmptyToken" | "Timeout" | "RateLimited" |
        //   "NetworkError" | "Unauthorized" | "NoEntries" | "DirectoryMissing"
        // `MissingField` / `EmptyToken` come from the credentials layer;
        // `NoEntries` / `DirectoryMissing` from the JSONL aggregator.
        // `IoError` and `ParseError` can originate from either layer —
        // the tag alone doesn't disambiguate provenance.
        render_error(ctx.usage.error)
    }
}
```

For source-specific data shapes (`ctx.usage.data`, `ctx.git.data`, etc.), plugins consult the spec that owns the source: [rate-limit-segments.md](rate-limit-segments.md) for `usage`, [git-segments.md](git-segments.md) for `git`, [data-fetching.md](data-fetching.md) for `settings` / `claude_json` / `sessions`. Each spec's Rust type translates to a rhai Map by the naming convention below.

**Special cases:**

- **`ctx.git`**: the Rust accessor is `Arc<Result<Option<GitContext>, GitError>>` — a nested `Option` distinguishes "no git repo at cwd" from "gix failed." The rhai mirror collapses `Ok(None)` to `kind: "ok"` + `data: ()`. Plugins check `ctx.git.kind == "ok" && ctx.git.data != ()` before accessing fields like `ctx.git.data.head`.
- **`ctx.usage` error codes** mirror the `UsageError` variants from [rate-limit-segments.md](rate-limit-segments.md) plus the delegated tags from `CredentialError::code()` and `JsonlError::code()`: `"NoCredentials" | "SubprocessFailed" | "IoError" | "ParseError" | "MissingField" | "EmptyToken" | "Timeout" | "RateLimited" | "NetworkError" | "Unauthorized" | "NoEntries" | "DirectoryMissing"`. `MissingField` / `EmptyToken` surface from malformed credentials; `NoEntries` / `DirectoryMissing` from the JSONL aggregator; `IoError` and `ParseError` can originate from either layer (the tag doesn't disambiguate provenance). Plugins can branch on these codes without raw credentials being exposed.

**Legacy `ctx.*` access pre-v0.2.** Scripts written against v0.1 accessed stdin fields directly (`ctx.model.display_name`, `ctx.cost`, etc.). v0.2 moves those under `ctx.status.*` to make room for the DataContext sources. Plugin authors updating existing scripts do a one-time rename from `ctx.X` → `ctx.status.X` for every stdin field. `ctx.raw` (escape hatch for tool-specific fields) stays at `ctx.status.raw` in v0.2.

**`StatusContext` field shape** (accessible as `ctx.status.*`). Same as v0.1's `ctx.*` convention:

**Variant naming convention:** Rust `UpperCamelCase` variants are exposed to rhai as `snake_case` strings. `Tool::ClaudeCode` → `"claude_code"`; `RepoKind::LinkedWorktree` → `"linked_worktree"`. This convention is uniform across every enum exposed to plugins.

**Nullability:** Rust `Option<T>` surfaces as rhai `()` when `None` (rhai's unit, equivalent to JSON null). Always check for `()` before accessing sub-fields.

Example access patterns (all fields below live under `ctx.status.*` as of v0.2):

```rhai
// Tool identification. Map shape preserves the Tool::Other forensic id.
//   #{ kind: "claude_code" }
//   #{ kind: "qwen_code" }
//   #{ kind: "codex_cli" }
//   #{ kind: "copilot_cli" }
//   #{ kind: "other", name: "gemini" }     // `name` only present when kind == "other"
ctx.status.tool.kind
if ctx.status.tool.kind == "other" { ctx.status.tool.name }

// Base fields
ctx.status.model.id
ctx.status.model.display_name
ctx.status.session.id
ctx.status.workspace.cwd            // string; preserves platform-native separators
ctx.status.workspace.project_dir
ctx.status.workspace.git_worktree   // map or ()
ctx.status.workspace.git_worktree.name

// Nullable fields — check for () before accessing sub-fields
if ctx.status.context_window != () {
    ctx.status.context_window.used          // f32 in 0.0..=100.0 (Percent unwrapped)
    ctx.status.context_window.remaining     // f32 in 0.0..=100.0 (pre-computed host-side)
    ctx.status.context_window.size
    ctx.status.context_window.current_usage // map or ()
}

// Rate-limit data is not on ctx.status — read ctx.usage instead
// (declared via @data_deps = ["usage"]). The OAuth endpoint +
// JSONL fallback cascade is strictly richer than the old stdin
// rate_limits field it replaced.

// Escape hatch for tool-specific fields not in the canonical model
ctx.status.raw.some_custom_field
```

**`ctx.usage` shape** (present only when the plugin declared `@data_deps = ["usage"]`). The `data` payload mirrors `UsageData`. Per [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md), `UsageData` is an enum (`Endpoint` / `Jsonl`); the variant is discriminated by `data.kind` following the same tagged-map convention used by every other enum mirror (`repo_kind.kind`, `head.kind`, etc.). The two variants carry different fields — branch on `data.kind` before reading variant-specific keys.

```rhai
if ctx.usage.kind == "ok" {
    if ctx.usage.data.kind == "endpoint" {
        if ctx.usage.data.five_hour != () {
            ctx.usage.data.five_hour.utilization   // f64 in 0.0..=100.0
            ctx.usage.data.five_hour.resets_at     // RFC 3339 string or ()
        }
        // Same shape for seven_day, seven_day_opus, seven_day_sonnet, seven_day_oauth_apps
        if ctx.usage.data.extra_usage != () {
            ctx.usage.data.extra_usage.is_enabled     // bool or ()
            ctx.usage.data.extra_usage.monthly_limit  // f64 or ()
            ctx.usage.data.extra_usage.used_credits   // f64 or ()
            ctx.usage.data.extra_usage.currency       // ISO 4217 string or ()
        }
        // ctx.usage.data.unknown_buckets is a map of any codenamed
        // buckets Anthropic shipped that the core segments don't
        // recognize — plugins may inspect it, core segments ignore it.
    } else if ctx.usage.data.kind == "jsonl" {
        // JSONL fallback: raw token counts, no utilization percentage.
        // `seven_day` is always populated (zero-valued on an empty
        // transcript); `five_hour` is () when no active 5h block.
        if ctx.usage.data.five_hour != () {
            ctx.usage.data.five_hour.tokens.total          // i64 sum across all categories
            ctx.usage.data.five_hour.tokens.input          // i64 per category
            ctx.usage.data.five_hour.tokens.output
            ctx.usage.data.five_hour.tokens.cache_creation
            ctx.usage.data.five_hour.tokens.cache_read
            ctx.usage.data.five_hour.start                 // RFC 3339 string; block start
            ctx.usage.data.five_hour.ends_at               // RFC 3339 string; start + 5h
        }
        ctx.usage.data.seven_day.tokens.total              // same shape; always present
    }
}
// When ctx.usage.kind == "error", ctx.usage.error is a short tag
// string (e.g. "NoCredentials", "Timeout", "RateLimited").
```

Under `data.kind == "endpoint"` use `format_countdown_until(ctx.usage.data.five_hour.resets_at)` to render a human-friendly countdown. Under `data.kind == "jsonl"` use `ctx.usage.data.five_hour.ends_at` instead — `resets_at` is not present because the JSONL aggregator has no tier-aware reset timestamp.

**Immutability enforcement:** `ctx` is built once per render from a `&DataContext` reference. The host configures the rhai engine so the script scope cannot mutate `ctx`: `Engine::disable_symbol("=")` is disabled for identifiers starting with `ctx`, and `ctx` is passed as an immutable `Dynamic`. Attempts to assign (`ctx.foo = bar`) are rejected at parse or runtime as a `PluginError::Runtime`.

### Plugin return shape

The return value of `render(ctx)` must be either `()` (hide) or a map with these keys:

```rhai
#{
    // REQUIRED: array of styled runs
    runs: [
        #{
            text: "my text",          // string, required
            role: "success",          // one of: foreground | muted | primary | accent |
                                      //   success | warning | error | info |
                                      //   success_dim | warning_dim | error_dim |
                                      //   primary_dim | accent_dim | surface | border
                                      // (see specs/theming.md for the full list)
            fg: "#ff00ff",            // optional absolute color (hex)
            bg: "#000000",            // optional
            bold: false, italic: false, underline: false, dim: false,   // all optional
            hyperlink: "https://...",  // optional OSC 8 URL
        },
        // more runs...
    ],

    // OPTIONAL: explicit width in cells. Computed by the host from `runs` if absent.
    width: 42,

    // OPTIONAL: right-separator override for this segment only.
    // One of: "space" | "theme" | "none" | string-literal
    right_separator: "none",
}
```

The host validates the shape at render time. Missing `runs` or malformed entries become a `NormalizerError`-style plugin error, drop the segment for this invocation, and log once.

### Host-registered APIs

Rhai plugins can call a small set of host-exposed functions. No other ambient capabilities are granted.

```text
log(msg)                  // diagnostic line to stderr, routed through LINESMITH_LOG
                          //   and rate-limited to one line per plugin per run.
                          //   NOT a user-feedback channel: emit user-visible text
                          //   via the segment return value instead.
format_duration(ms)       // format milliseconds (i64) as "1h 23m"
format_cost_usd(dollars)  // format an f64 dollar amount as "$1.23" (matches ctx.cost.total_cost_usd)
format_tokens(count)      // format a token count (u64) as "1.2k", "3.5M", etc.
format_countdown_until(rfc3339_ts)  // format an ISO-8601 timestamp string as "2h 13m"
```

No filesystem APIs (`fs::read`, `fs::write`, `fs::list`). No network APIs. No `exec`. No `import`. No access to env vars except `ctx.env` (see below).

### `ctx.env`: whitelisted env snapshot

Plugins need some env awareness (terminal capability, locale). `ctx.env` exposes a whitelisted subset read once at startup:

```text
ctx.env.TERM               // string or ()
ctx.env.COLORTERM
ctx.env.NO_COLOR
ctx.env.FORCE_COLOR
ctx.env.LANG
ctx.env.OAKTERM_VERSION    // example: populated when a host terminal injects one
```

Any other env vars are invisible to plugins.

### Resource ceilings

Default limits (configurable via `config.toml` → `[plugins.limits]` in v0.2+):

| Limit                 | Default | Enforced via                                            |
| --------------------- | ------- | ------------------------------------------------------- |
| Max script operations | 50_000  | `rhai::Engine::set_max_operations`                      |
| Max array/map size    | 256     | `rhai::Engine::set_max_array_size` / `set_max_map_size` |
| Max string length     | 1024    | `rhai::Engine::set_max_string_size`                     |
| Max call depth        | 16      | `rhai::Engine::set_max_call_levels`                     |
| Max expression depth  | 32      | `rhai::Engine::set_max_expr_depths`                     |
| Per-render wallclock  | 50ms    | Host-side timer; kill + log on overrun                  |

Exceeding any limit drops the plugin for this invocation with a `PluginError::ResourceExceeded`.

### Plugin lifecycle

```text
startup
    │
    ▼
scan plugin_dirs + default dir → list of .rhai paths
    │
    ▼
for each path:
    ├─ rhai::Engine::compile(path) → AST
    │     │
    │     ├─ syntax error → log, skip; `linesmith doctor` reports
    │     └─ ok           → (id, AST, metadata from defaults()) cached
    │
    ▼
layout engine asks for RhaiSegment for id "foo"
    │
    ▼
RhaiSegment::render(ctx)
    │
    ▼
engine.call_fn(AST, "render", [ctx_as_dynamic])
    │
    ├─ runtime error → drop segment, log once
    ├─ resource exceeded → drop segment, log once
    ├─ returned () → hide
    └─ returned map  → validate shape → RenderedSegment
```

One shared `Arc<rhai::Engine>` is used for all plugins. `rhai::AST` is cloned into each `RhaiSegment` (rhai's `AST` is `Clone` and cheap). The engine is configured once at startup with registered host functions + resource limits, then reused.

### `RhaiSegment` wrapper (sketched in `segment-system.md` v0.3)

```rust
pub struct RhaiSegment {
    id: String,
    script: rhai::AST,
    engine: Arc<rhai::Engine>,
    metadata: SegmentDefaults,
    declared_deps: &'static [DataDep],  // leaked from parsed @data_deps header
}

impl Segment for RhaiSegment {
    fn id(&self) -> &str { &self.id }
    fn name(&self) -> &str { &self.id }
    fn data_deps(&self) -> &'static [DataDep] { self.declared_deps }
    fn render(&self, ctx: &DataContext) -> RenderResult {
        // 1. Build rhai Dynamic from &DataContext — only populate fields for
        //    declared deps (lazy sources the runtime pre-fetched)
        // 2. engine.call_fn("render", [ctx_dynamic])
        // 3. Validate return shape
        // 4. Return RenderResult
    }
    fn defaults(&self) -> SegmentDefaults { self.metadata.clone() }
    fn cache_policy(&self) -> CachePolicy { CachePolicy::AlwaysFresh }
}
```

`declared_deps` is `&'static [DataDep]` to match the canonical `Segment::data_deps` signature. The parser collects a `Vec<DataDep>` from the script header, leaks it with `Vec::leak` at config-load time, and stores the resulting slice. This is safe because plugin registry is built once per process and lives until exit. If the daemon mode arrives later, swap to an arena allocator.

Plugin cache policy defaults to `AlwaysFresh` in v0.1 because plugin scripts can declare arbitrary `visible_if` predicates that depend on anything. If the plugin exposes a `cache_policy()` function, the host may honor it (v0.3+ feature; deferred).

## Behavior

### Plugin registration

Plugins register by being in the discovery path; no manifest file, no `linesmith plugins enable` required. The id in the script is the canonical identifier; filenames are cosmetic.

### Config references

Plugin ids appear in `config.toml` just like built-in segments:

```toml
[line]
segments = ["model", "my_segment", "cost"]

[segments.my_segment]
# Per-plugin options; keys are plugin-defined (plugin picks up via ctx.config)
width = 16
```

`[segments.<plugin-id>]` sections are passed to the plugin as `ctx.config`, a rhai `Map`. Plugins access it in `render(ctx)`:

```rhai
fn render(ctx) {
    let width = ctx.config.width ?? 12;
    // ...
}
```

### Script caching

- AST is cached in memory for the lifetime of the process (no disk cache in v0.1)
- Modified script on disk triggers recompile at next startup (linesmith is short-lived, so "next startup" == "next prompt")
- Script-compilation errors are displayed only via `linesmith doctor`; the segment simply doesn't appear in the line if it fails to compile

### Interaction with the built-in cache

Plugin `RenderedSegment` outputs aren't cached to disk in v0.1 (`CachePolicy::AlwaysFresh`). v0.2+ may add an optional `cache_policy()` rhai function that returns a TTL or invalidator list.

## Edge cases

| Case                                                    | Handling                                                                                                          |
| ------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| Plugin file has syntax error                            | `linesmith doctor` reports; segment absent from layout; other plugins unaffected                                  |
| Plugin `id` collides with a built-in                    | Startup error; built-in wins; `linesmith doctor` reports                                                          |
| Two plugins share the same `id`                         | First discovered wins; `linesmith doctor` flags collision                                                         |
| `@data_deps` lists an unknown name                      | Startup error (`UnknownDataDep`); plugin rejected; `linesmith doctor` reports valid dep names                     |
| `@data_deps` is malformed (not a JSON array)            | Startup error (`MalformedDataDeps`); plugin rejected                                                              |
| Plugin accesses `ctx.usage` without declaring `usage`   | Returns `()`; plugin author must either declare or handle the `()` case                                           |
| Plugin reads `ctx.usage.data.X` while `kind == "error"` | rhai returns `()` (Map miss); plugin author must check `kind` first                                               |
| Plugin runtime error during `render`                    | Segment dropped for this invocation; rate-limited log; other segments render normally                             |
| Plugin returns malformed map                            | Segment dropped; validation error logged; suggests the expected shape                                             |
| Plugin returns `()`                                     | Segment hidden (normal flow, not an error)                                                                        |
| Plugin exceeds `max_operations`                         | Segment dropped; logged as ResourceExceeded; plugin marked suspect (future: disable after N consecutive failures) |
| Plugin exceeds wallclock timeout                        | Engine forcibly stops script; segment dropped; logged                                                             |
| Plugin accesses `ctx.undefined_field`                   | rhai returns `()`; plugin author must handle                                                                      |
| Plugin calls unregistered host fn (e.g. `fs::read`)     | rhai errors with "function not found"; caught as runtime error                                                    |
| Plugin file is 0 bytes                                  | Compiled as a no-op AST; `render` call returns `()`; segment hides                                                |
| Plugin dir doesn't exist                                | Treated as empty (no error); `linesmith doctor` may hint to create it if referenced                               |
| `config.toml` references plugin that isn't discovered   | Warn at config-load time; remove segment from layout                                                              |
| Plugin needs data not in any DataDep                    | Read from `ctx.status.raw` (escape hatch); if still missing, plugin declares `visible_if` returns false           |

## Testing strategy

Follows `AGENTS.md`: inline unit tests in the Rust host adapter, integration tests in `crates/linesmith/tests/plugins/`, no unit tests in rhai scripts themselves (rhai doesn't have a first-party test framework; we drive scripts from Rust integration tests).

### Unit tests (host adapter, `src/plugins/`)

- Discovery: `scan_plugin_dirs()` returns expected plugin list for a fixture directory
- Compile: malformed `.rhai` emits `PluginError::Compile` with path + message
- Compile: valid `.rhai` produces an AST with the expected exported `ID`
- `RhaiSegment::render`: valid script + canonical ctx → expected `RenderedSegment`
- `RhaiSegment::render`: malformed return → `None` + log
- `RhaiSegment::render`: runtime error → `None` + log
- Engine: attempts to call `fs::read` fail with "function not found"
- Engine: `max_operations` triggers on an infinite-loop script
- `ctx.env`: only whitelisted env vars surface

### Integration tests (`crates/linesmith/tests/plugins/`)

Fixture scripts in `tests/fixtures/plugins/`:

- `minimal.rhai`: smallest valid plugin (id + render returning one styled run)
- `uses_ctx_config.rhai`: reads `ctx.config` and adapts output
- `uses_visible_if.rhai`: hidden unless `ctx.status.cost != ()`
- `syntax_error.rhai`: compile-time error case
- `runtime_error.rhai`: runtime panic case
- `timeout.rhai`: infinite loop (triggers operation limit)
- `collision_built_in.rhai`: tries to register `id = "model"` (built-in)

Each integration test asserts either a snapshot of the rendered line or a specific `PluginError` variant.

### Benchmarks

`criterion` with:

- `plugin_compile`: AST compile time for `minimal.rhai`
- `plugin_render_cold`: first invocation (engine warmup)
- `plugin_render_warm`: subsequent invocations

Targets: <3ms compile, <2ms warm render.

## Plugin compatibility guarantee

The plugin API is versioned starting at v0.1 matching the linesmith version. A linesmith minor-version bump (v0.1 → v0.2) adds fields or APIs; existing plugins continue working. A major-version bump (v1 → v2) may break plugins; migration guide documents deltas.

Concretely, these are stable from v0.1 onwards:

- `ctx` top-level fields listed in the "Interface / Contract" section
- `render(ctx)` return-shape map keys
- Host-registered functions (`log`, `format_*`)
- Resource-limit semantics

Deprecations ship in the changelog at least one minor version before removal.

## Open questions

- **Config access via `ctx.config` vs. a separate rhai Scope?** Current design: `ctx.config` for ergonomic uniformity. May need to revisit if config keys conflict with context keys.
- **Per-plugin log verbosity?** No control in v0.1; every `log()` hits stderr. If noisy plugins become a problem, add `log_level` in `config.toml`.
- **Hot reload without restart?** linesmith is short-lived, so "next prompt" == "next startup." No separate hot-reload machinery needed in v0.1.
- **WASM plugin escape hatch for language-agnostic authors?** Explicitly deferred per [ADR-0004](../adrs/0004-rhai-for-plugins.md); revisit only if rhai's cold-start budget is met AND demand emerges.
- **Distinct `name` from `id`?** v0.1 uses `id` for both; v0.2+ may add a human-friendly `NAME` constant.
- **Plugin-exposed cache policy?** Deferred to v0.2+; v0.1 plugins always render fresh.

## Change log

- 2026-04-17: initial draft (v0.1) alongside the other foundational specs
- 2026-04-19: v0.2 incorporating [ADR-0010](../adrs/0010-data-fetching-architecture.md). `ctx` now mirrors `DataContext` (not `StatusContext`); stdin fields move from `ctx.X` to `ctx.status.X` (one-time rename for existing scripts). Adds `@data_deps = [...]` script-header declaration so plugins opt into non-stdin sources. Plugin-accessible dep set is `status | settings | claude_json | usage | sessions | git`; `credentials` is reserved to avoid token-leak paths and `jsonl` is reserved until `JsonlAggregate` has a concrete schema. Unknown dep names fail at startup via `UnknownDataDep`. Result-shaped accessors use `#{ kind: "ok" | "error", data | error }` tagged-map convention. `RhaiSegment` wrapper gains `declared_deps` field to implement the new trait's `data_deps()` method.
