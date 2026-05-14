# Layout decision observability: surfacing drop/truncate decisions to a live preview

- Date: 2026-05-13
- Author: Claude Code research agent (for Jace Babin)
- Scope: Survey how status-line, shell-prompt, and adjacent layout-pressure tools surface "this item got dropped/truncated/shrunk" decisions, and how Rust observability libraries shape the channel through which a layout engine could emit those events to a TUI live preview.

## Question

linesmith's layout engine (`crates/linesmith-core/src/layout/mod.rs`) makes five silent decisions today: priority-drop, `shrink_to_fit` success, truncatable end-ellipsis reflow, `apply_width_bounds` under-min drop, and `truncate_to` over-max ellipsis. Production stdout is happy with silence (the user sees the bytes; that's the contract). The editor TUI's live preview is not — the user is consciously evaluating layout decisions and needs a per-item signal when "cost was dropped at this width" or "workspace got truncated to claude-sonn…".

Three sub-decisions stack:

- **A. Event channel.** Extend the existing `warn: &mut dyn FnMut(&str)` callback with a verbose flag, OR add a separate `on_decision: Option<&mut dyn FnMut(LayoutDecision)>` callback distinct from `warn`, OR adopt a `tracing`-style structured event boundary.
- **B. Addressing scheme.** Extend `Segment` trait with `fn id(&self) -> &str`, OR carry the id alongside the `Box<dyn Segment>` in `LineItem`, OR address-by-index.
- **C. Scope of emitted info.** Just the decision tag, OR pre/post dimensions, OR the affected text, OR remediation hints.

## Sources

### Parity target

- ccstatusline (Sirmalloc, TypeScript). README, USAGE.md, and `src/utils/{renderer.ts,compaction.ts}` — <https://github.com/sirmalloc/ccstatusline>, fetched 2026-05-13. README says "Preview your status line in real-time" but the rendering pipeline silently truncates: `if (plainLength > maxWidth) { statusLine = truncateStyledText(statusLine, maxWidth, { ellipsis: true }); }`. No per-segment metadata about who was clipped. Widgets are addressed by enum-ish "type" strings (Directory, Git, Model, Usage, Time, Cost, OutputStyle).
- ccstatusline `src/utils/` directory listing: `renderer.ts`, `compaction.ts`, `powerline.ts`, `terminal.ts`, plus widget infra. No file named `debug.ts`, `explain.ts`, or `trace.ts` — confirmed via repo tree at fetch time.

### Adjacent Claude statusline tools

- CCometixLine (Haleclipse, Rust). README at <https://github.com/Haleclipse/CCometixLine>, fetched 2026-05-13. "TUI configuration interface with real-time preview." No debug/explain/trace mode in README. Segments named by enum-ish type strings (Directory, Git, Model, Usage, Time, Cost, OutputStyle).
- claude-powerline (Owloops, TS). <https://github.com/Owloops/claude-powerline>, fetched 2026-05-13. Has a `CLAUDE_POWERLINE_DEBUG=1` env var for "diagnostic output" — content not specified in README; not surfaced in a TUI. Auto-wrap mentioned: "Segments flow naturally and wrap to new lines when they exceed the terminal width." No truncation reasons, dropped-segment logs, or layout-decision traces documented.
- claude-powerline-rust (david-strejc, Rust). <https://github.com/david-strejc/claude-powerline-rust>, fetched 2026-05-13. No debug/explain mode in README; segments addressed by config key (directory, git, today, block, context).
- claude-statusline-powerline (spences10). <https://github.com/spences10/claude-statusline-powerline>, fetched 2026-05-13. Segments addressed by `type` string in JSON `lines` array. No observability documented.

### Shell prompt tools

