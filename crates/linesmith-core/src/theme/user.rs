//! User-authored theme TOML loading. Parses files from
//! `~/.config/linesmith/themes/*.toml` per `docs/specs/theming.md`
//! §Theme file format and merges them with the compiled-in themes
//! into a [`ThemeRegistry`]. The registry is the single source of
//! truth the driver consults for `theme = "..."` resolution.

use super::{AnsiColor, Color, Role, Theme};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// One theme in the registry, paired with its source for
/// `themes list` output.
#[derive(Debug, Clone)]
pub struct RegisteredTheme {
    pub theme: Theme,
    pub source: ThemeSource,
}

/// Where a registered theme came from. User files carry their path
/// so diagnostics can point the user at the right TOML to edit.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ThemeSource {
    BuiltIn,
    UserFile(PathBuf),
}

/// Built-in + user themes combined. User themes override built-ins
/// of the same name (with a warning). When two user files declare
/// the same `name`, the first one loaded wins and the later one is
/// warned + skipped — matches `docs/specs/theming.md` §Edge cases.
/// Files are processed in filename-sorted order, so "first" means
/// alphabetically first.
#[derive(Debug, Clone, Default)]
pub struct ThemeRegistry {
    themes: Vec<RegisteredTheme>,
}

impl ThemeRegistry {
    /// Start with the compiled-in themes; no file I/O.
    #[must_use]
    pub fn with_built_ins() -> Self {
        Self {
            themes: super::BUILTIN_THEMES
                .iter()
                .map(|t| RegisteredTheme {
                    theme: t.clone(),
                    source: ThemeSource::BuiltIn,
                })
                .collect(),
        }
    }

    /// Load every `*.toml` in `dir` and merge into the registry. One
    /// bad file never aborts the walk; each parse error emits a
    /// diagnostic via `warn` and the file is skipped. Files are
    /// processed in filename-sorted order. A user theme whose name
    /// collides with a built-in replaces the built-in (with a
    /// warning); a user theme whose name collides with an already-
    /// loaded user theme is warned + skipped — per the spec's
    /// "first-user-theme-found wins" rule.
    pub fn with_user_themes(mut self, dir: &Path, mut warn: impl FnMut(&str)) -> Self {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return self,
            Err(e) => {
                // Breadcrumb the likely downstream symptom so a later
                // "unknown theme 'X'" warning isn't puzzling in
                // isolation.
                warn(&format!(
                    "user themes dir {}: {} (themes referenced by name won't resolve)",
                    dir.display(),
                    e
                ));
                return self;
            }
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(|entry| match entry {
                Ok(e) => Some(e.path()),
                Err(e) => {
                    warn(&format!("skipping entry in {}: {e}", dir.display()));
                    None
                }
            })
            .filter(|p| p.extension().is_some_and(|ext| ext == "toml"))
            .collect();
        files.sort(); // deterministic order so collision warnings reproduce
        for path in files {
            match load_theme_file(&path, &mut warn) {
                Ok(theme) => self.insert(theme, ThemeSource::UserFile(path), &mut warn),
                Err(err) => warn(&format!("theme {}: {err}", path.display())),
            }
        }
        self
    }

    fn insert(&mut self, theme: Theme, source: ThemeSource, warn: &mut impl FnMut(&str)) {
        let Some(existing) = self
            .themes
            .iter_mut()
            .find(|r| r.theme.name() == theme.name())
        else {
            self.themes.push(RegisteredTheme { theme, source });
            return;
        };
        match &existing.source {
            ThemeSource::BuiltIn => {
                // User theme wins over built-in.
                warn(&format!(
                    "theme '{}' from {} overrides built-in",
                    theme.name(),
                    describe(&source),
                ));
                existing.theme = theme;
                existing.source = source;
            }
            ThemeSource::UserFile(_) => {
                // Per docs/specs/theming.md §Edge cases: first user
                // theme wins, later duplicates warn + skip.
                warn(&format!(
                    "theme '{}' from {} skipped; already loaded from {}",
                    theme.name(),
                    describe(&source),
                    describe(&existing.source),
                ));
            }
        }
    }

