//! Segment trait. Current shape is `render` only; see
//! `docs/specs/segment-system.md` for the full trait (layout intent,
//! cache policy, sub-composition) that grows as segments mature.

use crate::input::StatusContext;

pub mod context_window;
pub mod cost;
pub mod effort;
pub mod model;
pub mod rate_limit;
pub mod rate_limit_5h;
pub mod rate_limit_7d;
pub mod workspace;

/// Output of a successful segment render. Carries only `text` today;
/// width hints, styled runs, and per-segment separator preferences
/// are added per `docs/specs/segment-system.md`. `#[non_exhaustive]`
/// keeps those additions SemVer-compatible.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderedSegment {
    pub text: String,
}

impl RenderedSegment {
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

pub trait Segment: Send {
    /// Render this segment for the given context, or `None` to hide.
    #[must_use]
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment>;
}

// --- Shared render helpers --------------------------------------------

/// Format a rate-limit window as `"{label} {pct:.0}% · {countdown}"`.
pub(crate) fn format_window(
    label: &str,
    window: &crate::input::RateLimitWindow,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    format!(
        "{label} {pct:.0}% · {countdown}",
        pct = window.used.value(),
        countdown = format_countdown_until(window.resets_at, now),
    )
}

/// Format a future UTC timestamp as a coarse countdown like `"2h 13m"`,
/// `"45m"`, `"6d"`, or `"now"` for times at or in the past.
pub(crate) fn format_countdown_until(
    target: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let delta = target - now;
    let total_minutes = delta.num_minutes();
    if total_minutes <= 0 {
        return "now".to_string();
    }
    let days = delta.num_days();
    if days >= 2 {
        return format!("{days}d");
    }
    let hours = delta.num_hours();
    if hours >= 1 {
        let minutes = (total_minutes - hours * 60).max(0);
        return if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        };
    }
    format!("{total_minutes}m")
}

#[cfg(test)]
mod countdown_tests {
    use super::format_countdown_until;
    use chrono::{Duration, TimeZone, Utc};

    fn ref_time() -> chrono::DateTime<chrono::Utc> {
        Utc.with_ymd_and_hms(2026, 4, 17, 12, 0, 0).unwrap()
    }

    #[test]
    fn past_or_present_renders_as_now() {
        let now = ref_time();
        assert_eq!(format_countdown_until(now, now), "now");
        assert_eq!(
            format_countdown_until(now - Duration::minutes(5), now),
            "now"
        );
    }

    #[test]
    fn sub_hour_renders_minutes_only() {
        let now = ref_time();
        assert_eq!(
            format_countdown_until(now + Duration::minutes(45), now),
            "45m"
        );
    }

    #[test]
    fn multi_hour_renders_hours_and_minutes() {
        let now = ref_time();
        assert_eq!(
            format_countdown_until(now + Duration::minutes(73), now),
            "1h 13m"
        );
    }

    #[test]
    fn round_hour_drops_minutes_suffix() {
        let now = ref_time();
        assert_eq!(format_countdown_until(now + Duration::hours(3), now), "3h");
    }

    #[test]
    fn two_or_more_days_renders_days_only() {
        let now = ref_time();
        assert_eq!(format_countdown_until(now + Duration::days(2), now), "2d");
        assert_eq!(format_countdown_until(now + Duration::days(6), now), "6d");
    }

    #[test]
    fn under_two_days_still_uses_hours() {
        let now = ref_time();
        // 47h 30m: under the 2-day threshold, so hours-minutes form applies.
        assert_eq!(
            format_countdown_until(now + Duration::minutes(47 * 60 + 30), now),
            "47h 30m"
        );
    }

    #[test]
    fn seam_at_two_day_boundary_switches_units() {
        let now = ref_time();
        // 47h 59m: still hours form.
        assert_eq!(
            format_countdown_until(now + Duration::minutes(48 * 60 - 1), now),
            "47h 59m"
        );
        // Exactly 48h: flips to days form.
        assert_eq!(format_countdown_until(now + Duration::hours(48), now), "2d");
    }

    #[test]
    fn sub_minute_collapses_to_now() {
        let now = ref_time();
        assert_eq!(
            format_countdown_until(now + Duration::seconds(30), now),
            "now"
        );
    }

    #[test]
    fn exactly_one_hour_drops_minutes_suffix() {
        let now = ref_time();
        assert_eq!(format_countdown_until(now + Duration::hours(1), now), "1h");
    }
}