- Starship `explain`. <https://starship.rs/faq/>, <https://github.com/starship/starship/blob/master/src/print.rs> (the `pub fn explain` block), fetched 2026-05-13. Output format per-module: `" "{value}" ({duration}) - {description}"`, sorted, right-padded for description alignment. The module **name** is not printed in `explain` output; only the rendered value, duration, and the module's description string. No mention of "this module didn't render" — `explain` filters to non-empty modules and skips `line_break`.
- Starship trace logging. `STARSHIP_LOG=trace starship module rust` and `STARSHIP_LOG=trace starship timings` (from FAQ). Trace dumps per-module timing > 1ms; primarily a perf-debugging surface, not a layout-decision surface.
- Oh My Posh `oh-my-posh debug`. <https://ohmyposh.dev/docs/faq/> and `src/prompt/debug.go:41-66`, fetched 2026-05-13. Output is a `Segments:` heading followed by one line per segment, formatted via `fmt.Sprintf("%-*s - %3d ms\n", largestSegmentNameLength, segmentName, duration)` — name-padded to the widest name in the prompt for column alignment, with the `(active)` boolean folded into the name. The `(true)`/`(false)` flag means "enabled and rendered" vs "hidden/disabled by template or condition" — not "dropped for width." Segments are addressed by type-and-index (config has a numeric `index` to disambiguate two segments of the same `type`).
- Powerlevel10k `p10k help segment` / `p10k segment` builder. <https://github.com/romkatv/powerlevel10k>, fetched 2026-05-13. No introspection: `p10k segment` is the **constructor** users call inside their `prompt_example()` to emit colored chunks; there is no "explain" / "why didn't my segment show" command. Debugging is by-eye via the `p10k configure` re-runs.
- powerline-go (Go). <https://github.com/justjanne/powerline-go>, fetched 2026-05-13. Segments are first-class structs with a `Name` field. Modules loaded via `-modules aws,bzr,cwd,...`. External plugins are invoked as `powerline-go-MODULE` and return JSON `Segment` structs over stdout. No drop/truncate observability documented.
- powerline-rs (Rust, alxhill / jD91mZM2 forks). <https://crates.io/crates/powerline-rs>, fetched 2026-05-13. powerline-shell rewrite; module-based addressing parallels powerline-go. No explain mode.

### Rust observability libraries

- `tracing` crate. <https://docs.rs/tracing/latest/tracing/>, <https://docs.rs/tracing/latest/tracing/struct.Event.html>, <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/layer/index.html>, fetched 2026-05-13. Events carry structured fields via a `ValueSet`; subscribers/layers observe them through a visitor pattern (`Visit` trait). "If no currently active subscribers express interest in a given set of metadata by returning true, then the corresponding Span or Event will never be constructed" — i.e. disabled-path overhead is the cost of one interest check.
- `log` crate kv. <https://docs.rs/log/latest/log/kv/>, fetched 2026-05-13. Structured key-value logging stable as of `log` 0.4.27+ (kv finalized in late-2024 / early-2025; the kv module itself first landed unstable in 0.4.6). Syntax: `info!(a = 1; "Something of interest")`. Visitors via `VisitSource` / `VisitValue`. Simpler API than tracing but with less compositional infrastructure.
- `miette`. <https://docs.rs/miette/latest/miette/trait.Diagnostic.html>, fetched 2026-05-13. `Diagnostic` is `: Error`-bound: rich metadata (`code`, `severity`, `help`, `labels`, `related`, `source_code`) but bound to error reporting. Not a fit for non-error observability emit.
- `comfy-table`. <https://docs.rs/comfy-table/latest/comfy_table/>, fetched 2026-05-13. Performs grapheme-aware column truncation with a configurable ellipsis indicator but emits no events, warnings, or callbacks when truncation happens. Detecting it requires comparing input width to configured constraints outside the library.
- `tabled`. <https://github.com/zhiburt/tabled>, fetched 2026-05-13. Same shape — silent rendering with configurable formatting; no observer hooks for truncation/wrap.
- `ratatui` layout. <https://ratatui.rs/concepts/layout/>, <https://docs.rs/ratatui/latest/ratatui/layout/enum.Constraint.html>, fetched 2026-05-13. Cassowary solver via kasuari; constraints (`Length`, `Min`, `Max`, `Percentage`, `Ratio`, `Fill`) compose but the solver result is opaque — no per-constraint "this got starved" event. Ratatui has a `constraint-explorer` example (<https://ratatui.rs/examples/layout/constraint-explorer/>) that visualizes a layout interactively, which is a UI-side answer rather than a library-side emit channel.

### Web / CSS layout debuggers