    /// Look up a theme by its registered `name`. Returns `None` for
    /// unknown names so `resolve_theme` can warn + fall back.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&Theme> {
        self.themes
            .iter()
            .find(|r| r.theme.name() == name)
            .map(|r| &r.theme)
    }

    /// Enumerate registered themes in registration order: built-ins
    /// first (in compile-time order), then user themes sorted by
    /// filename.
    pub fn iter(&self) -> impl Iterator<Item = &RegisteredTheme> {
        self.themes.iter()
    }
}

fn describe(source: &ThemeSource) -> String {
    match source {
        ThemeSource::BuiltIn => "built-in".to_string(),
        ThemeSource::UserFile(p) => p.display().to_string(),
    }
}

// --- TOML parsing ---

/// Failure modes when converting a theme TOML file into a `Theme`.
/// Carries enough detail that the user can find the typo without
/// rerunning with a debugger.
#[derive(Debug)]
#[non_exhaustive]
pub enum ThemeParseError {
    Io(std::io::Error),
    Toml(toml::de::Error),
    InvalidColor { role: &'static str, value: String },
}

impl std::fmt::Display for ThemeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Toml(e) => write!(f, "{e}"),
            Self::InvalidColor { role, value } => {
                write!(f, "invalid color for '{role}': {value:?}")
            }
        }
    }
}

impl std::error::Error for ThemeParseError {}

fn load_theme_file(path: &Path, warn: &mut impl FnMut(&str)) -> Result<Theme, ThemeParseError> {
    let raw = std::fs::read_to_string(path).map_err(ThemeParseError::Io)?;
    let value: toml::Value = toml::from_str(&raw).map_err(ThemeParseError::Toml)?;
    validate_theme_file_keys(&value, path, warn);
    let file: ThemeFile = value.try_into().map_err(ThemeParseError::Toml)?;
    theme_file_to_theme(file)
}

/// Top-level keys recognized in user theme files. `author` and
/// `license` are metadata (spec-documented); `separators` is parsed
/// but unused until the powerline renderer lands.
const KNOWN_FILE_TOP_LEVEL: &[&str] = &["name", "author", "license", "roles", "separators"];
const KNOWN_ROLES: &[&str] = &[
    "foreground",
    "background",
    "muted",
    "primary",
    "accent",
    "success",
    "warning",
    "error",
    "info",
    "extended",
];
const KNOWN_ROLES_EXTENDED: &[&str] = &[
    "success_dim",
    "warning_dim",
    "error_dim",
    "primary_dim",
    "accent_dim",
    "surface",
    "border",
    "timer",
];
const KNOWN_SEPARATORS: &[&str] = &["default", "powerline", "ellipsis"];

/// Walk the raw TOML tree and emit one warning per key outside the
/// allow-list. Scope matches the spec's theme-file format: top-level,
/// `[roles]`, `[roles.extended]`, and `[separators]`. Typos like
/// `primray = "#ff00ff"` surface here instead of silently parsing.
fn validate_theme_file_keys(raw: &toml::Value, path: &Path, warn: &mut impl FnMut(&str)) {
    let Some(top) = raw.as_table() else { return };
    for (key, value) in top {
        if !KNOWN_FILE_TOP_LEVEL.contains(&key.as_str()) {
            warn(&format!(
                "theme {}: unknown top-level key '{key}'; ignoring",
                path.display()
            ));
            continue;
        }
        match key.as_str() {
            "roles" => validate_roles_table(value, path, warn),
            "separators" => {
                validate_flat_theme_table(value, path, "separators", KNOWN_SEPARATORS, warn)
            }
            _ => {}
        }
    }
}

fn validate_roles_table(value: &toml::Value, path: &Path, warn: &mut impl FnMut(&str)) {
    let Some(table) = value.as_table() else {
        return;
    };
    for (key, sub) in table {
        if !KNOWN_ROLES.contains(&key.as_str()) {
            warn(&format!(
                "theme {}: unknown key '{key}' in [roles]; ignoring",
                path.display()
            ));
            continue;
        }
        if key == "extended" {
            validate_flat_theme_table(sub, path, "roles.extended", KNOWN_ROLES_EXTENDED, warn);
        }
    }
}

