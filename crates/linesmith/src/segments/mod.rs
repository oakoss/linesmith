//! Segment trait and layout-intent types. Full contract lives in
//! `docs/specs/segment-system.md`; this module carries the subset the
//! layout engine uses today: visibility, cell width, priority,
//! separator preference, and theme role.

use crate::input::StatusContext;
use crate::theme::{Role, Style};
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
    pub(crate) style: Style,
}

impl RenderedSegment {
    /// Build a rendered segment from `text`, auto-computing its cell
    /// width. Use [`Self::with_separator`] when the segment wants to
    /// override its default right-separator for this boundary, and
    /// [`Self::with_role`] / [`Self::with_style`] to attach a theme
    /// role or full style.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = sanitize_control_chars(text.into());
        let width = text_width(&text);
        Self {
            text,
            width,
            right_separator: None,
            style: Style::default(),
        }
    }

    #[must_use]
    pub fn with_separator(text: impl Into<String>, separator: Separator) -> Self {
        let text = sanitize_control_chars(text.into());
        let width = text_width(&text);
        Self {
            text,
            width,
            right_separator: Some(separator),
            style: Style::default(),
        }
    }

    /// Chainable setter for the segment's theme role. The layout
    /// engine resolves the role against the active theme + terminal
    /// capability at render time; no ANSI bytes land in `text`.
    ///
    /// Preserves any decorations previously set by [`Self::with_style`].
    /// Pair with `with_style` carefully: `.with_style(s).with_role(r)`
    /// keeps `s`'s bold/fg/etc. and swaps role, whereas
    /// `.with_role(r).with_style(s)` wholesale-replaces everything.
    #[must_use]
    pub fn with_role(mut self, role: Role) -> Self {
        self.style.role = Some(role);
        self
    }

    /// Chainable setter for the full style (role + decorations).
    /// Wholesale-replaces the current style; use [`Self::with_role`]
    /// when you want to preserve decorations and swap only the role.
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Style this segment wants applied when the layout emits it.
    #[must_use]
    pub fn style(&self) -> &Style {
        &self.style
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
    /// `width` and `style`. Reserved for [`crate::layout::truncate_to`];
    /// every other caller goes through [`Self::new`] so the width stays
    /// a function of the text.
    #[must_use]
    pub(crate) fn from_parts(
        text: String,
        width: u16,
        right_separator: Option<Separator>,
        style: Style,
    ) -> Self {
        Self {
            text,
            width,
            right_separator,
            style,
        }
    }
}

/// Cell count of `s` on a standard terminal, saturating at `u16::MAX`.
#[must_use]
pub(crate) fn text_width(s: &str) -> u16 {
    u16::try_from(UnicodeWidthStr::width(s)).unwrap_or(u16::MAX)
}

/// Strip Unicode control characters from `s`.
///
/// Segment text often comes from untrusted input (a project dir
/// basename, a worktree name). `UnicodeWidthChar::width` reports
/// control chars as 0 cells, but terminals interpret them as
/// cursor-movement, screen-clear, or OSC payloads: a worktree named
/// `evil\x1b[2J` would blank the terminal on every statusline render.
/// Stripping at the `RenderedSegment` boundary protects every segment
/// that funnels user data through it.
///
/// Returns the input unchanged when it has no control chars.
fn sanitize_control_chars(s: String) -> String {
    if !s.chars().any(char::is_control) {
        return s;
    }
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Separator between adjacent segments. Chosen by the segment to its
/// left; themes and user config can override.
///
/// `Theme` is reserved for theme-provided padding and renders as a
/// single space when no theme is configured. `Literal` carries a
/// `Cow<'static, str>` so built-ins stay zero-alloc while user config
/// allocates once.
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

/// Shorthand for [`Segment::render`]'s return type.
///
/// Three states:
/// - `Ok(Some(r))`: the segment renders `r`.
/// - `Ok(None)`: the segment has no content this invocation and should
///   be hidden (intentional, e.g. rate-limit segment on an API-tier
///   user).
/// - `Err(e)`: the segment attempted to render but failed. The layout
///   engine logs `e` to stderr and hides the segment — same visual
///   result as `Ok(None)`, but the diagnostic distinguishes failure
///   from intentional absence.
pub type RenderResult = Result<Option<RenderedSegment>, SegmentError>;

/// Runtime failure from a segment's [`Segment::render`]. Built-in
/// segments return `Ok(...)` today; this surface is primarily for
/// plugin-authored segments (rhai script errors, unexpected input,
/// propagated I/O).
#[derive(Debug)]
#[non_exhaustive]
pub struct SegmentError {
    pub message: String,
    pub source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl SegmentError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    #[must_use]
    pub fn with_source(
        message: impl Into<String>,
        source: Box<dyn std::error::Error + Send + Sync>,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(source),
        }
    }
}

