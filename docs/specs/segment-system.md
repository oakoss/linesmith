# Segment System

- Status: draft
- Version: 0.2
- Last updated: 2026-04-17
- Driving ADRs: [ADR-0003](../adrs/0003-segment-widget-system.md), [ADR-0004](../adrs/0004-rhai-for-plugins.md), [ADR-0005](../adrs/0005-role-based-themes.md), [ADR-0008](../adrs/0008-canonical-type-refinements.md)

## Overview

Segments are the composable units linesmith renders into a status line. This spec defines:

1. The `Segment` trait: the contract every segment (built-in or plugin-authored) implements
2. The rendering pipeline: how `StatusContext` plus a segment list becomes bytes on stdout
3. The layout engine: how priority, width hints, and terminal width interact to produce final output
4. The cache layer: how expensive segments avoid re-running on every invocation
5. Sub-composition: how one "segment" can compose multiple others internally
6. How plugins (rhai scripts) use the same trait as built-ins, so there is no dual API

Segments know how to render themselves given a `StatusContext`. The layout engine knows how to combine many segments into a final line.

## Requirements

### Functional

- Segments render from a typed `StatusContext` ([spec: input-schema](input-schema.md)) and produce styled text
- Segments can return "no output" (`None`) to hide themselves (e.g. rate-limit segment hidden for API-tier users, worktree segment hidden outside a worktree)
- Segments declare layout intent: priority (drop-order under pressure), width bounds, separator preference
- Segments declare a cache policy so expensive computations don't run every invocation
- Segments can be composed: a "git group" segment may internally combine branch + dirty + ahead/behind
- Plugin-authored segments (in rhai, per [ADR-0004](../adrs/0004-rhai-for-plugins.md)) use the same `Segment` trait as built-ins (see [`specs/plugin-api.md`](plugin-api.md) for the rhai binding details)
- Ordering is user-controlled via config (see `specs/config.md`)
- Multi-line layouts supported; user can put segments on separate lines

### Non-functional

