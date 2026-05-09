//! User config: parse `config.toml`, resolve its path, and apply
//! per-segment overrides. Full contract in `docs/specs/config.md`.

use schemars::JsonSchema;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Parsed `config.toml`. Serde ignores unknown keys so a file from a
/// newer linesmith still parses on an older binary; fields this
/// version doesn't know are dropped rather than rejected. The
/// schema-side `additionalProperties: false` tightens editor
/// validation so typos like `thme = "default"` get flagged at the
/// authoring layer; the runtime stays permissive (the unknown-key
/// warning channel lives in `from_str_validated`).
#[derive(Debug, Default, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
#[schemars(extend("additionalProperties" = false))]
pub struct Config {
    pub line: Option<LineConfig>,
    pub theme: Option<String>,
    /// Top-level layout mode. Defaults to [`LayoutMode::SingleLine`]
    /// when the field is omitted, preserving pre-multi-line config
    /// behavior. [`LayoutMode::MultiLine`] triggers per-`[line.N]`
    /// rendering.
    pub layout: LayoutMode,
    pub layout_options: Option<LayoutOptions>,
    #[serde(default)]
    pub segments: BTreeMap<String, SegmentOverride>,
    /// Extra directories to scan for user plugin scripts (`.rhai`
    /// files). Scanned in list order before the default XDG
    /// directory. See `docs/specs/config.md` §Plugin directories and
    /// `docs/specs/plugin-api.md` §Plugin file location.
    #[serde(default)]
    pub plugin_dirs: Vec<PathBuf>,
    /// Spec-listed forward-compat key. Parsed and runtime-ignored;
    /// surfacing it in the schema so editor tooling doesn't flag
    /// user configs that include it. Allow-listed in `KNOWN_TOP_LEVEL`
    /// to suppress the unknown-key warning.
    pub preset: Option<String>,
    /// Forward-compat `[plugins.*]` table. Typed as a string-keyed
    /// map so a non-table value (`plugins = "oops"`) fails parse at
    /// load-time instead of silently dropping; per-plugin sub-table
    /// shape is open until the plugin-config spec lands. Schema
    /// mirror remaps `toml::Value` to `serde_json::Value` for the
    /// same reason as `extra` / `numbered`: `toml::Value` has no
    /// `JsonSchema` impl.
    #[serde(default)]
    #[schemars(with = "Option<BTreeMap<String, serde_json::Value>>")]
    pub plugins: Option<BTreeMap<String, toml::Value>>,
    /// Editor-tooling `$schema` directive. Some users put it as a
    /// top-level TOML key instead of (or alongside) the `#:schema`
    /// comment directive `linesmith init` writes. Must be quoted in
    /// TOML (`"$schema" = "..."`) — `$` is not legal in bare keys.
    /// Parsed and ignored at runtime; surfaced here so the schema
    /// validates configs using the alternate form.
    #[serde(default, rename = "$schema")]
    pub schema_url: Option<String>,
}

/// `[layout_options]` section: render-path tunables that aren't tied
/// to a specific segment. See `docs/specs/config.md` §layout_options.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(default)]
#[non_exhaustive]
#[schemars(extend("additionalProperties" = false))]
pub struct LayoutOptions {
    pub color: ColorPolicy,
    pub claude_padding: u16,
    /// Inter-segment separator. Stored as a raw string; the segment
    /// builder parses it into a [`crate::segments::Separator`] at
    /// build time so unknown values warn and fall back to `space`
    /// rather than failing the whole config load. See
    /// `docs/specs/config.md` for the reserved-keyword set
    /// (`space`, `powerline`, `capsule`, `flex`, `""`) and the
    /// arbitrary-literal fallback.
    pub separator: Option<String>,
    /// Cell-count for the Nerd Font powerline chevron (U+E0B0). Only
    /// `1` (the default; matches modern Nerd Fonts at standard sizes)
    /// and `2` (some older builds / larger sizes) are meaningful.
    /// Takes effect only when a powerline separator is in use; setting
    /// it under `separator = "space"` is harmless but inert.
    pub powerline_width: Option<u16>,
}

