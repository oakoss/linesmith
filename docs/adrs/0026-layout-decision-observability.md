# Surface layout decisions through a typed observer callback

- Status: accepted
- Date: 2026-05-13
- Deciders: Jace
- Surfacing bead: lsm-xd8n

## Context and Problem Statement

The layout engine in `crates/linesmith-core/src/layout/mod.rs` makes five silent decisions per render: priority-drop (lower-priority segment dropped to fit width), `try_shrink` success (segment shrinks via `Segment::shrink_to_fit`), `try_reflow` success (truncatable segment ends with an ellipsis), `apply_width_bounds` under-min drop (rendered width below `width.min` hides the segment), and `apply_width_bounds` over-max truncate (rendered width above `width.max` clips with an ellipsis). The existing `warn: &mut dyn FnMut(&str)` channel fires only on misbehavior (`try_shrink` contract violations); successful decisions emit nothing.

Production stdout is fine with silence — the user sees the bytes; that is the contract. The editor TUI's live preview is not. A user editing config evaluates layout decisions and needs a per-segment signal when "cost was dropped at this width" or "workspace truncated to claude-sonn…". How should the layout engine surface those decisions to callers — specifically to the TUI live preview — while keeping the production-stdout path free?

## Decision Drivers

- **Preview must address specific segments.** A signal that says "something dropped" without naming what is worse than silence; it forces a binary search through the config.
- **Production stdout pays zero observable cost.** The daily render path runs sub-20ms on cold start (ADR-0001). Adding overhead to every render for a feature only the editor consumes is a regression.
- **Decision set is small and load-bearing.** Five variants today; growth bounded by the algorithm. Exhaustive matching at the consumer catches a future variant addition at compile time on every consumer.
- **`linesmith-core` is dep-light by ADR-0019.** Adding a dependency to `linesmith-core` propagates to every downstream consumer of the scaffolding crate. Any solution that needs a new crate must clear that bar.
- **One canonical channel.** The existing `warn` channel signals "something went wrong"; mixing "this happened normally" into the same channel collapses two concerns.

## Considered Options

The decision splits into three sub-questions. Each option below names a concrete answer to the primary axis (event channel); the research note ([layout-decision-observability](../research/layout-decision-observability.md)) explores each axis independently.

- **Option 1 — Verbose flag on the existing `warn` callback.** Add a `verbose: bool` argument to `render_to_runs`; emit decision strings into `warn` when verbose is true. Production passes `verbose: false`; preview passes `verbose: true`.
- **Option 2 — Typed `on_decision` callback in a `LayoutObservers` struct, distinct from `warn`.** A `LayoutDecision` enum with five variants; the layout engine takes `observers: &mut LayoutObservers<'_>` bundling `warn` and `on_decision: Option<&mut dyn FnMut(&LayoutDecision)>`. Production constructs `LayoutObservers::new(warn)`; preview adds `.with_decision(callback)`.
- **Option 3 — `tracing` events.** Emit `tracing::event!(target: "linesmith::layout", ...)` at each decision site. The TUI installs a custom `Layer` filtering on the target.
- **Option 4 — `log::kv` structured logging.** Same shape as `tracing` with a smaller dependency and less compositional infrastructure.
- **Option 5 — `miette::Diagnostic` per decision.** Use the structured-diagnostic crate already in scope for parse errors.

## Decision Outcome

Chosen option: **Option 2 — typed `on_decision` callback in a `LayoutObservers` struct**, because (a) the disabled-path cost is a single `Option::is_none()` check per decision site (the engine emits through `emit_with(impl FnOnce() -> LayoutDecision)`, so `LayoutDecision` construction — including `Cow::Owned` user-config id clones — is deferred behind the observer-presence check; production-stdout pays zero allocations even when segment ids are `Cow::Owned`), (b) the typed enum gives the TUI consumer exhaustive matching so the engine cannot silently grow a new decision variant without every consumer failing to compile, (c) zero dependencies added to `linesmith-core`, honoring ADR-0019's scaffolding posture, (d) the "this went wrong" (`warn`) and "this happened" (`on_decision`) channels remain structurally separate, and (e) it is a strict additive scaffold — if a future second consumer wants `tracing` integration (e.g. an `lsm doctor explain` subcommand dumping events to a log), the typed callback wraps a `tracing` emit at the call site without forcing the engine to switch.

The decision rejects Option 1 (collapses two concerns into one stringly-typed channel), Option 3 (adds a dependency to `linesmith-core`; loses exhaustiveness; no second consumer yet to amortize), Option 4 (same shape as tracing with less infrastructure; no good story for the TUI subscriber), and Option 5 (`Diagnostic` is `: Error`-bound, not a fit for non-error emit).

