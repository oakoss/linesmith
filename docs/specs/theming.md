# Theming

- Status: draft
- Version: 0.1
- Last updated: 2026-04-17
- Driving ADRs: [ADR-0005](../adrs/0005-role-based-themes.md), [ADR-0003](../adrs/0003-segment-widget-system.md)

## Overview

Themes decide what colors and text styles segments render in. linesmith's theming is **role-based**: segments declare semantic roles (e.g. `success`, `warning`, `muted`); themes map roles to concrete colors. Adding a theme is a single TOML file; no segment code changes.

This spec defines:

1. The role vocabulary: every semantic color slot linesmith segments can target
2. The theme file format: TOML schema, naming, discovery
3. The override precedence: how user config, segment defaults, and theme defaults interact
4. Fallback rules: what happens when a theme omits a role or a terminal lacks color support
5. Style syntax: the string format segments and config use to describe styling
6. Built-in theme set shipped in v0.1
7. Catppuccin integration compatibility

Decisions here lock the contract between segment authors (built-in and plugin) and theme authors. Getting the role vocabulary right avoids a future where every new segment needs new roles.

## Requirements

### Functional

- A new theme is a single TOML file dropped into `~/.config/linesmith/themes/`; no code edits
- A theme maps every role in the vocabulary to a concrete color; themes that miss roles fall back to defaults
- Segments declare their intent via role names (e.g. `role:success bold`), not hex values
- Users can override the style of any segment per-segment via `config.toml`
- Built-in themes compile into the binary (no file I/O for defaults)
- User themes are discovered at startup from `~/.config/linesmith/themes/*.toml`
- Theme selection is a single config key: `theme = "catppuccin-mocha"`
- `linesmith themes list` enumerates available themes (built-in + user)
- Catppuccin's 4 flavors (Latte / Frappé / Macchiato / Mocha) ship in v0.1 with role mappings the Catppuccin integration team recognizes (per their contract)

### Non-functional

