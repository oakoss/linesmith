//! Render helpers for the rate-limit segment family. TOML-parsing,
//! the [`CommonRateLimitConfig`] struct, and the format enums live in
//! the `config` sibling.
//!
//! Canonical spec: `docs/specs/rate-limit-segments.md` §Render
//! semantics and §Error message table.

use chrono::{DateTime, Duration, Utc};

use super::config::{
    AbsoluteFormat, CommonRateLimitConfig, ExtraUsageFormat, HourFormat, Locale, PercentFormat,
    ResetFormat, Timezone,
};
use crate::data_context::{CredentialError, ExtraUsage, JsonlError, UsageBucket, UsageError};

/// Which rolling window a reset segment represents. Determines the
/// denominator when [`ResetFormat::Progress`] computes elapsed %.
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
/// caller already gated on `remaining > 0`. `resets_at` is passed
/// even for the duration/progress branches so the caller doesn't
/// need to know which fields each variant consumes.
#[must_use]
#[allow(clippy::too_many_arguments)] // an args struct here just renames the indirection.
pub(crate) fn format_reset(
    resets_at: DateTime<Utc>,
    remaining: Duration,
    format: &ResetFormat,
    compact: bool,
    use_days: bool,
    window: ResetWindow,
    jsonl: bool,
    cfg: &CommonRateLimitConfig,
) -> String {
    let value = match format {
        ResetFormat::Duration => format_duration_text(remaining, compact, use_days),
        ResetFormat::Absolute(absolute) => format_absolute_text(resets_at, absolute),
        ResetFormat::Progress => {
            format_progress_bar(reset_progress_pct(remaining, window), cfg.progress_width)
        }
    };
    wrap(&value, jsonl, cfg)
}

