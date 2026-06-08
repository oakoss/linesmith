//! Config types and TOML-extras parsers for the rate-limit segment
//! family. The [`CommonRateLimitConfig`] struct, family-shared format
//! enums, and `apply_common_extras` / `parse_*_format` helpers all
//! live here. Render-time helpers (`format_percent`, `format_duration`,
//! `render_error`, etc.) live in `format`.

use std::collections::BTreeMap;

use crate::segments::progress_bar::{
    self, is_single_cell, BarChars as PbChars, BarStyle, FillMode, Thresholds,
};

/// Sits between `model` (priority 64) and `effort` (priority 160) on
/// the layout-engine priority scale. Layout drops numerically-largest
/// priorities first under width pressure, so rate-limit segments
/// survive longer than effort but drop before the model name when
/// terminal width is tight. Shared by all four rate-limit segments
/// and `extra_usage`.
pub(crate) const PRIORITY: u8 = 96;

/// Config-driven rendering format for the percent/progress segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PercentFormat {
    Percent,
    Progress,
}

/// Config-driven rendering format for the reset segments. `Duration`
/// is the existing `"4h12m"` countdown; `Absolute` is the
/// ccstatusline-parity `"7:00 PM PT"` wall-clock variant; `Progress`
/// is the existing progress bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResetFormat {
    Duration,
    Absolute(AbsoluteFormat),
    Progress,
}

/// Wall-clock formatting knobs for `ResetFormat::Absolute`. Invalid
/// timezone/locale strings fall back to the default
/// (`Timezone::SystemLocal` + `Locale::EnUs`) with a structured warning
/// each.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AbsoluteFormat {
    pub timezone: Timezone,
    pub hour: HourFormat,
    pub locale: Locale,
}

/// Timezone selector for `AbsoluteFormat`. `SystemLocal` resolves at
/// render time via jiff's auto-detection. `Iana(_)` carries a
/// pre-resolved jiff zone; jiff is exposed only inside this payload
/// so future variants (`Utc`, `Fixed(offset)`, etc.) can land without
/// changing the field type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Timezone {
    #[default]
    SystemLocal,
    Iana(jiff::tz::TimeZone),
}

/// 12-hour vs 24-hour clock face for `AbsoluteFormat`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum HourFormat {
    Hour12,
    #[default]
    Hour24,
}

/// Locale for absolute-time formatting. v0.1 ships English-only; v0.2
/// locale support is planned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Locale {
    #[default]
    EnUs,
}

/// Config-driven rendering format for `extra_usage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtraUsageFormat {
    Currency,
    Percent,
}

/// Common config shared by every rate-limit segment per the spec's
/// §Config schema. Each concrete segment adds its own type-specific
/// knobs (`format`, `invert`, `compact`, `use_days`) on top.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommonRateLimitConfig {
    pub label: String,
    pub stale_marker: String,
    pub progress_width: u16,
    /// Set by `apply_common_extras` when the user supplied an
    /// out-of-range `progress_width`. Segments use it to flip
    /// `format = Progress` back to the per-family default so the
    /// spec's "fall back to percent format" rule at
    /// `rate-limit-segments.md` §Edge cases is honored, not just warned.
    pub invalid_progress_width: bool,
    /// Green→yellow→red ramp; `pub(crate)` because the public surface is
    /// [`Self::bar_style`] / [`Self::role_for`], which reference the
    /// crate-internal `progress_bar` types.
    pub(crate) thresholds: Thresholds,
    /// Escalate the segment's color by usage (green/yellow/red). On by
    /// default — a flat usage indicator hides the signal users care
    /// about. Set `false` for the pre-s0vw flat `Info`.
    pub(crate) threshold_color: bool,
    /// Sub-cell fill style for the progress bar. Default [`FillMode::Whole`]
    /// (round to whole cells, no partial glyph — the historical shape).
    pub(crate) fill: FillMode,
    pub(crate) brackets: bool,
    /// Render the progress bar's empty trough dim. Default on; forced
    /// off when `threshold_color` is off, since the flat opt-out means
    /// a fully flat `Info` bar.
    pub(crate) dim_empty: bool,
    pub(crate) full: String,
    pub(crate) empty: String,
    pub(crate) half: String,
    pub(crate) open: String,
    pub(crate) close: String,
}