### Sub-decision B: addressing scheme

The layout engine needs an id per `LineItem::Segment` so decision events can address a specific segment. The `Segment` trait does not expose an `id()`; ids live on `LineEntry::segment_id` at config-time. Three options:

1. Extend the `Segment` trait with `fn id(&self) -> &str`.
2. Change the variant to a struct-style variant: `LineItem::Segment { id: Cow<'static, str>, segment: Box<dyn Segment> }`.
3. Address by index into the post-`collect_items_with` slice.

Chosen: **option 2 — promote `LineItem::Segment` to a struct-style variant**, because id is a layout property (it names _which configured entry_ this is — the same `GitSegment` instance might be id `"git"` in one config and `"my_branch"` in a future renamed config), not a render property (segments don't need to know their own id to render). Pattern-matching becomes `LineItem::Segment { id, segment }` directly; no wrapper type to document or maintain.

This is a **SemVer-breaking change** for any downstream caller that constructs or pattern-matches `LineItem::Segment(...)` directly. `#[non_exhaustive]` on `LineItem` only protects against new-variant additions; it does not help when an existing variant's shape changes. `linesmith-core` is pre-1.0 and the only known external surface for `LineItem` today is the builder pipeline within this workspace, so the break is bounded. The CHANGELOG entry should give the migration recipe explicitly: `LineItem::Segment(seg)` → `LineItem::Segment { id, segment: seg }`.

**Fallback id for unnamed entries.** `LineEntry::segment_id()` returns `Option<&str>`. For entries that lack an explicit `segment_id` (anonymous inline segments, or future builder-synthesized segments), the builder synthesizes a stable name from the segment's type, disambiguated by occurrence index when more than one of the same type appears in a line: `"git"`, `"git#2"`, `"git#3"`. Mirrors Oh My Posh's `(name, index)` addressing.

Rejected: option 1 (trait churn across every `impl Segment` in-tree and in plugins for a layout-layer concern), option 3 (index is stable for one frame but `collect_items_with` prunes items mid-pipeline; cross-frame addressing breaks).

### Sub-decision C: payload scope

Per the Firefox Flexbox Inspector lesson (Bugzilla 1501066: the Firefox 65 fix removed a phrase-form "Item was set to shrink" hint that fired unconditionally), payload should carry _numbers_ and the consumer should _phrase_. Each variant carries its own load-bearing dimensions:

```rust
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutDecision {
    #[non_exhaustive]
    PriorityDrop {
        id: Cow<'static, str>,
        priority: u8,
        terminal_width: u16,
        overflow: u32,
        dropped_width: u16,
    },
    #[non_exhaustive]
    ShrinkApplied {
        id: Cow<'static, str>,
        from: u16,
        to: u16,
        target: u16,
    },
    #[non_exhaustive]
    ReflowApplied {
        id: Cow<'static, str>,
        from: u16,
        to: u16,
        target: u16,
    },
    #[non_exhaustive]
    WidthBoundUnderMinDrop {
        id: Cow<'static, str>,
        rendered_width: u16,
        min: u16,
    },
    #[non_exhaustive]
    WidthBoundOverMaxTruncate {
        id: Cow<'static, str>,
        rendered_width: u16,
        max: u16,
    },
}

impl LayoutDecision {
    /// Remediation hint for the decision variant. Method (not field) so
    /// the table lives in one place and is testable without touching emit
    /// sites. Returns `&'static str` — every remediation is a literal.
    pub fn remediation(&self) -> Option<&'static str> {
        match self {
            Self::ShrinkApplied { .. } => Some("Set `width.max` to clamp earlier"),
            Self::WidthBoundOverMaxTruncate { .. } => {
                Some("Increase `width.max` or lower `priority`")
            }
            _ => None,
        }
    }
}
```

`Cow<'static, str>` matches the crate's established id idiom (`Separator::Literal(Cow<'static, str>)` at `segments/mod.rs:185`, `Tool::Other(Cow<'static, str>)` at `input/mod.rs:55`): built-in segment ids stay zero-alloc as `Cow::Borrowed("git_branch")` (the per-emit step); user-config ids land as `Cow::Owned(String)` at `LineItem` construction time (the per-build step) and then stay `Cow::Owned` across every emit. The zero-alloc claim is per-emit, not end-to-end. `Arc<str>` would force a heap allocation per built-in emit for refcounting benefits the engine doesn't use (single-thread emit, synchronous consumer inside the same `render_to_runs` call).