- Total render time <20ms cold-start (the project's overall budget)
- Cached segments skip work entirely on hit (no serde or gix calls)
- Layout engine runs in O(n) over segment count; no n² sort-by-priority loops
- Trait is object-safe (`dyn Segment`) so the layout engine can hold a heterogeneous vector
- Plugin segments pay no more than ~2ms overhead per invocation (rhai engine init + script call)

## Interface / Contract

### Segment trait

```rust
pub trait Segment: Send {
    /// Stable identifier used in config: `[segments.<id>]`.
    /// Lowercase-kebab-case. Must not collide with another segment.
    fn id(&self) -> &str;

    /// Human-readable name for error messages and `linesmith segments list`.
    fn name(&self) -> &str;

    /// Produce output (or None to hide). Called on every render unless
    /// cache policy returns Hit.
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment>;

    /// Default layout intent. Can be overridden by user config.
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::default()
    }

    /// How long a successful render is valid for. Invoked before each render;
    /// if the previous render is still fresh, render() is skipped.
    fn cache_policy(&self) -> CachePolicy {
        CachePolicy::AlwaysFresh
    }

    /// Optional sub-composition: a group segment returns the segments it wraps.
    /// Renders run bottom-up; layout folds children into the parent's output.
    fn children(&self) -> &[Box<dyn Segment>] {
        &[]
    }
}
```

`Segment: Send` (not `Send + Sync`) per [ADR-0008](../adrs/0008-canonical-type-refinements.md); `rhai::AST` is `Send` but its `Sync` story depends on feature flags. Adding `Sync` later is a non-breaking extension.

### RenderedSegment

The output of a successful render.

```rust
pub struct RenderedSegment {
    /// Runs of styled text. Multi-run allows inline color changes without
    /// re-parsing ANSI.
    pub runs: Vec<StyledRun>,

    /// Effective width in terminal cells (not bytes, not chars; grapheme-
    /// cluster aware, ignoring ANSI). Computed once at render time.
    pub width: u16,

    /// The separator this segment prefers on its right edge. Overrides the
    /// default separator from the theme for this boundary only.
    pub right_separator: Option<Separator>,
}

pub struct StyledRun {
    pub text: String,
    pub style: Style,
}

pub struct Style {
    pub role: Option<Role>,        // semantic role (e.g. "success"); preferred
    pub fg: Option<Color>,         // absolute color override (rare)
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub hyperlink: Option<String>, // OSC 8 URL
}
```

Segments should prefer declaring a `role` (maps to theme colors; see `specs/theming.md`) over absolute `fg`/`bg`, so themes can restyle them consistently.

### Layout intent

```rust
use std::borrow::Cow;

pub struct SegmentDefaults {
    /// Drop order under width pressure: `255` drops first, `0` never
    /// drops. Defaults to `128`. Ties break by position (right-most
    /// first).
    pub priority: u8,

    /// Width bounds, if any. Construction enforces `min <= max`.
    pub width: Option<WidthBounds>,

    /// Default separator to the right of this segment. Theme or user
    /// config can override.
    pub default_separator: Separator,
}

/// Width bounds with `min <= max` enforced at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidthBounds {
    min: u16,
    max: u16,
}

impl WidthBounds {
    pub fn new(min: u16, max: u16) -> Option<Self> {
        (min <= max).then_some(Self { min, max })
    }
    pub fn min(self) -> u16 { self.min }
    pub fn max(self) -> u16 { self.max }
}

impl Default for SegmentDefaults {
    fn default() -> Self {
        Self {
            priority: 128,
            width: None,
            default_separator: Separator::Space,
        }
    }
}

pub enum Separator {
    /// Single space (default).
    Space,
    /// Theme-provided padding (e.g. space+symbol+space for powerline).
    Theme,
    /// Exact string. Built-in defaults can use `Cow::Borrowed` for zero-alloc;
    /// user config supplies runtime strings via `Cow::Owned`.
    Literal(Cow<'static, str>),
    /// No separator; direct concatenation.
    None,
}
```

`Separator::Literal` takes `Cow<'static, str>` per [ADR-0008](../adrs/0008-canonical-type-refinements.md); built-ins stay zero-alloc (`Cow::Borrowed("…")`), user-provided config strings allocate once (`Cow::Owned`).

### Cache policy

```rust
pub enum CachePolicy {
    /// Render every invocation. Default for cheap segments.
    AlwaysFresh,

    /// Cache for the specified duration. Examples:
    ///   - rate-limit segment scraping API headers: 60s
    ///   - git status segment: 5s
    Ttl(std::time::Duration),

    /// Cache until any of the listed invalidators matches (OR semantics).
    /// Optional `ttl` provides a ceiling so stale state can't persist
    /// indefinitely if an invalidator fails to fire.
    Invalidated {
        any_of: Vec<CacheInvalidator>,
        ttl: Option<std::time::Duration>,
    },
}

pub enum CacheInvalidator {
    /// File mtime changed (e.g. `.git/HEAD`, `.claude/settings.json`).
    FileChanged(std::path::PathBuf),

    /// A specific field in StatusContext changed.
    ContextFieldChanged(&'static str),   // e.g. "workspace.git_worktree.name"

    /// Session ID changed (i.e. /resume or new session).
    SessionChanged,
}
```

OR semantics (any invalidator triggers) named explicitly as `any_of` per [ADR-0008](../adrs/0008-canonical-type-refinements.md). The optional `ttl` guards against a missed invalidator leaving stale state indefinitely.

### Plugin wrapper

Rhai segments don't implement `Segment` directly; they're wrapped. The full design lives in [`specs/plugin-api.md`](plugin-api.md). Outline:

```rust
pub struct RhaiSegment {
    id: String,
    script: rhai::AST,
    engine: Arc<rhai::Engine>,
    metadata: SegmentDefaults,
}

impl Segment for RhaiSegment {
    fn id(&self) -> &str { &self.id }
    // delegates to the rhai script's `render(ctx)` function
}
```

## Behavior

### Rendering pipeline

```text
StatusContext + config
         │
         ▼
  load segment list
         │
         ▼
  check cache for each
         │         │
    hit  │    miss │
         │         ▼
         │     segment.render(ctx) → Option<RenderedSegment>
         │         │
         └─────────┴───── collected list (with None → dropped)
         │
         ▼
     layout engine
         │
         ▼
     stdout bytes
```

### Layout algorithm

Input: list of `Option<RenderedSegment>`, each with `SegmentDefaults`, terminal width `W`.

```text
1. Drop all `None` entries (visibility = hidden).
2. For each remaining, resolve effective width:
     - If render width < width.min: drop the segment entirely.
     - If render width > width.max: truncate with a single-cell ellipsis marker
       (theme-provided, default "…").
3. Compute total width = sum(segment widths) + sum(separator widths).
4. If total <= W: render as-is.
5. Else: priority-based drop loop:
     a. Find the highest-priority (numerically largest) remaining segment.
     b. Drop it.
     c. Recompute total width.
     d. Repeat until total <= W or only priority-0 segments remain.
6. Emit: for each remaining segment, write its runs, then its right-separator
   (either segment-declared override or theme default), to stdout.
```

Priority-0 segments are never dropped. If total width still exceeds `W` after all droppable segments are removed, render anyway (terminal wraps or truncates visually; worse UX than hiding, but priority-0 means "user said don't drop this").

### Multi-line layouts

Config can declare multiple lines (see `specs/config.md`). Each line runs the algorithm independently. Example config:

```toml
[line.1]
segments = ["model", "context_window", "rate_limit_5h"]

[line.2]
segments = ["workspace", "git_branch", "cost"]
```

Each line emits its own newline-terminated string.

### Cache behavior

- Cache lives in `linesmith/` under `XDG_CACHE_HOME` (falls back to `~/.cache/linesmith/`)
- One file per segment: `<segment-id>.json` containing `{ rendered, expires_at, invalidator_state }`
- Cache is read before `render()`; if still valid, deserialize `RenderedSegment` and use it
- Writes are atomic (temp file + rename)
- Segments with `AlwaysFresh` bypass the cache entirely (no write, no read)
- `CachePolicy::Invalidated` behavior:
  - `FileChanged`: on read, compare current mtime against the mtime stored in `invalidator_state`; mismatch = miss
  - `ContextFieldChanged`: hash the relevant `StatusContext` field; mismatch against stored hash = miss
  - `SessionChanged`: compare current `ctx.session.id` against stored id; mismatch = miss
  - The optional `ttl` is a hard ceiling; entries past `expires_at` are misses regardless of invalidator state

### Sub-composition

A group segment returns children via `Segment::children()`. At render time:

1. Group calls `children()` to get the list
2. Each child renders and is cached per its own policy
3. Group's own `render()` receives the children's `RenderedSegment`s (via a helper in `ctx`) and combines them
4. Group's combined output participates in the outer layout as a single unit

Example: a `git_group` segment combines branch + dirty + ahead/behind into one visual unit that drops as a unit under width pressure.

### Built-in segment set (v0.1)

Per [`docs/ideas/0001-feature-parity-matrix.md`](../ideas/0001-feature-parity-matrix.md):

1. `model`: model display_name
2. `context_window`: percentage + size (e.g. `45% · 200k`)
3. `context_bar`: visual bar, width configurable (4/6/8/10/12 cells)
4. `cost`: session cost in USD
5. `duration`: session duration
6. `workspace`: directory / worktree hybrid
7. `git_branch`: branch + dirty + ahead/behind (sub-composed)
8. `rate_limit_5h`: 5-hour percentage + resets-at countdown
9. `rate_limit_7d`: 7-day percentage + resets-at countdown
10. `rate_limit`: combined 5h/7d view; sub-composed from `rate_limit_5h` and `rate_limit_7d` with a tighter layout (users pick either the combined form or the individual segments, not both)
11. `effort`: current `/effort` level (renders `None` until Claude Code emits the effort field; see [user-demand research](../research/user-demand.md) for why this field doesn't flow live today)

Each has its own module in `crates/linesmith/src/segments/<id>.rs` with a small per-segment spec inline (doc comment).

### Nerd Font glyphs

Segments that render Nerd Font glyphs (powerline separators, git icons, model badges) source codepoints from a `const ICONS: &[(&str, char)]` table generated at build time from [`nerd-fonts` glyphnames.json](https://github.com/ryanoasis/nerd-fonts/blob/master/glyphnames.json) via `build.rs`. Only the codepoints we actually render ship in the binary; see `crates/linesmith/build.rs`.

## Edge cases

| Case                                                               | Handling                                                                                  |
| ------------------------------------------------------------------ | ----------------------------------------------------------------------------------------- |
| Segment returns `None`                                             | Dropped from layout. Separator also dropped (not left as a floating artifact)             |
| Segment panics during render                                       | Panic caught; segment dropped; error logged once per segment per run; rendering continues |
| Cache file corrupt / unparseable                                   | Treated as miss; re-rendered; new write replaces bad file                                 |
| Segment width exceeds terminal width                               | Truncated with ellipsis per `width.max`; if still too wide, dropped unless priority 0     |
| Terminal width unknown (detached tty)                              | Fall back to 200 cells                                                                    |
| All segments drop due to width pressure                            | Emit blank line (status line is empty but still emitted)                                  |
| Segment tries to construct `WidthBounds { min: 20, max: 10 }`      | `WidthBounds::new` returns `None`; segment must fix or drop bounds                        |
| Two segments have the same `id`                                    | Second one rejected at config-load time; first wins                                       |
| Rhai script errors at render                                       | Plugin segment dropped; error logged; rendering continues                                 |
| Segment writes to stdout directly (should never)                   | Undefined behavior; segments must return `RenderedSegment`, not print                     |
| Context has no `git_worktree`, but `workspace` segment expects one | Segment returns `None` (conditional visibility)                                           |
| `effort` segment requested but `ctx.effort == None`                | Segment returns `None`; user-visible reason documented in segment's doc comment           |

## Testing strategy

Follows `AGENTS.md`: inline `#[cfg(test)] mod tests` for unit tests, `tests/` for integration, `insta` for snapshots, `criterion` for benchmarks.

### Unit tests (per segment, inline `mod tests`)

Every segment in `crates/linesmith/src/segments/` has tests:

- Renders expected output for a canonical `StatusContext`
- Returns `None` for context without relevant data (e.g. `workspace` returns `None` with no cwd)
- Width calculation matches the rendered string (grapheme-aware)
- Respects its own cache policy
- `WidthBounds::new` rejects `min > max`

### Layout engine tests (inline `mod tests` in `crates/linesmith/src/layout/`)

Fixtures: lists of `(SegmentId, width, priority)` tuples.

- No width pressure → all segments render
- Width pressure → drops happen in priority order, highest first
- Priority-0 segments never dropped
- Separators dropped alongside hidden segments
- Multi-line layouts render each line independently

### Integration tests (`crates/linesmith/tests/`)

- Golden tests: JSON fixture + config → stdout output byte-compare against `insta` snapshot
- Terminal width matrix: run the same fixture at 80 / 120 / 200 / 40 cells, snapshot each
- Cache tests: render → modify context → render → verify cache invalidation per `CacheInvalidator` kind

### Benchmarks (`crates/linesmith/benches/`, criterion)

- Single-segment render time (cached vs. fresh)
- Full pipeline cold start (10 segments, 120-cell terminal)
- Target: <20ms cold, <5ms cached

## Open questions

- **Separator ownership**: should segments declare `right_separator` or should the layout engine own all separators via theme config? Current design: segments can _prefer_, layout engine applies theme default otherwise. Revisit if this produces inconsistent visuals.
- **Async segments in v0.1?** The matrix defers async prefetch to v0.2+. For v0.1 all renders are sync; any segment needing network I/O (rate-limit scraping) must cache aggressively to stay within budget.
- **Cache key model**: per-segment cache keys vs. a shared invalidation store. Current design: per-file with invalidators declared in `CachePolicy`. Simpler; may not scale.
- **Panic policy**: catch-and-continue vs. fail-loud in debug builds. Current design: always catch (statusline must never crash the user's terminal). Revisit if users report silent failures as confusing.
- **Rhai cold-start budget**: 2ms per plugin is a rough estimate. Benchmark when we build the rhai integration; if actual overhead is larger, revisit the plugin model.
- **Grapheme-cluster width crate**: `unicode-width` + `unicode-segmentation` vs. alternatives. Decision deferred to implementation; add a row to `research/rust-crate-survey.md` when benchmarking.
- **Catppuccin crate adoption** for palette data: resolved once, applies to both theming and segment color defaults. See [`specs/theming.md`](theming.md) Open Questions for the single point of decision.

## Change log

- 2026-04-17: initial draft (v0.1)
- 2026-04-17: v0.2 incorporating [ADR-0008](../adrs/0008-canonical-type-refinements.md) (Separator::Literal Cow, Segment: Send only, CachePolicy::Invalidated with any_of semantics, WidthBounds newtype) + rate_limit combined segment + Nerd Font glyph source + effort segment clarification + link to plugin-api.md