impl CommonRateLimitConfig {
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            stale_marker: "~".into(),
            progress_width: 20,
            invalid_progress_width: false,
            thresholds: Thresholds::new(50, 80).expect("50 <= 80"),
            threshold_color: true,
            fill: FillMode::Whole,
            brackets: false,
            dim_empty: true,
            full: progress_bar::DEFAULT_FULL.to_string(),
            empty: progress_bar::DEFAULT_EMPTY.to_string(),
            half: progress_bar::DEFAULT_HALF.to_string(),
            open: progress_bar::DEFAULT_OPEN.to_string(),
            close: progress_bar::DEFAULT_CLOSE.to_string(),
        }
    }

    /// Project the bar knobs onto the shared [`BarStyle`]. The inline
    /// percentage is fixed at one decimal — the historical rate-limit
    /// precision. The trough only dims when threshold color is on, so
    /// `threshold_color = false` yields a fully flat `Info` bar — the
    /// whole flat-opt-out rule lives here, beside `role_for`.
    pub(crate) fn bar_style(&self) -> BarStyle {
        BarStyle {
            width: self.progress_width,
            chars: PbChars {
                full: self.full.clone(),
                empty: self.empty.clone(),
                open: self.open.clone(),
                close: self.close.clone(),
                partial: self.fill.partial(&self.half),
            },
            brackets: self.brackets,
            percentage: Some(1),
            dim_empty: self.dim_empty && self.threshold_color,
        }
    }

    /// Role for `pct` when `threshold_color` is on, else flat [`Role::Info`].
    pub(crate) fn role_for(&self, pct: f64) -> crate::theme::Role {
        if self.threshold_color {
            self.thresholds.role_for(pct)
        } else {
            crate::theme::Role::Info
        }
    }
}

/// Apply `[segments.<id>]` common overrides (`label`,
/// `stale_marker`, `progress_width`) onto `cfg`. Wrong-type values
/// warn and leave the default; unknown keys are silently skipped
/// here because `config::validate_keys` owns that diagnostic.
pub(crate) fn apply_common_extras(
    cfg: &mut CommonRateLimitConfig,
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) {
    if let Some(v) = extras.get("label") {
        if let Some(s) = v.as_str() {
            cfg.label = s.to_string();
        } else {
            warn(&format!("segments.{id}.label: expected string; ignoring"));
        }
    }
    if let Some(v) = extras.get("stale_marker") {
        if let Some(s) = v.as_str() {
            cfg.stale_marker = s.to_string();
        } else {
            warn(&format!(
                "segments.{id}.stale_marker: expected string; ignoring"
            ));
        }
    }
    if let Some(v) = extras.get("progress_width") {
        match v.as_integer() {
            Some(n) if (1..=i64::from(u16::MAX)).contains(&n) => {
                cfg.progress_width = n as u16;
            }
            _ => {
                // Spec §Edge cases: 0/negative is invalid and forces
                // a fallback to percent/duration format at the
                // segment layer. Flag here; the segment flips its
                // `format` field after parsing.
                warn(&format!(
                    "segments.{id}.progress_width: expected 1..={}; ignoring",
                    u16::MAX,
                ));
                cfg.invalid_progress_width = true;
            }
        }
    }
    apply_bar_extras(cfg, extras, id, warn);
}

