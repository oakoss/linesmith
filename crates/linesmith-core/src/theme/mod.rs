//! Role-based theme system. Segments declare semantic roles; themes map
//! roles to colors; the render path emits ANSI SGR around each segment
//! based on the terminal's detected capability. Full contract in
//! `docs/specs/theming.md`.
//!
//! Built-ins compiled in: `default` (Palette16, terminal-default
//! anchor), `minimal` (decoration-only), the four Catppuccin flavors,
//! plus Dracula, Nord, Gruvbox, Tokyo Night, and Rose Pine. User
//! themes load from `~/.config/linesmith/themes/*.toml` via
//! [`user::ThemeRegistry`].

use std::fmt::Write;

mod catppuccin;
mod default;
mod dracula;
mod gruvbox;
mod minimal;
mod nord;
mod rose_pine;
pub mod style_syntax;
mod tokyo_night;
pub mod user;

pub use style_syntax::{parse_style, StyleParseError};
pub use user::{RegisteredTheme, ThemeRegistry, ThemeSource};

/// Semantic color slot a segment targets. Themes map every role to a
/// concrete color; segments never reference hex values directly.
/// Variants are ordered to match the 16-slot role array themes store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Role {
    // Base roles — always present in every theme.
    Foreground = 0,
    Background = 1,
    Muted = 2,
    Primary = 3,
    Accent = 4,
    Success = 5,
    Warning = 6,
    Error = 7,
    Info = 8,
    // Extended roles — themes may omit them; `Theme::color` falls back
    // to the base role named in the spec's extended-role table.
    SuccessDim = 9,
    WarningDim = 10,
    ErrorDim = 11,
    PrimaryDim = 12,
    AccentDim = 13,
    Surface = 14,
    Border = 15,
}

// Compile-time guard: the last variant's discriminant must equal
// `COUNT - 1` so theme color arrays stay in lockstep with the enum.
// If a new variant is added without bumping `COUNT`, this trips.
const _: () = assert!(Role::Border as usize == Role::COUNT - 1);

impl Role {
    /// Role count; the discriminants cover `0..ROLE_COUNT` densely so
    /// themes store colors in a fixed-size array.
    pub const COUNT: usize = 16;

    /// The base role this role falls back to when a theme leaves the
    /// extended slot unset. Base roles fall back to themselves.
    #[must_use]
    pub fn fallback(self) -> Role {
        match self {
            Self::SuccessDim => Self::Success,
            Self::WarningDim => Self::Warning,
            Self::ErrorDim => Self::Error,
            Self::PrimaryDim => Self::Primary,
            Self::AccentDim => Self::Accent,
            Self::Surface => Self::Background,
            Self::Border => Self::Muted,
            other => other,
        }
    }
}

/// Standard 16-color ANSI palette. Stored as the foreground base (30..=37
/// for normal, 90..=97 for bright); we add offsets at emit time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnsiColor {
    Black = 0,
    Red = 1,
    Green = 2,
    Yellow = 3,
    Blue = 4,
    Magenta = 5,
    Cyan = 6,
    White = 7,
    BrightBlack = 8,
    BrightRed = 9,
    BrightGreen = 10,
    BrightYellow = 11,
    BrightBlue = 12,
    BrightMagenta = 13,
    BrightCyan = 14,
    BrightWhite = 15,
}

/// Concrete color value a theme resolves a role to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Color {
    TrueColor {
        r: u8,
        g: u8,
        b: u8,
    },
    Palette256(u8),
    Palette16(AnsiColor),
    /// Explicit "no color" — the role renders plain, even if the
    /// terminal supports color. Used by the `minimal` theme to force
    /// decoration-only styling everywhere.
    NoColor,
}

impl Color {
    /// Downgrade this color to the richest form the terminal supports.
    /// `NoColor` is preserved; `Palette16` is already the lowest tier
    /// and passes through.
    #[must_use]
    pub fn downgrade(self, cap: Capability) -> Color {
        match (self, cap) {
            (_, Capability::None) => Color::NoColor,
            (Color::NoColor, _) => Color::NoColor,
            // Truecolor input.
            (Color::TrueColor { r, g, b }, Capability::Palette256) => {
                Color::Palette256(rgb_to_256(r, g, b))
            }
            (Color::TrueColor { r, g, b }, Capability::Palette16) => {
                Color::Palette16(rgb_to_ansi16(r, g, b))
            }
            // Palette256 input.
            (Color::Palette256(n), Capability::Palette16) => {
                Color::Palette16(palette256_to_ansi16(n))
            }
            // Same or richer capability: pass through.
            (c, _) => c,
        }
    }
}