fn validate_flat_theme_table(
    value: &toml::Value,
    path: &Path,
    label: &str,
    allowed: &[&str],
    warn: &mut impl FnMut(&str),
) {
    let Some(table) = value.as_table() else {
        return;
    };
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            warn(&format!(
                "theme {}: unknown key '{key}' in [{label}]; ignoring",
                path.display()
            ));
        }
    }
}

#[derive(Debug, Deserialize)]
struct ThemeFile {
    name: String,
    // `author` and `license` are metadata per the spec; accepted
    // silently so theme authors can document provenance.
    #[serde(default)]
    #[allow(dead_code)]
    author: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    license: Option<String>,
    roles: RolesSection,
    // `separators` is parsed but unused until the powerline renderer
    // lands; accepting the key now keeps user files forward-compatible.
    #[serde(default)]
    #[allow(dead_code)]
    separators: Option<toml::Table>,
}

#[derive(Debug, Deserialize)]
struct RolesSection {
    foreground: String,
    background: String,
    muted: String,
    primary: String,
    accent: String,
    success: String,
    warning: String,
    error: String,
    info: String,
    #[serde(default)]
    extended: Option<ExtendedRoles>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ExtendedRoles {
    success_dim: Option<String>,
    warning_dim: Option<String>,
    error_dim: Option<String>,
    primary_dim: Option<String>,
    accent_dim: Option<String>,
    surface: Option<String>,
    border: Option<String>,
    timer: Option<String>,
}

fn theme_file_to_theme(file: ThemeFile) -> Result<Theme, ThemeParseError> {
    let mut colors = [None; Role::COUNT];
    let name = Box::leak(file.name.into_boxed_str()) as &'static str;

    set_role(
        &mut colors,
        Role::Foreground,
        "foreground",
        &file.roles.foreground,
    )?;
    set_role(
        &mut colors,
        Role::Background,
        "background",
        &file.roles.background,
    )?;
    set_role(&mut colors, Role::Muted, "muted", &file.roles.muted)?;
    set_role(&mut colors, Role::Primary, "primary", &file.roles.primary)?;
    set_role(&mut colors, Role::Accent, "accent", &file.roles.accent)?;
    set_role(&mut colors, Role::Success, "success", &file.roles.success)?;
    set_role(&mut colors, Role::Warning, "warning", &file.roles.warning)?;
    set_role(&mut colors, Role::Error, "error", &file.roles.error)?;
    set_role(&mut colors, Role::Info, "info", &file.roles.info)?;

    if let Some(ext) = file.roles.extended {
        set_opt(
            &mut colors,
            Role::SuccessDim,
            "success_dim",
            ext.success_dim.as_deref(),
        )?;
        set_opt(
            &mut colors,
            Role::WarningDim,
            "warning_dim",
            ext.warning_dim.as_deref(),
        )?;
        set_opt(
            &mut colors,
            Role::ErrorDim,
            "error_dim",
            ext.error_dim.as_deref(),
        )?;
        set_opt(
            &mut colors,
            Role::PrimaryDim,
            "primary_dim",
            ext.primary_dim.as_deref(),
        )?;
        set_opt(
            &mut colors,
            Role::AccentDim,
            "accent_dim",
            ext.accent_dim.as_deref(),
        )?;
        set_opt(
            &mut colors,
            Role::Surface,
            "surface",
            ext.surface.as_deref(),
        )?;
        set_opt(&mut colors, Role::Border, "border", ext.border.as_deref())?;
        set_opt(&mut colors, Role::Timer, "timer", ext.timer.as_deref())?;
    }

    Ok(Theme::from_user_parts(name, colors))
}

fn set_role(
    colors: &mut [Option<Color>; Role::COUNT],
    role: Role,
    label: &'static str,
    raw: &str,
) -> Result<(), ThemeParseError> {
    let c = parse_color(raw).map_err(|_| ThemeParseError::InvalidColor {
        role: label,
        value: raw.to_string(),
    })?;
    colors[role as usize] = Some(c);
    Ok(())
}

fn set_opt(
    colors: &mut [Option<Color>; Role::COUNT],
    role: Role,
    label: &'static str,
    raw: Option<&str>,
) -> Result<(), ThemeParseError> {
    let Some(raw) = raw else {
        return Ok(());
    };
    set_role(colors, role, label, raw)
}

// --- color parsing ---

/// Parse a color spec from a user theme file. Accepts:
/// - `#rgb` or `#rrggbb` hex (normalized to `Color::TrueColor`)
/// - named 16-color (`"red"`, `"bright_black"`, ...) → `Color::Palette16`
/// - `rgb(r, g, b)` with 0..=255 channels → `Color::TrueColor`
///
/// Whitespace around the whole spec is trimmed. Named colors are
/// case-insensitive. `none` / empty string → `Color::NoColor`.
pub(super) fn parse_color(s: &str) -> Result<Color, ()> {
    let s = s.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(Color::NoColor);
    }
    if let Some(rest) = s.strip_prefix('#') {
        return parse_hex(rest);
    }
    if let Some(inner) = s.strip_prefix("rgb(").and_then(|s| s.strip_suffix(')')) {
        return parse_rgb_args(inner);
    }
    parse_named(s)
}