- Firefox Flexbox Inspector. <https://firefox-source-docs.mozilla.org/devtools-user/page_inspector/how_to/examine_flexbox_layouts/>, <https://hacks.mozilla.org/2019/01/designing-the-flexbox-inspector/>, <https://bugzilla.mozilla.org/show_bug.cgi?id=1501066>, fetched 2026-05-13. Per-item sidebar with: Content Size, Flexibility ("how much a flex item grew or shrunk based on its flex-grow value when there is extra free space or its flex-shrink value when there is not enough space"), Minimum Size ("only appears when an item is clamped to its minimum size"), Final Size. The inspector previously emitted a phrase-form hint ("Item was set to shrink") that bug 1501066 flagged as misleading; the Firefox 65 fix made the messaging bail out entirely when the engine didn't take that path. The inspector's "minimap" diagram + step chart visualizes each algorithm stage (independent of bug 1501066); items are addressed by selector and tree position, not numeric id.
- Chrome DevTools Flex/Grid. <https://developer.chrome.com/docs/devtools/css/flexbox>, fetched 2026-05-13. A `flex` badge appears next to flex containers in the Elements panel; clicking it overlays the layout. Container-side editing focus; per-item shrink narrative is weaker than Firefox's (confirmed at fetch).

### Build / plan / diff tools (explain-mode patterns)

- `terraform plan`. <https://www.hashicorp.com/en/blog/terraform-0-14-adds-a-new-concise-diff-format-to-terraform-plans>, fetched 2026-05-13. Per-resource action prefix: `+` create, `-` destroy, `~` in-place change, `-/+` destroy-and-recreate. Attribute-level inline `old -> new`. 0.14 introduced the concise renderer to hide unchanged fields.
- `ansible --diff`. <https://docs.ansible.com/projects/ansible/latest/playbook_guide/playbooks_checkmode.html>, fetched 2026-05-13. Unified diff format with `--- before` / `+++ after` headers and `±` lines. Module-scoped (template, copy, lineinfile, file, etc.).
- `git rebase --interactive`. <https://git-scm.com/docs/git-rebase>, fetched 2026-05-13. Action prefix per row: `p`/`pick`, `r`/`reword`, `e`/`edit`, `s`/`squash`, `f`/`fixup`, `x`/`exec`. The action is the layout decision; the entry identifies the commit.
- `rustc -Zprint-mono-items` / `-Zdump-mono-stats`. <https://doc.rust-lang.org/unstable-book/compiler-flags/dump-mono-stats.html>, fetched 2026-05-13. Dumps per-CGU monomorphized items with sizes — pure dump-style explain, no integration into a live UI.

### linesmith internals (current state, 2026-05-13)

- `Segment` trait — `crates/linesmith-core/src/segments/mod.rs:418`. No `id()` method. Methods: `render`, `shrink_to_fit`, `data_deps`, `defaults`, `truncatable`.
- `LineItem` enum — `crates/linesmith-core/src/segments/mod.rs:704`: `Segment(Box<dyn Segment>) | Separator(Separator)`. Marked `#[non_exhaustive]`.
- `LineEntry::segment_id` — `crates/linesmith-core/src/config.rs:221`, returns `Option<&str>`. The id lives at config-time, not at layout-time.
- `render_with_warn` — `crates/linesmith-core/src/layout/mod.rs:55`, threaded `warn: &mut dyn FnMut(&str)`. Single channel today, free-text only.
- The five silent decisions live at: priority-drop in `apply_layout`'s reflow loop, `try_shrink` success, `try_reflow` success (truncatable end-ellipsis), `apply_width_bounds` under-min drop, and `apply_width_bounds` over-max truncate (which internally calls `truncate_to`, a generic helper also reached from `try_reflow`). All five currently emit _nothing_ through `warn` on the success path; `try_shrink` emits via `lsm_warn!` only on the misbehaving-segment branch.

## Findings

### 1. The parity target is silent — ccstatusline doesn't explain its layout

ccstatusline's "Preview your status line in real-time" is a render preview, not a decision preview. The width-handling cascade — flex-mode subtraction, pre-rendered max widths, post-render truncation with ellipsis — is purely structural; the user sees the truncated string in the preview and infers what happened. There is **no** widget-level "this got dropped/clipped" channel: `truncateStyledText` operates on the joined line and emits the ellipsis as the only signal.

This is a parity gap, not a parity match. linesmith's TUI is being asked to show _more_ than ccstatusline shows. The decision is downstream of choosing a more expressive preview than ccstatusline, not matching it exactly.

### 2. Adjacent CC statusline tools have no precedent either

Across CCometixLine, claude-powerline, claude-powerline-rust, and claude-statusline-powerline: zero per-segment layout-decision observability. claude-powerline has a debug env var that produces unspecified diagnostic output; none of them expose drop/truncate reasons to a live preview surface. linesmith is in front of the field here.

For the ADR framing: there is no incumbent UX to defer to. The design space is open.

### 3. The strongest shell-prompt precedent is Starship's `explain` — but it's not a layout-decision channel

