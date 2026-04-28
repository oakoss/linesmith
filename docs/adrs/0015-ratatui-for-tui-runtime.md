# Use ratatui v0.30 for the config-TUI runtime

- Status: accepted
- Date: 2026-04-28
- Deciders: Jace
- Surfacing bead: lsm-1qg8

## Context and Problem Statement

linesmith ships an interactive `linesmith config` subcommand for editing the user's TOML config — adding/removing segments, swapping themes, tuning layout-options, and previewing the rendered statusline live. This is a separate execution path from the daily-driver `linesmith` render binary; it runs once when the user invokes `config`, not on every prompt. Which Rust TUI substrate gives us alt-screen + crossterm input + reusable widgets without blowing the binary-size budget or boxing us into a deprecated framework?

This ADR supersedes the tentative pick recorded in [`docs/research/rust-crate-survey.md`](../research/rust-crate-survey.md) §10 (`dialoguer` for v1, `ratatui` later). The live-preview-as-header UX in [ADR-0016](0016-tui-screen-state-machine.md) needs persistent screen state that `dialoguer`'s one-question-at-a-time model can't carry, so `linesmith config` lands on `ratatui` directly; `dialoguer` stays available for `linesmith init`'s onboarding flow.

## Decision Drivers

- **Maintenance trajectory.** TUI infra has to come from a healthy ecosystem we can pin for years. Pre-1.0 substrates are acceptable if the maintainers and contributor base are active.
- **Binary-size budget.** Project vision targets 3-5MB stripped (see [ADR-0007](0007-cargo-dist-distribution.md)). Whatever TUI substrate we pick has to fit inside that envelope with room for everything else.
- **Cold-start ceiling for the render path is unaffected.** The TUI lives in `linesmith config`; `linesmith` (no args, statusline render) never imports the TUI runtime modules. The <20ms cold-start budget applies to render only.
- **Layout-engine integration.** [`render_to_runs`](../../crates/linesmith/src/layout.rs) emits `Vec<StyledRun>`. Whatever TUI we adopt has to map runs to its native styled-text primitive without re-parsing ANSI; that's the contract that makes preview byte-equal to production output.
- **Reusable list-screen + property-screen widgets.** ccstatusline's UX (the parity target; see [ADR-0016](0016-tui-screen-state-machine.md)) is built on a small set of repeated screen templates. We need a substrate that lets us implement those once and instance per screen.
- **Inputs editable in-place.** Several screens edit free-form text (theme overrides, segment templates). We need a multi-line input widget rather than rolling our own.
- **OSS contributor friendliness.** Default to a substrate Rust contributors recognize so PRs aren't gated on substrate ramp-up.

## Considered Options

- **`ratatui` v0.30** — community successor to tui-rs after the original maintainer stepped back; widely adopted, with reference codebases including gitui (closest analogue), atuin, and helix. Pre-1.0 (last released 0.30.x as of 2026-04-28). Pairs natively with crossterm.
- **`cursive`** — higher-level form/dialog toolkit. Ncurses backend default; pure-Rust crossterm backend exists but isn't first-class. Dependency surface includes ncurses-sys on default builds, which complicates Windows distribution.
- **`crossterm` only** — drop higher-level layout/widget abstractions; build screen layout against terminal cells directly. Keeps deps minimal but rebuilds list-cursor / scrolling / styled-span emission ourselves.
- **`tui-rs`** — historical ancestor of ratatui. Officially deprecated in favor of ratatui (last release 2022-12-12).
- **`dialoguer` only** — the rust-crate-survey's earlier pick for v1. Single-question prompts (Select / MultiSelect / Input / Confirm). Sufficient for `linesmith init`'s linear onboarding flow but not for live-preview multi-screen editing.

For the input widget specifically:

- **`tui-textarea`** — multi-line text-area that integrates with ratatui buffers. Active maintenance, MIT-licensed.

## Decision Outcome

Chosen option: **`ratatui` v0.30 + `tui-textarea`**, because (a) ratatui is the only actively-maintained Rust TUI library that provides the layout/widget primitives we need (`Layout`, `Block`, `Paragraph`, `List`, `Tabs`) plus a stable text-area crate ecosystem, (b) ccstatusline's UX is built on exactly the screen patterns ratatui makes ergonomic (cursor over a list, framed paragraphs with title/help, `Span`s for styled inline runs), (c) the [`render_to_runs`](../../crates/linesmith/src/layout.rs) → `ratatui::text::Span` mapping is one-to-one and trivial — no ANSI re-parsing — which preserves preview accuracy and avoids `ansi-to-tui` as a dep, and (d) gitui / atuin / helix prove the pattern at our complexity tier, so onboarding contributors who've touched a ratatui app is realistic.