fn parse_hex(rest: &str) -> Result<Color, ()> {
    let bytes = match rest.len() {
        3 => {
            let r = double_nibble(rest.as_bytes()[0])?;
            let g = double_nibble(rest.as_bytes()[1])?;
            let b = double_nibble(rest.as_bytes()[2])?;
            [r, g, b]
        }
        6 => {
            let r = hex_pair(rest.as_bytes()[0], rest.as_bytes()[1])?;
            let g = hex_pair(rest.as_bytes()[2], rest.as_bytes()[3])?;
            let b = hex_pair(rest.as_bytes()[4], rest.as_bytes()[5])?;
            [r, g, b]
        }
        _ => return Err(()),
    };
    Ok(Color::TrueColor {
        r: bytes[0],
        g: bytes[1],
        b: bytes[2],
    })
}

fn double_nibble(b: u8) -> Result<u8, ()> {
    let n = nibble(b)?;
    Ok((n << 4) | n)
}

fn hex_pair(hi: u8, lo: u8) -> Result<u8, ()> {
    Ok((nibble(hi)? << 4) | nibble(lo)?)
}

fn nibble(b: u8) -> Result<u8, ()> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(()),
    }
}

fn parse_rgb_args(inner: &str) -> Result<Color, ()> {
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.len() != 3 {
        return Err(());
    }
    let r: u8 = parts[0].parse().map_err(|_| ())?;
    let g: u8 = parts[1].parse().map_err(|_| ())?;
    let b: u8 = parts[2].parse().map_err(|_| ())?;
    Ok(Color::TrueColor { r, g, b })
}

