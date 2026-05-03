//! Config types and TOML-extras parsers for the rate-limit segment
//! family. The [`CommonRateLimitConfig`] struct, family-shared format
//! enums, and `apply_common_extras` / `parse_*_format` helpers all
//! live here. Render-time helpers (`format_percent`, `format_duration`,
//! `render_error`, etc.) live in `format`.

use std::collections::BTreeMap;

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

/// Config-driven rendering format for the reset segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurationFormat {
    Duration,
    Progress,
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
    pub icon: String,
    pub label: String,
    pub stale_marker: String,
    pub progress_width: u16,
    /// Set by `apply_common_extras` when the user supplied an
    /// out-of-range `progress_width`. Segments use it to flip
    /// `format = Progress` back to the per-family default so the
    /// spec's "fall back to percent format" rule at
    /// `rate-limit-segments.md` §Edge cases is honored, not just warned.
    pub invalid_progress_width: bool,
}

impl CommonRateLimitConfig {
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            icon: String::new(),
            label: label.into(),
            stale_marker: "~".into(),
            progress_width: 20,
            invalid_progress_width: false,
        }
    }
}

/// Apply `[segments.<id>]` common overrides (`icon`, `label`,
/// `stale_marker`, `progress_width`) onto `cfg`. Wrong-type values
/// warn and leave the default; unknown keys are silently skipped
/// here because `config::validate_keys` owns that diagnostic.
pub(crate) fn apply_common_extras(
    cfg: &mut CommonRateLimitConfig,
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) {
    if let Some(v) = extras.get("icon") {
        if let Some(s) = v.as_str() {
            cfg.icon = s.to_string();
        } else {
            warn(&format!("segments.{id}.icon: expected string; ignoring"));
        }
    }
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

/// Read `format` from `[segments.<id>]` as "duration" | "progress".
#[must_use]
pub(crate) fn parse_duration_format(
    extras: &BTreeMap<String, toml::Value>,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> Option<DurationFormat> {
    match extras.get("format")?.as_str() {
        Some("duration") => Some(DurationFormat::Duration),
        Some("progress") => Some(DurationFormat::Progress),
        _ => {
            warn(&format!(
                "segments.{id}.format: expected \"duration\" or \"progress\"; ignoring"
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
