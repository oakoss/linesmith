# TUI screen-state enum + reusable ListScreen / PropertyScreen templates

- Status: accepted
- Date: 2026-04-28
- Deciders: Jace
- Surfacing bead: lsm-yevo

## Context and Problem Statement

The `linesmith config` TUI replicates ccstatusline's UX: roughly a dozen screens (Main Menu, Items Editor, Color Editor, Theme Picker, Line Picker, Global Overrides, Powerline Setup, Terminal Options, Install to Claude Code, etc.) sharing recurring patterns — a list with a cursor and verb-letter actions, or a property page with labeled rows addressed by single keys. Live preview re-renders on every state change. How do we structure the runtime so adding a screen is a small, predictable change rather than a one-off rewrite?

## Decision Drivers

- **ccstatusline parity.** The reference UX is the target; their architecture uses an `AppScreen` discriminated union covering roughly a dozen screens. Mismatch costs real ergonomics for users coming from ccstatusline.
- **Screen reusability.** Five planned screens (Main Menu, Items, Color Editor, Theme Picker, Line Picker) all want: cursor (▶) on a row, ↑↓ navigation, single-letter verbs (a/i/k/d/c/r/m), a description string for the highlighted item, a one-line help row. Re-implementing per screen is 5× the surface area for one bug.
- **Property screens.** Global Overrides, Powerline Setup, Terminal Options, Install to Claude Code don't have a list — they're labeled-row property pages, each row addressed by a letter key. Different widget shape; same need for reuse.
- **Live preview correctness.** Preview is a persistent header re-rendered on every state change; the model→view path has to be deterministic so the preview byte-matches what `linesmith` would render at the same cwd.
- **Save UX.** ccstatusline's Ctrl+S immediate save (no modal dialog) is the right default for a TUI users return to often. Dirty-check via stringify diff against the loaded TOML; confirm-on-quit only when dirty.
- **Plugin-author cost.** Plugins (rhai) eventually surface in screens for per-script config. Whatever architecture we pick has to extend to plugin-authored screens without a second runtime.

## Considered Options

- **Option 1 — `AppScreen` state enum + `ListScreen`/`PropertyScreen` template widgets, Elm-style Model/Update/View.** Single `AppScreen` enum carries the active screen variant; each variant holds its own state. Reusable widgets (`ListScreen`, `PropertyScreen`) are configured per variant. `update` is a pure function `(Model, Event) -> Model`; `view` renders Model into a ratatui buffer. Mirrors ccstatusline's approach.
- **Option 2 — Single mega-screen with mode flags.** One model, one update, mode-switching via boolean/enum flags. Avoids the AppScreen ceremony but blends concerns; adding a screen means modifying the central model.
- **Option 3 — Imperative event loop with mutable state.** Explicit `loop { event = poll(); match event { ... } }` with direct mutation of a model struct. Fewer types but every screen reinvents the cursor / scroll / keymap pattern.
- **Option 4 — Component tree (React/Ink-style).** Each screen is a component that owns its sub-tree; ratatui isn't designed for this and the closest crate (`tui-react`) is unmaintained. Doesn't match the substrate.

## Decision Outcome

Chosen option: **Option 1 — `AppScreen` enum + `ListScreen`/`PropertyScreen` reusable widgets + Elm-style Model/Update/View**, because (a) it's the proven shape for ccstatusline at the exact size of UX we're targeting, (b) the two reusable widgets collapse five list-screens and four property-screens into roughly two render paths, (c) Elm's pure `(Model, Event) -> Model` keeps preview rendering deterministic — the TUI just calls `crate::layout::render_to_runs` with the in-memory Config and maps runs to ratatui Spans, and (d) screen additions are local: a new `AppScreen::X` variant + a constructor that configures one of the existing widgets.

### Architecture

```rust
pub enum AppScreen {
    MainMenu(MainMenuState),
    ItemsEditor(ItemsEditorState),
    ColorEditor(ColorEditorState),
    ThemePicker(ThemePickerState),
    LinePicker(LinePickerState),
    GlobalOverrides(GlobalOverridesState),
    PowerlineSetup(PowerlineSetupState),
    TerminalOptions(TerminalOptionsState),
    InstallToClaudeCode(InstallState),
}

pub struct Model {
    pub screen: AppScreen,
    pub config: toml_edit::DocumentMut,    // mutable in-memory config
    pub original: String,                  // serialized snapshot for dirty-check
    pub preview_runs: Vec<StyledRun>,      // re-rendered on every Update
    pub status: Option<StatusLine>,        // bottom status, e.g. "saved"
    pub quit: bool,
}

pub fn update(model: Model, event: Event) -> Model { /* ... */ }
pub fn view(model: &Model, frame: &mut Frame) { /* ... */ }
```

Two reusable widgets handle the recurring shapes:

- **`ListScreen`** — cursor (▶) on the highlighted row; ↑↓ navigation that wraps at top/bottom (matches ccstatusline); verb-letter dispatch via a `letter -> Action` callback registered by the caller; description string pulled from the highlighted item; one-line help string from the caller; move-mode toggle (Enter enters; ↑↓ swaps adjacent rows; Esc/Enter exits — only modal verb).
- **`PropertyScreen`** — labeled rows, each row addressed by a single letter key (no list cursor); letters dispatch to caller-registered actions; optional inline edit via tui-textarea on the row's value column.

### Contracts that apply across all screens

- **Preview as persistent header.** Anchored to the top of every screen, re-rendered on every state change. Renders the in-memory Config through `crate::layout::render_to_runs` and maps each `StyledRun` → ratatui `Span`. Holds last-valid output during transient invalid states (e.g. mid-typing in the template editor).
- **Ctrl+S saves immediately.** No save dialog. Atomic write via temp + rename; `toml_edit::DocumentMut` preserves comments and formatting on round-trip.
- **Dirty-check via stringify diff.** On every Update, serialize the current config and compare against `model.original`; if they differ, the model is dirty. Quit prompts a confirm only when dirty.
- **Bottom-of-screen description.** One-line description string pulled from the highlighted item (list screens) or the focused row (property screens).
- **One-line help row.** Renders under the screen title; lists the active verbs (e.g. "a add · i insert · k clone · d delete · r rename · m move").

### Consequences

- Good, because adding a screen is bounded: a new `AppScreen::X` variant, a state struct, and a `view`/`update` arm. The reusable widgets handle cursor / keymap / help.
- Good, because the Elm-style update gives us free testability: `(Model, Event) -> Model` is a pure function, so screen behavior is unit-testable without ratatui in the loop.
- Good, because preview accuracy is structural: preview = `render_to_runs(model.config) → Spans`, the exact path stdout uses for production rendering.
- Good, because dirty-check via stringify diff sidesteps tracking which fields changed; a single comparison after every Update is O(config size), trivially cheap at TOML scale.
- Bad, because screen state lives inside `AppScreen` variants — sharing state across screens requires lifting it into the top-level Model and passing it back into the next-screen state on transition. Small ceremony cost per screen-to-screen handoff.
- Bad, because `toml_edit::DocumentMut` round-tripping has known edge cases (occasional formatting churn on certain inline-table edits); ccstatusline reports the same shape and lives with it.
- Neutral, because the `Vec<StyledRun>` → `Vec<Span>` mapping is roughly 10 lines of glue; if ratatui changes its `Span` shape, the surface to update is small and isolated.

### Confirmation

Revisit if:

- We add a screen that doesn't fit `ListScreen` or `PropertyScreen` — signal that we may need a third template, or that the screen wants a custom layout.
- The Elm-style update path becomes a bottleneck for a large config (e.g. >1000 lines TOML). Today's stringify-on-every-Update is cheap; a future high-end use case might force a structural diff.
- ccstatusline diverges substantially from the AppScreen pattern in a way that affects parity expectations.

## Pros and Cons of the Options

### Option 1 — `AppScreen` + reusable widgets + Model/Update/View

- Good: matches ccstatusline's proven shape; reuse covers all 9 planned v0.1 screens via the two widget templates.
- Good: pure update is unit-testable.
- Good: preview correctness is structural — same code path as production render.
- Bad: state plumbing on screen transitions adds small per-handoff ceremony.

### Option 2 — Single mega-screen with mode flags

- Good: simpler initial type surface — no enum dispatch.
- Bad: every screen edits the same Model; concerns blend; bug fixes risk regressions across unrelated screens.
- Bad: doesn't scale; ccstatusline's history shows mode-flag soup at ~5+ screens.

### Option 3 — Imperative event loop with mutable state

- Good: shortest line count for a single screen.
- Bad: every screen reinvents cursor / scroll / keymap; copy-paste drift across screens.
- Bad: testability is poor — screen behavior coupled to terminal I/O.

### Option 4 — Component tree (React/Ink-style)

- Good: composability story is strong in principle.
- Bad: ratatui isn't designed for this; no maintained component-tree crate.
- Bad: imposes a runtime that doesn't match ratatui's idioms; long-term churn risk.

## More Information

- Bead: lsm-herx (TUI epic) — the v0.1 critical-path screen list and contracts
- Beads: lsm-herx.4 (ListScreen widget), lsm-herx.6 (Main Menu), lsm-herx.7 (Items Editor) — implementation deliverables
- Companion ADR: [ADR-0015](0015-ratatui-for-tui-runtime.md) — substrate decision (ratatui v0.30)
- Reference: ccstatusline's `AppScreen` union — roughly a dozen screen variants, two screen templates, verb-letter dispatch
- Related: [ADR-0014](0014-best-effort-parse-with-segment-isolation.md) — segments degrade independently; the TUI inherits this contract via `render_to_runs`
- Related: [`docs/research/competitor-landscape.md`](../research/competitor-landscape.md) — ccstatusline overview and feature parity matrix
- `toml_edit`: <https://docs.rs/toml_edit> — preserves comments / formatting on round-trip
- Out of scope: the `StatusLine` bottom-row shape (success / error / info levels) — to be settled once screen implementations exist