/// Config-level color override. `auto` honors CLI flags and env vars;
/// `always` forces color even in non-TTY output; `never` strips all
/// color. Sits below CLI flags and env vars in the precedence chain.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum ColorPolicy {
    #[default]
    Auto,
    Always,
    Never,
}

/// `[line]` section: ordered list of segment ids to render in
/// single-line mode, plus any numbered child tables (`[line.1]`,
/// `[line.2]`, ...) for multi-line mode. The `flatten`-captured
/// [`numbered`](Self::numbered) map carries every other key as a
/// raw [`toml::Value`]. Key validation (positive integer pointing
/// at a table with a `segments` array) and ordering happen in the
/// segment builder, which keeps the spec's "unknown keys are
/// warnings, not errors" forward-compat contract: a typo like
/// `[line] segmnts = [...]` parses as a `toml::Value::Array`,
/// reaches the builder, and emits a warning rather than failing the
/// config load. Per spec `docs/specs/config.md` §Multi-line layouts.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LineConfig {
    /// Custom deserializer routes each entry through `try_into` per-
    /// item so a single malformed inline-table (e.g.
    /// `{ type = 42 }`) doesn't abort the whole `Config::from_str`
    /// — it surfaces as a kindless [`LineEntry::Item`] that the
    /// builder warns and drops. Mirrors the per-item warn-and-drop
    /// behavior the numbered-line path already had via
    /// [`crate::segments::builder`]'s `extract_line_segments`. Without
    /// this, single-line configs with one bad boundary override
    /// fail to load entirely while multi-line configs degrade
    /// gracefully — an asymmetry users hit when porting between
    /// layouts.
    #[serde(deserialize_with = "deserialize_line_entries")]
    pub segments: Vec<LineEntry>,
    /// Anything under `[line]` other than `segments`. Holds
    /// `[line.N]` table values plus any forward-compat scalar keys
    /// future versions may add. The builder routes table values
    /// with positive-integer keys to multi-line rendering and warns
    /// on the rest.
    ///
    /// Schema bypass: `toml::Value` has no `JsonSchema` impl, so
    /// remap to `serde_json::Value`'s open-ended schema (any JSON
    /// type) for the `additionalProperties` fallthrough.
    #[serde(flatten)]
    #[schemars(with = "serde_json::Value")]
    pub numbered: BTreeMap<String, toml::Value>,
}

/// One entry in `[line].segments`. Per ADR-0024, the array is a
/// mixed shape: bare strings (`"model"`) round-trip as
/// [`LineEntry::Id`] for backward compatibility with the v0.x string-
/// only schema; inline tables (`{ type = "separator", character = " | " }`)
/// round-trip as [`LineEntry::Item`] and carry per-boundary settings.
///
/// Untagged because the strict-tagged form would reject the bare-string
/// shorthand at parse time. Typo'd keys inside an inline table (e.g.
/// `{ tpye = "separator" }`) land in [`LineEntryItem::extra`]
/// rather than failing parse, preserving the spec's "unknown keys
/// warn, never fail" contract. The runtime builder warns when a
/// kindless inline table reaches it; per-key typo diagnostics
/// inside `[line].segments` array entries are not yet surfaced
/// at config-load time (the existing `validate_keys` pass walks
/// only top-level / `[layout_options]` / `[segments.<id>]` shapes).
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum LineEntry {
    /// Bare string: `"model"` is equivalent to `{ type = "model" }`.
    Id(String),
    /// Inline table: `{ type = "...", ... }`. Carries the kind tag
    /// plus optional per-entry knobs (separator glyph, merge flag,
    /// future ccstatusline-parity fields under [`LineEntryItem::extra`]).
    Item(LineEntryItem),
}