/// Text decorations a segment may layer over its theme color. `role`
/// is the role the segment wants; `fg` is an explicit color override
/// that wins over `role`. `hyperlink` carries an OSC 8 URL that
/// [`crate::layout::runs_to_ansi`] wraps around the run's text when
/// the terminal advertises hyperlink support. `bg` lives in the spec
/// but isn't wired through.
///
/// Not `Copy` — `hyperlink` carries an owned `String`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Style {
    pub role: Option<Role>,
    pub fg: Option<Color>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub dim: bool,
    pub hyperlink: Option<String>,
}

impl Style {
    /// Shorthand for a style that only carries a role.
    #[must_use]
    pub const fn role(role: Role) -> Self {
        Self {
            role: Some(role),
            fg: None,
            bold: false,
            italic: false,
            underline: false,
            dim: false,
            hyperlink: None,
        }
    }

    /// Chainable: attach an OSC 8 URL to this style. The link emits
    /// only when the terminal advertises hyperlink support; capable
    /// terminals render the run as a clickable link to `url`. An
    /// empty `url` folds to `None`, matching the plugin-output path
    /// — emitting `\x1b]8;;\x1b\\` would wrap text in a link to
    /// nothing.
    #[must_use]
    pub fn with_hyperlink(mut self, url: impl Into<String>) -> Self {
        let url = url.into();
        self.hyperlink = if url.is_empty() { None } else { Some(url) };
        self
    }
}

/// Text + style emitted by the layout engine. The flat run sequence is
/// what [`crate::layout::render_to_runs`] returns: one run per segment
/// plus one run per non-empty inter-segment separator. Consumers map
/// runs to their target surface — ANSI SGR for terminal stdout,
/// ratatui `Span` for the TUI preview pane — without re-parsing
/// escape sequences.
///
/// Fields are `pub(crate)` so external callers go through
/// [`StyledRun::new`] and the accessors; this mirrors
/// [`crate::segments::RenderedSegment`] and keeps room to enforce
/// text-content invariants later without a SemVer break.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StyledRun {
    pub(crate) text: String,
    pub(crate) style: Style,
}

impl StyledRun {
    /// Build a run from `text` and `style`. Out-of-tree consumers of
    /// [`crate::layout::runs_to_ansi`] go through this so future
    /// invariants (escape-sequence rejection, width caching) can
    /// land without breaking them.
    ///
    /// Today the constructor performs no validation; sanitizing
    /// untrusted text is the segment's responsibility (see
    /// [`crate::segments::RenderedSegment::new`]).
    #[must_use]
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }

    /// The run's text. Map directly to a target surface (ratatui
    /// `Span`, ANSI SGR pair, etc.).
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The run's style. Returned by reference; `Style` carries an
    /// owned `hyperlink` URL so it is no longer `Copy`.
    #[must_use]
    pub fn style(&self) -> &Style {
        &self.style
    }
}

/// Terminal color capability detected from the environment. Truecolor
/// is preferred when available; `None` strips all color and keeps only
/// decorations.
///
/// Variants are ordered richest→poorest so `cap >= Capability::Palette256`
/// reads "at least 256 colors." Downgrade logic can branch on ordering
/// rather than re-matching the ladder each time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    None,
    Palette16,
    Palette256,
    TrueColor,
}

impl Capability {
    /// Detect honoring `NO_COLOR` (per no-color.org). Kept for callers
    /// that want a single-shot detect without threading the env through
    /// a precedence chain.
    #[must_use]
    pub fn detect() -> Self {
        if std::env::var_os("NO_COLOR").is_some() {
            return Self::None;
        }
        Self::from_terminal()
    }

    /// Raw terminal capability from `supports-color`, ignoring
    /// `NO_COLOR`. The color-policy precedence chain uses this so a
    /// `--force-color` flag can outrank the env var. Returns `None`
    /// when stdout isn't a TTY so `color = "auto"` stays plain-text
    /// under pipes and redirects.
    #[must_use]
    pub fn from_terminal() -> Self {
        match supports_color::on(supports_color::Stream::Stdout) {
            Some(c) if c.has_16m => Self::TrueColor,
            Some(c) if c.has_256 => Self::Palette256,
            Some(_) => Self::Palette16,
            None => Self::None,
        }
    }

    /// Env-only capability probe from the `COLORTERM` and `TERM`
    /// values carried on `CliEnv`. Used by the force-color path to
    /// recover a tier when `supports-color` gives up (e.g. piped
    /// stdout under Claude Code). Never reads the live process env;
    /// callers must snapshot into `CliEnv::from_process` first so
    /// tests and embedders stay hermetic.
    ///
    /// Returns `None` for `TERM=dumb`, empty `TERM`, or no `TERM` at
    /// all.
    #[must_use]
    pub fn from_env_vars(colorterm: Option<&str>, term: Option<&str>) -> Self {
        if let Some(c) = colorterm {
            if c.eq_ignore_ascii_case("truecolor") || c.eq_ignore_ascii_case("24bit") {
                return Self::TrueColor;
            }
        }
        match term.map(str::trim) {
            None | Some("") => Self::None,
            Some(t) if t.eq_ignore_ascii_case("dumb") => Self::None,
            Some(t) if t.contains("256color") => Self::Palette256,
            _ => Self::Palette16,
        }
    }