`starship explain` is the closest existing UX in the prompt-tool space, and it deliberately shows three things per module: the rendered value, the duration, and the description string. It does **not** show:

- Whether the module _would_ have rendered but didn't (it filters to non-empty).
- Whether the value was clipped for width (Starship doesn't do width-aware drop).
- The module's internal id alongside the value (description text identifies it, in prose).

The print loop addresses the modules by their internal slug for filtering (e.g. `line_break` is excluded), but the user-facing output identifies modules by their _description string_, not their slug. This is a small but interesting choice: it makes the explain output speak the user's language rather than the implementer's.

`STARSHIP_LOG=trace starship timings` is a developer-grade trace, not a user-grade explain. linesmith will want both surfaces eventually; the preview channel is the user-grade one.

### 4. Oh My Posh's `debug` is the most directly applicable precedent

`oh-my-posh debug` is the only surveyed tool that prints, per segment: identity, boolean enabled state, and timing. Output is a `Segments:` heading followed by one row per segment:

```text
Segments:

ConsoleTitle(true) -   0 ms
session(true)      -   0 ms
path(true)         -   1 ms
git(true)          -   3 ms
```

(Format string: `"%-*s - %3d ms\n"` with the name padded to the widest segment name in the prompt; source at `src/prompt/debug.go:41-66`.) The `(true)`/`(false)` is "is this segment going to emit in this prompt" — which folds template-driven and condition-driven hides into one flag. It does _not_ differentiate "hidden by template" from "dropped for width" because Oh My Posh's prompts don't compete for terminal cells the way linesmith's segments do. The shape — _segment-id paired with a structured outcome tag and a numeric_ — is exactly what linesmith's preview needs, and proven readable.

The relevant lesson: address by user-known name, pair it with a tagged outcome, attach the numeric that matters (width for linesmith, ms for omp).

### 5. The richest decision-observability precedent is the Firefox Flexbox Inspector

This is the conceptual analogue to linesmith's problem: a layout engine with constraint-resolution (basis, grow, shrink, min/max clamp) running inside a live editor surface. Firefox displays, _per flex item_:

- Content Size (raw)
- Flexibility (grow/shrink amount, signed)
- Minimum Size — **only when the item is clamped to it**
- Final Size

Plus a "minimap" diagram color-coded to a step chart that walks the algorithm stages. The Bugzilla history (1501066) is instructive: the inspector previously emitted "Item was set to shrink" even when no shrink occurred, and the Firefox 65 fix made the messaging bail out entirely when the engine didn't take that path. **Conditional surfacing — show the field only when the decision was actually taken — was the bugfix.**

For linesmith: emit only when the decision was taken, and let the preview pick a rendering. Don't emit "ShrinkSkipped" for every non-shrunk segment.

### 6. Rust observability: callback-with-tagged-enum vs `tracing`

Two shapes are worth considering.

**Shape α — extend the `warn` callback into a typed-event callback** (`on_decision: Option<&mut dyn FnMut(LayoutDecision)>`):

- Zero new dependencies. linesmith's layout engine already threads `warn` through every entry point; adding a second optional `&mut dyn FnMut(LayoutDecision)` parameter mirrors the existing pattern.
- Tagged enum gives the consumer exhaustive matching (the TUI preview can `match` on the five variants — `PriorityDrop | ShrinkApplied | ReflowApplied | WidthBoundUnderMinDrop | WidthBoundOverMaxTruncate` — defined in §9 — and statically know it handled every one). When the engine adds a new decision type, every consumer fails to compile until updated. This is a real win for an engine where the decision set is small, named, and load-bearing.
- Compile-time disabled-path overhead: `None` means the engine skips the emission entirely; one `is_none()` check per decision site. Effectively free.
- Cost: every new entry point through the layout engine grows another parameter (or stays in a struct). `render_to_runs` takes 4 args today (the inner entry point); `render_with_warn` takes 7 — neither benefits from another positional parameter. A `LayoutObservers { warn, on_decision }` struct is cleaner at both call sites.

**Shape β — `tracing` events with a `LayoutDecision` newtype as fields**:

- Idiomatic Rust observability. `tracing::Event` is already designed for "this happened, here are the fields" with subscriber-side composition. A `linesmith-core` crate that emits `tracing::event!(target: "linesmith::layout", Level::INFO, decision = "priority_drop", id = %seg_id, ...)` would be consumable by the TUI via a custom `Layer` that filters `target == "linesmith::layout"`.
- Bigger dependency. `tracing` (core, not subscriber) is small but non-trivial; `linesmith-core` is dep-light by ADR. Adding `tracing` to core means every consumer takes the dependency.
- Loses exhaustiveness. `tracing` events are open-ended key-value bags; a TUI consumer can't statically know every decision type was handled. Visitor-pattern field extraction is also runtime-typed (Debug / Display / valuable), so the TUI would re-parse decision strings into an internal enum — duplicate work plus a string-typed API surface where a typed one would do.
- Compile-time disabled-path overhead: documented "if no subscribers express interest, the event is never constructed" — i.e. one interest check.
- Production-stdout codepath has no subscriber today; adding the macro calls won't cost it anything material, but they will run a metadata-interest check per decision site.

**Shape γ — `log::info!(...; "...")` with kv fields**: Same shape as tracing but smaller. Loses the same exhaustiveness. Less compositional (no Layer ecosystem), and no clean TUI subscriber story (would need a custom `log::Log` impl). Not a contender against α.

α (typed callback) wins on three counts for linesmith's TUI-only preview consumer:

1. The decision set is small and load-bearing — exhaustive matching is more valuable than the open-ended schema of `tracing`.
2. Zero deps on core.
3. The TUI is the _only_ current consumer; the open-channel benefits of `tracing` (multiple subscribers, level filtering, span context) don't pay off until a second consumer exists.

If a future consumer appears (`lsm doctor explain`, a log dump, a remote diagnostic), wrapping the callback in a `tracing` subscriber bridge is straightforward — α is the conservative scaffold for β.

### 7. Comfy-table, tabled, ratatui: layout-pressure libs are universally silent

None of comfy-table, tabled, or ratatui emit per-item events when truncation or clamping happens. This is _evidence of a missing pattern in the Rust layout-library ecosystem_, not a model to follow. The pattern linesmith adopts is **closer to a compiler's "explain" mode** (rustc `-Z` flags, terraform plan symbols) than to anything in the table-rendering space.

ratatui's `constraint-explorer` example is a UI answer to the same problem (visualize an opaque solver result) — a good UX reference for what the preview should _show_ once the engine emits, but it doesn't reduce the API question.

### 8. Addressing: ids should travel at the layout layer, not the segment trait

linesmith's `Segment` trait has no `id()` today; the id lives on `LineEntry::segment_id` (config-time). Three options for plumbing id to the layout engine:

- **Extend the `Segment` trait** with `fn id(&self) -> &str`. Clean at emit sites (the layout walks `LineItem::Segment(seg)`, asks `seg.id()`, includes it in the event). Cost: every `impl Segment` in tree and in plugins now has a new method.
- **Carry the id alongside the `Box<dyn Segment>`**: change `LineItem::Segment(Box<dyn Segment>)` to `Segment { id: Cow<'static, str>, segment: Box<dyn Segment> }`. The layout already pattern-matches on the variant, so the id is in scope at every emit site without trait churn. **This is a SemVer-breaking change** for any downstream caller that constructs or pattern-matches `LineItem::Segment(...)` directly; `#[non_exhaustive]` only protects against new-variant additions, not existing-variant shape changes. linesmith-core is pre-1.0 and the only known external surface for `LineItem` today is the builder pipeline within this workspace, so the break is bounded — but the ADR should call it out explicitly and the changelog should flag it. An alternative shape — keep the tuple, wrap the existing payload in `pub struct SegmentItem { id, segment }`, and change the variant to `Segment(SegmentItem)` — minimizes the migration to one extra `.segment` deref per call site while still requiring downstream callers to update their construction syntax.
- **Address by index** (`items[3]`). Stable for one frame, useless across frames (separator pruning shifts indices). Don't.

**Carry the id alongside the Box** is the cleanest approach. The id is a _layout property_ (which configured item this is), not a _segment-rendering property_ (segments don't need their own id to render — they're constructed by config; the same `GitSegment` might be `"git"` in one config and `"my_branch"` in another after future renaming). Pushing id onto the trait conflates the trait's mission (render) with the layout layer's mission (route decisions back to a UI).

Two finer points:

- Starship's `explain` (description string rather than slug) suggests linesmith should distinguish a stable machine id (`"git"`) from a user-facing label. Today both can be the same; the data type should leave room to diverge.
- ccstatusline addresses widgets by enum-ish type names (`Directory`, `Git`); claude-powerline by config-key dot paths (`context.bar`, `git.head`). For linesmith's preview-event use, the stable id from `LineEntry::segment_id` is what the user sees in their TOML — the right addressing scheme.

### 9. Scope of the payload: emit the dimensions, not the prose

Firefox bug 1501066 is the decisive datum: a hint that fires unconditionally goes stale. Numeric fields stay accurate because they describe what actually happened.

A `LayoutDecision::ShrinkApplied { id: Arc<str>, from: u16, to: u16, target: u16 }` event is self-describing: the preview can render "git: shrunk 22→17 cells (target 17)" or paint a width-arrow icon. The engine is not locked to a specific UI string.

A method-based remediation lookup — `decision.remediation()` returning `Option<&'static str>` — carries the engine's preferred phrasing as a method, not a stored field. The TUI can format it one way; a doctor-mode CLI another; tests pin the table without touching every emit site.

Recommended payload, keyed by decision type:

- `PriorityDrop { id, priority, terminal_width, overflow_at_drop }`
- `ShrinkApplied { id, from: u16, to: u16, target: u16 }`
- `ReflowApplied { id, from: u16, to: u16, target: u16 }` (truncatable end-ellipsis)
- `WidthBoundUnderMinDrop { id, rendered_width: u16, min: u16 }`
- `WidthBoundOverMaxTruncate { id, rendered_width: u16, max: u16 }`

Pre/post dimensions are the load-bearing numerics. Affected text is _not_ needed at the layout layer — the preview already has the rendered string. Remediation hints should be a function on `LayoutDecision`, not a stored field, so they stay testable.

## Conclusions

**A. Event channel: typed callback on a `LayoutObservers` struct** (Shape α), distinct from `warn`. Promote `render_with_warn`'s `warn: &mut dyn FnMut(&str)` parameter to `observers: &mut LayoutObservers<'_>` where:

```rust
pub struct LayoutObservers<'a> {
    pub warn: &'a mut dyn FnMut(&str),
    pub on_decision: Option<&'a mut dyn FnMut(LayoutDecision<'a>)>,
}
```

`on_decision = None` is the production-stdout case; preview supplies `Some(&mut |d| events.push(d.to_owned()))`. The exhaustive `match` on `LayoutDecision` at the consumer keeps the small decision set honest as it grows.

Reject `tracing` (β) for now: more deps, loses exhaustiveness, no second consumer to amortize. Reject extending `warn` with a verbose flag: collapses two concerns (errors and decisions) into one stringly-typed channel, exactly what is being fixed.

**B. Addressing: carry the id alongside `Box<dyn Segment>` in `LineItem`.** Change `LineItem::Segment(Box<dyn Segment>)` to `LineItem::Segment { id: Arc<str>, segment: Box<dyn Segment> }` (or `Cow<'static, str>` if static-only suffices in practice). The layout engine sees the id at every emit site; the `Segment` trait stays focused on rendering. Source the id from the config builder (`LineEntry::segment_id`, falling back to a stable type-based default for unnamed entries — `"git"`, `"git#2"`, etc. — same scheme Oh My Posh uses with its `index`).