impl std::fmt::Display for SegmentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)?;
        if let Some(src) = &self.source {
            write!(f, ": {src}")?;
        }
        Ok(())
    }
}

impl std::error::Error for SegmentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|e| e as &dyn std::error::Error)
    }
}

pub trait Segment: Send {
    /// Render this segment for the given context.
    ///
    /// Returns `Ok(None)` to hide, `Ok(Some(_))` to render, or `Err` on
    /// a runtime failure that the layout engine logs and treats as
    /// hidden. See [`RenderResult`].
    fn render(&self, ctx: &StatusContext) -> RenderResult;

    /// Layout defaults (priority, width bounds, separator preference).
    /// User config may override each field via [`OverriddenSegment`].
    #[must_use]
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::default()
    }
}

// --- Built-in registry + config-driven override wrapper ----------------

/// Default segment order when no config supplies one.
pub const DEFAULT_SEGMENT_IDS: &[&str] = &[
    "model",
    "context_window",
    "rate_limit",
    "cost",
    "effort",
    "workspace",
];

/// Construct a built-in segment by its config id. Unknown ids return
/// `None` so config loaders can warn and skip.
#[must_use]
pub fn built_in_by_id(id: &str) -> Option<Box<dyn Segment>> {
    match id {
        "model" => Some(Box::new(model::ModelSegment)),
        "context_window" => Some(Box::new(context_window::ContextWindowSegment)),
        "workspace" => Some(Box::new(workspace::WorkspaceSegment)),
        "cost" => Some(Box::new(cost::CostSegment)),
        "effort" => Some(Box::new(effort::EffortSegment)),
        "rate_limit" => Some(Box::new(rate_limit::RateLimitSegment)),
        "rate_limit_5h" => Some(Box::new(rate_limit_5h::RateLimit5hSegment)),
        "rate_limit_7d" => Some(Box::new(rate_limit_7d::RateLimit7dSegment)),
        _ => None,
    }
}

/// Wraps a `Segment` to override its `defaults()` output while
/// delegating `render` unchanged. Applying `[segments.<id>]` overrides
/// without touching the inner segment.
pub struct OverriddenSegment {
    inner: Box<dyn Segment>,
    priority: Option<u8>,
    width: Option<WidthBounds>,
    default_separator: Option<Separator>,
}

impl OverriddenSegment {
    #[must_use]
    pub fn new(inner: Box<dyn Segment>) -> Self {
        Self {
            inner,
            priority: None,
            width: None,
            default_separator: None,
        }
    }

    #[must_use]
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = Some(priority);
        self
    }

    #[must_use]
    pub fn with_width(mut self, bounds: WidthBounds) -> Self {
        self.width = Some(bounds);
        self
    }

    #[must_use]
    pub fn with_default_separator(mut self, separator: Separator) -> Self {
        self.default_separator = Some(separator);
        self
    }
}

impl Segment for OverriddenSegment {
    fn render(&self, ctx: &StatusContext) -> RenderResult {
        self.inner.render(ctx)
    }