`tui-textarea` covers the multi-line text-input need on roughly the same maintenance footing.

The TUI is feature-gated behind a `config-ui` cargo feature, **default-on** for v0.1. Distributors and binary-size-sensitive users can drop the feature with `--no-default-features` to trim the ratatui + crossterm + tui-textarea cost; the daily render path doesn't import any TUI modules so the only price of default-on is binary size, not cold-start time.

### Pre-1.0 stability posture

ratatui is pre-1.0 (0.30.x as of 2026-04-28). Pin the minor version (`ratatui = "0.30"`) and bump deliberately when we touch `linesmith config`. Breaking changes in ratatui 0.31+ become an explicit decision in a follow-up commit, not a transitive surprise from `cargo update`.

### Layout integration

`crate::layout::render_to_runs` emits `Vec<StyledRun>` independent of any output surface. The TUI maps each run into a `ratatui::text::Span` whose `Style` resolves the run's `theme::Style` against the active palette. Preview = production rendering, no `ansi-to-tui` round-trip.

### Consequences

- Good, because ratatui is actively maintained with a known release cadence; pre-1.0 risk is bounded by lock-file pinning rather than upstream API churn.
- Good, because the binary-size cost (estimated ~1-2MB stripped over a crossterm-only build; verify on first cargo-dist run) sits inside [ADR-0007](0007-cargo-dist-distribution.md)'s 3-5MB target with margin.
- Good, because gitui / atuin / helix establish the ecosystem patterns we'll use (Model/Update/View, screen-state enums, key-event dispatch); contributor onboarding has a known story.
- Good, because feature-gating preserves the minimum-binary path for distributors; the render binary's cold-start budget never sees ratatui in its module graph.
- Bad, because ratatui pre-1.0 means we accept periodic API churn on minor-version bumps; mitigated by pinning and reviewing each bump deliberately.
- Bad, because we ship crossterm as a transitive dep alongside `terminal_size` (already a render-path dep); they overlap on terminal-capability detection but don't conflict.
- Neutral, because `tui-textarea` is a separate crate with its own release cadence; if it stagnates we can replace it with an in-house `Paragraph`-backed editor. Its surface is small.

### Confirmation

Revisit if:

- ratatui announces v1.0; review the migration path and bump deliberately rather than as a `cargo update` side effect.
- ratatui ceases active maintenance for 6+ months; consider forking or migrating.
- `linesmith config` cold-start exceeds 200ms p50 (separate budget from the render path); profile the boot sequence.
- Stripped binary size with `config-ui` on exceeds 5MB; flip the feature default to off and document the opt-in.

## Pros and Cons of the Options

### `ratatui` v0.30

- Good: layout + widget primitives that match the ccstatusline screen shapes.
- Good: `Span`-based styled text maps cleanly from `Vec<StyledRun>`.
- Good: contributor-familiar — three reference codebases at our complexity tier.
- Bad: pre-1.0 — accept minor-version churn cost on deliberate bumps.

### `cursive`

- Good: highest-level abstractions; forms and dialogs are first-class.
- Bad: ncurses backend default complicates Windows distribution; crossterm backend is second-class.
- Bad: less idiomatic for "live preview as persistent header" rendering — cursive expects a stack of focusable views, not a flowed buffer.

### `crossterm` only

- Good: minimum dependency surface.
- Bad: rebuilds list-cursor, scroll, keymap, layout, and styled-span emission for every screen — multiplies the work across the v0.1 screen set.
- Bad: no input widget — multi-line text editing has to be hand-rolled.

### `tui-rs`

- Good: historically proven (ratatui's ancestor).
- Bad: deprecated; no upstream maintenance.

### `dialoguer` only

- Good: tiny dependency footprint; appropriate for `linesmith init`'s one-question-at-a-time onboarding flow.
- Bad: no concept of a persistent screen; live preview has nowhere to live.

## More Information

- Rust crate survey: [`docs/research/rust-crate-survey.md`](../research/rust-crate-survey.md) §10 (TUI framework)
- Reference codebases: gitui, atuin, helix
- ratatui homepage: <https://ratatui.rs>
- tui-textarea: <https://github.com/rhysd/tui-textarea>
- Companion ADR: [ADR-0016](0016-tui-screen-state-machine.md) — screen architecture on top of ratatui
- Related: [ADR-0007](0007-cargo-dist-distribution.md) — binary-size budget and distribution
- Related: [ADR-0012](0012-per-process-execution.md) — per-process model that justifies feature-gating