Reject extending the `Segment` trait. Reject index-based addressing.

**C. Payload: per-decision struct fields with pre/post dimensions; remediation as a method.** Match the Firefox-Inspector lesson: emit numbers, let the consumer phrase. Don't bake user-facing strings into the engine.

Payload size: ~5 fields, ~20 bytes per decision if `id` is borrowed. Worst-case 6 segments × 5 decisions = ~600 bytes per frame. Negligible.

## Implications / actions

### ADR-0026 sections this should drive

- **Context**: The TUI live preview is a second consumer of the layout engine with different requirements than the production stdout path. Production wants silence; preview wants per-decision narration. The current `warn` channel conflates these (and would corrupt production if decision events flowed through it).
- **Decision Drivers**:
  - The TUI must be able to point at a specific segment and say "this got dropped/clipped/shrunk."
  - The production stdout path must pay zero observable cost.
  - The decision set is small (5 today, growth bounded) and load-bearing — exhaustive matching at the consumer is more valuable than open schemas.
  - linesmith-core is currently dep-light by prior ADR; adding `tracing` would propagate to every downstream consumer.
- **Considered Options** (cite this doc):
  1. Extend `warn` with a verbose flag. Rejected: conflates two channels.
  2. **Typed `on_decision` callback in a `LayoutObservers` struct.** _Recommended._ Mirrors the existing warn-callback pattern, zero new deps, exhaustive matching, free in the disabled path.
  3. `tracing` events. Rejected for now: deps, open-schema, no second consumer to amortize. Cite as future migration when a third consumer appears.
  4. `log::kv`. Rejected: same shape as tracing with less compositional infrastructure.
  5. miette `Diagnostic`. Rejected: bound to `: Error`; not a fit for non-error emit.
