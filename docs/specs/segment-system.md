# Segment System

- Status: draft
- Version: 0.9
- Last updated: 2026-05-13
- Driving ADRs: [ADR-0003](../adrs/0003-segment-widget-system.md), [ADR-0004](../adrs/0004-rhai-for-plugins.md), [ADR-0005](../adrs/0005-role-based-themes.md), [ADR-0008](../adrs/0008-canonical-type-refinements.md), [ADR-0010](../adrs/0010-data-fetching-architecture.md), [ADR-0024](../adrs/0024-per-boundary-separator-toml.md), [ADR-0026](../adrs/0026-layout-decision-observability.md)

## Overview

Segments are the composable units linesmith renders into a status line. This spec defines:

1. The `Segment` trait: the contract every segment (built-in or plugin-authored) implements
2. The rendering pipeline: how `DataContext` plus a segment list becomes bytes on stdout
3. The layout engine: how priority, width hints, and terminal width interact to produce final output
4. The cache layer: how expensive segments avoid re-running on every invocation
5. Sub-composition: how one "segment" can compose multiple others internally
6. How plugins (rhai scripts) use the same trait as built-ins, so there is no dual API
7. How segments declare their data dependencies so the runtime only fetches what they need

Segments know how to render themselves given a `DataContext` ([spec: data-fetching](data-fetching.md)), which owns the parsed `StatusContext` (stdin payload, accessible as `ctx.status`) plus lazy accessors for other sources (settings, `~/.claude.json`, JSONL transcripts, OAuth usage, credentials). The layout engine knows how to combine many segments into a final line.

## Requirements

### Functional

- Segments render from a typed `DataContext` (which wraps `StatusContext` from [spec: input-schema](input-schema.md)) and produce styled text
- Segments declare their data dependencies via `data_deps()` so the runtime only fetches sources that some enabled segment needs (see [spec: data-fetching](data-fetching.md))
- Segments can return "no output" (`None`) to hide themselves (e.g. rate-limit segment hidden for API-tier users, worktree segment hidden outside a worktree)
- Segments declare layout intent: priority (drop-order under pressure), width bounds, truncate-before-drop opt-in
- Separators between segments are positional [`LineItem`](#line-items-and-separators) entries built by the layout pipeline from `[layout_options].separator`; segments don't own them
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

    /// Declare which data sources this segment reads. The runtime computes
    /// the union across all enabled segments and lazy-fetches only those
    /// sources; undeclared sources never trigger file I/O, subprocess, or
    /// HTTP calls. Defaults to the stdin payload only; segments that read
    /// `~/.claude.json`, the OAuth usage endpoint, JSONL transcripts, etc.
    /// must override. See [spec: data-fetching](data-fetching.md) for the
    /// full `DataDep` enum.
    fn data_deps(&self) -> &'static [DataDep] {
        &[DataDep::Status]
    }

    /// Produce output (or `Ok(None)` to hide). Called on every render
    /// unless cache policy returns Hit. `Err` surfaces runtime failures
    /// (plugin script errors, unexpected state); the layout engine logs
    /// the error to stderr and hides the segment. `ctx` owns the parsed
    /// stdin payload (`ctx.status`) plus lazy accessors for other sources
    /// declared in `data_deps()`. `rc` is the per-render layout state
    /// — terminal width today, more fields later — for segments that
    /// pick their own shape based on available room.
    fn render(&self, ctx: &DataContext, rc: &RenderContext) -> RenderResult;

    /// Layout-pressure-aware compaction hook. The reflow loop calls
    /// this on any segment about to be dropped (truncatable or not),
    /// asking whether it can produce a render at most `target` cells
    /// wide. Default returns `None` (no compact form; segment drops
    /// whole). Segments with structured tail content (`git_branch`'s
    /// `* ↑2 ↓1`) override to shed decoration while keeping the
    /// signal-bearing prefix. The returned render must be at most
    /// `target` cells; wider results are rejected and treated as
    /// `None`. Runs before generic `truncatable` end-ellipsis
    /// truncation, so segment-side intelligence beats string
    /// clipping when both apply.
    fn shrink_to_fit(
        &self,
        _ctx: &DataContext,
        _rc: &RenderContext,
        _target: u16,
    ) -> Option<RenderedSegment> {
        None
    }

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