fn parse_named(s: &str) -> Result<Color, ()> {
    let ansi = match s.to_ascii_lowercase().as_str() {
        "black" => AnsiColor::Black,
        "red" => AnsiColor::Red,
        "green" => AnsiColor::Green,
        "yellow" => AnsiColor::Yellow,
        "blue" => AnsiColor::Blue,
        "magenta" => AnsiColor::Magenta,
        "cyan" => AnsiColor::Cyan,
        "white" => AnsiColor::White,
        "bright_black" | "bright-black" => AnsiColor::BrightBlack,
        "bright_red" | "bright-red" => AnsiColor::BrightRed,
        "bright_green" | "bright-green" => AnsiColor::BrightGreen,
        "bright_yellow" | "bright-yellow" => AnsiColor::BrightYellow,
        "bright_blue" | "bright-blue" => AnsiColor::BrightBlue,
        "bright_magenta" | "bright-magenta" => AnsiColor::BrightMagenta,
        "bright_cyan" | "bright-cyan" => AnsiColor::BrightCyan,
        "bright_white" | "bright-white" => AnsiColor::BrightWhite,
        _ => return Err(()),
    };
    Ok(Color::Palette16(ansi))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{built_in, Capability};

    // --- parse_color ---

    #[test]
    fn parse_hex_6_digit() {
        assert_eq!(
            parse_color("#cba6f7"),
            Ok(Color::TrueColor {
                r: 203,
                g: 166,
                b: 247,
            })
        );
    }

    #[test]
    fn parse_hex_3_digit_expands() {
        // #abc → #aabbcc
        assert_eq!(
            parse_color("#abc"),
            Ok(Color::TrueColor {
                r: 0xaa,
                g: 0xbb,
                b: 0xcc,
            })
        );
    }

    #[test]
    fn parse_hex_case_insensitive() {
        assert_eq!(parse_color("#ABCDEF"), parse_color("#abcdef"));
    }

    #[test]
    fn parse_hex_rejects_bad_length() {
        assert!(parse_color("#12").is_err());
        assert!(parse_color("#12345").is_err());
    }

    #[test]
    fn parse_hex_rejects_non_hex_chars() {
        assert!(parse_color("#xyzxyz").is_err());
    }

    #[test]
    fn parse_named_canonical_forms() {
        assert_eq!(parse_color("red"), Ok(Color::Palette16(AnsiColor::Red)));
        assert_eq!(parse_color("BLUE"), Ok(Color::Palette16(AnsiColor::Blue)));
        assert_eq!(
            parse_color("bright_cyan"),
            Ok(Color::Palette16(AnsiColor::BrightCyan))
        );
        assert_eq!(
            parse_color("bright-magenta"),
            Ok(Color::Palette16(AnsiColor::BrightMagenta))
        );
    }

    #[test]
    fn parse_rgb_function_form() {
        assert_eq!(
            parse_color("rgb(203, 166, 247)"),
            Ok(Color::TrueColor {
                r: 203,
                g: 166,
                b: 247,
            })
        );
        assert_eq!(
            parse_color("rgb(0,0,0)"),
            Ok(Color::TrueColor { r: 0, g: 0, b: 0 })
        );
    }

    #[test]
    fn parse_rgb_rejects_out_of_range_channels() {
        assert!(parse_color("rgb(256, 0, 0)").is_err());
        assert!(parse_color("rgb(1, 2)").is_err());
        assert!(parse_color("rgb(1, 2, 3, 4)").is_err());
    }

    #[test]
    fn parse_none_and_empty_yield_no_color() {
        assert_eq!(parse_color(""), Ok(Color::NoColor));
        assert_eq!(parse_color("  "), Ok(Color::NoColor));
        assert_eq!(parse_color("none"), Ok(Color::NoColor));
        assert_eq!(parse_color("NONE"), Ok(Color::NoColor));
    }

    #[test]
    fn parse_unknown_named_color_errors() {
        assert!(parse_color("mauve").is_err());
    }

    // --- theme_file_to_theme ---

    const MIN_THEME_TOML: &str = r##"
        name = "mytheme"

        [roles]
        foreground  = "#ffffff"
        background  = "#000000"
        muted       = "#888888"
        primary     = "#ff00ff"
        accent      = "#00ffff"
        success     = "#00ff00"
        warning     = "#ffff00"
        error       = "#ff0000"
        info        = "#0080ff"
    "##;

    #[test]
    fn theme_file_parses_minimum_required_sections() {
        let file: ThemeFile = toml::from_str(MIN_THEME_TOML).expect("parse");
        let theme = theme_file_to_theme(file).expect("convert");
        assert_eq!(theme.name(), "mytheme");
        assert_eq!(
            theme.color(Role::Primary),
            Color::TrueColor {
                r: 255,
                g: 0,
                b: 255,
            }
        );
    }

    #[test]
    fn theme_file_missing_required_role_fails_to_deserialize() {
        // Drop `info` from the minimum; TOML deserialization rejects.
        let src = MIN_THEME_TOML.replace("info        = \"#0080ff\"\n", "");
        let err = toml::from_str::<ThemeFile>(&src).unwrap_err();
        assert!(err.to_string().contains("info"));
    }

    #[test]
    fn theme_file_invalid_color_value_surfaces_role_label() {
        let src = MIN_THEME_TOML.replace("#ff00ff", "not-a-color");
        let file: ThemeFile = toml::from_str(&src).expect("still parses as TOML");
        let err = theme_file_to_theme(file).unwrap_err();
        match err {
            ThemeParseError::InvalidColor { role, value } => {
                assert_eq!(role, "primary");
                assert_eq!(value, "not-a-color");
            }
            other => panic!("expected InvalidColor, got {other:?}"),
        }
    }

    #[test]
    fn theme_file_extended_roles_populate_when_present() {
        let src = format!(
            "{MIN_THEME_TOML}\n[roles.extended]\nsurface = \"#202020\"\nborder = \"#303030\"\n"
        );
        let file: ThemeFile = toml::from_str(&src).expect("parse");
        let theme = theme_file_to_theme(file).expect("convert");
        assert_eq!(
            theme.color(Role::Surface),
            Color::TrueColor {
                r: 32,
                g: 32,
                b: 32,
            }
        );
    }

    #[test]
    fn theme_file_timer_role_populates_when_present() {
        let src = format!("{MIN_THEME_TOML}\n[roles.extended]\ntimer = \"#f5c2e7\"\n");
        let file: ThemeFile = toml::from_str(&src).expect("parse");
        let theme = theme_file_to_theme(file).expect("convert");
        assert_eq!(
            theme.color(Role::Timer),
            Color::TrueColor {
                r: 245,
                g: 194,
                b: 231,
            }
        );
    }

    #[test]
    fn theme_file_omitting_timer_falls_back_to_muted() {
        let file: ThemeFile = toml::from_str(MIN_THEME_TOML).expect("parse");
        let theme = theme_file_to_theme(file).expect("convert");
        assert_eq!(theme.color(Role::Timer), theme.color(Role::Muted));
    }

    #[test]
    fn theme_file_timer_invalid_color_surfaces_role_label() {
        let src = format!("{MIN_THEME_TOML}\n[roles.extended]\ntimer = \"#zzz\"\n");
        let file: ThemeFile = toml::from_str(&src).expect("still parses as TOML");
        match theme_file_to_theme(file).unwrap_err() {
            ThemeParseError::InvalidColor { role, value } => {
                assert_eq!(role, "timer");
                assert_eq!(value, "#zzz");
            }
            other => panic!("expected InvalidColor, got {other:?}"),
        }
    }

    #[test]
    fn valid_timer_extended_key_emits_no_warning() {
        // The allowlist (KNOWN_ROLES_EXTENDED) and the struct field must
        // stay in sync; the populate test bypasses validation, so cover
        // the full load path here — a valid `timer` must not warn.
        let dir = tempdir();
        std::fs::write(
            dir.path().join("t.toml"),
            format!(
                "{}\n[roles.extended]\ntimer = \"#f5c2e7\"\n",
                MIN_THEME_TOML.trim()
            ),
        )
        .unwrap();
        let mut warnings = Vec::new();
        let _ = ThemeRegistry::with_built_ins()
            .with_user_themes(dir.path(), |m| warnings.push(m.to_string()));
        assert!(
            !warnings.iter().any(|w| w.contains("timer")),
            "valid timer key should not warn: {warnings:?}",
        );
    }

    // --- ThemeRegistry ---

    #[test]
    fn registry_with_built_ins_contains_every_compiled_theme() {
        let r = ThemeRegistry::with_built_ins();
        for name in super::super::builtin_names() {
            assert!(r.lookup(name).is_some(), "{name} missing from registry");
        }
    }

    #[test]
    fn registry_user_theme_overrides_built_in_with_warning() {
        let dir = tempdir();
        std::fs::write(
            dir.path().join("override.toml"),
            r##"
                name = "default"
                [roles]
                foreground = "#ff0000"
                background = "#000000"
                muted = "#888888"
                primary = "#ff00ff"
                accent = "#00ffff"
                success = "#00ff00"
                warning = "#ffff00"
                error = "#ff0000"
                info = "#0080ff"
            "##,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let r = ThemeRegistry::with_built_ins()
            .with_user_themes(dir.path(), |m| warnings.push(m.to_string()));
        let t = r.lookup("default").expect("default present");
        assert_eq!(
            t.color(Role::Foreground),
            Color::TrueColor { r: 255, g: 0, b: 0 }
        );
        assert!(
            warnings.iter().any(|w| w.contains("overrides")),
            "no override warning: {warnings:?}",
        );
    }

    #[test]
    fn registry_user_vs_user_collision_first_file_wins() {
        // Per docs/specs/theming.md §Edge cases: when two user files
        // declare the same `name`, the first one loaded wins. Files
        // are processed in filename-sorted order, so `a_theme.toml`
        // wins over `b_theme.toml` and the later one is warned + skipped.
        let dir = tempdir();
        std::fs::write(
            dir.path().join("a_theme.toml"),
            r##"
                name = "clash"
                [roles]
                foreground = "#111111"
                background = "#000000"
                muted = "#888888"
                primary = "#ff00ff"
                accent = "#00ffff"
                success = "#00ff00"
                warning = "#ffff00"
                error = "#ff0000"
                info = "#0080ff"
            "##,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("b_theme.toml"),
            r##"
                name = "clash"
                [roles]
                foreground = "#222222"
                background = "#000000"
                muted = "#888888"
                primary = "#ff00ff"
                accent = "#00ffff"
                success = "#00ff00"
                warning = "#ffff00"
                error = "#ff0000"
                info = "#0080ff"
            "##,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let r = ThemeRegistry::with_built_ins()
            .with_user_themes(dir.path(), |m| warnings.push(m.to_string()));
        let t = r.lookup("clash").expect("clash present");
        assert_eq!(
            t.color(Role::Foreground),
            Color::TrueColor {
                r: 0x11,
                g: 0x11,
                b: 0x11,
            },
            "first file (a_theme.toml) should win"
        );
        assert!(
            warnings.iter().any(|w| w.contains("a_theme.toml")
                && w.contains("b_theme.toml")
                && w.contains("skipped")),
            "skip warning missing path or 'skipped' keyword: {warnings:?}",
        );
    }

    #[test]
    fn unknown_role_in_theme_file_warns_with_path() {
        // Spec: "Theme file references a role not in vocabulary ->
        // Parse succeeds; unknown role ignored with warning."
        let dir = tempdir();
        let file = dir.path().join("typo.toml");
        std::fs::write(
            &file,
            r##"
                name = "typo"
                [roles]
                foreground = "#ffffff"
                background = "#000000"
                muted = "#888888"
                primray = "#ff00ff"
                primary = "#cba6f7"
                accent = "#00ffff"
                success = "#00ff00"
                warning = "#ffff00"
                error = "#ff0000"
                info = "#0080ff"
            "##,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let r = ThemeRegistry::with_built_ins()
            .with_user_themes(dir.path(), |m| warnings.push(m.to_string()));
        // Theme still loads (`primary` is present), but the typo is
        // surfaced.
        assert!(r.lookup("typo").is_some(), "theme should still load");
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("primray") && w.contains("[roles]")),
            "typo not warned: {warnings:?}",
        );
    }

    #[test]
    fn unknown_top_level_key_in_theme_file_warns() {
        let dir = tempdir();
        std::fs::write(
            dir.path().join("stray.toml"),
            format!(
                r##"
                    bogus = "value"
                    {}
                "##,
                MIN_THEME_TOML.trim()
            ),
        )
        .unwrap();
        let mut warnings = Vec::new();
        let _ = ThemeRegistry::with_built_ins()
            .with_user_themes(dir.path(), |m| warnings.push(m.to_string()));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("bogus") && w.contains("top-level")),
            "unknown top-level key not warned: {warnings:?}",
        );
    }

    #[test]
    fn unknown_key_in_roles_extended_warns() {
        let dir = tempdir();
        std::fs::write(
            dir.path().join("ext.toml"),
            format!(
                "{}\n[roles.extended]\nsurfaec = \"#202020\"\n",
                MIN_THEME_TOML.trim()
            ),
        )
        .unwrap();
        let mut warnings = Vec::new();
        let _ = ThemeRegistry::with_built_ins()
            .with_user_themes(dir.path(), |m| warnings.push(m.to_string()));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("surfaec") && w.contains("[roles.extended]")),
            "typo in extended not warned: {warnings:?}",
        );
    }

    #[test]
    fn spec_metadata_keys_parse_silently() {
        // `author` and `license` are spec-documented metadata fields.
        // Including them must not produce unknown-key warnings.
        let dir = tempdir();
        std::fs::write(
            dir.path().join("meta.toml"),
            r##"
                name = "metaed"
                author = "Someone <a@b>"
                license = "MIT"
                [roles]
                foreground = "#ffffff"
                background = "#000000"
                muted = "#888888"
                primary = "#ff00ff"
                accent = "#00ffff"
                success = "#00ff00"
                warning = "#ffff00"
                error = "#ff0000"
                info = "#0080ff"
            "##,
        )
        .unwrap();
        let mut warnings = Vec::new();
        let r = ThemeRegistry::with_built_ins()
            .with_user_themes(dir.path(), |m| warnings.push(m.to_string()));
        assert!(r.lookup("metaed").is_some());
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn registry_missing_user_dir_is_silent() {
        let dir = tempdir();
        let missing = dir.path().join("does_not_exist");
        let mut warnings = Vec::new();
        let r = ThemeRegistry::with_built_ins()
            .with_user_themes(&missing, |m| warnings.push(m.to_string()));
        assert!(warnings.is_empty());
        assert!(r.lookup("default").is_some());
    }

    #[test]
    fn registry_skips_bad_files_with_diagnostic() {
        let dir = tempdir();
        std::fs::write(dir.path().join("broken.toml"), "not valid toml [[").unwrap();
        std::fs::write(dir.path().join("good.toml"), MIN_THEME_TOML).unwrap();
        let mut warnings = Vec::new();
        let r = ThemeRegistry::with_built_ins()
            .with_user_themes(dir.path(), |m| warnings.push(m.to_string()));
        assert!(
            r.lookup("mytheme").is_some(),
            "good theme should still load"
        );
        assert!(
            warnings.iter().any(|w| w.contains("broken.toml")),
            "bad file didn't warn: {warnings:?}",
        );
    }

    #[test]
    fn registry_ignores_non_toml_files() {
        let dir = tempdir();
        std::fs::write(dir.path().join("readme.md"), "not a theme").unwrap();
        std::fs::write(dir.path().join("theme.toml"), MIN_THEME_TOML).unwrap();
        let mut warnings = Vec::new();
        let r = ThemeRegistry::with_built_ins()
            .with_user_themes(dir.path(), |m| warnings.push(m.to_string()));
        assert!(r.lookup("mytheme").is_some());
        assert!(warnings.is_empty());
    }

    #[test]
    fn registry_iter_yields_built_ins_first_then_user() {
        let dir = tempdir();
        std::fs::write(dir.path().join("mine.toml"), MIN_THEME_TOML).unwrap();
        let r = ThemeRegistry::with_built_ins().with_user_themes(dir.path(), |_| {});
        let names: Vec<&str> = r.iter().map(|rt| rt.theme.name()).collect();
        // Built-ins come before the user theme.
        let user_idx = names
            .iter()
            .position(|n| *n == "mytheme")
            .expect("user theme");
        let default_idx = names.iter().position(|n| *n == "default").expect("default");
        assert!(default_idx < user_idx);
    }

    #[test]
    fn user_theme_renders_through_downgrade_without_panicking() {
        // Catches a regression in downgrade arithmetic for arbitrary
        // user-supplied TrueColor values (above + beyond the built-in
        // Catppuccin coverage).
        let file: ThemeFile = toml::from_str(MIN_THEME_TOML).unwrap();
        let theme = theme_file_to_theme(file).unwrap();
        for role in [Role::Primary, Role::Success, Role::Warning] {
            let _ = theme.color(role).downgrade(Capability::Palette256);
            let _ = theme.color(role).downgrade(Capability::Palette16);
        }
        // Reference the built_in registry to keep the import live.
        assert!(built_in("default").is_some());
    }

    // --- tempdir helper (separate from driver/config helpers) ---

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "linesmith-user-theme-test-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&base).expect("mkdir");
        TempDir(base)
    }
}