- **Addressing sub-decision**: `LineItem::Segment { id, segment }` — id lives at the layout layer, not the trait.
- **Payload sub-decision**: per-decision struct fields with pre/post dimensions; remediation as a method on `LayoutDecision`, not a stored field.

### Beads this implies (file once ADR-0026 is accepted)

- Refactor `LineItem::Segment(Box<dyn Segment>)` → struct variant carrying `id`. Update all builder sites (`segments/builder/dispatch.rs`) and test sites.
- Introduce `LayoutObservers<'a>` and migrate `render_with_warn` / `render_to_runs` to it. Keep a `render(items, ctx, width)` no-observer convenience entry point.
- Define `LayoutDecision<'a>` enum with the five variants and a `remediation(&self) -> Option<&'static str>` method.
- Emit decisions at the five sites in `layout/mod.rs`: priority-drop (in the reflow loop), `try_shrink` success, `try_reflow` success, `apply_width_bounds` under-min drop, and `apply_width_bounds` over-max truncate (at the call site, NOT inside `truncate_to` — that helper is also reached from `try_reflow` and a generic emit there would double-fire on reflow paths).
- TUI preview consumer in `crates/linesmith/src/tui/preview.rs`: collect events per frame, render per-segment decision badges/footer.
- (Stretch) `lsm doctor explain` CLI subcommand that runs the layout engine with the decision callback and dumps a Starship-explain-style report.