    /// Combine a TTY probe (from `supports-color`) and an env probe
    /// (from `COLORTERM` / `TERM`) into a capability under force-color
    /// intent. Picks the richer of the two, flooring at `Palette16`
    /// so an explicit `color = "always"` / `--force-color` / `FORCE_COLOR`
    /// never collapses to no-color. The `Palette16` floor overrides
    /// `TERM=dumb` — the user explicitly asked for color, so a dumb
    /// terminal signal loses.
    #[must_use]
    pub fn force_from(tty: Self, env: Self) -> Self {
        tty.max(env).max(Self::Palette16)
    }
}

/// Named color palette keyed by [`Role`]. `colors[role as usize]` is
/// either `Some(color)` when the theme defines it or `None` so
/// [`Theme::color`] falls back via [`Role::fallback`]. Fields stay
/// crate-private so callers can't mint themes with arbitrary names or
/// bypass the fallback contract; user-authored themes will go through
/// a validated constructor when TOML loading lands.
#[derive(Debug, Clone)]
pub struct Theme {
    name: &'static str,
    colors: [Option<Color>; Role::COUNT],
}

impl Theme {
    /// Name this theme is registered under. Callers use this to surface
    /// a `--list-themes`-style view or to echo the active theme in
    /// diagnostics.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Crate-internal constructor for themes loaded from user files.
    /// Keeps the private-fields contract intact (so external callers
    /// can't mint impostor themes) while letting the `user` loader
    /// produce themes from parsed TOML.
    #[must_use]
    pub(super) fn from_user_parts(
        name: &'static str,
        colors: [Option<Color>; Role::COUNT],
    ) -> Self {
        Self { name, colors }
    }

    /// Resolve a role to its color, following the extended-role
    /// fallback chain. Returns `Color::NoColor` when nothing in the
    /// chain maps — which in practice only happens for the `minimal`
    /// theme.
    #[must_use]
    pub fn color(&self, role: Role) -> Color {
        let mut current = role;
        loop {
            if let Some(c) = self.colors[current as usize] {
                return c;
            }
            let next = current.fallback();
            if next == current {
                return Color::NoColor;
            }
            current = next;
        }
    }
}

/// Built-in themes. `default` fully populates base roles so
/// `Theme::color`'s fallback loop never returns `NoColor` for it;
/// `minimal` leaves every role unset so everything falls through to
/// `NoColor`, forcing decoration-only output. The rest are popular
/// curated palettes shipped per the v0.1 vision (#3 — preset
/// onboarding).
const BUILTIN_THEMES: &[Theme] = &[
    default::DEFAULT,
    minimal::MINIMAL,
    catppuccin::LATTE,
    catppuccin::FRAPPE,
    catppuccin::MACCHIATO,
    catppuccin::MOCHA,
    dracula::DRACULA,
    nord::NORD,
    gruvbox::GRUVBOX,
    tokyo_night::TOKYO_NIGHT,
    rose_pine::ROSE_PINE,
];

/// Look up a built-in theme by name. Unknown names return `None` so
/// callers can warn and fall back to `default`.
#[must_use]
pub fn built_in(name: &str) -> Option<&'static Theme> {
    BUILTIN_THEMES.iter().find(|t| t.name == name)
}

/// The default theme when config names none (or names an unknown one).
/// Guaranteed to exist.
#[must_use]
pub fn default_theme() -> &'static Theme {
    built_in("default").expect("`default` theme is always compiled in")
}

/// Names of all compiled-in themes, in registration order.
pub fn builtin_names() -> impl Iterator<Item = &'static str> {
    BUILTIN_THEMES.iter().map(|t| t.name)
}

/// Emit the ANSI SGR prefix for `style` under `cap`. Returns an empty
/// string when the style is plain or the capability is `None` with no
/// decorations.
#[must_use]
pub fn sgr_open(style: &Style, theme: &Theme, cap: Capability) -> String {
    let mut params = Vec::<u16>::with_capacity(4);
    if style.bold {
        params.push(1);
    }
    if style.dim {
        params.push(2);
    }
    if style.italic {
        params.push(3);
    }
    if style.underline {
        params.push(4);
    }
    let fg = resolve_fg(style, theme, cap);
    emit_fg_params(fg, &mut params);
    if params.is_empty() {
        return String::new();
    }
    let mut out = String::from("\x1b[");
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push(';');
        }
        let _ = write!(out, "{p}");
    }
    out.push('m');
    out
}