/// Apply shared progress-bar knobs onto `cfg`, mirroring context_bar's
/// surface so both segments expose the same config keys.
fn apply_bar_extras(
    cfg: &mut CommonRateLimitConfig,
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) {
    // `fill` first so its preset full/empty land before `characters.*`.
    if let Some(v) = extras.get("fill") {
        match v.as_str().and_then(FillMode::parse) {
            Some(mode) => {
                cfg.fill = mode;
                let (full, empty) = mode.preset_chars();
                cfg.full = full.to_string();
                cfg.empty = empty.to_string();
            }
            None => warn(&format!(
                "segments.{id}.fill: expected one of half|whole|eighth|braille; ignoring"
            )),
        }
    }

    if let Some(v) = extras.get("threshold_color") {
        match v.as_bool() {
            Some(b) => cfg.threshold_color = b,
            None => warn(&format!(
                "segments.{id}.threshold_color: expected boolean; ignoring"
            )),
        }
    }
    for (key, slot) in [
        ("brackets", &mut cfg.brackets),
        ("dim_empty", &mut cfg.dim_empty),
    ] {
        let Some(v) = extras.get(key) else { continue };
        match v.as_bool() {
            Some(b) => *slot = b,
            None => warn(&format!("segments.{id}.{key}: expected boolean; ignoring")),
        }
    }

    if let Some(t) = extras.get("thresholds").and_then(|v| v.as_table()) {
        let parse_field = |field: &str, warn: &mut dyn FnMut(&str)| -> Option<u8> {
            let v = t.get(field)?;
            match v.as_integer().and_then(|n| u8::try_from(n).ok()) {
                Some(n) if n <= 100 => Some(n),
                _ => {
                    warn(&format!(
                        "segments.{id}.thresholds.{field}: expected 0..=100; ignoring"
                    ));
                    None
                }
            }
        };
        let green = parse_field("green", &mut |m| warn(m)).unwrap_or(cfg.thresholds.green());
        let yellow = parse_field("yellow", &mut |m| warn(m)).unwrap_or(cfg.thresholds.yellow());
        match Thresholds::new(green, yellow) {
            Some(t) => cfg.thresholds = t,
            None => warn(&format!(
                "segments.{id}.thresholds: green ({green}) must be <= yellow ({yellow}); ignoring both"
            )),
        }
    }

    if let Some(c) = extras.get("characters").and_then(|v| v.as_table()) {
        for (field, slot) in [
            ("full", &mut cfg.full),
            ("partial", &mut cfg.half),
            ("empty", &mut cfg.empty),
            ("open", &mut cfg.open),
            ("close", &mut cfg.close),
        ] {
            let Some(v) = c.get(field) else { continue };
            match v.as_str() {
                Some(s) if is_single_cell(s) => *slot = s.to_string(),
                Some(_) => warn(&format!(
                    "segments.{id}.characters.{field}: expected a single-cell string; ignoring"
                )),
                None => warn(&format!(
                    "segments.{id}.characters.{field}: expected string; ignoring"
                )),
            }
        }
    }
}