/// Inline-table form of [`LineEntry`]. Typed fields cover today's
/// known knobs; everything else lands in [`extra`](Self::extra) so
/// future fields parse without a schema bump.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LineEntryItem {
    /// `"separator"` or a segment id (`"model"`, `"git_branch"`, ...).
    /// When absent, the builder warns and drops the entry.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Separator glyph for `type = "separator"` entries. Ignored
    /// (with warning) on non-separator entries. When `None` on a
    /// separator entry, the builder falls back to
    /// `[layout_options].separator`.
    pub character: Option<String>,
    /// When `true` on a segment entry, the boundary to its right
    /// renders without a separator (suppresses the implicit
    /// interleave AND any explicit [`LineEntry::Item`] separator at
    /// that boundary). Ignored (with warning) on separator entries.
    pub merge: Option<bool>,
    /// Forward-compat bag: keys outside the typed fields land here
    /// per the `toml::Value` flatten pattern. The builder
    /// warn-and-drops unknown keys today; future ADRs may consume.
    ///
    /// Schema bypass: `toml::Value` has no `JsonSchema` impl, so
    /// remap to `serde_json::Value`'s open-ended schema for the
    /// `additionalProperties` fallthrough.
    #[serde(flatten)]
    #[schemars(with = "serde_json::Value")]
    pub extra: BTreeMap<String, toml::Value>,
}

impl LineEntry {
    /// The entry's `type` tag — segment id, `"separator"`, or `None`
    /// for a malformed inline table missing `type`. The builder
    /// warns and drops `None` entries.
    #[must_use]
    pub fn kind(&self) -> Option<&str> {
        match self {
            Self::Id(s) => Some(s.as_str()),
            Self::Item(item) => item.kind.as_deref(),
        }
    }

    /// `true` when the entry is `type = "separator"`. Bare strings
    /// are never separators; an inline table without a `type` field
    /// is also not classified as a separator (the builder drops it).
    #[must_use]
    pub fn is_separator(&self) -> bool {
        self.kind() == Some("separator")
    }

    /// The segment id, or `None` for separators / kindless entries.
    #[must_use]
    pub fn segment_id(&self) -> Option<&str> {
        match self.kind() {
            Some("separator") | None => None,
            Some(id) => Some(id),
        }
    }

    /// The separator-glyph override on a `type = "separator"` entry,
    /// or `None` when the entry uses the global default. Always
    /// `None` for non-separator entries.
    #[must_use]
    pub fn separator_character(&self) -> Option<&str> {
        match self {
            Self::Item(item) if item.kind.as_deref() == Some("separator") => {
                item.character.as_deref()
            }
            _ => None,
        }
    }

    /// `true` when this entry sets `merge = true`. Always `false`
    /// for separators and bare-string entries. Inline tables on
    /// separators with a `merge` field warn at build time and the
    /// flag is not honored here.
    #[must_use]
    pub fn merge(&self) -> bool {
        match self {
            Self::Item(item) if item.kind.as_deref() != Some("separator") => {
                item.merge.unwrap_or(false)
            }
            _ => false,
        }
    }
}

impl From<&str> for LineEntry {
    fn from(s: &str) -> Self {
        Self::Id(s.to_string())
    }
}

impl From<String> for LineEntry {
    fn from(s: String) -> Self {
        Self::Id(s)
    }
}

/// Per-item-tolerant deserialization for `LineConfig.segments`.
/// Reads the array as `Vec<toml::Value>` then converts each entry
/// individually: a string becomes [`LineEntry::Id`], a well-formed
/// inline-table becomes [`LineEntry::Item`], and any malformed item
/// (wrong-typed `type`, non-string non-table value, table that
/// fails the `LineEntryItem` shape) falls through to a kindless
/// [`LineEntry::Item`] that the builder warns and drops.
///
/// Mirrors the per-item warn-and-drop behavior of the numbered-line
/// path so single-line and multi-line configs treat malformed items
/// identically: parse never aborts on one bad entry; the builder
/// surfaces the diagnostic at render time.
fn deserialize_line_entries<'de, D>(deserializer: D) -> Result<Vec<LineEntry>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Vec::<toml::Value>::deserialize(deserializer)?;
    Ok(raw.into_iter().map(value_to_line_entry).collect())
}