### Follow-up research needed

- **`Arc<str>` vs `Cow<'static, str>` vs `&str` for the id**: the `LayoutDecision` lifetime story isn't fully settled until the TUI's event-collection storage shape is fixed. Spike when implementing.
- **Multi-frame preview**: if the preview re-renders on every keystroke, collect-and-clear per frame or maintain a sticky "last-decision-per-segment" map? The Firefox Inspector pattern suggests sticky-by-segment-id with auto-clear on a recomputed frame.
- **Plugin-emitted decisions**: when rhai-plugin segments take their own width-pressure path inside `shrink_to_fit`, do they emit `LayoutDecision::Shrink`, or does the engine emit on their behalf? Spec when plugin layout-awareness lands.

## Open questions

- **Does the production codepath need a no-observer fast path?** The disabled-callback case is one `Option::None` check per decision site — effectively free. The cleaner API may be to always pass `LayoutObservers` and let `on_decision` default to `None`, removing the dual-entry-point API entirely. Bikeshed in the ADR.
- **Should decision events distinguish "shrink-then-still-overflows-so-dropped" from "dropped-without-trying-shrink"?** The loop tries shrink → reflow → drop in order; a segment that fails all three emits one `PriorityDrop`. The preview could benefit from knowing the engine _tried_ shrink and got `None`. Out of scope for v1 — the post-mortem of "engine tried but failed" is doctor-mode terrain.
- **Conditional surfacing in the preview UI**: the Firefox-Inspector lesson is "show the field only when the decision was taken." How does the linesmith preview show _nothing_ for the 80% of segments that rendered cleanly? A per-segment status badge — invisible when status is "Rendered", colored otherwise. UX design, not ADR-0026.
- **`tracing` migration path**: if a future doctor-mode wants structured event dumps, the typed callback can wrap a `tracing` subscriber at the call site — but the inverse (a `tracing`-emitting engine driving a typed-match preview) is harder. α does not preclude β; β would preclude α-style exhaustiveness. Document this in `LayoutDecision`.

## Raw data

### Comparison matrix: layout-decision observability across surveyed tools

