//! User config: parse `config.toml`, resolve its path, and apply
//! per-segment overrides. Full contract in `docs/specs/config.md`.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Parsed `config.toml`. Serde ignores unknown keys so a file from a
/// newer linesmith still parses on an older binary; fields this
/// version doesn't know are dropped rather than rejected.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub line: Option<LineConfig>,
    pub theme: Option<String>,
    pub layout_options: Option<LayoutOptions>,
    #[serde(default)]
    pub segments: BTreeMap<String, SegmentOverride>,
    /// Extra directories to scan for user plugin scripts (`.rhai`
    /// files). Scanned in list order before the default XDG
    /// directory. See `docs/specs/config.md` §Plugin directories and
    /// `docs/specs/plugin-api.md` §Plugin file location.
    #[serde(default)]
    pub plugin_dirs: Vec<PathBuf>,
}

/// `[layout_options]` section: render-path tunables that aren't tied
/// to a specific segment. See `docs/specs/config.md` §layout_options.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct LayoutOptions {
    pub color: ColorPolicy,
    pub claude_padding: u16,
}

/// Config-level color override. `auto` honors CLI flags and env vars;
/// `always` forces color even in non-TTY output; `never` strips all
/// color. Sits below CLI flags and env vars in the precedence chain.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ColorPolicy {
    #[default]
    Auto,
    Always,
    Never,
}

/// `[line]` section: ordered list of segment ids to render.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LineConfig {
    pub segments: Vec<String>,
}

/// `[segments.<id>]` override block. Each typed field, when `Some`,
/// replaces the segment's built-in default. Any unrecognized keys land
/// in [`extra`](Self::extra), which the segment builder forwards to
/// plugin scripts as `ctx.config.<key>`. `style` is stored as a raw
/// string; `segments::builder::apply_override` parses it at build time
/// so parse errors emit warnings through the same callback that
/// handles unknown-ID and inverted-bounds diagnostics.
///
/// `Eq` isn't derived because [`toml::Value`] holds `f64` and so is
/// `PartialEq` only — `extra` propagates that constraint.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct SegmentOverride {
    pub priority: Option<u8>,
    pub width: Option<WidthBoundsConfig>,
    pub style: Option<String>,
    /// Plugin-config bag: every TOML key under `[segments.<plugin-id>]`
    /// not matched by a typed field. Surfaced to the rhai script as
    /// `ctx.config.<key>` per `docs/specs/plugin-api.md` §ctx shape.
    /// Built-in segments ignore this; the unknown-key validator still
    /// warns when a built-in's table contains keys outside its schema.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// Width-bounds override. Either side may be omitted; a missing side
/// inherits from the segment's built-in default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct WidthBoundsConfig {
    pub min: Option<u16>,
    pub max: Option<u16>,
}

/// Failure modes when loading a config. A missing file is not an error;
/// callers treat it as "use defaults."
#[derive(Debug)]
#[non_exhaustive]
pub enum ConfigError {
    /// `fs::read` failed for a reason other than `NotFound`. Carries
    /// the offending path so the stderr diagnostic is self-describing.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Invalid TOML. `path` is `None` for in-memory parses.
    Parse {
        path: Option<PathBuf>,
        source: toml::de::Error,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "config I/O at {}: {source}", path.display()),
            Self::Parse {
                path: Some(p),
                source,
            } => write!(f, "config parse at {}: {source}", p.display()),
            Self::Parse { path: None, source } => write!(f, "config parse: {source}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
        }
    }
}

impl FromStr for Config {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        toml::from_str(s).map_err(|source| ConfigError::Parse { path: None, source })
    }
}

