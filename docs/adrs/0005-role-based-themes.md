# Theme with role-based semantic colors, not per-segment hex values

- Status: accepted
- Date: 2026-04-17
- Deciders: Jace

## Context and Problem Statement

Users want theme support, specifically popular palettes like Catppuccin (4 flavors), Dracula, Nord, Gruvbox, Tokyo Night, Rose Pine. Existing tools either hardcode colors per segment (every new theme requires editing every segment) or force users to map colors to individual widgets by hex. How should linesmith structure its theme system so that adding a new theme is a single-file change, and user-authored plugins inherit theme colors automatically?

## Decision Drivers

- Adding a new theme should be a one-file change: no code edits, no per-segment tweaks
- Users should be able to swap themes with a single config key
- Plugin-authored segments should inherit theme colors without the plugin author naming any hex values
- Catppuccin's 4-flavor structure and similar theme families should map cleanly
- We want to be on the Catppuccin official integration list eventually (requires their contract)

## Considered Options

- **Hardcoded hex per segment**: every segment names its colors directly
- **Role-based semantic colors**: themes define roles (primary, success, warning, etc.); segments reference roles
- **Per-segment theme overrides** (hybrid): roles by default, with per-segment override capability
- **Oh My Posh JSON format reuse**: adopt OMP's portable theme schema

## Decision Outcome

Chosen option: **Role-based semantic colors with optional per-segment overrides** (a hybrid of options 2 and 3), because it's the only design where adding a theme is a one-file change AND users retain the ability to override specific segments when they want. Segments declare which role they want (`role:success bold`); themes map roles to hex. This is how Starship and Helix work, and it's why Catppuccin has 100+ integrations: their contract is role-based.

Theme file shape (TOML):

```toml
name = "Catppuccin Mocha"
[roles]
primary    = "#cba6f7"
success    = "#a6e3a1"
warning    = "#f9e2af"
error      = "#f38ba8"
muted      = "#6c7086"
accent     = "#89b4fa"
background = "#1e1e2e"
foreground = "#cdd6f4"
```

Segment reference:

```rust
format!("{} {}", style("✓").role("success").bold(), text)
```

User config can override per segment:

```toml
[segments.git_branch]
style = "role:accent bold"  # or "role:custom.mygreen" referring to user-defined role
```

### Consequences

- Good, because adding a new theme is a single TOML file; no code changes
- Good, because we can ship 8+ built-in themes with low maintenance cost (Catppuccin 4 flavors + Dracula + Nord + Gruvbox + Tokyo Night + Rose Pine + minimal + default)
- Good, because user plugins inherit theme colors automatically; plugin authors don't touch hex
- Good, because the role vocabulary is Catppuccin-compatible; we can submit for official integration
- Good, because per-segment overrides give users an escape hatch for specific tweaks
- Bad, because we must define a role vocabulary up front; too few roles and segments feel cramped; too many and themes become tedious to author
- Bad, because a segment's visual design is partly dictated by the role vocabulary; if a theme doesn't define a role a segment wants, we need fallback logic
- Neutral, because this adds a small runtime cost (role lookup per styled emission) but it's negligible relative to our <20ms budget

### Confirmation

Revisit if:

- A user-requested theme's design intent can't be expressed within the role vocabulary
- Per-segment overrides become the norm rather than the exception (signals the role vocabulary is wrong)
- Catppuccin's official contract changes incompatibly

## Pros and Cons of the Options

### Hardcoded hex per segment

- Good: absolute rendering control
- Bad: every new theme requires editing every segment (doesn't scale)
- Bad: plugin authors must pick colors, which looks wrong under non-default themes
- Bad: no reusability across themes

### Role-based semantic colors (without overrides)

- Good: clean abstraction, easy theme swapping
- Bad: no escape hatch when a user wants to tweak one specific thing
- Bad: occasionally, a segment has legitimately distinct visual needs that no role captures

### Role-based + per-segment overrides (chosen)

- Good: best of both worlds; clean defaults, escape hatch when needed
- Bad: slightly more complex mental model for users
- Bad: config schema is richer

### Oh My Posh JSON format reuse

- Good: immediate access to OMP's existing theme library (hundreds of themes)
- Good: cross-tool interchangeability with terminal prompts
- Bad: OMP's format is more complex than we need (prompt-level concepts that don't apply to statuslines)
- Bad: couples us to OMP's schema decisions
- Bad: licenses and attribution get murky if we ship OMP themes verbatim

## More Information

- Driven by: `research/user-demand.md` (preset onboarding as top complaint), `research/competitor-landscape.md` (no tool has portable theme format)
- Related ADRs: [ADR-0003](0003-segment-widget-system.md) (segments declare roles, theming lives at render-time)
- Catppuccin integration contract: <https://github.com/catppuccin/catppuccin>
- Will drive: `specs/theming.md` (role vocabulary, override precedence, theme file format)
- Open: consider whether to use the [catppuccin](https://lib.rs/crates/catppuccin) Rust crate or ship our own TOML palette data