| Tool                          | Per-item decision channel                                                                            | Addressing                                   | Payload shape                              | UI surface                                   |
| ----------------------------- | ---------------------------------------------------------------------------------------------------- | -------------------------------------------- | ------------------------------------------ | -------------------------------------------- |
| ccstatusline                  | None (silent truncate)                                                                               | type-string ("Directory")                    | n/a                                        | live preview shows result only               |
| CCometixLine                  | None                                                                                                 | type-string                                  | n/a                                        | live preview, result only                    |
| claude-powerline              | `CLAUDE_POWERLINE_DEBUG=1` env var (content unspecified)                                             | config-key, dot-paths                        | n/a                                        | stderr/log; no preview                       |
| claude-powerline-rust         | None                                                                                                 | config-key                                   | n/a                                        | result only                                  |
| claude-statusline-powerline   | None                                                                                                 | `type` string                                | n/a                                        | result only                                  |
| Starship `explain`            | Per-module (value, duration, description) — but only for modules that _did_ render                   | description string (slug for filtering only) | value + duration + description             | CLI subcommand, not live                     |
| Starship `STARSHIP_LOG=trace` | Per-module timing in trace logs                                                                      | module slug                                  | text trace                                 | stderr, dev-grade                            |
| Oh My Posh `debug`            | Per-segment (enabled flag, timing)                                                                   | `type` + numeric `index`                     | text line, segment-by-segment              | CLI command                                  |
| Powerlevel10k                 | None (no explain command)                                                                            | function name                                | n/a                                        | n/a                                          |
| powerline-go                  | None at runtime                                                                                      | Segment.Name field, plugin process           | n/a                                        | n/a                                          |
| Firefox Flexbox Inspector     | Per-item (Content / Flexibility / MinSize / FinalSize) — minimap + step chart, conditional surfacing | selector + tree position                     | numeric diagram + chart                    | live editor side panel                       |
| Chrome DevTools Flex/Grid     | Container-side editor + overlay; per-item shrink weaker than Firefox                                 | selector                                     | overlay graphics                           | live editor                                  |
| comfy-table                   | None                                                                                                 | n/a                                          | n/a                                        | n/a                                          |
| tabled                        | None                                                                                                 | n/a                                          | n/a                                        | n/a                                          |
| ratatui Layout                | None (Cassowary result opaque)                                                                       | n/a                                          | n/a                                        | (constraint-explorer example is UI, not API) |
| Terraform `plan`              | Per-resource action prefix (+, -, ~, -/+)                                                            | resource address                             | per-attribute old → new                    | CLI diff renderer                            |
| Ansible `--diff`              | Per-task unified diff                                                                                | task name + file path                        | unified diff                               | CLI                                          |
| git `rebase -i`               | Per-commit action prefix (p/r/e/s/f/x)                                                               | commit SHA                                   | action keyword                             | $EDITOR sheet                                |
| rustc `-Zprint-mono-items`    | Per-CGU dump                                                                                         | CGU + mono item name                         | text dump                                  | stdout                                       |
| **linesmith (proposed)**      | **Per-segment `LayoutDecision` via typed callback**                                                  | **`LineItem::Segment.id`**                   | **enum variants with pre/post dimensions** | **TUI live preview**                         |

### Excerpts worth keeping

- ccstatusline `renderer.ts`: `if (plainLength > maxWidth) { statusLine = truncateStyledText(statusLine, maxWidth, { ellipsis: true }); }` — the only width-handling, no per-widget signal.
- Oh My Posh debug format string at `src/prompt/debug.go:41-66`: `fmt.Sprintf("%-*s - %3d ms\n", largestSegmentNameLength, segmentName, duration)` — multi-line, name-padded per-segment tag format that has proven user-readable.
- Starship `print.rs` explain row: `" "{value}" ({duration}){padding} - {description}"` — note the module slug is **not** in the user-facing row.
- Firefox Flexbox Inspector: "Minimum Size (only appears when an item is clamped to its minimum size)" — conditional surfacing is the right pattern.
- Mozilla bug 1501066: the inspector emitted "Item was set to shrink" even when no shrink occurred; the Firefox 65 fix bailed out of the messaging entirely when the engine didn't take that path. Lesson: emit numbers, surface only when the decision was actually taken.
- `tracing` docs: "if no currently active subscribers express interest in a given set of metadata by returning true, then the corresponding Span or Event will never be constructed" — disabled-path overhead is one interest check.
- miette `Diagnostic`: `pub trait Diagnostic: Error` — strictly error-bound, not a fit for non-error emit.
- linesmith `Segment` trait at `crates/linesmith-core/src/segments/mod.rs:418`: no `id()` method exists today.
- linesmith `LineItem` enum at `crates/linesmith-core/src/segments/mod.rs:704`: `Segment(Box<dyn Segment>) | Separator(Separator)` — id has no home.
- linesmith `LineEntry::segment_id` at `crates/linesmith-core/src/config.rs:221`: the id lives at config-time today; needs to travel to layout-time.