/// SGR reset. Emitted after every styled span so color and decorations
/// don't leak across segment boundaries.
#[must_use]
pub const fn sgr_reset() -> &'static str {
    "\x1b[0m"
}

/// Resolve the effective foreground color for a style: explicit `fg`
/// wins over `role`; `role` is looked up in the theme and downgraded
/// to the terminal's capability; absence → `NoColor`.
fn resolve_fg(style: &Style, theme: &Theme, cap: Capability) -> Color {
    let raw = style
        .fg
        .or_else(|| style.role.map(|r| theme.color(r)))
        .unwrap_or(Color::NoColor);
    raw.downgrade(cap)
}

/// Push the SGR params (38;2;r;g;b / 38;5;n / 30–37 / 90–97) for a
/// foreground color. `NoColor` pushes nothing.
fn emit_fg_params(color: Color, out: &mut Vec<u16>) {
    match color {
        Color::NoColor => {}
        Color::Palette16(ansi) => {
            let code = ansi_to_sgr_fg(ansi);
            out.push(code);
        }
        Color::Palette256(n) => {
            out.extend_from_slice(&[38, 5, u16::from(n)]);
        }
        Color::TrueColor { r, g, b } => {
            out.extend_from_slice(&[38, 2, u16::from(r), u16::from(g), u16::from(b)]);
        }
    }
}

fn ansi_to_sgr_fg(c: AnsiColor) -> u16 {
    let n = c as u8;
    if n < 8 {
        30 + u16::from(n)
    } else {
        // 90..=97 for bright variants; n is 8..=15.
        90 + u16::from(n - 8)
    }
}

// --- Downgrade algorithms ---

/// Map an RGB triple to the nearest xterm-256 index. Uses the standard
/// 6×6×6 color cube (16..=231) plus the 24-step grayscale ramp
/// (232..=255). Picks grayscale when r≈g≈b to avoid muddy cube greys.
fn rgb_to_256(r: u8, g: u8, b: u8) -> u8 {
    if r == g && g == b {
        if r < 8 {
            return 16;
        }
        // `238` is the last channel value whose mapped ramp index
        // stays within `232..=255`: `232 + (238-8)/10 = 255`. Anything
        // higher would overflow the ramp, so map it to palette-white.
        if r > 238 {
            return 231;
        }
        return 232 + (r - 8) / 10;
    }
    let r6 = scale_to_cube(r);
    let g6 = scale_to_cube(g);
    let b6 = scale_to_cube(b);
    16 + 36 * r6 + 6 * g6 + b6
}

fn scale_to_cube(channel: u8) -> u8 {
    // xterm cube levels: 0, 95, 135, 175, 215, 255.
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let mut best = 0u8;
    let mut best_dist = u16::MAX;
    for (i, &level) in LEVELS.iter().enumerate() {
        let d = u16::from(channel.abs_diff(level));
        if d < best_dist {
            best_dist = d;
            best = u8::try_from(i).expect("cube index fits in u8");
        }
    }
    best
}

/// Map an RGB triple to the nearest 16-color ANSI slot via luminance
/// plus primary-channel heuristic. This is coarse on purpose: themes
/// that care about 16-color output are expected to declare Palette16
/// values directly; this path exists for truecolor-only themes
/// running under a legacy terminal.
fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> AnsiColor {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let bright = max >= 128;
    // Achromatic: pick from black / grey / white by luminance.
    if max - min < 32 {
        return match max {
            0..=42 => AnsiColor::Black,
            43..=170 => {
                if bright {
                    AnsiColor::BrightBlack
                } else {
                    AnsiColor::White
                }
            }
            _ => AnsiColor::BrightWhite,
        };
    }
    // Dominant channel picks the hue.
    let dominant_r = r >= g && r >= b;
    let dominant_g = g >= r && g >= b;
    let dominant_b = b >= r && b >= g;
    let r_hi = r >= 128;
    let g_hi = g >= 128;
    let b_hi = b >= 128;
    match (dominant_r, dominant_g, dominant_b, r_hi, g_hi, b_hi) {
        (true, _, _, _, true, _) => {
            if bright {
                AnsiColor::BrightYellow
            } else {
                AnsiColor::Yellow
            }
        }
        (true, _, _, _, _, true) => {
            if bright {
                AnsiColor::BrightMagenta
            } else {
                AnsiColor::Magenta
            }
        }
        (_, true, _, _, _, true) => {
            if bright {
                AnsiColor::BrightCyan
            } else {
                AnsiColor::Cyan
            }
        }
        (_, true, _, true, _, _) => {
            if bright {
                AnsiColor::BrightYellow
            } else {
                AnsiColor::Yellow
            }
        }
        (true, _, _, _, _, _) => {
            if bright {
                AnsiColor::BrightRed
            } else {
                AnsiColor::Red
            }
        }
        (_, true, _, _, _, _) => {
            if bright {
                AnsiColor::BrightGreen
            } else {
                AnsiColor::Green
            }
        }
        (_, _, true, _, _, _) => {
            if bright {
                AnsiColor::BrightBlue
            } else {
                AnsiColor::Blue
            }
        }
        _ => AnsiColor::White,
    }
}