/// Read `format` from `[segments.<id>]` as `"percent"` or
/// `"progress"`. Unknown values warn and return `None` so callers
/// keep their default.
#[must_use]
pub(crate) fn parse_percent_format(
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> Option<PercentFormat> {
    match extras.get("format")?.as_str() {
        Some("percent") => Some(PercentFormat::Percent),
        Some("progress") => Some(PercentFormat::Progress),
        _ => {
            warn(&format!(
                "segments.{id}.format: expected \"percent\" or \"progress\"; ignoring"
            ));
            None
        }
    }
}

/// Read `format` from `[segments.<id>]` as "duration" | "absolute" |
/// "progress". For `"absolute"`, also reads the sibling `timezone` /
/// `hour_format` / `locale` keys; tz/locale parse errors fall back to
/// system-local + 24h + en-US with a structured warning each.
#[must_use]
pub(crate) fn parse_reset_format(
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> Option<ResetFormat> {
    match extras.get("format")?.as_str() {
        Some("duration") => Some(ResetFormat::Duration),
        Some("progress") => Some(ResetFormat::Progress),
        Some("absolute") => Some(ResetFormat::Absolute(parse_absolute_format(
            extras, id, warn,
        ))),
        _ => {
            warn(&format!(
                "segments.{id}.format: expected \"duration\", \"absolute\", or \"progress\"; ignoring"
            ));
            None
        }
    }
}

fn parse_absolute_format(
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> AbsoluteFormat {
    AbsoluteFormat {
        timezone: parse_timezone(extras, id, warn).unwrap_or_default(),
        hour: parse_hour_format(extras, id, warn).unwrap_or_default(),
        locale: parse_locale(extras, id, warn).unwrap_or_default(),
    }
}

fn parse_timezone(
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> Option<Timezone> {
    let raw = extras.get("timezone")?;
    let Some(s) = raw.as_str() else {
        warn(&format!(
            "segments.{id}.timezone: expected string IANA name (e.g. \"America/Los_Angeles\"); falling back to system local"
        ));
        return None;
    };
    match jiff::tz::TimeZone::get(s) {
        Ok(tz) => Some(Timezone::Iana(tz)),
        Err(e) => {
            warn(&format!(
                "segments.{id}.timezone: \"{s}\" not found in tzdb ({e}); falling back to system local"
            ));
            None
        }
    }
}

fn parse_hour_format(
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> Option<HourFormat> {
    match extras.get("hour_format")?.as_str() {
        Some("12h") => Some(HourFormat::Hour12),
        Some("24h") => Some(HourFormat::Hour24),
        _ => {
            warn(&format!(
                "segments.{id}.hour_format: expected \"12h\" or \"24h\"; using 24h"
            ));
            None
        }
    }
}

fn parse_locale(
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> Option<Locale> {
    let raw = extras.get("locale")?;
    let Some(s) = raw.as_str() else {
        warn(&format!(
            "segments.{id}.locale: expected string (e.g. \"en-US\"); using en-US"
        ));
        return None;
    };
    match s {
        "en" | "en-US" => Some(Locale::EnUs),
        other => {
            warn(&format!(
                "segments.{id}.locale: \"{other}\" not yet supported in v0.1; using en-US"
            ));
            None
        }
    }
}

/// Read `format` from `[segments.<id>]` as "currency" | "percent".
#[must_use]
pub(crate) fn parse_extra_usage_format(
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> Option<ExtraUsageFormat> {
    match extras.get("format")?.as_str() {
        Some("currency") => Some(ExtraUsageFormat::Currency),
        Some("percent") => Some(ExtraUsageFormat::Percent),
        _ => {
            warn(&format!(
                "segments.{id}.format: expected \"currency\" or \"percent\"; ignoring"
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extras(pairs: &[(&str, toml::Value)]) -> BTreeMap<String, toml::Value> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    struct CapturedWarns {
        msgs: Vec<String>,
    }
    impl CapturedWarns {
        fn new() -> Self {
            Self { msgs: Vec::new() }
        }
        fn push(&mut self, m: &str) {
            self.msgs.push(m.to_string());
        }
        fn any_contains(&self, needle: &str) -> bool {
            self.msgs.iter().any(|m| m.contains(needle))
        }
    }

    #[test]
    fn defaults_preserve_pre_s0vw_shape_but_enable_threshold_color() {
        let cfg = CommonRateLimitConfig::new("5h");
        assert_eq!(cfg.progress_width, 20);
        assert_eq!(cfg.fill, FillMode::Whole);
        assert!(!cfg.brackets);
        assert!(cfg.threshold_color, "threshold color is on by default");
        assert_eq!(cfg.thresholds, Thresholds::new(50, 80).unwrap());
    }

    #[test]
    fn apply_bar_extras_parses_fill_brackets_threshold_and_thresholds() {
        let mut t = toml::value::Table::new();
        t.insert("green".to_string(), toml::Value::Integer(60));
        t.insert("yellow".to_string(), toml::Value::Integer(90));
        let e = extras(&[
            ("fill", toml::Value::String("eighth".into())),
            ("brackets", toml::Value::Boolean(true)),
            ("threshold_color", toml::Value::Boolean(false)),
            ("thresholds", toml::Value::Table(t)),
        ]);
        let mut cfg = CommonRateLimitConfig::new("5h");
        let mut w = CapturedWarns::new();
        apply_common_extras(&mut cfg, &e, "rate_limit_5h", &mut |m| w.push(m));
        assert_eq!(cfg.fill, FillMode::Eighth);
        assert!(cfg.brackets);
        assert!(!cfg.threshold_color);
        assert_eq!(cfg.thresholds, Thresholds::new(60, 90).unwrap());
        assert!(w.msgs.is_empty(), "unexpected warnings: {:?}", w.msgs);
    }

    #[test]
    fn apply_bar_extras_warns_on_bad_fill_and_inverted_thresholds() {
        let mut t = toml::value::Table::new();
        t.insert("green".to_string(), toml::Value::Integer(90));
        t.insert("yellow".to_string(), toml::Value::Integer(50));
        let e = extras(&[
            ("fill", toml::Value::String("sparkle".into())),
            ("thresholds", toml::Value::Table(t)),
        ]);
        let mut cfg = CommonRateLimitConfig::new("5h");
        let mut w = CapturedWarns::new();
        apply_common_extras(&mut cfg, &e, "rate_limit_5h", &mut |m| w.push(m));
        // Bad fill and inverted ramp both rejected; defaults preserved.
        assert_eq!(cfg.fill, FillMode::Whole);
        assert_eq!(cfg.thresholds, Thresholds::new(50, 80).unwrap());
        assert!(w.any_contains("fill"));
        assert!(w.any_contains("must be <="));
    }

    #[test]
    fn parse_reset_format_absolute_with_full_knobs() {
        let e = extras(&[
            ("format", toml::Value::String("absolute".into())),
            (
                "timezone",
                toml::Value::String("America/Los_Angeles".into()),
            ),
            ("hour_format", toml::Value::String("12h".into())),
            ("locale", toml::Value::String("en-US".into())),
        ]);
        let mut w = CapturedWarns::new();
        let f = parse_reset_format(&e, "rate_limit_5h_reset", &mut |m| w.push(m));
        let Some(ResetFormat::Absolute(abs)) = f else {
            panic!("expected ResetFormat::Absolute, got {f:?}");
        };
        assert_eq!(abs.hour, HourFormat::Hour12);
        assert_eq!(abs.locale, Locale::EnUs);
        assert!(matches!(abs.timezone, Timezone::Iana(_)));
        assert!(w.msgs.is_empty(), "no warnings expected: {:?}", w.msgs);
    }

    #[test]
    fn parse_reset_format_absolute_defaults_apply_when_knobs_missing() {
        // Bare `format = "absolute"` defaults to system-local tz +
        // 24h + en-US per the bead's "Default tz = system local;
        // default hour = 24" line.
        let e = extras(&[("format", toml::Value::String("absolute".into()))]);
        let mut w = CapturedWarns::new();
        let f = parse_reset_format(&e, "rate_limit_5h_reset", &mut |m| w.push(m));
        let Some(ResetFormat::Absolute(abs)) = f else {
            panic!("expected ResetFormat::Absolute");
        };
        assert!(matches!(abs.timezone, Timezone::SystemLocal));
        assert_eq!(abs.hour, HourFormat::Hour24);
        assert_eq!(abs.locale, Locale::EnUs);
    }

    #[test]
    fn parse_reset_format_unknown_tz_warns_and_falls_back_to_system_local() {
        let e = extras(&[
            ("format", toml::Value::String("absolute".into())),
            ("timezone", toml::Value::String("Mars/Olympus_Mons".into())),
        ]);
        let mut w = CapturedWarns::new();
        let f = parse_reset_format(&e, "rate_limit_5h_reset", &mut |m| w.push(m));
        let Some(ResetFormat::Absolute(abs)) = f else {
            panic!("expected ResetFormat::Absolute");
        };
        assert!(matches!(abs.timezone, Timezone::SystemLocal));
        assert!(
            w.any_contains("Mars/Olympus_Mons"),
            "warn must mention bad tz: {:?}",
            w.msgs
        );
    }

    #[test]
    fn parse_reset_format_unknown_hour_format_warns_and_uses_24h() {
        let e = extras(&[
            ("format", toml::Value::String("absolute".into())),
            ("hour_format", toml::Value::String("48h".into())),
        ]);
        let mut w = CapturedWarns::new();
        let f = parse_reset_format(&e, "rate_limit_5h_reset", &mut |m| w.push(m));
        let Some(ResetFormat::Absolute(abs)) = f else {
            panic!("expected ResetFormat::Absolute");
        };
        assert_eq!(abs.hour, HourFormat::Hour24);
        assert!(w.any_contains("hour_format"));
    }

    #[test]
    fn parse_reset_format_unsupported_locale_warns_and_uses_en_us() {
        // Forward-compat plumbing: v0.1 ships English only, but a
        // user config setting `locale = "fr-FR"` must not error —
        // warn-and-fallback so v0.2 locale support drops in without
        // breaking existing configs.
        let e = extras(&[
            ("format", toml::Value::String("absolute".into())),
            ("locale", toml::Value::String("fr-FR".into())),
        ]);
        let mut w = CapturedWarns::new();
        let f = parse_reset_format(&e, "rate_limit_5h_reset", &mut |m| w.push(m));
        let Some(ResetFormat::Absolute(abs)) = f else {
            panic!("expected ResetFormat::Absolute");
        };
        assert_eq!(abs.locale, Locale::EnUs);
        assert!(w.any_contains("fr-FR"));
    }

    #[test]
    fn parse_reset_format_duration_value_parses() {
        // The `format = "duration"` TOML key continues to parse
        // cleanly with no warnings.
        let e = extras(&[("format", toml::Value::String("duration".into()))]);
        let mut w = CapturedWarns::new();
        let f = parse_reset_format(&e, "rate_limit_5h_reset", &mut |m| w.push(m));
        assert!(matches!(f, Some(ResetFormat::Duration)));
        assert!(w.msgs.is_empty());
    }
}