impl Config {
    /// Read and parse the file at `path`. Returns `Ok(None)` when the
    /// file doesn't exist (normal case for first-run users); other I/O
    /// errors propagate so callers can log them. Unknown keys are
    /// silently ignored — callers that want typo warnings use
    /// [`Config::load_validated`] instead.
    pub fn load(path: &Path) -> Result<Option<Self>, ConfigError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_owned(),
                    source,
                })
            }
        };
        toml::from_str(&raw)
            .map(Some)
            .map_err(|source| ConfigError::Parse {
                path: Some(path.to_owned()),
                source,
            })
    }

    /// Same as [`Config::load`] but emits one warning per unknown key
    /// encountered (top-level, `[layout_options]`, or `[segments.<id>]`).
    /// The allow-list tolerates spec-documented keys we haven't
    /// implemented yet (`preset`, `layout`, `plugins`, `$schema`), so
    /// forward-compat configs stay silent while typos surface.
    pub fn load_validated(
        path: &Path,
        warn: impl FnMut(&str),
    ) -> Result<Option<Self>, ConfigError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_owned(),
                    source,
                })
            }
        };
        Self::from_str_validated_impl(&raw, Some(path), warn).map(Some)
    }

    /// [`FromStr`]-equivalent with unknown-key warnings. The plain
    /// `FromStr` impl remains the non-validating form; validation is
    /// opt-in so callers that don't want the allow-list surface (unit
    /// tests, programmatic config construction) bypass it.
    pub fn from_str_validated(s: &str, warn: impl FnMut(&str)) -> Result<Self, ConfigError> {
        Self::from_str_validated_impl(s, None, warn)
    }

    fn from_str_validated_impl(
        s: &str,
        path: Option<&Path>,
        mut warn: impl FnMut(&str),
    ) -> Result<Self, ConfigError> {
        let raw: toml::Value = toml::from_str(s).map_err(|source| ConfigError::Parse {
            path: path.map(Path::to_owned),
            source,
        })?;
        validate_keys(&raw, &mut warn);
        raw.try_into()
            .map_err(|source: toml::de::Error| ConfigError::Parse {
                path: path.map(Path::to_owned),
                source,
            })
    }
}

/// Top-level config keys we recognize. Implemented keys + spec-documented
/// keys for features not yet shipped (`preset`, `layout`, `plugins`) +
/// `$schema` for editor tooling.
const KNOWN_TOP_LEVEL: &[&str] = &[
    "line",
    "theme",
    "layout_options",
    "segments",
    "plugin_dirs",
    "preset",
    "layout",
    "plugins",
    "$schema",
];

/// Fields under `[layout_options]`. `separator` is tolerated ahead
/// of its implementation so forward-compat configs don't warn.
const KNOWN_LAYOUT_OPTIONS: &[&str] = &["color", "claude_padding", "separator"];

/// Per-segment override schema. Returns `None` for segment ids we
/// don't recognize so plugin segments (which own their own schema)
/// bypass validation. Most built-ins share the universal allow-list;
/// rate-limit segments extend it with per-family knobs that
/// `segments::rate_limit_format` reads from the TOML extras bag.
fn segment_override_schema(id: &str) -> Option<&'static [&'static str]> {
    const BUILT_IN_COMMON: &[&str] = &["priority", "width", "style", "visible_if"];
    const RATE_LIMIT_COMMON: &[&str] = &[
        "priority",
        "width",
        "style",
        "visible_if",
        "icon",
        "label",
        "stale_marker",
        "progress_width",
        "format",
    ];
    const PERCENT_SEGMENT: &[&str] = &[
        "priority",
        "width",
        "style",
        "visible_if",
        "icon",
        "label",
        "stale_marker",
        "progress_width",
        "format",
        "invert",
    ];
    const RESET_SEGMENT: &[&str] = &[
        "priority",
        "width",
        "style",
        "visible_if",
        "icon",
        "label",
        "stale_marker",
        "progress_width",
        "format",
        "compact",
        "use_days",
    ];
    // Nested tables like `dirty` are validated shallowly per
    // `validate_segments_table` — their inner keys pass through
    // without warning. Inner schemas live in the segment's
    // `from_extras` validator.
    const GIT_BRANCH_SEGMENT: &[&str] = &[
        "priority",
        "width",
        "style",
        "visible_if",
        "icon",
        "label",
        "max_length",
        "truncation_marker",
        "short_sha_length",
        "dirty",
    ];
    match id {
        "model" | "workspace" | "cost" | "effort" | "context_window" => Some(BUILT_IN_COMMON),
        "rate_limit_5h" | "rate_limit_7d" => Some(PERCENT_SEGMENT),
        "rate_limit_5h_reset" | "rate_limit_7d_reset" => Some(RESET_SEGMENT),
        "extra_usage" => Some(RATE_LIMIT_COMMON),
        "git_branch" => Some(GIT_BRANCH_SEGMENT),
        _ => None,
    }
}