/// Render an absolute wall-clock time like `"7:00 PM PT"` (12h) or
/// `"19:00 PT"` (24h).
fn format_absolute_text(resets_at: DateTime<Utc>, cfg: &AbsoluteFormat) -> String {
    let secs = resets_at.timestamp();
    let nanos = i32::try_from(resets_at.timestamp_subsec_nanos()).unwrap_or(0);
    let Ok(ts) = jiff::Timestamp::new(secs, nanos) else {
        // `chrono::DateTime<Utc>` always fits jiff's range; this arm
        // is defensive against future chrono-side range changes.
        return String::new();
    };
    let tz = match &cfg.timezone {
        Timezone::SystemLocal => jiff::tz::TimeZone::system(),
        Timezone::Iana(tz) => tz.clone(),
    };
    let zdt = ts.to_zoned(tz);
    let pattern = match (cfg.hour, cfg.locale) {
        (HourFormat::Hour24, Locale::EnUs) => "%H:%M %Z",
        (HourFormat::Hour12, Locale::EnUs) => "%-I:%M %p %Z",
    };
    zdt.strftime(pattern).to_string()
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

    // --- absolute reset format ---

    fn fixed_utc(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        chrono::TimeZone::with_ymd_and_hms(&Utc, year, month, day, hour, minute, 0)
            .single()
            .expect("valid utc")
    }

    #[test]
    fn absolute_24h_renders_with_tz_abbreviation() {
        // 19:00 UTC → 12:00 PDT in Los Angeles on July 15, 2025
        let resets_at = fixed_utc(2025, 7, 15, 19, 0);
        let cfg = AbsoluteFormat {
            timezone: Timezone::Iana(jiff::tz::TimeZone::get("America/Los_Angeles").unwrap()),
            hour: HourFormat::Hour24,
            locale: Locale::EnUs,
        };
        assert_eq!(format_absolute_text(resets_at, &cfg), "12:00 PDT");
    }

    #[test]
    fn absolute_12h_renders_with_am_pm() {
        // 19:00 UTC → 12:00 PM PDT (12-hour clock, post-meridiem boundary)
        let resets_at = fixed_utc(2025, 7, 15, 19, 0);
        let cfg = AbsoluteFormat {
            timezone: Timezone::Iana(jiff::tz::TimeZone::get("America/Los_Angeles").unwrap()),
            hour: HourFormat::Hour12,
            locale: Locale::EnUs,
        };
        assert_eq!(format_absolute_text(resets_at, &cfg), "12:00 PM PDT");
    }

    #[test]
    fn absolute_12h_morning_strips_zero_pad() {
        // 14:30 UTC → 7:30 AM PDT — `%-I` strips leading zero from
        // single-digit 12h hours per the bead's "7:00 PM PT" example.
        let resets_at = fixed_utc(2025, 7, 15, 14, 30);
        let cfg = AbsoluteFormat {
            timezone: Timezone::Iana(jiff::tz::TimeZone::get("America/Los_Angeles").unwrap()),
            hour: HourFormat::Hour12,
            locale: Locale::EnUs,
        };
        assert_eq!(format_absolute_text(resets_at, &cfg), "7:30 AM PDT");
    }

    #[test]
    fn absolute_renders_in_explicit_zone_distinct_from_utc() {
        // Tokyo is UTC+9 with no DST — pinning a non-American zone so
        // any future tz-detection regression that silently picks UTC
        // instead of the configured zone fails this test loudly.
        let resets_at = fixed_utc(2025, 1, 15, 0, 0);
        let cfg = AbsoluteFormat {
            timezone: Timezone::Iana(jiff::tz::TimeZone::get("Asia/Tokyo").unwrap()),
            hour: HourFormat::Hour24,
            locale: Locale::EnUs,
        };
        assert_eq!(format_absolute_text(resets_at, &cfg), "09:00 JST");
    }

    #[test]
    fn format_reset_dispatches_absolute_branch() {
        // Pin: `ResetFormat::Absolute` reaches `format_absolute_text`
        // and the result is wrapped with the common label, rather
        // than silently collapsing to a duration string.
        let cfg = cfg();
        let resets_at = fixed_utc(2025, 7, 15, 19, 0);
        let format = ResetFormat::Absolute(AbsoluteFormat {
            timezone: Timezone::Iana(jiff::tz::TimeZone::get("America/Los_Angeles").unwrap()),
            hour: HourFormat::Hour24,
            locale: Locale::EnUs,
        });
        let out = format_reset(
            resets_at,
            Duration::hours(1),
            &format,
            false,
            true,
            ResetWindow::FiveHour,
            false,
            &cfg,
        );
        assert!(
            out.contains("12:00 PDT"),
            "absolute output missing time string: {out}"
        );
        assert!(out.contains("5h"), "absolute output missing label: {out}");
    }

    #[test]
    fn format_reset_absolute_under_jsonl_keeps_stale_marker() {
        // Pin that the JSONL stale-marker prefix lands on the
        // absolute-time string, not just on duration text.
        let mut cfg = cfg();
        cfg.stale_marker = "~".into();
        let resets_at = fixed_utc(2025, 7, 15, 19, 0);
        let format = ResetFormat::Absolute(AbsoluteFormat {
            timezone: Timezone::Iana(jiff::tz::TimeZone::get("America/Los_Angeles").unwrap()),
            hour: HourFormat::Hour24,
            locale: Locale::EnUs,
        });
        let out = format_reset(
            resets_at,
            Duration::hours(1),
            &format,
            false,
            true,
            ResetWindow::FiveHour,
            true, // jsonl
            &cfg,
        );
        assert!(
            out.starts_with("~"),
            "expected stale marker on JSONL absolute output: {out}"
        );
        assert!(
            out.contains("12:00 PDT"),
            "absolute output missing time string: {out}"
        );
    }

    #[test]
    fn absolute_renders_correct_zone_across_dst_transition() {
        // 2025-03-09: America/Los_Angeles springs forward at 02:00
        // PST → 03:00 PDT (= 10:00 UTC). Pin both sides of the jump
        // so a regression that picks the wrong offset shows up.
        let cfg = AbsoluteFormat {
            timezone: Timezone::Iana(jiff::tz::TimeZone::get("America/Los_Angeles").unwrap()),
            hour: HourFormat::Hour24,
            locale: Locale::EnUs,
        };
        // 09:30 UTC = 01:30 PST (pre-jump, UTC-8).
        let pre = fixed_utc(2025, 3, 9, 9, 30);
        assert_eq!(format_absolute_text(pre, &cfg), "01:30 PST");
        // 11:30 UTC = 04:30 PDT (post-jump, UTC-7).
        let post = fixed_utc(2025, 3, 9, 11, 30);
        assert_eq!(format_absolute_text(post, &cfg), "04:30 PDT");
    }
}