    fn defaults(&self) -> SegmentDefaults {
        let mut d = self.inner.defaults();
        if let Some(p) = self.priority {
            d.priority = p;
        }
        if let Some(w) = self.width {
            d.width = Some(w);
        }
        if let Some(sep) = self.default_separator.clone() {
            d.default_separator = sep;
        }
        d
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
    fn rendered_segment_strips_csi_clear_screen_injection() {
        // \x1b[2J clears the screen if it reaches stdout.
        let r = RenderedSegment::new("evil\x1b[2J");
        assert_eq!(r.text(), "evil[2J");
        assert_eq!(r.width(), 7);
        assert!(!r.text().contains('\x1b'));
    }

    #[test]
    fn rendered_segment_strips_osc_set_title_with_bel_terminator() {
        // OSC 0 sets the terminal title; BEL (\x07) terminates it.
        // Both entry/exit bytes are controls and must drop out.
        let r = RenderedSegment::new("\x1b]0;pwn\x07rest");
        assert_eq!(r.text(), "]0;pwnrest");
        assert!(!r.text().contains('\x1b'));
        assert!(!r.text().contains('\x07'));
    }

    #[test]
    fn rendered_segment_strips_common_c0_controls() {
        let r = RenderedSegment::new("a\x07b\x08c\td\ne\rf");
        assert_eq!(r.text(), "abcdef");
        assert_eq!(r.width(), 6);
    }

    #[test]
    fn rendered_segment_strips_c1_controls_and_del() {
        let r = RenderedSegment::new("x\u{007F}y\u{0085}z\u{009B}");
        assert_eq!(r.text(), "xyz");
        assert_eq!(r.width(), 3);
    }

    #[test]
    fn rendered_segment_preserves_unicode_without_controls() {
        let r = RenderedSegment::new("café · 日本語");
        assert_eq!(r.text(), "café · 日本語");
    }

    #[test]
    fn rendered_segment_empty_string_stays_empty() {
        let r = RenderedSegment::new("");
        assert_eq!(r.text(), "");
        assert_eq!(r.width(), 0);
    }

    #[test]
    fn rendered_segment_all_control_input_collapses_to_empty() {
        // Downstream layout math must cope with zero-width non-None
        // renders; the `width == text_width(text)` invariant still holds.
        let r = RenderedSegment::new("\x1b\x07\n\t");
        assert_eq!(r.text(), "");
        assert_eq!(r.width(), 0);
    }

    #[test]
    fn rendered_segment_with_separator_also_strips_controls() {
        let r = RenderedSegment::with_separator("hi\x1bthere", Separator::None);
        assert_eq!(r.text(), "hithere");
        assert_eq!(r.width(), 7);
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

    #[test]
    fn segment_error_display_includes_message_only_without_source() {
        let err = SegmentError::new("missing rate_limits field");
        assert_eq!(err.to_string(), "missing rate_limits field");
    }

    #[test]
    fn segment_error_display_chains_source() {
        let src = std::io::Error::new(std::io::ErrorKind::NotFound, "cache.json");
        let err = SegmentError::with_source("cache read failed", Box::new(src));
        let rendered = err.to_string();
        assert!(rendered.starts_with("cache read failed: "));
        assert!(rendered.contains("cache.json"));
    }

    #[test]
    fn segment_error_source_chain_is_walkable() {
        use std::error::Error;
        let src = std::io::Error::other("inner");
        let err = SegmentError::with_source("outer", Box::new(src));
        let source = err.source().expect("source present");
        assert_eq!(source.to_string(), "inner");
    }

    // --- registry ---

    #[test]
    fn built_in_by_id_resolves_every_default_segment() {
        for id in DEFAULT_SEGMENT_IDS {
            assert!(
                built_in_by_id(id).is_some(),
                "expected built-in registry to know {id}"
            );
        }
    }

    #[test]
    fn built_in_by_id_resolves_additional_documented_ids() {
        // Not in the default line, but valid config ids.
        assert!(built_in_by_id("rate_limit_5h").is_some());
        assert!(built_in_by_id("rate_limit_7d").is_some());
    }

    #[test]
    fn built_in_by_id_rejects_unknown() {
        assert!(built_in_by_id("nope").is_none());
        assert!(built_in_by_id("").is_none());
    }

    // --- OverriddenSegment ---

    #[test]
    fn overridden_segment_replaces_priority() {
        let base = built_in_by_id("workspace").expect("known id");
        let base_priority = base.defaults().priority;
        let wrapped = OverriddenSegment::new(base).with_priority(200);
        assert_eq!(wrapped.defaults().priority, 200);
        assert_ne!(wrapped.defaults().priority, base_priority);
    }

    #[test]
    fn overridden_segment_replaces_width_bounds() {
        let base = built_in_by_id("workspace").expect("known id");
        assert_eq!(base.defaults().width, None);
        let bounds = WidthBounds::new(5, 40).expect("valid");
        let wrapped = OverriddenSegment::new(base).with_width(bounds);
        assert_eq!(wrapped.defaults().width, Some(bounds));
    }

    #[test]
    fn overridden_segment_replaces_default_separator() {
        let base = built_in_by_id("workspace").expect("known id");
        let wrapped = OverriddenSegment::new(base).with_default_separator(Separator::None);
        assert_eq!(wrapped.defaults().default_separator, Separator::None);
    }

    #[test]
    fn overridden_segment_delegates_render_to_inner() {
        // The wrapper doesn't intercept render; it only overrides
        // defaults.
        use crate::input::{ModelInfo, Tool, WorkspaceInfo};
        use std::path::PathBuf;
        use std::sync::Arc;

        let ctx = StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "Claude".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo/linesmith"),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            rate_limits: None,
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
        };
        let wrapped = OverriddenSegment::new(built_in_by_id("workspace").unwrap()).with_priority(0);
        let rendered = wrapped.render(&ctx).unwrap().expect("rendered");
        assert_eq!(rendered.text(), "linesmith");
    }
}