/// Walk `raw` and emit one warning per key outside the allow-list.
/// Scope is intentionally shallow: top-level, `[layout_options]`
/// fields, and fields directly under each `[segments.<id>]` table.
/// Deeper nesting (plugin configs, per-line segments) stays silent
/// until those features land with their own schemas.
fn validate_keys(raw: &toml::Value, warn: &mut impl FnMut(&str)) {
    let Some(top) = raw.as_table() else {
        return;
    };
    for (key, value) in top {
        if !KNOWN_TOP_LEVEL.contains(&key.as_str()) {
            warn(&format!("unknown top-level config key '{key}'; ignoring"));
            continue;
        }
        match key.as_str() {
            "layout_options" => {
                validate_flat_table(value, "layout_options", KNOWN_LAYOUT_OPTIONS, warn)
            }
            "segments" => validate_segments_table(value, warn),
            _ => {}
        }
    }
}

fn validate_flat_table(
    value: &toml::Value,
    label: &str,
    allowed: &[&str],
    warn: &mut impl FnMut(&str),
) {
    let Some(table) = value.as_table() else {
        return;
    };
    for key in table.keys() {
        if !allowed.contains(&key.as_str()) {
            warn(&format!("unknown key '{key}' in [{label}]; ignoring"));
        }
    }
}

fn validate_segments_table(value: &toml::Value, warn: &mut impl FnMut(&str)) {
    let Some(segments) = value.as_table() else {
        return;
    };
    for (id, block) in segments {
        let Some(block_table) = block.as_table() else {
            continue;
        };
        let Some(allowed) = segment_override_schema(id) else {
            // Plugin or not-yet-shipped segment id; skip so plugin
            // config keys pass through. Plugins own their schema via
            // the plugin API when that lands.
            continue;
        };
        for key in block_table.keys() {
            if !allowed.contains(&key.as_str()) {
                warn(&format!("unknown key '{key}' in [segments.{id}]; ignoring"));
            }
        }
    }
}

/// Where linesmith found its config path and how. `explicit = true`
/// means the user named a path directly (`--config` or
/// `LINESMITH_CONFIG`); the run-time diagnostics use this to decide
/// whether a missing file is worth warning about (explicit paths
/// warn, implicit XDG fallbacks stay silent for first-run users).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPath {
    pub path: PathBuf,
    pub explicit: bool,
}