fn value_to_line_entry(value: toml::Value) -> LineEntry {
    if let toml::Value::String(s) = &value {
        return LineEntry::Id(s.clone());
    }
    if let toml::Value::Table(_) = &value {
        if let Ok(item) = value.clone().try_into::<LineEntryItem>() {
            return LineEntry::Item(item);
        }
    }
    // Malformed: capture in a kindless `LineEntryItem` so the entry
    // survives parse + round-trips through the document but reaches
    // the builder as a "no `type`" warn-and-drop. Tables preserve
    // their keys in `extra` for forward-compat; bare scalars stash
    // under a synthetic key so the value isn't silently dropped at
    // load time.
    let mut extra: BTreeMap<String, toml::Value> = BTreeMap::new();
    if let toml::Value::Table(table) = value {
        for (k, v) in table {
            extra.insert(k, v);
        }
    } else {
        extra.insert("__malformed__".to_string(), value);
    }
    LineEntry::Item(LineEntryItem {
        kind: None,
        character: None,
        merge: None,
        extra,
    })
}

/// Top-level `layout = "..."` selector. Defaults to `SingleLine`
/// (preserves pre-multi-line config behavior). `MultiLine` instructs
/// the builder + render loop to consume `[line.N]` sub-tables.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LayoutMode {
    #[default]
    SingleLine,
    MultiLine,
}

/// `[segments.<id>]` override block. Each typed field, when `Some`,
/// replaces the segment's built-in default. Any unrecognized keys land
/// in [`extra`](Self::extra), which the segment builder forwards to
/// plugin scripts as `ctx.config.<key>`. `style` is stored as a raw
/// string; the segment builder parses it at build time
/// so parse errors emit warnings through the same callback that
/// handles unknown-ID and inverted-bounds diagnostics.
///
/// `Eq` isn't derived because [`toml::Value`] holds `f64` and so is
/// `PartialEq` only — `extra` propagates that constraint.
#[derive(Debug, Default, Clone, PartialEq, Deserialize, JsonSchema)]
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
    ///
    /// Schema bypass: `toml::Value` has no `JsonSchema` impl, so
    /// remap to `serde_json::Value`'s open-ended schema for the
    /// `additionalProperties` fallthrough.
    #[serde(flatten)]
    #[schemars(with = "serde_json::Value")]
    pub extra: BTreeMap<String, toml::Value>,
}

/// URL for the published JSON Schema, pinned to `main`. Single
/// canonical URL — same shape bacon, starship, and dprint ship.
/// The schema evolves forward-compatibly (fields added, not
/// removed); editors validate "config field is allowed by schema"
/// rather than "binary supports field," so a schema slightly ahead
/// of the installed binary loosens validation rather than tightens
/// it. Versioned per-tag self-hosted URLs (biome's model) are the
/// destination once `linesmith` has its own website plus
/// schemastore.org coverage.
pub const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/oakoss/linesmith/main/config.schema.json";

/// Prepend `#:schema <url>` directive (taplo / VS Code / Zed
/// convention) to a freshly-generated config body so editors pick up
/// the published schema without per-user setup.
pub fn with_schema_directive(body: &str) -> String {
    format!("#:schema {SCHEMA_URL}\n\n{body}")
}

/// Width-bounds override. Either side may be omitted; a missing side
/// inherits from the segment's built-in default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
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
    /// The allow-list tolerates every spec-documented key (see
    /// `KNOWN_TOP_LEVEL` and `KNOWN_LAYOUT_OPTIONS`), so forward-compat
    /// configs stay silent while typos surface.
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

/// Top-level config keys we recognize. Spec-documented keys (some
/// runtime-consumed, some forward-compat parsed-and-ignored) plus
/// `$schema` for editor tooling. Anything not in this list raises a
/// per-key warning through `from_str_validated_impl`.
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
const KNOWN_LAYOUT_OPTIONS: &[&str] = &["color", "claude_padding", "separator", "powerline_width"];

