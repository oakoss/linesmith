//! Shared render helpers for the rate-limit segments.
//!
//! Canonical spec: `docs/specs/rate-limit-segments.md` §Render
//! semantics and §Error message table.

use std::collections::BTreeMap;

use chrono::Duration;

use crate::data_context::{CredentialError, ExtraUsage, JsonlError, UsageBucket, UsageError};

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

/// Which rolling window a reset segment represents. Determines the
/// denominator when [`DurationFormat::Progress`] computes elapsed %.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ResetWindow {
    FiveHour,
    SevenDay,
}

impl ResetWindow {
    fn total(self) -> Duration {
        match self {
            Self::FiveHour => Duration::hours(5),
            Self::SevenDay => Duration::days(7),
        }
    }
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

/// Read a boolean knob from `[segments.<id>]`. Wrong-type values
/// emit a warning and return `None`.
#[must_use]
pub(crate) fn parse_bool(
    extras: &BTreeMap<String, toml::Value>,
    key: &str,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> Option<bool> {
    let v = extras.get(key)?;
    match v.as_bool() {
        Some(b) => Some(b),
        None => {
            warn(&format!("segments.{id}.{key}: expected bool; ignoring"));
            None
        }
    }
}

/// Render `rate_limit_5h` / `rate_limit_7d` from endpoint-sourced data:
/// apply `invert`, clamp, and pick the percent/progress form, then wrap
/// in label + icon. JSONL-sourced renders use [`format_jsonl_tokens`]
/// instead — the two shapes diverge in both unit (`%` vs compact tokens)
/// and modifier support (`invert` / `progress` don't apply without a
/// ceiling).
#[must_use]
pub(crate) fn format_percent(
    bucket: &UsageBucket,
    format: PercentFormat,
    invert: bool,
    cfg: &CommonRateLimitConfig,
) -> String {
    let raw = f64::from(bucket.utilization.value());
    let shown = if invert { 100.0 - raw } else { raw };
    let value = match format {
        PercentFormat::Percent => format!("{shown:.1}%"),
        PercentFormat::Progress => format_progress_bar(shown, cfg.progress_width),
    };
    wrap(&value, false, cfg)
}

/// Render the JSONL-mode token total for `rate_limit_5h` / `rate_limit_7d`
/// per `docs/specs/rate-limit-segments.md` §JSONL-fallback display.
/// Applies the `stale_marker` prefix. Callers skip `invert` /
/// `progress_width` when routing to this function — those knobs
/// require a 0-100 axis JSONL lacks — so they never reach the
/// signature.
#[must_use]
pub(crate) fn format_jsonl_tokens(total: u64, cfg: &CommonRateLimitConfig) -> String {
    wrap(&format_tokens(total), true, cfg)
}

/// Render `rate_limit_5h_reset` / `rate_limit_7d_reset`. Assumes the
/// caller already gated on `remaining > 0`. `window` identifies the
/// rolling window so the progress bar divides by the right total.
/// `jsonl` adds the stale-marker prefix.
#[must_use]
pub(crate) fn format_duration(
    remaining: Duration,
    format: DurationFormat,
    compact: bool,
    use_days: bool,
    window: ResetWindow,
    jsonl: bool,
    cfg: &CommonRateLimitConfig,
) -> String {
    let value = match format {
        DurationFormat::Duration => format_duration_text(remaining, compact, use_days),
        DurationFormat::Progress => {
            format_progress_bar(reset_progress_pct(remaining, window), cfg.progress_width)
        }
    };
    wrap(&value, jsonl, cfg)
}

/// Render `extra_usage` (endpoint only). Falls back from currency →
/// percent when the account is missing `monthly_limit` (spec §Edge
/// cases). Returns `None` when no value can be rendered; callers hide
/// the segment in that case.
#[must_use]
pub(crate) fn format_extra_usage(
    extra: &ExtraUsage,
    format: ExtraUsageFormat,
    cfg: &CommonRateLimitConfig,
) -> Option<String> {
    let value = match format {
        ExtraUsageFormat::Currency => match (extra.monthly_limit, extra.used_credits) {
            (Some(limit), Some(used)) => {
                Some(format_currency(limit - used, extra.currency.as_deref()))
            }
            _ => extra.utilization.map(|p| format!("{:.1}%", p.value())),
        },
        ExtraUsageFormat::Percent => extra.utilization.map(|p| format!("{:.1}%", p.value())),
    }?;
    Some(wrap(&value, false, cfg))
}

/// Render a `UsageError` to the user-facing bracket string from
/// `docs/specs/rate-limit-segments.md` §Error message table. Wrapping
/// variants (`Credentials`, `Jsonl`) dispatch on the inner variant so
/// `[Keychain error]` / `[Credentials unreadable]` surface per-spec.
#[must_use]
pub(crate) fn render_error(err: &UsageError, cfg: &CommonRateLimitConfig) -> String {
    let body = match err {
        UsageError::NoCredentials => "[No credentials]",
        UsageError::Credentials(inner) => match inner {
            CredentialError::NoCredentials
            | CredentialError::MissingField { .. }
            | CredentialError::EmptyToken { .. } => "[No credentials]",
            CredentialError::SubprocessFailed(_) => "[Keychain error]",
            CredentialError::IoError { .. } => "[Credentials unreadable]",
            CredentialError::ParseError { .. } => "[Parse error]",
        },
        UsageError::Timeout => "[Timeout]",
        UsageError::RateLimited { .. } => "[Rate limited]",
        UsageError::NetworkError => "[Network error]",
        UsageError::ParseError => "[Parse error]",
        UsageError::Unauthorized => "[Unauthorized]",
        // JSONL-layer failures only surface when the endpoint path
        // also failed AND JSONL itself couldn't aggregate. Render
        // generically; doctor command carries the detail.
        UsageError::Jsonl(JsonlError::NoEntries | JsonlError::DirectoryMissing) => "[No data]",
        UsageError::Jsonl(_) => "[Parse error]",
    };
    wrap_label_only(body, cfg)
}

fn wrap(value: &str, jsonl: bool, cfg: &CommonRateLimitConfig) -> String {
    let marker = if jsonl { cfg.stale_marker.as_str() } else { "" };
    let label_sep = if cfg.label.is_empty() { "" } else { ": " };
    let icon_sep = if cfg.icon.is_empty() { "" } else { " " };
    format!(
        "{marker}{icon}{icon_sep}{label}{label_sep}{value}",
        icon = cfg.icon,
        label = cfg.label,
    )
}

/// Label + icon wrap for error strings: the bracketed error body
/// replaces the value slot. Stale marker is never applied here — if
/// we're rendering an error, the source is moot.
fn wrap_label_only(body: &str, cfg: &CommonRateLimitConfig) -> String {
    let label_sep = if cfg.label.is_empty() { "" } else { ": " };
    let icon_sep = if cfg.icon.is_empty() { "" } else { " " };
    format!(
        "{icon}{icon_sep}{label}{label_sep}{body}",
        icon = cfg.icon,
        label = cfg.label,
    )
}

#[must_use]
pub(crate) fn format_progress_bar(pct: f64, width: u16) -> String {
    // Progress-width 0 means "disabled" per spec §Edge cases; callers
    // should have already fallen back to percent format, but defend
    // here so a stray `progress_width = 0` config doesn't panic.
    if width == 0 {
        return format!("{pct:.1}%");
    }
    let clamped = pct.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * f64::from(width)).round() as usize;
    let empty = usize::from(width).saturating_sub(filled);
    format!(
        "{filled_bar}{empty_bar} {clamped:.1}%",
        filled_bar = "█".repeat(filled),
        empty_bar = "░".repeat(empty),
    )
}

/// Duration → text per spec §Config schema: `compact=true` collapses
/// to `"4h37m"`, `compact=false` uses `"4hr 37m"`; `use_days=true`
/// emits `"1d 3hr"` once the remainder is >= 1 day.
fn format_duration_text(remaining: Duration, compact: bool, use_days: bool) -> String {
    let total_minutes = remaining.num_minutes().max(0);
    if total_minutes == 0 {
        // Per spec: never show "0m"; compact/non-compact both say "<1m".
        return "<1m".into();
    }

    let total_hours = total_minutes / 60;
    let sub_hour_minutes = total_minutes - total_hours * 60;

    if use_days && total_hours >= 24 {
        // Clamp days to 4 digits per spec §Edge cases.
        let days = (total_hours / 24).min(9999);
        let hours_within_day = total_hours - (total_hours / 24) * 24;
        return format_two(days, 'd', hours_within_day, 'h', compact, true);
    }

    if total_hours > 0 {
        return if sub_hour_minutes == 0 {
            format_one(total_hours, 'h', compact, true)
        } else {
            format_two(total_hours, 'h', sub_hour_minutes, 'm', compact, false)
        };
    }

    format_one(sub_hour_minutes, 'm', compact, false)
}

fn format_one(value: i64, unit: char, compact: bool, hours_suffix: bool) -> String {
    match (compact, unit, hours_suffix) {
        (true, u, _) => format!("{value}{u}"),
        (false, 'd', _) => format!("{value}d"),
        (false, 'h', true) => format!("{value}hr"),
        (false, 'm', _) => format!("{value}m"),
        (false, c, _) => format!("{value}{c}"),
    }
}

fn format_two(
    a: i64,
    unit_a: char,
    b: i64,
    unit_b: char,
    compact: bool,
    hours_suffix: bool,
) -> String {
    // Non-compact form uses "hr" for hours regardless of whether days
    // appear (`1d 3hr`, not `1d 3h`) — the only rule not obvious from
    // the spec config comments.
    if compact {
        if b == 0 {
            return format!("{a}{unit_a}");
        }
        return format!("{a}{unit_a}{b}{unit_b}");
    }
    let a_str = match unit_a {
        'd' => format!("{a}d"),
        'h' => format!("{a}hr"),
        _ => format!("{a}{unit_a}"),
    };
    if b == 0 {
        return a_str;
    }
    let b_str = match (unit_b, hours_suffix) {
        ('h', true) => format!("{b}hr"),
        _ => format!("{b}{unit_b}"),
    };
    format!("{a_str} {b_str}")
}

/// Reset-progress as a percentage: how far the window has elapsed,
/// computed as `1 - remaining / window_total`.
fn reset_progress_pct(remaining: Duration, window: ResetWindow) -> f64 {
    let total = window.total();
    let elapsed = total - remaining;
    (elapsed.num_milliseconds() as f64 / total.num_milliseconds() as f64).clamp(0.0, 1.0) * 100.0
}

/// Compact token formatting matching the spec's JSONL-mode examples
/// (`420k`, `1.2M`). Under 1k renders as a bare integer; k/M/G use one
/// decimal only when it adds precision (i.e. the integer part has < 2
/// significant digits) so values like `420_000` render as `420k`, not
/// `420.0k`.
#[must_use]
pub(crate) fn format_tokens(total: u64) -> String {
    const K: u64 = 1_000;
    const M: u64 = 1_000_000;
    const G: u64 = 1_000_000_000;
    if total < K {
        return total.to_string();
    }
    if total < M {
        return format_unit(total, K, 'k');
    }
    if total < G {
        return format_unit(total, M, 'M');
    }
    format_unit(total, G, 'G')
}

fn format_unit(total: u64, divisor: u64, unit: char) -> String {
    // Keep one decimal for values below 10× the divisor (`5.4k`,
    // `1.2M`); drop it above so `420_000` → `420k` matches the spec
    // example exactly.
    if total < divisor * 10 {
        let scaled = (total as f64) / (divisor as f64);
        format!("{scaled:.1}{unit}")
    } else {
        let scaled = total / divisor;
        format!("{scaled}{unit}")
    }
}

/// Render `amount` as "$X.XX" for USD, or "CODE X.XX" for any other
/// ISO currency (spec §Precision and clamping). Negatives clamp to
/// zero.
fn format_currency(amount: f64, currency: Option<&str>) -> String {
    let clamped = amount.max(0.0);
    match currency {
        None | Some("USD") => format!("${clamped:.2}"),
        Some(code) => format!("{code} {clamped:.2}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_context::{ExtraUsage, UsageBucket};
    use crate::input::Percent;

    fn cfg() -> CommonRateLimitConfig {
        CommonRateLimitConfig::new("5h")
    }

    fn bucket(pct: f64) -> UsageBucket {
        UsageBucket {
            utilization: Percent::new(pct as f32).unwrap(),
            resets_at: None,
        }
    }

    #[test]
    fn percent_format_rounds_to_one_decimal_with_label() {
        let s = format_percent(&bucket(22.0), PercentFormat::Percent, false, &cfg());
        assert_eq!(s, "5h: 22.0%");
    }

    #[test]
    fn percent_format_inverts_when_configured() {
        let s = format_percent(&bucket(22.0), PercentFormat::Percent, true, &cfg());
        assert_eq!(s, "5h: 78.0%");
    }

    #[test]
    fn percent_format_progress_bar_at_50pct() {
        let s = format_percent(&bucket(50.0), PercentFormat::Progress, false, &cfg());
        assert!(s.starts_with("5h: "), "{s}");
        assert!(s.contains("█"));
        assert!(s.contains("░"));
        assert!(s.ends_with("50.0%"), "{s}");
    }

    #[test]
    fn jsonl_tokens_compact_format_with_stale_marker() {
        // Spec §JSONL-fallback display: under JSONL `rate_limit_5h`
        // renders raw tokens with the `~` prefix.
        let s = format_jsonl_tokens(420_000, &cfg());
        assert_eq!(s, "~5h: 420k");
    }

    #[test]
    fn jsonl_tokens_renders_megabytes_when_above_one_million() {
        let s = format_jsonl_tokens(1_200_000, &cfg());
        assert_eq!(s, "~5h: 1.2M");
    }

    #[test]
    fn jsonl_tokens_empty_stale_marker_suppresses_prefix() {
        let mut c = cfg();
        c.stale_marker = String::new();
        let s = format_jsonl_tokens(420_000, &c);
        assert_eq!(s, "5h: 420k");
    }

    #[test]
    fn empty_label_drops_colon_separator() {
        let mut c = cfg();
        c.label = String::new();
        let s = format_percent(&bucket(22.0), PercentFormat::Percent, false, &c);
        assert_eq!(s, "22.0%");
    }

    #[test]
    fn icon_renders_with_space_separator() {
        let mut c = cfg();
        c.icon = "⏱".into();
        let s = format_percent(&bucket(22.0), PercentFormat::Percent, false, &c);
        assert_eq!(s, "⏱ 5h: 22.0%");
    }

    #[test]
    fn format_tokens_under_one_thousand_renders_integer() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    #[test]
    fn format_tokens_single_decimal_for_small_k_values() {
        assert_eq!(format_tokens(5_400), "5.4k");
        assert_eq!(format_tokens(1_200), "1.2k");
    }

    #[test]
    fn format_tokens_drops_decimal_at_ten_k_and_above() {
        assert_eq!(format_tokens(10_000), "10k");
        assert_eq!(format_tokens(420_000), "420k");
    }

    #[test]
    fn format_tokens_switches_to_megabytes_and_gigabytes() {
        assert_eq!(format_tokens(1_200_000), "1.2M");
        assert_eq!(format_tokens(10_000_000), "10M");
        assert_eq!(format_tokens(1_200_000_000), "1.2G");
    }

    #[test]
    fn format_tokens_pins_unit_boundaries() {
        // Exact unit thresholds: 1k, 1M, 1G must flip branches cleanly.
        assert_eq!(format_tokens(1_000), "1.0k");
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(1_000_000_000), "1.0G");
        // Just under the decimal-drop threshold renders `10.0k` (one
        // decimal) rather than `10k`; at 10_000 the branch flips.
        assert_eq!(format_tokens(9_999), "10.0k");
        // Integer truncation on the no-decimal branch: 999_999 / 1_000
        // = 999, not rounded up to 1000. Acceptable UX (the next unit
        // flips at the exact boundary), pinned so future rounding
        // changes are deliberate.
        assert_eq!(format_tokens(999_999), "999k");
    }

    #[test]
    fn format_tokens_handles_u64_max_without_panic() {
        // Past any realistic aggregation but the u64 cast path must
        // stay panic-free. Precision decays past f64's 53-bit mantissa
        // (~9e15) — the value becomes an approximation, not a wrap.
        let s = format_tokens(u64::MAX);
        assert!(s.ends_with('G'), "expected G suffix, got {s}");
    }

    #[test]
    fn progress_bar_full_at_100pct() {
        let s = format_progress_bar(100.0, 4);
        assert_eq!(s, "████ 100.0%");
    }

    #[test]
    fn progress_bar_empty_at_zero() {
        let s = format_progress_bar(0.0, 4);
        assert_eq!(s, "░░░░ 0.0%");
    }

    #[test]
    fn progress_bar_zero_width_collapses_to_percent() {
        let s = format_progress_bar(50.0, 0);
        assert_eq!(s, "50.0%");
    }

    #[test]
    fn duration_text_sub_minute_renders_lt_1m() {
        assert_eq!(
            format_duration_text(Duration::seconds(30), false, true),
            "<1m"
        );
    }

    #[test]
    fn duration_text_minutes_only() {
        assert_eq!(
            format_duration_text(Duration::minutes(45), false, true),
            "45m"
        );
    }

    #[test]
    fn duration_text_hours_and_minutes_non_compact() {
        assert_eq!(
            format_duration_text(Duration::minutes(4 * 60 + 37), false, true),
            "4hr 37m"
        );
    }

    #[test]
    fn duration_text_hours_and_minutes_compact() {
        assert_eq!(
            format_duration_text(Duration::minutes(4 * 60 + 37), true, true),
            "4h37m"
        );
    }

    #[test]
    fn duration_text_uses_days_when_configured() {
        assert_eq!(
            format_duration_text(Duration::minutes(27 * 60), false, true),
            "1d 3hr"
        );
    }

    #[test]
    fn duration_text_skips_days_when_use_days_false() {
        assert_eq!(
            format_duration_text(Duration::minutes(27 * 60), false, false),
            "27hr"
        );
    }

    #[test]
    fn duration_text_clamps_days_to_four_digits() {
        let huge = Duration::days(99_999);
        let s = format_duration_text(huge, false, true);
        assert!(s.starts_with("9999d"), "{s}");
    }

    #[test]
    fn duration_text_round_hour_drops_minutes() {
        assert_eq!(format_duration_text(Duration::hours(3), false, true), "3hr");
    }

    #[test]
    fn currency_defaults_to_dollar_when_unset() {
        assert_eq!(format_currency(12.5, None), "$12.50");
    }

    #[test]
    fn currency_uses_dollar_for_usd() {
        assert_eq!(format_currency(12.5, Some("USD")), "$12.50");
    }

    #[test]
    fn currency_uses_iso_code_prefix_for_non_usd() {
        assert_eq!(format_currency(12.5, Some("EUR")), "EUR 12.50");
    }

    #[test]
    fn currency_clamps_negative_to_zero() {
        assert_eq!(format_currency(-5.0, None), "$0.00");
    }

    fn enabled_extra(limit: Option<f64>, used: Option<f64>, pct: Option<f32>) -> ExtraUsage {
        ExtraUsage {
            is_enabled: Some(true),
            utilization: pct.map(|v| Percent::new(v).unwrap()),
            monthly_limit: limit,
            used_credits: used,
            currency: Some("USD".into()),
        }
    }

    #[test]
    fn extra_currency_renders_remaining_credits() {
        let e = enabled_extra(Some(100.0), Some(40.0), None);
        let mut c = cfg();
        c.label = "extra".into();
        let s = format_extra_usage(&e, ExtraUsageFormat::Currency, &c).expect("renders");
        assert_eq!(s, "extra: $60.00");
    }

    #[test]
    fn extra_currency_falls_back_to_percent_when_monthly_limit_missing() {
        // Spec §Edge cases: `monthly_limit` missing with is_enabled=true
        // falls back to percent format from utilization.
        let e = enabled_extra(None, Some(40.0), Some(42.5));
        let mut c = cfg();
        c.label = "extra".into();
        let s = format_extra_usage(&e, ExtraUsageFormat::Currency, &c).expect("renders");
        assert_eq!(s, "extra: 42.5%");
    }

    #[test]
    fn extra_returns_none_when_no_data_available() {
        let e = enabled_extra(None, None, None);
        let s = format_extra_usage(&e, ExtraUsageFormat::Currency, &cfg());
        assert_eq!(s, None);
    }

    #[test]
    fn error_rendering_covers_spec_table() {
        let c = cfg();
        for (err, expected) in [
            (UsageError::NoCredentials, "5h: [No credentials]"),
            (
                UsageError::Credentials(CredentialError::SubprocessFailed(std::io::Error::other(
                    "x",
                ))),
                "5h: [Keychain error]",
            ),
            (
                UsageError::Credentials(CredentialError::IoError {
                    path: std::path::PathBuf::from("/x"),
                    cause: std::io::Error::other("x"),
                }),
                "5h: [Credentials unreadable]",
            ),
            (
                UsageError::Credentials(CredentialError::ParseError {
                    path: std::path::PathBuf::from("/x"),
                    cause: serde_json::Error::io(std::io::Error::other("x")),
                }),
                "5h: [Parse error]",
            ),
            (
                UsageError::Credentials(CredentialError::MissingField {
                    path: std::path::PathBuf::from("/x"),
                }),
                "5h: [No credentials]",
            ),
            (
                UsageError::Credentials(CredentialError::EmptyToken {
                    path: std::path::PathBuf::from("/x"),
                }),
                "5h: [No credentials]",
            ),
            (UsageError::Timeout, "5h: [Timeout]"),
            (
                UsageError::RateLimited { retry_after: None },
                "5h: [Rate limited]",
            ),
            (UsageError::NetworkError, "5h: [Network error]"),
            (UsageError::ParseError, "5h: [Parse error]"),
            (UsageError::Unauthorized, "5h: [Unauthorized]"),
            (UsageError::Jsonl(JsonlError::NoEntries), "5h: [No data]"),
            (
                UsageError::Jsonl(JsonlError::DirectoryMissing),
                "5h: [No data]",
            ),
        ] {
            assert_eq!(render_error(&err, &c), expected, "err = {err:?}");
        }
    }

    #[test]
    fn error_rendering_drops_label_when_empty() {
        let mut c = cfg();
        c.label = String::new();
        assert_eq!(render_error(&UsageError::Timeout, &c), "[Timeout]");
    }
}