The render signature receives `&DataContext` as of v0.3. `DataContext` owns `StatusContext` as a field (`ctx.status`), so segments that only need stdin data write `ctx.status.model.id` with no functional loss compared to the v0.2 signature. Segments that need other sources call `ctx.usage()`, `ctx.credentials()`, `ctx.claude_json()`, etc. — each is an `Arc<Result<T, E>>` that lazy-initializes on first call and returns cached results thereafter.

As of v0.5 the signature also takes `&RenderContext` — the per-render layout state the engine builds before walking segments. `DataContext` is the data layer (one instance per process invocation, shared across segments); `RenderContext` is the layout layer (built once per `render` call from terminal width and any future per-line state). The split keeps the data cache stable while the layout state can rebuild cheaply.

```rust
#[non_exhaustive]
pub struct RenderContext {
    /// Total cells available to this line. The layout engine sources this
    /// from the terminal (or 200 when stdout is detached, per the input-
    /// schema fallback).
    pub terminal_width: u16,
}
```

Segments that don't care about width ignore the argument (`_rc: &RenderContext`); width-aware segments read `rc.terminal_width` to ladder their own output. Planned consumers (tracked under their own beads) include `git_branch` dropping dirty/ahead-behind markers before truncating the branch — its rendered string carries structured tail content that generic end-ellipsis truncation would misrepresent. The layout engine's reflow pass (§Layout algorithm) handles prose-like segments where end-ellipsis truncation reads correctly. Some segments addressed the same UX concern at the data layer instead: `model`'s `format = "compact"` config strips the trailing `(X context)` filler unconditionally, and `context_bar` relies on its higher priority to drop before the textual `context_window`. `RenderContext` is the right tool only when the laddering decision genuinely depends on terminal width.

```rust
pub type RenderResult = Result<Option<RenderedSegment>, SegmentError>;

pub struct SegmentError {
    pub message: String,
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}
```

`Ok(Some(r))` renders, `Ok(None)` hides, `Err(e)` is logged to stderr and hidden — the distinction matters for plugin-authored segments that want to surface runtime failures without silently vanishing.

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

    /// Per-render override for the segment's right-edge separator.
    /// Replaces the inline `LineItem::Separator` at that one boundary
    /// when present. See §Line items and separators for the override
    /// precedence.
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

    /// Shipped icon prefix for this segment. The builder applies it
    /// only when `[layout_options].icons = "nerdfont"`; per-segment
    /// `icon = "..."` overrides it, and `icon = ""` disables it.
    pub icon: Option<&'static str>,

    /// May this segment be truncated under width pressure before being
    /// dropped? Defaults to `false` — opt in for prose-like content
    /// (workspace name, branch name) where a partial value is more
    /// useful than disappearing. Numeric or structured segments
    /// (model, cost, percent, countdown) leave this `false`: a
    /// half-cut percentage reads as the wrong number, which is worse
    /// than no number.
    pub truncatable: bool,
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
            truncatable: false,
        }
    }
}