/// Per-segment override schema. Returns `None` for segment ids we
/// don't recognize so plugin segments (which own their own schema)
/// bypass validation. Most built-ins share the universal allow-list;
/// rate-limit segments extend it with per-family knobs that
/// `segments::rate_limit::format` reads from the TOML extras bag.
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
        // Absolute-format knobs — consumed when `format = "absolute"`,
        // ignored (without warning) under "duration" / "progress".
        "timezone",
        "hour_format",
        "locale",
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
        "ahead_behind",
    ];
    const MODEL_SEGMENT: &[&str] = &["priority", "width", "style", "visible_if", "format"];
    match id {
        "model" => Some(MODEL_SEGMENT),
        "workspace" | "cost" | "effort" | "context_window" => Some(BUILT_IN_COMMON),
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
/// `OsStr`-typed env args so non-UTF-8 paths (`/srv/café-bin` in a
/// non-UTF-8 locale) survive the cascade rather than collapse to
/// `None` upstream.
#[must_use]
pub fn resolve_config_path(
    cli_override: Option<PathBuf>,
    env_override: Option<&std::ffi::OsStr>,
    xdg_config_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
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
    let env_override = std::env::var_os("LINESMITH_CONFIG");
    let xdg_config_home = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("HOME");
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
        assert_eq!(
            entry_ids(&line.segments),
            vec!["model", "workspace", "cost"]
        );
        assert!(line.numbered.is_empty(), "no numbered tables expected");
    }

    #[test]
    fn layout_field_defaults_to_single_line_when_omitted() {
        let c = Config::from_str("").expect("parse ok");
        assert_eq!(c.layout, LayoutMode::SingleLine);
    }

    #[test]
    fn layout_field_parses_kebab_case_variants() {
        let c = Config::from_str(r#"layout = "single-line""#).expect("parse ok");
        assert_eq!(c.layout, LayoutMode::SingleLine);
        let c = Config::from_str(r#"layout = "multi-line""#).expect("parse ok");
        assert_eq!(c.layout, LayoutMode::MultiLine);
    }

    /// Pull the `segments` array out of a `[line.N]` raw value. The
    /// flatten map carries `toml::Value`, so test helpers do the
    /// same shape-walk the builder's `extract_line_segments` does
    /// without depending on the production helper directly.
    fn numbered_segments(value: &toml::Value) -> Vec<String> {
        let table = value.as_table().expect("expected table value");
        let array = table["segments"]
            .as_array()
            .expect("expected segments array");
        array
            .iter()
            .map(|v| v.as_str().expect("expected string").to_string())
            .collect()
    }

    /// Convenience accessor: project a `Vec<LineEntry>` to the
    /// segment-id sequence (separators filtered out, kindless
    /// entries filtered out). Tests that don't care about the
    /// inline-table form use this to keep assertions readable as
    /// `vec!["model", "git_branch"]`.
    fn entry_ids(entries: &[LineEntry]) -> Vec<&str> {
        entries.iter().filter_map(LineEntry::segment_id).collect()
    }

    #[test]
    fn line_numbered_only_parses() {
        // Multi-line shape without a sibling `segments`: every key
        // under `[line]` is a numbered child table.
        let c = Config::from_str(
            r#"
                [line.1]
                segments = ["model"]
                [line.2]
                segments = ["workspace", "cost"]
            "#,
        )
        .expect("parse ok");
        let line = c.line.expect("line present");
        assert!(
            line.segments.is_empty(),
            "no top-level segments key expected"
        );
        assert_eq!(line.numbered.len(), 2);
        assert_eq!(numbered_segments(&line.numbered["1"]), vec!["model"]);
        assert_eq!(
            numbered_segments(&line.numbered["2"]),
            vec!["workspace", "cost"]
        );
    }

    #[test]
    fn line_with_segments_and_numbered_children_coexist() {
        // The serde flatten + sibling field combination must accept
        // both shapes simultaneously: `[line].segments` parses to the
        // typed field, `[line.N]` sub-tables flatten into the
        // numbered map. Edge case #3 from spec §Edge cases.
        let c = Config::from_str(
            r#"
                [line]
                segments = ["fallback"]
                [line.1]
                segments = ["a", "b"]
                [line.2]
                segments = ["c"]
            "#,
        )
        .expect("parse ok");
        let line = c.line.expect("line present");
        assert_eq!(entry_ids(&line.segments), vec!["fallback"]);
        assert_eq!(line.numbered.len(), 2);
        assert_eq!(numbered_segments(&line.numbered["1"]), vec!["a", "b"]);
        assert_eq!(numbered_segments(&line.numbered["2"]), vec!["c"]);
    }

    #[test]
    fn line_numbered_keys_preserved_verbatim_for_builder_validation() {
        // The parser doesn't validate that numbered keys are positive
        // integers — that's the builder's job (with a warning). Pin
        // that contract so a future "smart" parser doesn't silently
        // start dropping `[line.foo]` and break the warn-and-skip
        // edge-case path.
        let c = Config::from_str(
            r#"
                [line.foo]
                segments = ["bogus"]
                [line.10]
                segments = ["valid"]
            "#,
        )
        .expect("parse ok");
        let line = c.line.expect("line present");
        assert_eq!(line.numbered.len(), 2);
        assert!(line.numbered.contains_key("foo"));
        assert!(line.numbered.contains_key("10"));
    }

    #[test]
    fn line_unknown_scalar_key_does_not_fail_parse_forward_compat() {
        // CX-2-A regression guard: a typo'd or future-version scalar
        // key under `[line]` (e.g. `[line] segmnts = [...]` or
        // `[line] separator = "..."`) must NOT fail config load.
        // The flatten map captures it as a raw `toml::Value`; the
        // builder's `extract_line_segments` will warn-and-drop at
        // render time. Without this contract, the spec's "unknown
        // keys are warnings" forward-compat rule would silently
        // regress for everything under `[line]`.
        let c = Config::from_str(
            r#"
                [line]
                segments = ["model"]
                segmnts = ["typo"]              # scalar / array
                future_separator = " | "        # scalar string
                [line.1]
                segments = ["valid"]
            "#,
        )
        .expect("parse ok despite unknown sibling keys");
        let line = c.line.expect("line present");
        assert_eq!(entry_ids(&line.segments), vec!["model"]);
        // Unknown siblings show up in the flatten map; the [line.1]
        // table sits next to them.
        assert!(line.numbered.contains_key("segmnts"));
        assert!(line.numbered.contains_key("future_separator"));
        assert!(line.numbered.contains_key("1"));
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
        // Spec-listed keys parse cleanly. Forward-compat keys
        // (`preset`, `plugins`, `$schema`) populate their fields so
        // a future `#[serde(skip_deserializing)]` regression or a
        // dropped `rename = "$schema"` shows up here, not silently.
        // `"$schema"` is quoted because TOML rejects `$` in bare keys.
        let toml = r#"
            "$schema" = "https://example.invalid/schema.json"
            theme = "default"
            preset = "developer"
            layout = "single-line"
            [line]
            segments = ["model"]
            [layout_options]
            color = "auto"
            [plugins.example]
            foo = "bar"
        "#;
        let warnings = collect_warnings(toml);
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        let cfg = Config::from_str(toml).expect("parses");
        assert_eq!(cfg.preset.as_deref(), Some("developer"));
        assert_eq!(
            cfg.schema_url.as_deref(),
            Some("https://example.invalid/schema.json")
        );
        let plugins = cfg.plugins.expect("plugins table populated");
        assert!(plugins.contains_key("example"));
    }

    #[test]
    fn schema_for_config_round_trips_as_valid_json() {
        // The drift check catches *changes* in generator output but
        // not whether the output is well-formed JSON Schema in the
        // first place. A future schemars-API typo could produce
        // unserializable output; CI would only catch it after the
        // committed schema bit-rotted. This pins basic validity.
        let schema = schemars::schema_for!(Config);
        let json = serde_json::to_string(&schema).expect("schema serializes as JSON");
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("schema round-trips as JSON");
        let obj = parsed.as_object().expect("schema root is an object");
        assert_eq!(
            obj.get("$schema").and_then(|v| v.as_str()),
            Some("https://json-schema.org/draft/2020-12/schema"),
            "schema must declare its meta-schema URI"
        );
        assert_eq!(
            obj.get("title").and_then(|v| v.as_str()),
            Some("Config"),
            "schema must title the root type"
        );
        // Pin that the round-7→round-8 forward-compat fields
        // actually materialized into the schema. A future
        // `#[serde(skip)]` slipping onto one of them would still
        // round-trip cleanly through the asserts above; this
        // catches the materialization gap directly.
        let properties = obj
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("schema declares properties");
        for key in ["preset", "plugins", "$schema"] {
            assert!(
                properties.contains_key(key),
                "schema must expose {key:?} as a top-level property"
            );
        }
    }

    #[test]
    fn schema_directive_wrapped_body_round_trips_as_toml() {
        // `with_schema_directive` prepends `#:schema URL\n\n` ahead
        // of the preset body. A future regression that drops the
        // separator (yielding `#:schema URL[body-first-line]`) would
        // pass the position-pin tests in driver.rs but corrupt TOML
        // parsing on bodies that start with `#` comments. Pin both
        // the structural separator and the round-trip here.
        let body = "[line]\nsegments = [\"model\"]\n";
        let wrapped = with_schema_directive(body);
        assert!(
            wrapped.starts_with("#:schema https://"),
            "directive at byte 0"
        );
        assert!(
            wrapped.contains("\n\n["),
            "blank-line separator before first table"
        );
        let parsed: Config = wrapped.parse().expect("wrapped body parses as Config");
        assert_eq!(
            entry_ids(&parsed.line.expect("line").segments),
            vec!["model"]
        );
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
        // The known-keys allow-list lets `separator` through without
        // an unknown-key warning; the segment builder parses the
        // string and emits its own warnings (unknown values, v0.2+
        // stubs).
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
    fn reset_segment_allows_absolute_format_keys_without_warning() {
        let warnings = collect_warnings(
            r#"
                [segments.rate_limit_5h_reset]
                format = "absolute"
                timezone = "America/Los_Angeles"
                hour_format = "12h"
                locale = "en-US"

                [segments.rate_limit_7d_reset]
                format = "absolute"
                timezone = "Europe/London"
                hour_format = "24h"
            "#,
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
    }

    #[test]
    fn model_segment_allows_format_key_without_warning() {
        let warnings = collect_warnings(
            r#"
                [segments.model]
                format = "compact"
            "#,
        );
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");

        let warnings_full = collect_warnings(
            r#"
                [segments.model]
                format = "full"
            "#,
        );
        assert!(
            warnings_full.is_empty(),
            "unexpected warnings: {warnings_full:?}"
        );
    }

    #[test]
    fn workspace_segment_warns_when_format_key_set() {
        // `format` is a model-only key; the validator's per-id schema
        // split should reject it on `workspace` (and the rest of
        // `BUILT_IN_COMMON`) so silent typos don't slip through.
        let warnings = collect_warnings(
            r#"
                [segments.workspace]
                format = "compact"
            "#,
        );
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("format"));
        assert!(warnings[0].contains("[segments.workspace]"));
    }

    #[test]
    fn git_branch_allows_per_marker_hide_below_cells_without_warning() {
        // `[segments.git_branch.dirty]` and `.ahead_behind` are
        // pass-through sub-tables, so per-marker `hide_below_cells`
        // reaches `from_extras` instead of tripping the unknown-key
        // validator.
        let warnings = collect_warnings(
            r#"
                [segments.git_branch.dirty]
                hide_below_cells = 50

                [segments.git_branch.ahead_behind]
                hide_below_cells = 80
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
        assert_eq!(entry_ids(&c.line.expect("line").segments), vec!["model"]);
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
        resolve_config_path(
            cli.map(PathBuf::from),
            env.map(std::ffi::OsStr::new),
            xdg.map(std::ffi::OsStr::new),
            home.map(std::ffi::OsStr::new),
        )
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