/// Route a Palette256 entry down to the nearest 16-color slot. For
/// indices 0..=15 the mapping is identity; for the cube we decompose
/// back to approximate RGB and hand off to [`rgb_to_ansi16`].
fn palette256_to_ansi16(n: u8) -> AnsiColor {
    if n < 16 {
        return match n {
            0 => AnsiColor::Black,
            1 => AnsiColor::Red,
            2 => AnsiColor::Green,
            3 => AnsiColor::Yellow,
            4 => AnsiColor::Blue,
            5 => AnsiColor::Magenta,
            6 => AnsiColor::Cyan,
            7 => AnsiColor::White,
            8 => AnsiColor::BrightBlack,
            9 => AnsiColor::BrightRed,
            10 => AnsiColor::BrightGreen,
            11 => AnsiColor::BrightYellow,
            12 => AnsiColor::BrightBlue,
            13 => AnsiColor::BrightMagenta,
            14 => AnsiColor::BrightCyan,
            _ => AnsiColor::BrightWhite,
        };
    }
    if n >= 232 {
        // Grayscale ramp: map to bright/dim black or white by index.
        let step = n - 232;
        return match step {
            0..=7 => AnsiColor::Black,
            8..=15 => AnsiColor::BrightBlack,
            16..=19 => AnsiColor::White,
            _ => AnsiColor::BrightWhite,
        };
    }
    // Color cube: decompose (n-16) into r6/g6/b6, expand to approx RGB.
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let c = n - 16;
    let r6 = usize::from(c / 36);
    let g6 = usize::from((c / 6) % 6);
    let b6 = usize::from(c % 6);
    rgb_to_ansi16(LEVELS[r6], LEVELS[g6], LEVELS[b6])
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Role ---

    #[test]
    fn role_count_matches_enum_discriminant_range() {
        assert_eq!(Role::Foreground as usize, 0);
        assert_eq!(Role::Border as usize, Role::COUNT - 1);
    }

    #[test]
    fn extended_role_fallbacks_match_spec_table() {
        assert_eq!(Role::SuccessDim.fallback(), Role::Success);
        assert_eq!(Role::WarningDim.fallback(), Role::Warning);
        assert_eq!(Role::ErrorDim.fallback(), Role::Error);
        assert_eq!(Role::PrimaryDim.fallback(), Role::Primary);
        assert_eq!(Role::AccentDim.fallback(), Role::Accent);
        assert_eq!(Role::Surface.fallback(), Role::Background);
        assert_eq!(Role::Border.fallback(), Role::Muted);
    }

    #[test]
    fn base_roles_fall_back_to_themselves() {
        for role in [
            Role::Foreground,
            Role::Background,
            Role::Muted,
            Role::Primary,
            Role::Accent,
            Role::Success,
            Role::Warning,
            Role::Error,
            Role::Info,
        ] {
            assert_eq!(role.fallback(), role);
        }
    }

    // --- Theme::color ---

    #[test]
    fn default_theme_maps_every_base_role() {
        let t = built_in("default").expect("default exists");
        for role in [
            Role::Foreground,
            Role::Muted,
            Role::Primary,
            Role::Accent,
            Role::Success,
            Role::Warning,
            Role::Error,
            Role::Info,
        ] {
            let c = t.color(role);
            assert!(
                !matches!(c, Color::NoColor),
                "default theme left {role:?} as NoColor",
            );
        }
    }

    #[test]
    fn default_theme_extended_roles_fall_back_to_base() {
        let t = built_in("default").expect("default exists");
        assert_eq!(t.color(Role::SuccessDim), t.color(Role::Success));
        assert_eq!(t.color(Role::PrimaryDim), t.color(Role::Primary));
        assert_eq!(t.color(Role::Border), t.color(Role::Muted));
    }

    #[test]
    fn minimal_theme_returns_no_color_for_every_role() {
        let t = built_in("minimal").expect("minimal exists");
        for role in [
            Role::Foreground,
            Role::Primary,
            Role::Warning,
            Role::SuccessDim,
        ] {
            assert_eq!(t.color(role), Color::NoColor);
        }
    }

    #[test]
    fn unknown_theme_name_returns_none() {
        assert!(built_in("nope").is_none());
    }

    #[test]
    fn default_theme_always_available() {
        assert_eq!(default_theme().name, "default");
    }

    #[test]
    fn builtin_names_lists_default_and_minimal() {
        let names: Vec<&str> = builtin_names().collect();
        assert!(names.contains(&"default"));
        assert!(names.contains(&"minimal"));
    }

    #[test]
    fn builtin_names_lists_all_curated_presets() {
        // Pin the v0.1 preset pack: a regression that drops one of
        // these from BUILTIN_THEMES would hide a user-visible theme
        // from `linesmith themes list` and the future TUI picker.
        // The count check ratchets — adding a new theme must update
        // both the membership list and the count, forcing deliberate
        // review.
        let names: Vec<&str> = builtin_names().collect();
        for theme in ["dracula", "nord", "gruvbox", "tokyo-night", "rose-pine"] {
            assert!(names.contains(&theme), "missing {theme} in builtin_names");
        }
        assert_eq!(
            builtin_names().count(),
            11,
            "BUILTIN_THEMES count drift: default + minimal + 4 catppuccin + 5 curated = 11"
        );
    }

    #[test]
    fn every_curated_preset_maps_every_base_role() {
        // Contract: no curated preset leaves a base role unmapped. A
        // typo dropping `c[Role::Warning as usize] = ...` from any
        // theme module would silently leave `NoColor` in production
        // for that role; this test catches it. Mirrors
        // `catppuccin::tests::every_flavor_maps_every_base_role`.
        for name in ["dracula", "nord", "gruvbox", "tokyo-night", "rose-pine"] {
            let t = built_in(name).expect(name);
            for role in [
                Role::Foreground,
                Role::Background,
                Role::Muted,
                Role::Primary,
                Role::Accent,
                Role::Success,
                Role::Warning,
                Role::Error,
                Role::Info,
            ] {
                assert!(
                    !matches!(t.color(role), Color::NoColor),
                    "{name} left {role:?} as NoColor"
                );
            }
        }
    }

    // --- Downgrade ---

    #[test]
    fn downgrade_strips_color_under_no_capability() {
        assert_eq!(
            Color::TrueColor { r: 255, g: 0, b: 0 }.downgrade(Capability::None),
            Color::NoColor
        );
        assert_eq!(
            Color::Palette16(AnsiColor::Red).downgrade(Capability::None),
            Color::NoColor
        );
    }

    #[test]
    fn downgrade_preserves_matching_or_richer_capability() {
        let c = Color::Palette16(AnsiColor::Green);
        assert_eq!(c.downgrade(Capability::Palette16), c);
        assert_eq!(c.downgrade(Capability::Palette256), c);
        assert_eq!(c.downgrade(Capability::TrueColor), c);
    }

    #[test]
    fn downgrade_truecolor_to_256_grayscale_uses_gray_ramp() {
        // Mid-grey should land in the 232..=255 ramp, not the cube.
        let g = Color::TrueColor {
            r: 128,
            g: 128,
            b: 128,
        };
        match g.downgrade(Capability::Palette256) {
            Color::Palette256(n) => assert!((232..=255).contains(&n), "got {n}"),
            other => panic!("expected Palette256, got {other:?}"),
        }
    }

    #[test]
    fn downgrade_truecolor_near_white_grayscale_saturates_without_overflow() {
        // Regression: the ramp formula `232 + (r-8)/10` overflows u8 at
        // r >= 239 (`(239-8)/10 = 23` is the last safe index mapping
        // to 255). Channels above 238 must saturate to 231, not panic.
        for v in [239u8, 240, 248, 249, 255] {
            let c = Color::TrueColor { r: v, g: v, b: v };
            match c.downgrade(Capability::Palette256) {
                Color::Palette256(n) => assert!(
                    (232..=255).contains(&n) || n == 231,
                    "r={v}: n={n} out of ramp/white range"
                ),
                other => panic!("expected Palette256, got {other:?}"),
            }
        }
    }

    #[test]
    fn downgrade_truecolor_to_256_uses_cube_for_color() {
        // Pure red: (255, 0, 0). r6=5, g6=0, b6=0 → 16 + 180 = 196.
        let red = Color::TrueColor { r: 255, g: 0, b: 0 };
        assert_eq!(
            red.downgrade(Capability::Palette256),
            Color::Palette256(196)
        );
        // Pure green: r6=0, g6=5, b6=0 → 16 + 30 = 46.
        let green = Color::TrueColor { r: 0, g: 255, b: 0 };
        assert_eq!(
            green.downgrade(Capability::Palette256),
            Color::Palette256(46)
        );
        // Pure blue: r6=0, g6=0, b6=5 → 16 + 5 = 21.
        let blue = Color::TrueColor { r: 0, g: 0, b: 255 };
        assert_eq!(
            blue.downgrade(Capability::Palette256),
            Color::Palette256(21)
        );
    }

    #[test]
    fn downgrade_palette256_cube_to_16_picks_a_color() {
        // Index 196 is pure red in the cube; must route to a red slot.
        match Color::Palette256(196).downgrade(Capability::Palette16) {
            Color::Palette16(ac) => assert!(
                matches!(ac, AnsiColor::Red | AnsiColor::BrightRed),
                "got {ac:?}"
            ),
            other => panic!("expected Palette16, got {other:?}"),
        }
    }

    #[test]
    fn downgrade_palette256_low_16_passes_through_identity() {
        // Indices 0..=15 map to their AnsiColor counterparts.
        assert_eq!(
            Color::Palette256(1).downgrade(Capability::Palette16),
            Color::Palette16(AnsiColor::Red),
        );
        assert_eq!(
            Color::Palette256(15).downgrade(Capability::Palette16),
            Color::Palette16(AnsiColor::BrightWhite),
        );
    }

    #[test]
    fn downgrade_palette256_grayscale_routes_to_blacks_or_whites() {
        // Index 232 is darkest grey → Black; 255 is lightest → BrightWhite.
        assert_eq!(
            Color::Palette256(232).downgrade(Capability::Palette16),
            Color::Palette16(AnsiColor::Black),
        );
        assert_eq!(
            Color::Palette256(255).downgrade(Capability::Palette16),
            Color::Palette16(AnsiColor::BrightWhite),
        );
    }

    #[test]
    fn downgrade_truecolor_red_to_16_picks_red_family() {
        let red = Color::TrueColor {
            r: 200,
            g: 30,
            b: 30,
        };
        match red.downgrade(Capability::Palette16) {
            Color::Palette16(ac) => assert!(
                matches!(ac, AnsiColor::Red | AnsiColor::BrightRed),
                "got {ac:?}"
            ),
            other => panic!("expected Palette16, got {other:?}"),
        }
    }

    // --- SGR emission ---

    #[test]
    fn sgr_open_plain_style_emits_nothing() {
        let s = Style::default();
        let t = default_theme();
        assert_eq!(sgr_open(&s, t, Capability::TrueColor), "");
    }

    #[test]
    fn sgr_open_bold_only_emits_sgr1() {
        let s = Style {
            bold: true,
            ..Style::default()
        };
        assert_eq!(sgr_open(&s, default_theme(), Capability::None), "\x1b[1m");
    }

    #[test]
    fn sgr_open_role_under_palette16_emits_ansi_code() {
        let s = Style::role(Role::Success);
        let out = sgr_open(&s, default_theme(), Capability::Palette16);
        // Default theme maps Success → BrightGreen (SGR 92).
        assert_eq!(out, "\x1b[92m");
    }

    #[test]
    fn sgr_open_role_under_no_capability_drops_color_keeps_decoration() {
        let s = Style {
            role: Some(Role::Error),
            bold: true,
            ..Style::default()
        };
        assert_eq!(sgr_open(&s, default_theme(), Capability::None), "\x1b[1m");
    }

    #[test]
    fn sgr_open_explicit_fg_wins_over_role() {
        let s = Style {
            role: Some(Role::Warning),
            fg: Some(Color::Palette16(AnsiColor::Blue)),
            ..Style::default()
        };
        // Blue is SGR 34; Warning would have been 93 (BrightYellow).
        assert_eq!(
            sgr_open(&s, default_theme(), Capability::Palette16),
            "\x1b[34m"
        );
    }

    #[test]
    fn sgr_open_truecolor_fg_emits_38_2_sequence() {
        let s = Style {
            fg: Some(Color::TrueColor {
                r: 12,
                g: 34,
                b: 56,
            }),
            ..Style::default()
        };
        assert_eq!(
            sgr_open(&s, default_theme(), Capability::TrueColor),
            "\x1b[38;2;12;34;56m"
        );
    }

    #[test]
    fn sgr_open_combines_decorations_and_color() {
        let s = Style {
            role: Some(Role::Primary),
            bold: true,
            italic: true,
            ..Style::default()
        };
        // SGR params emit in order: bold(1), italic(3), color(95 = BrightMagenta).
        assert_eq!(
            sgr_open(&s, default_theme(), Capability::Palette16),
            "\x1b[1;3;95m"
        );
    }

    #[test]
    fn sgr_open_dim_only_emits_sgr2() {
        let s = Style {
            dim: true,
            ..Style::default()
        };
        assert_eq!(sgr_open(&s, default_theme(), Capability::None), "\x1b[2m");
    }

    #[test]
    fn sgr_open_underline_only_emits_sgr4() {
        let s = Style {
            underline: true,
            ..Style::default()
        };
        assert_eq!(sgr_open(&s, default_theme(), Capability::None), "\x1b[4m");
    }

    #[test]
    fn sgr_open_full_decoration_stack_keeps_order() {
        let s = Style {
            role: Some(Role::Primary),
            bold: true,
            dim: true,
            italic: true,
            underline: true,
            ..Style::default()
        };
        // Order: bold(1), dim(2), italic(3), underline(4), color(95).
        assert_eq!(
            sgr_open(&s, default_theme(), Capability::Palette16),
            "\x1b[1;2;3;4;95m"
        );
    }

    #[test]
    fn sgr_reset_is_stable_and_short() {
        assert_eq!(sgr_reset(), "\x1b[0m");
    }

    #[test]
    fn minimal_theme_emits_only_decorations() {
        let s = Style {
            role: Some(Role::Success),
            bold: true,
            ..Style::default()
        };
        let t = built_in("minimal").expect("minimal");
        assert_eq!(sgr_open(&s, t, Capability::TrueColor), "\x1b[1m");
    }

    // --- env-fallback capability probe ---

    #[test]
    fn from_env_vars_prefers_colorterm_truecolor() {
        assert_eq!(
            Capability::from_env_vars(Some("truecolor"), Some("xterm")),
            Capability::TrueColor
        );
        assert_eq!(
            Capability::from_env_vars(Some("24bit"), Some("xterm")),
            Capability::TrueColor
        );
        assert_eq!(
            Capability::from_env_vars(Some("TRUECOLOR"), None),
            Capability::TrueColor
        );
    }

    #[test]
    fn from_env_vars_falls_back_to_term_256color() {
        assert_eq!(
            Capability::from_env_vars(None, Some("xterm-256color")),
            Capability::Palette256
        );
        assert_eq!(
            Capability::from_env_vars(None, Some("tmux-256color")),
            Capability::Palette256
        );
    }

    #[test]
    fn from_env_vars_unknown_term_is_palette16() {
        assert_eq!(
            Capability::from_env_vars(None, Some("xterm")),
            Capability::Palette16
        );
        assert_eq!(
            Capability::from_env_vars(None, Some("vt100")),
            Capability::Palette16
        );
    }

    #[test]
    fn from_env_vars_dumb_or_missing_term_is_none() {
        assert_eq!(
            Capability::from_env_vars(None, Some("dumb")),
            Capability::None
        );
        // Case-insensitive on TERM=dumb to mirror the COLORTERM check.
        assert_eq!(
            Capability::from_env_vars(None, Some("DUMB")),
            Capability::None
        );
        assert_eq!(
            Capability::from_env_vars(None, Some("  dumb  ")),
            Capability::None
        );
        assert_eq!(Capability::from_env_vars(None, Some("")), Capability::None);
        assert_eq!(Capability::from_env_vars(None, None), Capability::None);
    }

    #[test]
    fn from_env_vars_colorterm_truecolor_overrides_term_dumb() {
        // COLORTERM=truecolor is the stronger signal and wins over TERM=dumb.
        assert_eq!(
            Capability::from_env_vars(Some("truecolor"), Some("dumb")),
            Capability::TrueColor
        );
    }

    // --- force-color combinator ---

    #[test]
    fn force_from_picks_max_of_both_inputs() {
        assert_eq!(
            Capability::force_from(Capability::Palette256, Capability::TrueColor),
            Capability::TrueColor
        );
        assert_eq!(
            Capability::force_from(Capability::TrueColor, Capability::Palette256),
            Capability::TrueColor
        );
        assert_eq!(
            Capability::force_from(Capability::Palette16, Capability::Palette256),
            Capability::Palette256
        );
    }

    #[test]
    fn force_from_truecolor_from_either_side_wins() {
        assert_eq!(
            Capability::force_from(Capability::None, Capability::TrueColor),
            Capability::TrueColor
        );
        assert_eq!(
            Capability::force_from(Capability::TrueColor, Capability::None),
            Capability::TrueColor
        );
    }

    #[test]
    fn force_from_floors_at_palette16_when_both_inputs_are_none() {
        // Force-color intent must emit something; the user opted in
        // explicitly via --force-color or `color = "always"`.
        assert_eq!(
            Capability::force_from(Capability::None, Capability::None),
            Capability::Palette16
        );
    }

    #[test]
    fn force_from_floor_overrides_dumb_term_when_env_probe_returns_none() {
        // Even when TERM=dumb made the env probe return None, force-color
        // intent wins — the user knows what they're doing.
        let env = Capability::from_env_vars(None, Some("dumb"));
        assert_eq!(env, Capability::None);
        assert_eq!(
            Capability::force_from(Capability::None, env),
            Capability::Palette16
        );
    }

    #[test]
    fn from_env_vars_colorterm_garbage_falls_through_to_term() {
        // COLORTERM=yes (some terminals set this as a boolean hint) does
        // not claim truecolor; fall through to the TERM probe.
        assert_eq!(
            Capability::from_env_vars(Some("yes"), Some("xterm-256color")),
            Capability::Palette256
        );
    }
}