#[non_exhaustive]
pub enum Separator {
    /// Single space (default).
    Space,
    /// Theme-provided padding (e.g. space+symbol+space for powerline).
    Theme,
    /// Exact string. Built-in defaults can use `Cow::Borrowed` for zero-alloc;
    /// user config supplies runtime strings via `Cow::Owned`.
    Literal(Cow<'static, str>),
    /// Powerline chevron (U+E0B0) flanked by single-space padding.
    Powerline { width: PowerlineWidth },
    /// No separator; direct concatenation.
    None,
}
```

`Separator::Literal` takes `Cow<'static, str>` per [ADR-0008](../adrs/0008-canonical-type-refinements.md); built-ins stay zero-alloc (`Cow::Borrowed("…")`), user-provided config strings allocate once (`Cow::Owned`).

### Line items and separators

The layout pipeline takes a `Vec<LineItem>` rather than a flat segment list. A `LineItem` is either a configured segment or an inline separator between segments:

```rust
#[non_exhaustive]
pub enum LineItem {
    Segment {
        /// Stable identifier for this configured entry. Sourced from
        /// `LineEntry::segment_id()` (the TOML key) when set; falls back
        /// to a type-based name disambiguated by occurrence index for
        /// unnamed inline entries: `"git"`, `"git#2"`, `"git#3"`.
        /// Built-in ids are `Cow::Borrowed`; user-config ids are `Cow::Owned`.
        id: Cow<'static, str>,
        segment: Box<dyn Segment>,
    },
    Separator(Separator),
}
```

The `id` field routes layout-decision events (see [Layout decision contract](#layout-decision-contract)) back to the user-known config name — "cost was dropped" rather than "the segment at index 3 was dropped." It lives on the `LineItem`, not on `Segment`, because the same segment type can carry different ids across configs; the id is a layout property, not a render property.

`build_segments` / `build_lines` resolve `[layout_options].separator` once and interleave it between every adjacent pair of segments. The renderer walks the list directly; there is no implicit "default separator between segments" — every gap is an explicit `LineItem::Separator` produced by the builder.

**Adjacency invariants** (enforced by the layout engine — pruning happens during the collect pass; drop-with-adjacent-separator happens during the priority-drop loop):

- A separator survives only when it sits between two surviving segments. Leading separators, trailing separators, and separators flanking a dropped segment are pruned.
- When a segment drops under width pressure, the adjacent separator drops with it: the right-edge separator first, falling back to the left-edge when the dropped segment was the last in the line.

**Override precedence.** `RenderedSegment::right_separator` is a plugin-facing per-render override (set via `RenderedSegment::with_separator`). When a segment's render returns it, the layout engine replaces the inline separator immediately to the segment's right with the override value. If the segment is the last in the line, or its right-edge separator has already been pruned for adjacency reasons, there is no boundary to apply to and the override is silently discarded. Built-in segments don't set runtime overrides; plugins use this slot to vary their right edge per render.

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
    // Deps parsed from the script's @data_deps header. Full shape
    // (field type, leak strategy) defined in plugin-api.md §RhaiSegment
    // wrapper — that spec owns the rhai-plugin contract.
    declared_deps: &'static [DataDep],
}

impl Segment for RhaiSegment {
    fn id(&self) -> &str { &self.id }
    fn data_deps(&self) -> &'static [DataDep] { self.declared_deps }
    // delegates to the rhai script's `render(ctx)` function
}
```

Plugins declare their data dependencies via a script metadata header (e.g., `// @data_deps = ["usage", "claude_json"]`) or a rhai-level config file sibling. The host parses this once at config load, hands the parsed `Vec<DataDep>` to `RhaiSegment`, and the runtime prefetcher includes it in the union just like built-in segments. Without this, rhai plugins that need non-stdin sources would fall through to the default `&[DataDep::Status]` and their `ctx.usage()` / `ctx.credentials()` / etc. calls would block on lazy-init inside the hot render path — the exact cost that segment-driven lazy loading exists to avoid.

The exact metadata syntax and the rhai-side API for reading `DataContext` fields live in [`specs/plugin-api.md`](plugin-api.md), which needs a v0.2 rev to reflect the v0.3 trait contract (tracked as a follow-up bead).

## Behavior

### Rendering pipeline

```text
stdin payload → StatusContext + config
         │
         ▼
  wrap in DataContext (lazy: usage, credentials, claude_json, jsonl, ...)
         │
         ▼
  compute union of segment.data_deps() across enabled segments
         │
         ▼
  pre-populate OnceCells for declared deps only
         │
         ▼
  load segment list
         │
         ▼
  build RenderContext { terminal_width, ... } once per render
         │
         ▼
  check cache for each
         │         │
    hit  │    miss │
         │         ▼
         │     segment.render(&DataContext, &RenderContext) → Option<RenderedSegment>
         │         │
         └─────────┴───── collected list (with None → dropped)
         │
         ▼
     layout engine
         │
         ▼
     stdout bytes
```

The layout engine takes a `&mut LayoutObservers<'_>` rather than a bare `warn` callback, bundling the existing error-warn channel with a typed `on_decision` channel for layout-pressure events (see [Layout decision contract](#layout-decision-contract)). Production stdout passes an `lsm_error!`-routing warn closure and no decision callback; the TUI live preview adds `.with_decision(callback)` to receive `LayoutDecision` events per render.

### Layout algorithm

Input: `Vec<LineItem>` (segments interleaved with inline separators, per [Line items and separators](#line-items-and-separators)), terminal width `W`.

```text
1. Collect pass: walk LineItems, render each segment, and emit a
   LayoutItem sequence. Segments that return `Ok(None)` or `Err(..)`
   drop, and so does the adjacent separator (per the adjacency
   invariants). Width-bounds: if render width < width.min the segment
   drops; if render width > width.max it's truncated with a single-cell
   ellipsis marker (theme-provided, default "…"). Plugin per-render
   `right_separator` overrides are applied here, replacing the inline
   separator immediately to the segment's right.
2. Compute total width = sum of every surviving LayoutItem's width.
3. If total <= W: render as-is.
4. Else: priority-based reflow loop:
     a. Find the highest-priority (numerically largest) segment slot.
        If only priority-0 segments remain, stop.
     b. Compute `overflow = total - W` and `target = cur_width - overflow`.
     c. Call `segment.shrink_to_fit(ctx, rc, target)`. If it returns
        `Some(r)` where `r.width` lies in `[width.min, target]` (the
        configured `width.min` floor, default `0`, is honored the
        same way `apply_width_bounds` honors it), replace the segment's
        rendered output with `r` and recompute total. Segment-side
        intelligence runs first because the segment knows things the
        engine doesn't (which decoration to shed, which prefix is
        signal-bearing).
     d. Else, if the chosen segment declares `truncatable = true`,
        attempt to shrink it to `target` cells via end-ellipsis
        truncation. The shrunk width must be at least `floor`, which
        is the segment's `width.min` if declared, else `2` (one
        content cell plus the ellipsis); a declared `width.min` below
        `2` is clamped up. If feasible, replace and recompute total.
     e. Else (no compact form, no end-ellipsis, or end-ellipsis would
        fall below `floor`) drop the segment outright. Drop the
        adjacent separator with it (right-edge first, left-edge
        fallback when the segment was last in the line).
     f. Recompute total width.
     g. Repeat until total <= W or only priority-0 segments remain.
5. Emit: walk the surviving LayoutItems and write each one's runs to
   stdout. `Separator::None` (text == "") is filtered out so consumers
   don't see empty-text runs.
```

Priority-0 segments are never dropped or truncated by the reflow loop. If total width still exceeds `W` after all droppable segments are removed, render anyway (terminal wraps or truncates visually; worse UX than hiding, but priority-0 means "user said don't drop this").

The `truncatable` flag is opt-in (default `false`). The built-in `workspace` segment opts in so a long `repo/feature-branch-name` shrinks under width pressure instead of disappearing. Numeric segments (model, cost, percent meters, countdowns) leave it `false`: a half-cut percentage reads as the wrong number, which is worse than no number.

### Layout decision contract

Per [ADR-0026](../adrs/0026-layout-decision-observability.md), the layout engine emits typed events at five decision sites so the TUI live preview can address segments by name when a layout decision fires. Production stdout pays no observable cost: the engine emits through a lazy-construction callback that only runs when an observer is attached.

**Observer channel.** Layout engine entry points take a `&mut LayoutObservers<'_>` rather than a bare `warn` callback:

```rust
pub struct LayoutObservers<'a> {
    // private fields — closures captured at the caller's stack frame
}

impl<'a> LayoutObservers<'a> {
    pub fn new(warn: &'a mut dyn FnMut(&str)) -> Self;
    pub fn with_decision(self, on_decision: &'a mut dyn FnMut(&LayoutDecision)) -> Self;
}
```

Production callers construct `LayoutObservers::new(warn)`; the TUI preview chains `.with_decision(callback)` to receive decision events. The engine emits via `observers.emit_with(|| LayoutDecision::shrink_applied(id.clone(), from, to, target))` — the closure only runs when an observer is attached, so disabled-path cost stays at one `Option::is_none()` check per decision site even when segment ids are `Cow::Owned`.

**Decision variants.** One per emit site, with pre/post numerics so the consumer phrases the user-facing string:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutDecision {
    #[non_exhaustive]
    PriorityDrop { id: Cow<'static, str>, priority: u8, terminal_width: u16, overflow: u32, dropped_width: u16 },
    #[non_exhaustive]
    ShrinkApplied { id: Cow<'static, str>, from: u16, to: u16, target: u16 },
    #[non_exhaustive]
    ReflowApplied { id: Cow<'static, str>, from: u16, to: u16, target: u16 },
    #[non_exhaustive]
    WidthBoundUnderMinDrop { id: Cow<'static, str>, rendered_width: u16, min: u16 },
    #[non_exhaustive]
    WidthBoundOverMaxTruncate { id: Cow<'static, str>, rendered_width: u16, max: u16 },
}

impl LayoutDecision {
    /// Remediation hint for the variant, or `None` if not applicable.
    /// Returns `&'static str` so the table is testable without touching emit sites.
    pub fn remediation(&self) -> Option<&'static str>;
}
```

Per-variant struct bodies are `#[non_exhaustive]` so the engine can add a field without breaking pattern-matchers (consumers spell `, ..` in struct-variant matches). The enum itself is NOT `#[non_exhaustive]` — adding a sixth variant SHOULD break every consumer's `match` at compile time, by design.

**Emit sites.** The five decision sites in `apply_layout`:

1. `PriorityDrop` — reflow loop drops a segment outright: `try_shrink` returned no compact form, `try_reflow` end-ellipsis was infeasible (e.g. target falls below the ellipsis floor), or the segment is not `truncatable`. Fires whenever the engine removes a segment under width pressure, regardless of which earlier path it tried.
2. `ShrinkApplied` — `try_shrink` returned a valid compact render.
3. `ReflowApplied` — `try_reflow` end-ellipsis succeeded.
4. `WidthBoundUnderMinDrop` — `apply_width_bounds` returned `None` because rendered width fell below `width.min`.
5. `WidthBoundOverMaxTruncate` — `apply_width_bounds` clipped a too-wide render via `truncate_to`. The emit happens at the `apply_width_bounds` call site, NOT inside `truncate_to` itself — that helper is also reached from `try_reflow`, and a generic emit there would double-fire on reflow paths.

Engine-only `pub(crate)` constructors (`LayoutDecision::shrink_applied(...)` and siblings) `debug_assert!` the implicit width relations: `to <= target < from` for compaction (the `apply_layout` call site enforces `overflow >= 1`), `priority > 0` for drop (mirrors `highest_priority_droppable`'s filter), `rendered_width < min` for under-min drop, `rendered_width > max` for over-max truncate.

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

1. `model`: model display_name. Default `format = "compact"` strips the trailing word "context" from `(X context)` parentheticals (`Opus 4.7 (1M context)` → `Opus 4.7 (1M)`); `format = "full"` renders Anthropic's wire value verbatim. The strip is gated on `Tool::ClaudeCode` since the suffix shape is Claude-specific; other tools (Qwen, Codex CLI, Copilot CLI) render verbatim regardless of `format`.
2. `context_window`: percentage + size (e.g. `45% · 200k`)
3. `context_bar`: visual bar, width configurable (4/6/8/10/12 cells)
4. `cost`: session cost in USD
5. `duration`: session duration
6. `workspace`: directory / worktree hybrid
7. `git_branch`: branch + dirty + ahead/behind (sub-composed). Two complementary compaction layers: per-marker `[segments.git_branch.dirty].hide_below_cells` and `[segments.git_branch.ahead_behind].hide_below_cells` knobs (user-preference layer; default `0` = never auto-hide; key on `rc.terminal_width` only — fire on narrow terminals), and engine-driven `shrink_to_fit` (layout-pressure layer; sheds dirty + ahead/behind to keep the `label + head` prefix when neighboring segments pressure the line, regardless of terminal size — the generic icon wrapper prefixes the compact render afterward when enabled; `head` is the branch name on a normal checkout, the short SHA on detached HEAD, or the symbolic-ref target on unborn HEAD). Generic end-ellipsis truncation isn't safe (the structured tail would be mangled), so `truncatable` stays `false` — under further pressure even after `shrink_to_fit`, the segment drops whole via priority.
8. `rate_limit_5h`: 5-hour percentage + resets-at countdown
9. `rate_limit_7d`: 7-day percentage + resets-at countdown
10. `rate_limit`: combined 5h/7d view; sub-composed from `rate_limit_5h` and `rate_limit_7d` with a tighter layout (users pick either the combined form or the individual segments, not both)
11. `effort`: current `/effort` level (renders `None` until Claude Code emits the effort field; see [user-demand research](../research/user-demand.md) for why this field doesn't flow live today)

Each has its own module in `crates/linesmith/src/segments/<id>.rs` with a small per-segment spec inline (doc comment).

### Icons and Nerd Font glyphs

`[layout_options].icons` controls shipped segment icon defaults. The default `nerdfont` mode prefixes segments whose `SegmentDefaults.icon` is set; `off` suppresses shipped defaults globally. A per-segment `icon = "..."` override always wins, including when global icons are off. `icon = ""` disables the icon for only that segment. Icons are applied by the generic override wrapper after segment render and `shrink_to_fit`, so the icon inherits the segment's style and preserves per-render right separators.

Shipped defaults:

| Segment                                      | Icon codepoint |
| -------------------------------------------- | -------------- |
| `version`                                    | `\u{f121}`     |
| `model`                                      | `\u{2726}`     |
| `context_bar`                                | `\u{f035b}`    |
| `git_branch`                                 | `\u{f126}`     |
| `workspace`                                  | `\u{f07b}`     |
| `session_duration`                           | `\u{f252}`     |
| `rate_limit_5h`                              | `\u{f017}`     |
| `rate_limit_7d`                              | `\u{f073}`     |
| `rate_limit_5h_reset`, `rate_limit_7d_reset` | `\u{21bb}`     |

`cost`, `effort`, `tokens_*`, `vim`, `agent`, `output_style`, `context_window`, and `extra_usage` ship without default icons.

## Edge cases

| Case                                                               | Handling                                                                                                                                                         |
| ------------------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Segment returns `None`                                             | Dropped from layout. Separator also dropped (not left as a floating artifact)                                                                                    |
| Segment panics during render                                       | Panic caught; segment dropped; error logged once per segment per run; rendering continues                                                                        |
| Cache file corrupt / unparseable                                   | Treated as miss; re-rendered; new write replaces bad file                                                                                                        |
| Segment width exceeds terminal width                               | Truncated with ellipsis per `width.max` (if declared); reflow loop further truncates `truncatable` segments before dropping; otherwise dropped unless priority 0 |
| Terminal width unknown (detached tty)                              | Fall back to 200 cells                                                                                                                                           |
| All segments drop due to width pressure                            | Emit blank line (status line is empty but still emitted)                                                                                                         |
| Segment tries to construct `WidthBounds { min: 20, max: 10 }`      | `WidthBounds::new` returns `None`; segment must fix or drop bounds                                                                                               |
| Two segments have the same `id`                                    | Second one rejected at config-load time; first wins                                                                                                              |
| Rhai script errors at render                                       | Plugin segment dropped; error logged; rendering continues                                                                                                        |
| Segment writes to stdout directly (should never)                   | Undefined behavior; segments must return `RenderedSegment`, not print                                                                                            |
| Context has no `git_worktree`, but `workspace` segment expects one | Segment returns `None` (conditional visibility)                                                                                                                  |
| `effort` segment requested but `ctx.effort == None`                | Segment returns `None`; user-visible reason documented in segment's doc comment                                                                                  |

## Testing strategy

Follows `AGENTS.md`: inline `#[cfg(test)] mod tests` for unit tests, `tests/` for integration, `insta` for snapshots, `criterion` for benchmarks.

### Unit tests (per segment, inline `mod tests`)

Every segment in `crates/linesmith/src/segments/` has tests:

- Renders expected output for a canonical `DataContext` (with a fixture `StatusContext` wrapped in it; lazy sources stubbed to `Ok(...)` or `Err(...)` as the segment requires)
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

- **Async segments in v0.1?** The matrix defers async prefetch to v0.2+. For v0.1 all renders are sync; any segment needing network I/O (rate-limit scraping) must cache aggressively to stay within budget.
- **Cache key model**: per-segment cache keys vs. a shared invalidation store. Current design: per-file with invalidators declared in `CachePolicy`. Simpler; may not scale.
- **Panic policy**: catch-and-continue vs. fail-loud in debug builds. Current design: always catch (statusline must never crash the user's terminal). Revisit if users report silent failures as confusing.
- **Rhai cold-start budget**: 2ms per plugin is a rough estimate. Benchmark when we build the rhai integration; if actual overhead is larger, revisit the plugin model.
- **Grapheme-cluster width crate**: `unicode-width` + `unicode-segmentation` vs. alternatives. Decision deferred to implementation; add a row to `research/rust-crate-survey.md` when benchmarking.
- **Catppuccin crate adoption** for palette data: resolved once, applies to both theming and segment color defaults. See [`specs/theming.md`](theming.md) Open Questions for the single point of decision.

## Change log

- 2026-04-17: initial draft (v0.1)
- 2026-04-17: v0.2 incorporating [ADR-0008](../adrs/0008-canonical-type-refinements.md) (Separator::Literal Cow, Segment: Send only, CachePolicy::Invalidated with any_of semantics, WidthBounds newtype) + rate_limit combined segment + Nerd Font glyph source + effort segment clarification + link to plugin-api.md
- 2026-04-19: v0.3 incorporating [ADR-0010](../adrs/0010-data-fetching-architecture.md). Render signature moves from `&StatusContext` to `&DataContext`; `StatusContext` remains accessible via `ctx.status`. Adds `data_deps()` method with a default of `&[DataDep::Status]`. Existing stdin-only segments pick up the default `data_deps()` for free but still migrate their render signature to `&DataContext` and reach stdin fields via `ctx.status`. Rendering pipeline diagram updated to show the dep-union pre-fetch step. See [data-fetching.md](data-fetching.md) for the `DataContext` shape and `DataDep` enum.
- 2026-04-27: v0.4. Adds `SegmentDefaults.truncatable` opt-in (default `false`); under width pressure the layout engine shrinks `truncatable` segments to `(cur_width - overflow)` cells before dropping, with a floor of `max(width.min, 2)`. `workspace` opts in. Numeric segments stay opt-out so end-ellipsis truncation never produces a wrong number.
- 2026-04-27: v0.5. Render signature gains `&RenderContext` as a second argument: `fn render(&self, ctx: &DataContext, rc: &RenderContext) -> RenderResult`. The new struct carries `terminal_width` today and is `#[non_exhaustive]` for additive growth (line index, capability, neighbor info). Segments that don't care about layout state ignore the argument; width-aware segments read `rc.terminal_width` to ladder their own output before the engine's reflow pass runs. Rendering pipeline diagram updated to show the per-render `RenderContext` build step.
- 2026-04-27: v0.6. Adds `Segment::shrink_to_fit(&self, ctx, rc, target) -> Option<RenderedSegment>`. The reflow loop now calls it before falling back to `truncatable` end-ellipsis or drop, letting structured-tail segments (`git_branch`'s `* ↑2 ↓1`) shed decoration while keeping the signal-bearing prefix under layout pressure — not just under terminal narrowness. Default impl returns `None` (current behavior preserved); `git_branch` overrides to suppress its dirty + ahead/behind markers when `target` is below the full-assembly width.
- 2026-05-05: v0.7. Separator-as-item refactor. Separators are now positional `LineItem::Separator` entries the builder produces from `[layout_options].separator`, not a `default_separator` field on `SegmentDefaults`. Strikes that field, the `with_default_separator` chainable, and the `apply_layout_separator` helper. The plugin per-render override path (`RenderedSegment::with_separator`) stays and beats the inline separator at that one boundary. Resolves the prior §Open questions "Separator ownership" item: the layout owns separators authoritatively. Drop logic now removes the adjacent separator with the segment (right-edge first, left-edge fallback when the segment was last in the line).
- 2026-05-08: v0.8. Per [ADR-0024](../adrs/0024-per-boundary-separator-toml.md), `[line].segments` accepts a mixed array of bare strings and inline tables. Bare strings (`"model"`) keep parsing as before — string-only configs are byte-identical at the renderer. Inline tables (`{ type = "separator", character = " | " }`, `{ type = "model", merge = true }`) reach the builder as `config::LineEntry::Item`. The builder now walks `Vec<LineEntry>` instead of `Vec<&str>`: explicit `type = "separator"` entries materialize as `LineItem::Separator(...)` using the entry's `character` override or the global `[layout_options].separator` fallback; segment entries with `merge = true` suppress the boundary at their right edge (both implicit interleave AND any adjacent explicit separator entry). Adjacency invariants in §Line items and separators apply unchanged — leading/trailing/orphan separators are pruned at the renderer's adjacency pass.
- 2026-05-13: v0.9. Per [ADR-0026](../adrs/0026-layout-decision-observability.md), the layout engine surfaces typed events at five decision sites. SemVer-breaking variant shape change: `LineItem::Segment(Box<dyn Segment>)` → `LineItem::Segment { id: Cow<'static, str>, segment: Box<dyn Segment> }` — migration recipe `LineItem::Segment(seg)` → `LineItem::Segment { id, segment: seg }` at every construction and pattern-match site. New `pub struct LayoutObservers<'a>` bundles `warn` (required) and `on_decision` (optional); engine entry points take `&mut LayoutObservers<'_>` instead of a bare `warn` callback. New `pub enum LayoutDecision` with five variants (`PriorityDrop`, `ShrinkApplied`, `ReflowApplied`, `WidthBoundUnderMinDrop`, `WidthBoundOverMaxTruncate`) — per-variant struct bodies are `#[non_exhaustive]` for field-additive forward-compat, the enum itself is NOT (so a future variant breaks every consumer's `match` at compile time, by design). Engine-only `pub(crate)` constructors `debug_assert!` width-relation invariants. The engine emits via `observers.emit_with(|| LayoutDecision::shrink_applied(id.clone(), from, to, target))` — lazy construction means the production-stdout path pays only one `Option::is_none()` check per decision site, zero allocations even when segment ids are `Cow::Owned`.
