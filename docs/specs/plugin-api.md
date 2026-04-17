# Plugin API

- Status: draft
- Version: 0.1
- Last updated: 2026-04-17
- Driving ADRs: [ADR-0003](../adrs/0003-segment-widget-system.md), [ADR-0004](../adrs/0004-rhai-for-plugins.md), [ADR-0008](../adrs/0008-canonical-type-refinements.md)

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
- Plugins receive a `ctx` object mirroring `StatusContext`: fields accessible as `ctx.model.display_name`, `ctx.context_window.used`, etc.
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
// REQUIRED: stable id for this segment. Lowercase-kebab-case. Used in
// config as `[segments.<id>]`. Must not collide with a built-in.
const ID = "my_segment";

// REQUIRED: the renderer. Returns a map shaped like RenderedSegment,
// or () to hide the segment.
fn render(ctx) {
    let model_name = ctx.model.display_name;
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
    ctx.rate_limits != ()
}
```

### `ctx` shape exposed to rhai

`ctx` is an **immutable** rhai `Map` that mirrors `StatusContext`. The Rust-side `StatusContext` uses typed enums and newtypes; the rhai mirror flattens enums into tagged maps and unwraps newtypes to their underlying primitive.

**Variant naming convention:** Rust `UpperCamelCase` variants are exposed to rhai as `snake_case` strings. `Tool::ClaudeCode` → `"claude_code"`; `RateLimits::FiveHourOnly` → kind tag `"five_hour_only"`. This convention is uniform across every enum exposed to plugins.

**Nullability:** Rust `Option<T>` surfaces as rhai `()` when `None` (rhai's unit, equivalent to JSON null). Always check for `()` before accessing sub-fields.

Example access patterns:

```rhai
// Tool identification. Map shape preserves the Tool::Other forensic id.
//   #{ kind: "claude_code" }
//   #{ kind: "qwen_code" }
//   #{ kind: "codex_cli" }
//   #{ kind: "copilot_cli" }
//   #{ kind: "other", name: "gemini" }     // `name` only present when kind == "other"
ctx.tool.kind
if ctx.tool.kind == "other" { ctx.tool.name }

// Base fields
ctx.model.id
ctx.model.display_name
ctx.session.id
ctx.workspace.cwd               // string; preserves platform-native separators
ctx.workspace.project_dir
ctx.workspace.git_worktree      // map or ()
ctx.workspace.git_worktree.name

// Nullable fields — check for () before accessing sub-fields
if ctx.context_window != () {
    ctx.context_window.used          // f32 in 0.0..=100.0 (Percent unwrapped)
    ctx.context_window.remaining     // f32 in 0.0..=100.0 (pre-computed host-side)
    ctx.context_window.size
    ctx.context_window.current_usage // map or ()
}

// Rate limits. The `kind` tag determines which sub-fields are present;
// accessing the wrong sub-field (e.g. `.seven_day` on a "five_hour_only"
// payload) returns () rather than erroring, so always check `kind` first.
if ctx.rate_limits != () {
    //   #{ kind: "five_hour_only", five_hour: <window> }
    //   #{ kind: "seven_day_only", seven_day: <window> }
    //   #{ kind: "both", five_hour: <window>, seven_day: <window> }
    switch ctx.rate_limits.kind {
        "five_hour_only" => ctx.rate_limits.five_hour.used,
        "seven_day_only" => ctx.rate_limits.seven_day.used,
        "both"           => ctx.rate_limits.five_hour.used,
    }
}

// Escape hatch for tool-specific fields not in the canonical model
ctx.raw.some_custom_field
```

**RateLimitWindow fields:** `used` is `f32` (Percent unwrapped), `resets_at` is an RFC 3339 string. Use `format_countdown_until(ctx.rate_limits.five_hour.resets_at)` to render a human-friendly countdown.

**Immutability enforcement:** `ctx` is built once per render from a `&StatusContext` reference. The host configures the rhai engine so the script scope cannot mutate `ctx`: `Engine::disable_symbol("=")` is disabled for identifiers starting with `ctx`, and `ctx` is passed as an immutable `Dynamic`. Attempts to assign (`ctx.foo = bar`) are rejected at parse or runtime as a `PluginError::Runtime`.

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
log(msg)                  // debug log line to stderr, rate-limited per plugin per run
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

### `RhaiSegment` wrapper (defined in `segment-system.md`)

```rust
pub struct RhaiSegment {
    id: String,
    script: rhai::AST,
    engine: Arc<rhai::Engine>,
    metadata: SegmentDefaults,
}

impl Segment for RhaiSegment {
    fn id(&self) -> &str { &self.id }
    fn name(&self) -> &str { &self.id } // plugins have no separate name in v0.1
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment> {
        // 1. Build rhai Dynamic from &StatusContext
        // 2. engine.call_fn("render", [ctx_dynamic])
        // 3. Validate return shape
        // 4. Return Option<RenderedSegment>
    }
    fn defaults(&self) -> SegmentDefaults { self.metadata.clone() }
    fn cache_policy(&self) -> CachePolicy { CachePolicy::AlwaysFresh }
}
```

Plugin cache policy defaults to `AlwaysFresh` in v0.1 because plugin scripts can declare arbitrary `visible_if` predicates that depend on anything. If the plugin exposes a `cache_policy()` function, the host may honor it (v0.2+ feature; deferred).

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

| Case                                                  | Handling                                                                                                          |
| ----------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------- |
| Plugin file has syntax error                          | `linesmith doctor` reports; segment absent from layout; other plugins unaffected                                  |
| Plugin `id` collides with a built-in                  | Startup error; built-in wins; `linesmith doctor` reports                                                          |
| Two plugins share the same `id`                       | First discovered wins; `linesmith doctor` flags collision                                                         |
| Plugin runtime error during `render`                  | Segment dropped for this invocation; rate-limited log; other segments render normally                             |
| Plugin returns malformed map                          | Segment dropped; validation error logged; suggests the expected shape                                             |
| Plugin returns `()`                                   | Segment hidden (normal flow, not an error)                                                                        |
| Plugin exceeds `max_operations`                       | Segment dropped; logged as ResourceExceeded; plugin marked suspect (future: disable after N consecutive failures) |
| Plugin exceeds wallclock timeout                      | Engine forcibly stops script; segment dropped; logged                                                             |
| Plugin accesses `ctx.undefined_field`                 | rhai returns `()`; plugin author must handle                                                                      |
| Plugin calls unregistered host fn (e.g. `fs::read`)   | rhai errors with "function not found"; caught as runtime error                                                    |
| Plugin file is 0 bytes                                | Compiled as a no-op AST; `render` call returns `()`; segment hides                                                |
| Plugin dir doesn't exist                              | Treated as empty (no error); `linesmith doctor` may hint to create it if referenced                               |
| `config.toml` references plugin that isn't discovered | Warn at config-load time; remove segment from layout                                                              |
| Plugin needs data not in `StatusContext`              | Read from `ctx.raw` (escape hatch); if still missing, plugin declares `visible_if` returns false                  |

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
- `uses_visible_if.rhai`: hidden unless `ctx.rate_limits != ()`
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