**`#[non_exhaustive]` story.** Per-variant struct bodies _are_ `#[non_exhaustive]` so the engine can add a fifth or later field (e.g. an `engine_pressure_source` discriminator on `ShrinkApplied` if the algorithm grows) without breaking pattern-matchers. The enum itself is _not_ `#[non_exhaustive]` — that would defeat the chosen-option rationale (exhaustive matching catches new variants at compile time on every consumer). Consumer ergonomics cost: every struct-variant `match` arm must end with `..` (rustc enforces this via `E0638` when matching against a `#[non_exhaustive]` external variant), so `LayoutDecision::ShrinkApplied { id, from, to, target }` becomes `LayoutDecision::ShrinkApplied { id, from, to, target, .. }`. The trailing `..` buys forward-field-compat against engine growth.

**Engine-only constructors.** Emit sites go through `LayoutDecision::shrink_applied(id, from, to, target)` and siblings (`pub(crate)` per ADR-0019's scaffolding posture), which `debug_assert!` the implicit width relations: `to <= target < from` for compaction (engine enforces `overflow >= 1` at the `apply_layout` call site, so `target < from` is invariant); `priority > 0` for drop (mirrors `highest_priority_droppable`'s filter — catches future refactors that bypass the helper); `rendered_width < min` for under-min drop; `rendered_width > max` for over-max truncate. Contract violations are engine bugs and the assertions are debug-only.

Pre/post dimensions are the load-bearing numerics. Affected text is _not_ needed at the layout layer — the preview already has the rendered string. Remediation hints are a method, not a stored field, so they stay testable.

### Sub-decision A details: `LayoutObservers` shape

```rust
pub struct LayoutObservers<'a> {
    warn: &'a mut dyn FnMut(&str),
    on_decision: Option<&'a mut dyn FnMut(&LayoutDecision)>,
}

impl<'a> LayoutObservers<'a> {
    pub fn new(warn: &'a mut dyn FnMut(&str)) -> Self { /* ... */ }

    pub fn with_decision(mut self, on_decision: &'a mut dyn FnMut(&LayoutDecision)) -> Self {
        self.on_decision = Some(on_decision);
        self
    }

    pub(crate) fn warn(&mut self, msg: &str) { (self.warn)(msg); }

    /// Emit a layout decision, constructing it only when an observer is
    /// attached. Defers `Cow::Owned(String)` clones of user-config ids
    /// behind the observer-presence check; the disabled path allocates nothing.
    pub(crate) fn emit_with(&mut self, decision: impl FnOnce() -> LayoutDecision) {
        if let Some(cb) = self.on_decision.as_mut() {
            cb(&decision());
        }
    }
}
```

Fields are private; callers go through `new` / `with_decision` to construct, and the engine calls `observers.warn(msg)` / `observers.emit_with(|| LayoutDecision::shrink_applied(id.clone(), from, to, target))` internally. The `impl FnOnce` lazy-construct shape is load-bearing: each `LayoutDecision` carries an `id` that may be a `Cow::Owned(String)` clone of a user-config segment id (one heap allocation per emit), so deferring construction behind the `is_none()` check keeps the disabled path truly allocation-free. The closure compiles to a zero-sized type that LLVM inlines; call-site cost is one set of `||` braces per emit and runtime cost matches `emit(&decision)` on the active path. Single lifetime `'a` on `LayoutObservers<'a>` because Rust unifies independent borrows to their shortest common lifetime; a second lifetime parameter buys no flexibility for `&mut dyn FnMut` borrows. Add one only if a future field needs an independent variance story.