- Theme file parse <1ms; compiled binary lookup O(1)
- No allocations during role → color resolution on hot paths (`&'static str` roles, small enum for colors)
- Terminal capability detection (truecolor / 256 / 16 / no-color) honored at render time
- `NO_COLOR` env var (see [no-color.org](https://no-color.org)) forces no-color mode
- `FORCE_COLOR` env var forces color even in non-TTY output

## Interface / Contract

### Role vocabulary

The canonical role list for v0.1. Themes map every role to a color; segments reference these names.

**Base roles** (always present in every theme):

| Role         | Intent                                                                  |
| ------------ | ----------------------------------------------------------------------- |
| `foreground` | Default text color (unstyled segments)                                  |
| `background` | Default background (rarely used in a status line)                       |
| `muted`      | De-emphasized text (labels, separators, less important info)            |
| `primary`    | Brand / accent for the most prominent segment (usually the model name)  |
| `accent`     | Secondary accent (highlights, non-primary emphasis)                     |
| `success`    | Positive state (clean git, healthy rate limits, cache hit)              |
| `warning`    | Needs attention (approaching rate limit, modified files, high context)  |
| `error`      | Critical / broken state (429s, failed rate-limit fetch, merge conflict) |
| `info`       | Neutral informational (e.g. session duration, model id)                 |

**Extended roles** (optional; themes without these fall back to sensible base-role defaults):

| Role          | Falls back to | Intent                                               |
| ------------- | ------------- | ---------------------------------------------------- |
| `success_dim` | `success`     | Quiet variant for backgrounds / fills                |
| `warning_dim` | `warning`     | Same                                                 |
| `error_dim`   | `error`       | Same                                                 |
| `primary_dim` | `primary`     | Same                                                 |
| `accent_dim`  | `accent`      | Same                                                 |
| `surface`     | `background`  | Elevated surface (used by capsule / powerline fills) |
| `border`      | `muted`       | Subtle dividers                                      |

Roles are stored as a small enum in the binary (not strings at runtime) to avoid per-render allocation.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    Foreground,
    Background,
    Muted,
    Primary,
    Accent,
    Success,
    Warning,
    Error,
    Info,
    SuccessDim,
    WarningDim,
    ErrorDim,
    PrimaryDim,
    AccentDim,
    Surface,
    Border,
}
```

### Theme file format

Location: `~/.config/linesmith/themes/<name>.toml` or built-in (embedded).

```toml
# Required metadata
name = "Catppuccin Mocha"
author = "Catppuccin <https://github.com/catppuccin>"
license = "MIT"

# Base roles — all required in shipped themes
[roles]
foreground  = "#cdd6f4"
background  = "#1e1e2e"
muted       = "#6c7086"
primary     = "#cba6f7"  # mauve
accent      = "#89b4fa"  # blue
success     = "#a6e3a1"  # green
warning     = "#f9e2af"  # yellow
error       = "#f38ba8"  # red
info        = "#94e2d5"  # teal

# Extended roles — optional; omission falls back per the table above
[roles.extended]
success_dim = "#5b8a5b"
warning_dim = "#8a7a5a"
error_dim   = "#8a5b5b"
primary_dim = "#8a6ba7"
accent_dim  = "#5b7a9e"
surface     = "#313244"
border      = "#45475a"

# Optional separator styling
[separators]
default    = " "       # between segments in plain mode
powerline  = ""       # triangle chevron; requires Nerd Font
ellipsis   = "…"
```

### Color formats accepted

- Hex: `"#rgb"`, `"#rrggbb"` (most common)
- Named: `"red"`, `"blue"`, ... (16-color palette names)
- RGB: `"rgb(203, 166, 247)"` (accepted, normalized to hex at parse)

Parsed internally into:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    TrueColor { r: u8, g: u8, b: u8 },
    Palette256(u8),
    Palette16(AnsiColor),
    NoColor,
}
```

### Style syntax

Segments and user config describe styles as strings:

```text
role:success bold
role:warning
role:primary bold italic
fg:#ff8800 bold          # absolute color (rare, for specific segments)
fg:red underline
(no style)               # empty = foreground, no decoration
```

Parsed into:

```rust
pub struct Style {
    pub role: Option<Role>,
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub hyperlink: Option<String>,
}
```

### Resolution precedence

When a segment wants to render styled text, resolution runs this order:

1. **User per-segment override** from `config.toml` (if present for this segment)
2. **Segment's declared style** from its `render()` output
3. **Theme role mapping** (if segment requested a role)
4. **Terminal capability downgrade** (truecolor → 256 → 16 → no-color)
5. **`NO_COLOR` / `FORCE_COLOR` env vars** (final override)

```text
user config style?   ──► yes ──► use it
         │ no
         ▼
segment style?       ──► has role? ──► look up in theme
         │ no                              │
         ▼                                 ▼
foreground default                    map Role → Color (with fallback)
                                           │
                                           ▼
                           terminal capability downgrade
                                           │
                                           ▼
                                NO_COLOR / FORCE_COLOR
```

### Built-in themes (v0.1)

Per `docs/ideas/0001-feature-parity-matrix.md`:

- `default`: neutral, terminal-default colors (uses 16-color palette only)
- `minimal`: no colors, just bold / dim / italic
- `catppuccin-latte`
- `catppuccin-frappe`
- `catppuccin-macchiato`
- `catppuccin-mocha`

All compile into the binary via `include_str!`. User themes override built-ins of the same name.

### Catppuccin integration

linesmith targets inclusion on the [Catppuccin integration list](https://github.com/catppuccin/catppuccin#portlist). Requirements we meet:

- All four flavors (Latte, Frappé, Macchiato, Mocha)
- Colors match the official Catppuccin palette exactly (sourced from `catppuccin` Rust crate)
- README documents how to switch flavors
- We maintain theme files under the Catppuccin license/attribution

## Behavior

### Theme loading

1. Startup reads `~/.config/linesmith/themes/*.toml` (XDG-compliant path)
2. User themes are parsed and registered by `name` field (not filename)
3. Built-in themes are registered from embedded TOML strings
4. On name collision: user theme wins, debug log records the override
5. Config's `theme = "<name>"` is resolved against this registry

### Parse errors

Invalid theme file:

- Missing required base role → fail loudly, theme rejected, fall back to `default`
- Malformed TOML → log error with line/col, theme rejected
- Invalid color format → log error with role name + value, theme rejected

A rejected theme does not crash linesmith; it falls back to `default` and emits one warning to stderr (not stdout; stdout is reserved for rendering).

### Terminal capability

Detected once at startup via `supports-color` crate:

- Truecolor supported → themes render as-is
- 256-color → truecolor downsampled to nearest 256-palette entry (fast table lookup, not arithmetic)
- 16-color → further downsampled; shipped themes explicitly specify 16-color fallbacks via extended metadata if they care
- No color (per `NO_COLOR` or capability) → strip all color; styles fall back to text decoration only (bold/dim/italic)

### Cache interaction

Themes don't cache anything themselves. Segment caches are invalidated implicitly when the theme changes (segment cache files include theme name in the key).

## Edge cases

| Case                                             | Handling                                                             |
| ------------------------------------------------ | -------------------------------------------------------------------- |
| Theme file references a role not in vocabulary   | Parse succeeds; unknown role ignored with warning                    |
| Theme omits a base role                          | Parse fails; theme rejected                                          |
| Theme omits an extended role                     | Falls back per the extended-role table above                         |
| User config overrides a segment with a hex color | Used as-is; theme role not consulted                                 |
| User requests theme that doesn't exist           | Fall back to `default`, log warning                                  |
| Terminal doesn't support any color               | All styles render as text decoration only (bold/italic/underline)    |
| Segment declares role only, no decoration        | Renders with theme's color and no bold/italic                        |
| Segment declares decoration only (bold), no role | Renders bold in foreground color                                     |
| Two themes with the same `name`                  | User theme wins over built-in; first-user-theme-found wins otherwise |
| Theme file not UTF-8                             | TOML parse fails; rejected with warning                              |
| Color out of range (e.g. `#gghhii`)              | Parse fails; theme rejected                                          |
| `NO_COLOR` env var set                           | All colors stripped; text decorations preserved                      |

## Testing strategy

### Unit tests

- Parse valid theme TOML; assert all roles present
- Parse theme missing base role → expected error
- Parse theme with unknown role → warning, parses successfully
- Color format parsing: hex (3/6 char), named, rgb(...)
- Style string parsing: all combinations of role + fg + decorations
- Downgrade table: truecolor → 256 for known values
- Extended role fallback table

### Integration tests

- Load all built-in themes; assert each renders a known segment's output to an expected string
- Load a user theme from fixture directory; assert it's discoverable
- User theme with same name as built-in → user wins
- NO_COLOR / FORCE_COLOR override truecolor rendering
- Snapshot tests: one per built-in theme × a representative segment list

### Catppuccin conformance tests

For each flavor:

- Hex values match the official Catppuccin palette exactly (sourced from the catppuccin Rust crate)
- Rendering a canonical status-line produces the expected bytes in a snapshot
- Changes to a Catppuccin palette upstream are detected by palette-sync CI

## Open questions

- **Should extended roles be required or optional?** Current design: optional with fallbacks. Makes theme authoring easier; cost is slightly less visual control. Revisit if feedback suggests themes want more granular fallbacks.
- **Truecolor / 256 downgrade strategy** — use an official palette mapping (what table?) vs. nearest-rgb arithmetic. Decision: table lookup at build time (fast, deterministic); may revisit if visual quality is poor.
- **OMP theme import** — deferred to v0.2+. Would require a separate parser that maps OMP's JSON schema to our role vocabulary.
- **Should we ship Catppuccin via the [`catppuccin`](https://lib.rs/crates/catppuccin) crate or embed our own TOML?** Crate is lightweight (~20KB) and keeps palette values accurate. Leaning toward crate; cost is one extra dep.
- **User theme hot reload?** Deferred to v0.2+ matrix. v0.1 requires restart to pick up theme changes.
- **Per-line theming** — can `line.1` and `line.2` use different themes? Current design: no, one theme per invocation. Revisit if there's demand.

## Change log

- 2026-04-17: initial draft (v0.1)