/// Where linesmith looks for its config file, in precedence order.
#[must_use]
pub fn resolve_config_path(
    cli_override: Option<PathBuf>,
    env_override: Option<&str>,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Option<ConfigPath> {
    if let Some(p) = cli_override.filter(|p| !p.as_os_str().is_empty()) {
        return Some(ConfigPath {
            path: p,
            explicit: true,
        });
    }
    if let Some(p) = env_override.filter(|s| !s.is_empty()) {
        return Some(ConfigPath {
            path: PathBuf::from(p),
            explicit: true,
        });
    }
    if let Some(p) = xdg_config_home.filter(|s| !s.is_empty()) {
        return Some(ConfigPath {
            path: PathBuf::from(p).join("linesmith").join("config.toml"),
            explicit: false,
        });
    }
    home.filter(|s| !s.is_empty()).map(|h| ConfigPath {
        path: PathBuf::from(h).join(".config/linesmith/config.toml"),
        explicit: false,
    })
}

/// Thin wrapper around [`resolve_config_path`] that reads the process
/// env directly. Used at startup.
#[must_use]
pub fn detect_config_path(cli_override: Option<PathBuf>) -> Option<ConfigPath> {
    let env_override = std::env::var("LINESMITH_CONFIG").ok();
    let xdg_config_home = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").ok();
    resolve_config_path(
        cli_override,
        env_override.as_deref(),
        xdg_config_home.as_deref(),
        home.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse ---

    #[test]
    fn empty_config_parses() {
        let c = Config::from_str("").expect("parse ok");
        assert_eq!(c.line, None);
        assert!(c.segments.is_empty());
    }

    #[test]
    fn line_segments_parse_in_order() {
        let c = Config::from_str(
            r#"
                [line]
                segments = ["model", "workspace", "cost"]
            "#,
        )
        .expect("parse ok");
        let line = c.line.expect("line present");
        assert_eq!(line.segments, vec!["model", "workspace", "cost"]);
    }

    #[test]
    fn segment_override_priority_parses() {
        let c = Config::from_str(
            r#"
                [segments.model]
                priority = 16
            "#,
        )
        .expect("parse ok");
        assert_eq!(c.segments["model"].priority, Some(16));
        assert_eq!(c.segments["model"].width, None);
    }

    #[test]
    fn layout_options_color_and_padding_parse() {
        let c = Config::from_str(
            r#"
                [layout_options]
                color = "always"
                claude_padding = 3
            "#,
        )
        .expect("parse ok");
        let lo = c.layout_options.expect("layout_options present");
        assert_eq!(lo.color, ColorPolicy::Always);
        assert_eq!(lo.claude_padding, 3);
    }

    #[test]
    fn layout_options_color_accepts_all_three_variants() {
        for (toml_val, expected) in [
            ("auto", ColorPolicy::Auto),
            ("always", ColorPolicy::Always),
            ("never", ColorPolicy::Never),
        ] {
            let src = format!("[layout_options]\ncolor = \"{toml_val}\"\n");
            let c = Config::from_str(&src).expect("parse ok");
            assert_eq!(c.layout_options.map(|l| l.color), Some(expected));
        }
    }

    // --- unknown-key validation ---

    fn collect_warnings(src: &str) -> Vec<String> {
        let mut warnings = Vec::new();
        let _ = Config::from_str_validated(src, |msg| warnings.push(msg.to_string()));
        warnings
    }

    #[test]
    fn plugin_dirs_deserializes_from_toml_as_path_list() {
        // Lock in the serde contract: `plugin_dirs = [...]` → Vec<PathBuf>
        // with each entry preserved as written. This is the public
        // entry point from user config into plugin discovery; a
        // renamed field or lost `#[serde(default)]` would silently
        // stop discovery from seeing user-declared dirs.
        let cfg: Config = Config::from_str(
            r#"
                plugin_dirs = ["/etc/linesmith/segments", "./vendor/plugins"]
                [line]
                segments = ["model"]
            "#,
        )
        .expect("parse");
        assert_eq!(
            cfg.plugin_dirs,
            vec![
                PathBuf::from("/etc/linesmith/segments"),
                PathBuf::from("./vendor/plugins"),
            ]
        );
    }

    #[test]
    fn plugin_dirs_defaults_to_empty_when_absent() {
        let cfg: Config = Config::from_str("theme = \"default\"\n").expect("parse");
        assert!(cfg.plugin_dirs.is_empty());
    }

    #[test]
    fn from_str_validated_warns_on_unknown_top_level_key() {
        let warnings = collect_warnings("thme = \"oops\"\n[line]\nsegments = []\n");
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("thme"));
        assert!(warnings[0].contains("top-level"));
    }

    #[test]
    fn from_str_validated_allows_implemented_and_forward_compat_top_level_keys() {
        // `theme` / `line` / `layout_options` / `segments` are
        // implemented; `preset` / `layout` / `plugins` / `$schema`
        // are tolerated per the allow-list until they land.
        let warnings = collect_warnings(
            r#"
                $schema = "https://example.invalid/schema.json"
                theme = "default"
                preset = "developer"
                layout = "single-line"
                [line]
                segments = ["model"]
                [layout_options]
                color = "auto"
                [plugins.example]
                foo = "bar"
            "#,
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn from_str_validated_warns_on_unknown_layout_options_key() {
        let warnings = collect_warnings(
            r#"
                [layout_options]
                separatr = "powerline"
            "#,
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("separatr"));
        assert!(warnings[0].contains("[layout_options]"));
    }

    #[test]
    fn from_str_validated_allows_separator_and_other_known_layout_options_keys() {
        // `separator` is spec'd but not yet implemented; the allow-list
        // tolerates it so forward-compat configs stay silent.
        let warnings = collect_warnings(
            r#"
                [layout_options]
                color = "always"
                claude_padding = 2
                separator = "powerline"
            "#,
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn from_str_validated_warns_on_unknown_segment_override_key() {
        let warnings = collect_warnings(
            r#"
                [segments.model]
                priorty = 16
            "#,
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("priorty"));
        assert!(warnings[0].contains("[segments.model]"));
    }

    #[test]
    fn from_str_validated_names_the_segment_id_in_warnings() {
        // Each segment block gets its own warnings namespaced by id so
        // users with many segments can find which one has the typo.
        let warnings = collect_warnings(
            r#"
                [segments.workspace]
                bogus = "x"
                [segments.cost]
                alsobogus = 1
            "#,
        );
        assert_eq!(warnings.len(), 2);
        assert!(warnings
            .iter()
            .any(|w| w.contains("[segments.workspace]") && w.contains("bogus")));
        assert!(warnings
            .iter()
            .any(|w| w.contains("[segments.cost]") && w.contains("alsobogus")));
    }

    #[test]
    fn from_str_validated_skips_unknown_segment_ids_because_plugins_own_their_schema() {
        // A segment id not in the built-in registry is either a future
        // built-in or a plugin segment; plugins declare their own
        // override keys, so we can't know what's valid. Skip rather
        // than emit false positives.
        let warnings = collect_warnings(
            r#"
                [segments.my_plugin]
                foo = "bar"
                baz = 42

                [segments.another_plugin]
                show_ahead_behind = true
                show_dirty = true
            "#,
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn from_str_validated_rejects_segment_specific_keys_on_wrong_built_in() {
        // `show_dirty` is a git_branch concept; putting it on `model`
        // is a user mistake the validator should catch.
        let warnings = collect_warnings(
            r#"
                [segments.model]
                show_dirty = true
            "#,
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("show_dirty"));
        assert!(warnings[0].contains("[segments.model]"));
    }

    #[test]
    fn from_str_validated_allows_spec_documented_segment_override_keys() {
        // `style` (style-string syntax) and `visible_if` (rhai plugin
        // expressions) are spec'd but not yet implemented; tolerated
        // so spec example configs parse cleanly.
        let warnings = collect_warnings(
            r#"
                [segments.workspace]
                priority = 16
                width = { min = 10, max = 40 }
                style = "role:info"
                visible_if = "true"
            "#,
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn rate_limit_percent_segments_allow_format_and_invert_without_warning() {
        let warnings = collect_warnings(
            r#"
                [segments.rate_limit_5h]
                format = "progress"
                invert = true
                icon = "⏱"
                label = "5h"
                stale_marker = "~"
                progress_width = 20

                [segments.rate_limit_7d]
                format = "percent"
                invert = false
            "#,
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn rate_limit_reset_segments_allow_compact_and_use_days_without_warning() {
        let warnings = collect_warnings(
            r#"
                [segments.rate_limit_5h_reset]
                format = "duration"
                compact = true
                use_days = false

                [segments.rate_limit_7d_reset]
                format = "progress"
                use_days = true
            "#,
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn extra_usage_allows_currency_and_percent_format_without_warning() {
        let warnings = collect_warnings(
            r#"
                [segments.extra_usage]
                format = "currency"
                icon = ""
                label = "extra"
                stale_marker = "~"
            "#,
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn invert_warns_on_reset_segment_schema() {
        // `invert` is percent-family only; allow-list for reset
        // segments must reject it.
        let warnings = collect_warnings(
            r#"
                [segments.rate_limit_5h_reset]
                invert = true
            "#,
        );
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("invert") && warnings[0].contains("rate_limit_5h_reset"),
            "{:?}",
            warnings[0]
        );
    }

    #[test]
    fn use_days_warns_on_percent_segment_schema() {
        // `use_days` is reset-family only; allow-list for percent
        // segments must reject it.
        let warnings = collect_warnings(
            r#"
                [segments.rate_limit_5h]
                use_days = true
            "#,
        );
        assert_eq!(warnings.len(), 1);
        assert!(
            warnings[0].contains("use_days") && warnings[0].contains("rate_limit_5h"),
            "{:?}",
            warnings[0]
        );
    }

    #[test]
    fn from_str_validated_returns_parse_error_for_malformed_toml() {
        let mut warnings = Vec::new();
        let err =
            Config::from_str_validated("[line\nsegments =", |msg| warnings.push(msg.to_string()))
                .unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn validated_and_silent_parse_yield_identical_config_on_clean_input() {
        // Locks the "validation is purely observational" contract:
        // from_str_validated must not mutate parse semantics.
        let src = r#"
            theme = "default"
            [line]
            segments = ["model", "workspace"]
            [segments.model]
            priority = 8
        "#;
        let silent = Config::from_str(src).expect("silent parse");
        let validated = Config::from_str_validated(src, |_| {}).expect("validated parse");
        assert_eq!(silent, validated);
    }

    #[test]
    fn load_validated_file_path_surfaces_parse_error_with_path() {
        // The in-memory variant returns ConfigError::Parse { path: None };
        // the file variant must populate path for user-facing diagnostics.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[line\nsegments =").unwrap();
        let err = Config::load_validated(&path, |_| {}).unwrap_err();
        match err {
            ConfigError::Parse { path: Some(p), .. } => assert_eq!(p, path),
            other => panic!("expected Parse with Some(path), got {other:?}"),
        }
    }

    #[test]
    fn load_validated_returns_none_for_missing_file() {
        let dir = tempdir();
        let path = dir.path().join("missing.toml");
        let mut warnings = Vec::new();
        let got = Config::load_validated(&path, |m| warnings.push(m.to_string())).expect("ok");
        assert!(got.is_none());
        assert!(warnings.is_empty());
    }

    #[test]
    fn load_validated_surfaces_unknown_key_warnings() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "thme = \"bad\"\n").unwrap();
        let mut warnings = Vec::new();
        let _ = Config::load_validated(&path, |m| warnings.push(m.to_string())).unwrap();
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("thme"));
    }

    #[test]
    fn layout_options_defaults_populate_missing_keys() {
        // `[layout_options]` with no fields inside still parses; missing
        // color defaults to Auto, missing claude_padding defaults to 0.
        let c = Config::from_str("[layout_options]\n").expect("parse ok");
        let lo = c.layout_options.expect("layout_options present");
        assert_eq!(lo.color, ColorPolicy::Auto);
        assert_eq!(lo.claude_padding, 0);
    }

    #[test]
    fn layout_options_rejects_unknown_color_variant() {
        let err = Config::from_str(
            r#"
                [layout_options]
                color = "bogus"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn layout_options_omitted_entirely_is_ok() {
        let c = Config::from_str("[line]\nsegments = [\"model\"]\n").expect("parse ok");
        assert!(c.layout_options.is_none());
    }

    #[test]
    fn segment_override_width_parses_both_sides() {
        let c = Config::from_str(
            r#"
                [segments.workspace.width]
                min = 10
                max = 40
            "#,
        )
        .expect("parse ok");
        let w = c.segments["workspace"].width.expect("width present");
        assert_eq!(w.min, Some(10));
        assert_eq!(w.max, Some(40));
    }

    #[test]
    fn unknown_top_level_key_is_forward_compatible() {
        // Config files from a newer linesmith must still parse on an
        // older binary; fields this version doesn't implement are
        // ignored rather than rejected.
        let c = Config::from_str(
            r#"
                theme = "catppuccin-mocha"
                layout = "single-line"
                [layout_options]
                separator = "powerline"
            "#,
        )
        .expect("parse ok");
        assert_eq!(c.line, None);
        assert!(c.segments.is_empty());
    }

    #[test]
    fn malformed_toml_reports_parse_error() {
        let err = Config::from_str("[line").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn io_error_carries_path_in_display() {
        use std::io::ErrorKind;
        let err = ConfigError::Io {
            path: PathBuf::from("/etc/linesmith/config.toml"),
            source: std::io::Error::new(ErrorKind::PermissionDenied, "denied"),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("/etc/linesmith/config.toml"));
        assert!(rendered.contains("denied"));
    }

    #[test]
    fn bom_prefixed_config_parses() {
        // Windows editors sometimes save configs with a leading UTF-8
        // BOM. The `toml` crate tolerates it, so no explicit strip is
        // needed; this test locks that behavior.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "\u{FEFF}[line]\nsegments = [\"model\"]\n").unwrap();
        let c = Config::load(&path).expect("ok").expect("present");
        assert_eq!(c.line.expect("line").segments, vec!["model".to_string()]);
    }

    #[test]
    fn load_returns_none_for_missing_file() {
        let dir = tempdir();
        let path = dir.path().join("nonexistent.toml");
        assert!(Config::load(&path).unwrap().is_none());
    }

    // --- path resolution ---

    fn resolved(
        cli: Option<&str>,
        env: Option<&str>,
        xdg: Option<&str>,
        home: Option<&str>,
    ) -> Option<ConfigPath> {
        resolve_config_path(cli.map(PathBuf::from), env, xdg, home)
    }

    #[test]
    fn cli_override_wins_over_everything_and_is_explicit() {
        let got = resolved(
            Some("/explicit.toml"),
            Some("/env.toml"),
            Some("/xdg"),
            Some("/home"),
        )
        .expect("resolved");
        assert_eq!(got.path, PathBuf::from("/explicit.toml"));
        assert!(got.explicit);
    }

    #[test]
    fn env_wins_over_xdg_and_home_and_is_explicit() {
        let got = resolved(None, Some("/env.toml"), Some("/xdg"), Some("/home")).expect("resolved");
        assert_eq!(got.path, PathBuf::from("/env.toml"));
        assert!(got.explicit);
    }

    #[test]
    fn xdg_config_home_is_implicit() {
        let got = resolved(None, None, Some("/xdg"), Some("/home")).expect("resolved");
        assert_eq!(got.path, PathBuf::from("/xdg/linesmith/config.toml"));
        assert!(!got.explicit);
    }

    #[test]
    fn home_fallback_is_implicit() {
        let got = resolved(None, None, None, Some("/home")).expect("resolved");
        assert_eq!(
            got.path,
            PathBuf::from("/home/.config/linesmith/config.toml")
        );
        assert!(!got.explicit);
    }

    #[test]
    fn returns_none_when_no_home_and_no_xdg() {
        assert_eq!(resolved(None, None, None, None), None);
    }

    #[test]
    fn empty_env_values_are_ignored() {
        let got = resolved(None, Some(""), Some(""), Some("/home")).expect("resolved");
        assert_eq!(
            got.path,
            PathBuf::from("/home/.config/linesmith/config.toml")
        );
    }

    #[test]
    fn empty_cli_override_does_not_count_as_explicit() {
        // A shell expansion like `--config "$MISSING_VAR"` can produce
        // an empty path; skip past it rather than silently treating it
        // as "load ''" which would NotFound-swallow.
        let got = resolved(Some(""), None, Some("/xdg"), None).expect("resolved");
        assert_eq!(got.path, PathBuf::from("/xdg/linesmith/config.toml"));
        assert!(!got.explicit);
    }

    // --- helpers ---

    struct TempDir(PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
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
            "linesmith-config-test-{}-{}",
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
