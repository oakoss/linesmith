//! Segment trait and layout-intent types. Full contract lives in
//! `docs/specs/segment-system.md`; this module carries the subset the
//! v0.1 layout engine uses: visibility, cell width, priority, and
//! separator preference.

use crate::input::StatusContext;
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

pub mod context_window;
pub mod cost;
pub mod effort;
pub mod model;
pub mod rate_limit;
pub mod rate_limit_5h;
pub mod rate_limit_7d;
pub mod workspace;

/// Output of a successful segment render.
///
/// Fields are `pub(crate)` so the engine can read them directly;
/// external callers go through the constructors and accessors so the
/// `width == text_width(text)` invariant can't desync via a mutable
/// `text`. `#[non_exhaustive]` keeps future additions SemVer-safe.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderedSegment {
    pub(crate) text: String,
    pub(crate) width: u16,
    pub(crate) right_separator: Option<Separator>,
}

impl RenderedSegment {
    /// Build a rendered segment from `text`, auto-computing its cell
    /// width. Use [`Self::with_separator`] when the segment wants to
    /// override its default right-separator for this boundary.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let width = text_width(&text);
        Self {
            text,
            width,
            right_separator: None,
        }
    }

    #[must_use]
    pub fn with_separator(text: impl Into<String>, separator: Separator) -> Self {
        let text = text.into();
        let width = text_width(&text);
        Self {
            text,
            width,
            right_separator: Some(separator),
        }
    }

    /// The rendered text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Cell width of the rendered text.
    #[must_use]
    pub fn width(&self) -> u16 {
        self.width
    }

    /// Separator this render prefers on its right edge, if any. `None`
    /// means "fall back to the segment's default separator."
    #[must_use]
    pub fn right_separator(&self) -> Option<&Separator> {
        self.right_separator.as_ref()
    }

    /// Trusted crate-internal constructor that accepts an explicit
    /// `width`. Reserved for [`crate::layout::truncate_to`]; every
    /// other caller goes through [`Self::new`] so the width stays a
    /// function of the text.
    #[must_use]
    pub(crate) fn from_parts(text: String, width: u16, right_separator: Option<Separator>) -> Self {
        Self {
            text,
            width,
            right_separator,
        }
    }
}

/// Cell count of `s` on a standard terminal, saturating at `u16::MAX`.
#[must_use]
pub(crate) fn text_width(s: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(s)).unwrap_or(u16::MAX)
}

/// Separator between adjacent segments. Chosen by the segment to its
/// left; themes and user config can override.
///
/// `Theme` renders as a single space until the theme system lands.
/// `Literal` carries a `Cow<'static, str>` so built-ins stay zero-alloc
/// while user-supplied config can allocate once.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Separator {
    Space,
    Theme,
    Literal(Cow<'static, str>),
    None,
}

impl Separator {
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Space | Self::Theme => " ",
            Self::Literal(s) => s,
            Self::None => "",
        }
    }

    #[must_use]
    pub fn width(&self) -> u16 {
        match self {
            Self::Space | Self::Theme => 1,
            Self::Literal(s) => text_width(s),
            Self::None => 0,
        }
    }
}

/// Width bounds in cells with `min <= max` enforced at construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WidthBounds {
    min: u16,
    max: u16,
}

impl WidthBounds {
    /// Returns `None` when `min > max`.
    #[must_use]
    pub fn new(min: u16, max: u16) -> Option<Self> {
        (min <= max).then_some(Self { min, max })
    }

    #[must_use]
    pub fn min(self) -> u16 {
        self.min
    }

    #[must_use]
    pub fn max(self) -> u16 {
        self.max
    }
}

/// Layout intent declared by a segment; user config may override each
/// field.
///
/// Under width pressure the engine drops segments in descending
/// `priority` order: `255` drops first, `0` never drops. Default `128`.
/// Ties break by position: the right-most segment drops first.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SegmentDefaults {
    pub priority: u8,
    pub width: Option<WidthBounds>,
    pub default_separator: Separator,
}

impl SegmentDefaults {
    /// Constructor shorthand for the common case of "default layout
    /// intent with a specific priority." Chainable with
    /// [`Self::with_width`] and [`Self::with_default_separator`].
    #[must_use]
    pub fn with_priority(priority: u8) -> Self {
        Self {
            priority,
            ..Self::default()
        }
    }

    /// Chainable setter for width bounds.
    #[must_use]
    pub fn with_width(mut self, bounds: WidthBounds) -> Self {
        self.width = Some(bounds);
        self
    }

    /// Chainable setter for the default right-separator.
    #[must_use]
    pub fn with_default_separator(mut self, separator: Separator) -> Self {
        self.default_separator = separator;
        self
    }
}

impl Default for SegmentDefaults {
    fn default() -> Self {
        Self {
            priority: 128,
            width: None,
            default_separator: Separator::Space,
        }
    }
}

pub trait Segment: Send {
    /// Render this segment for the given context, or `None` to hide.
    #[must_use]
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment>;

    /// Layout defaults (priority, width bounds, separator preference).
    /// User config may override each field once the config layer lands.
    #[must_use]
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::default()
    }
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

#[cfg(test)]
mod layout_type_tests {
    use super::*;

    #[test]
    fn rendered_segment_computes_width() {
        let r = RenderedSegment::new("hello");
        assert_eq!(r.text(), "hello");
        assert_eq!(r.width(), 5);
        assert_eq!(r.right_separator(), None);
    }

    #[test]
    fn rendered_segment_counts_cells_not_bytes_for_middle_dot() {
        // U+00B7 MIDDLE DOT is 2 bytes but 1 cell.
        let r = RenderedSegment::new("42% · 200k");
        assert_eq!(r.width(), 10);
    }

    #[test]
    fn rendered_segment_with_separator_exposes_override() {
        let r = RenderedSegment::with_separator("x", Separator::None);
        assert_eq!(r.right_separator(), Some(&Separator::None));
    }

    #[test]
    fn separator_widths_match_expected() {
        assert_eq!(Separator::Space.width(), 1);
        assert_eq!(Separator::Theme.width(), 1);
        assert_eq!(Separator::None.width(), 0);
        assert_eq!(Separator::Literal(Cow::Borrowed(" | ")).width(), 3);
    }

    #[test]
    fn width_bounds_rejects_inverted_range() {
        assert!(WidthBounds::new(20, 10).is_none());
        assert!(WidthBounds::new(10, 10).is_some());
        assert!(WidthBounds::new(0, u16::MAX).is_some());
    }

    #[test]
    fn segment_defaults_default_priority_is_128() {
        assert_eq!(SegmentDefaults::default().priority, 128);
    }

    #[test]
    fn with_priority_preserves_other_defaults() {
        let d = SegmentDefaults::with_priority(64);
        assert_eq!(d.priority, 64);
        assert_eq!(d.width, None);
        assert_eq!(d.default_separator, Separator::Space);
    }

    #[test]
    fn builders_chain_on_segment_defaults() {
        let bounds = WidthBounds::new(4, 40).expect("valid bounds");
        let d = SegmentDefaults::with_priority(32)
            .with_width(bounds)
            .with_default_separator(Separator::Literal(Cow::Borrowed(" | ")));
        assert_eq!(d.priority, 32);
        assert_eq!(d.width, Some(bounds));
        assert_eq!(
            d.default_separator,
            Separator::Literal(Cow::Borrowed(" | "))
        );
    }
}