**`warn` is mandatory by type.** Callers wanting a no-op warn channel construct a local closure on their stack frame: `let mut noop = |_: &str| {}; LayoutObservers::new(&mut noop)`. The convenience `render(items, ctx, width)` entry point already owns a closure for the duration of the call — today's `lsm_error!`-routing one at `layout/mod.rs:26-27`, not a no-op — and will thread `LayoutObservers::new(&mut warn)` into `render_to_runs` after migration. No `silent()` constructor exists because there is no `'a` lifetime to bind it to.

The observer callback receives `&LayoutDecision`. Consumers that need to retain events past the call (the TUI's per-frame collector) clone via `events.push(decision.clone())`. `LayoutDecision: Clone` costs one `Cow` clone per event — pointer-copy for built-in segment ids (`Cow::Borrowed`), one short-string heap clone for user-config ids (`Cow::Owned`). Both are negligible at per-frame scale (≤5 decisions × ~16-byte ids). The borrow + opt-in clone keeps both the disabled-path and the immediately-rendered consumer at zero clones.

## Consequences

- Good, because the production-stdout path pays one `Option::is_none()` check per decision site and no allocations for built-in `Cow::Borrowed` ids — measurable in microseconds even at five-decisions-per-frame.
- Good, because exhaustive matching at the consumer means a future engine change adding a sixth decision variant breaks every consumer's `match` at compile time. The compiler catches stale UIs.
- Good, because the addressing change (id-in-`LineItem::Segment`) means the preview can render "git: shrunk 22→17" rather than "something shrunk at slot 3" — surfaces the user-facing config name.
- Good, because `Cow<'static, str>` keeps built-in segment ids zero-alloc on the production-stdout path, matching the crate's existing `Separator::Literal` idiom.
- Good, because the warn channel keeps its error-only contract.
- Good, because `LayoutDecision: Clone + Debug + PartialEq + Eq` lets tests pin each of the five decision sites by collecting events into a `Vec<LayoutDecision>` and asserting the captured sequence.
- Bad, because `LineItem::Segment(...)` is a SemVer-breaking shape change for any external pattern-match or constructor. Mitigated by linesmith-core being pre-1.0 and the only current external surface being this workspace's builder. Migration recipe: `LineItem::Segment(seg)` → `LineItem::Segment { id, segment: seg }` at every construction and pattern-match site. The CHANGELOG entry should quote this verbatim.
- Bad, because the engine signatures grow one reference. `render_to_runs(items, ctx, terminal_width, warn)` becomes `render_to_runs(items, ctx, terminal_width, observers)` where `observers` bundles `warn` and `on_decision`. Per-test boilerplate is two lines (`let mut noop = |_: &str| {}; let mut observers = LayoutObservers::new(&mut noop);`); a `#[cfg(test)]` `macro_rules!` shim can expand that inline if the boilerplate becomes load-bearing (a fn-shaped helper can't return `LayoutObservers<'a>` from a locally-allocated closure — the borrow can't escape the helper's frame).
- Neutral, because the emit sites for `try_reflow` and `apply_width_bounds` over-max both internally invoke `truncate_to`. Decisions emit at the **call sites** of `truncate_to`, not inside the helper, so a reflow does not double-fire as both a `ReflowApplied` and a `WidthBoundOverMaxTruncate`.
- Neutral, because `tracing` migration is deferred but not precluded. The typed callback wraps a `tracing::event!` emit at the call site if a future consumer needs it; the inverse (a `tracing`-emitting engine driving a typed-match preview) is harder, making the typed callback the more conservative scaffold.
- Neutral on threading. The closures are `&mut dyn FnMut` (not `Send`/`Sync`/`Clone`) and decisions emit on the rendering thread. Consumers wanting cross-thread propagation clone into an owned `LayoutDecision` and pass through their own channel.

### Confirmation

This decision is confirmed when:

- The TUI live preview renders a per-segment status indicator for each non-clean decision (drop / shrink / reflow / under-min / over-max), addressing segments by their config id.
- Production stdout's render benchmark shows no measurable regression after the observer is plumbed (sub-20ms cold-start budget per ADR-0001 holds).
- Layout-engine tests pin each of the five decision sites by passing a `Vec<LayoutDecision>`-collecting observer and asserting the captured events match.

Revisit if:

- A second non-TUI consumer of layout decisions appears (an `lsm doctor explain` subcommand; a remote-debugging endpoint; a log-file dump). At that point the open-channel benefits of `tracing` start to amortize. The typed-callback scaffold does not preclude `tracing` — wrap one inside the other at the call site.
- A future free-text editor (lsm-herx.33 format-template editor) needs decisions on _every_ segment, including those that rendered cleanly, to power conditional-surfacing badges. Today's "emit only when the engine took the path" matches the Firefox-Inspector lesson; a richer UI may want a `Rendered` variant to mark clean evaluations explicitly.

## Pros and Cons of the Options

### Option 1 — Verbose flag on `warn`

- Good, because zero API additions; one boolean.
- Bad, because conflates "this went wrong" with "this happened normally" into a string channel. The TUI cannot distinguish a real warn from a decision narration without parsing the string back into structure.
- Bad, because the preview consumer cannot match exhaustively. A new decision string lands as free-form text; consumers updated independently drift.

### Option 2 — Typed `on_decision` callback in a `LayoutObservers` struct (chosen)

See [Decision Outcome](#decision-outcome).

### Option 3 — `tracing` events

- Good, because composes with the broader Rust observability ecosystem. Subscribers, layers, level filtering, span context.
- Good, because metadata-interest checks gate event construction; disabled-path cost is one check per emit site.
- Bad, because adds `tracing` as a `linesmith-core` dependency, propagating to every downstream consumer of the scaffolding crate (ADR-0019).
- Bad, because loses exhaustive matching. Events are open-ended key-value bags; consumers re-parse field values via the visitor pattern.
- Bad, because no second consumer exists yet. The infrastructure benefits don't amortize until a third or fourth consumer appears.

### Option 4 — `log::kv` structured logging

- Good, because smaller than `tracing` (one less crate).
- Bad, because still loses exhaustiveness.
- Bad, because no compositional layer ecosystem; the TUI would have to install a custom `log::Log` impl.
- Bad, because still adds a dependency to `linesmith-core`.

### Option 5 — `miette::Diagnostic`

- Bad, because `Diagnostic: Error`. Layout decisions are not errors and forcing them through an error type misnames the channel.
- Bad, because adds `miette` to layout-engine paths it doesn't otherwise touch.

## More Information

- Driving research: [layout-decision-observability](../research/layout-decision-observability.md). Surveyed ccstatusline / CCometixLine / claude-powerline / claude-statusline-powerline (no precedent), Starship `explain` and `STARSHIP_LOG=trace` (closest existing UX in the prompt space), Oh My Posh `debug` (proven user-readable per-segment `name(active) - duration` format), Powerlevel10k / powerline-go / powerline-rs (no introspection), Firefox Flexbox Inspector (richest decision-observability precedent — bug 1501066's conditional-surfacing lesson is decisive), `tracing` / `log::kv` / `miette` / `comfy-table` / `tabled` / `ratatui` (Rust observability and layout-pressure libs are universally silent), `terraform plan` / `ansible --diff` / `git rebase -i` / `rustc -Zprint-mono-items` (explain-mode patterns).
- Related ADRs: [ADR-0019](0019-publish-linesmith-core-as-scaffolding-from-v0-1.md) (dep-light scaffolding crate constraint), [ADR-0014](0014-best-effort-parse-with-segment-isolation.md) (existing `lsm_warn!` channel philosophy), [ADR-0016](0016-tui-screen-state-machine.md) (preview-as-persistent-header context).
- Open follow-ups (to file as beads after acceptance):
  - Refactor `LineItem::Segment(Box<dyn Segment>)` → `LineItem::Segment { id: Cow<'static, str>, segment: Box<dyn Segment> }`. Update every builder site (`segments/builder/dispatch.rs`) to source the id from `LineEntry::segment_id()` with the type-based fallback for unnamed entries (`"git"`, `"git#2"`). Pin built-in ids to `Cow::Borrowed` by looking the `&str` up against `BUILT_IN_SEGMENT_IDS` (defined at `segments/mod.rs`) before falling back to `Cow::Owned(id.to_string())` for plugin/user-config ids — this is what preserves the zero-alloc per-emit story for the production-stdout path. CHANGELOG flag with the migration recipe.
  - Introduce `pub struct LayoutObservers<'a>` with private fields, `new` / `with_decision` constructors, and `pub(crate)` `warn` / `emit` accessors. Migrate `render_to_runs` and `render_with_warn` to accept `&mut LayoutObservers<'_>`. The existing `render(items, ctx, width)` convenience entry point constructs its `lsm_error!`-routing warn closure on its stack frame (preserving today's behavior — it is _not_ a no-op) and threads `LayoutObservers::new(&mut warn)` through. Test helpers wanting a true no-op observer either accept the warn closure by `&mut dyn FnMut(&str)` parameter (so the closure lives on the caller's frame) or use a `macro_rules!` shim that expands inline — `LayoutObservers<'a>` cannot escape the frame that owns the closure.
  - Define `pub enum LayoutDecision` with the five variants (each `#[non_exhaustive]` on its struct body) and `pub fn remediation(&self) -> Option<&'static str>`. Engine-only `pub(crate)` constructors with `debug_assert!` for width-relation invariants. Derives: `Clone + Debug + PartialEq + Eq`.
  - Emit decisions at the five sites in `layout/mod.rs`: priority-drop in `apply_layout`'s reflow loop, `try_shrink` success, `try_reflow` success, `apply_width_bounds` under-min drop, and `apply_width_bounds` over-max truncate (at the call site, NOT inside `truncate_to` — that helper is also reached from `try_reflow` and a generic emit there would double-fire on reflow paths).
  - TUI preview consumer in `crates/linesmith/src/tui/preview.rs`: collect events per frame, render per-segment status badges in the warnings panel.
  - Update `docs/specs/segment-system.md` §Layout algorithm with the decision-emit contract.
