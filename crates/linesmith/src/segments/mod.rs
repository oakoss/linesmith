//! Segment trait and layout-intent types. Full contract lives in
//! `docs/specs/segment-system.md`; this module carries the subset the
//! layout engine uses today: visibility, cell width, priority,
//! separator preference, and theme role.

use crate::data_context::{DataContext, DataDep};
use crate::theme::{Role, Style};
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

pub(crate) mod builder;
pub mod context_bar;
pub mod context_window;
pub mod cost;
pub mod effort;
pub mod extra_usage;
pub mod git_branch;
pub mod model;
pub mod rate_limit_5h;
pub mod rate_limit_5h_reset;
pub mod rate_limit_7d;
pub mod rate_limit_7d_reset;
pub mod rate_limit_format;
pub mod session_duration;
pub mod tokens;
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
pub(crate) fn sanitize_control_chars(s: String) -> String {
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
    /// May the layout engine shrink this segment under width pressure
    /// before dropping it? Default `false` — only prose-like segments
    /// (workspace name, branch name) opt in. Numeric or structured
    /// segments leave this `false`: a half-cut percentage reads as
    /// the wrong number, which is worse than no number.
    /// See `docs/specs/segment-system.md` §Layout algorithm.
    pub truncatable: bool,
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

    /// Chainable setter for the truncate-before-drop opt-in.
    #[must_use]
    pub fn with_truncatable(mut self, truncatable: bool) -> Self {
        self.truncatable = truncatable;
        self
    }
}

impl Default for SegmentDefaults {
    fn default() -> Self {
        Self {
            priority: 128,
            width: None,
            default_separator: Separator::Space,
            truncatable: false,
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
    /// hidden. See [`RenderResult`]. `ctx` owns the parsed stdin
    /// payload (`ctx.status`) plus lazy accessors for other sources
    /// (`ctx.usage()`, `ctx.git()`, etc.) declared in
    /// [`data_deps`](Self::data_deps).
    fn render(&self, ctx: &DataContext) -> RenderResult;

    /// Declare which data sources this segment reads. The runtime
    /// computes the union across all enabled segments and lazy-fetches
    /// only those sources. Defaults to the stdin payload only; segments
    /// that read other sources must override. See
    /// `docs/specs/data-fetching.md` §Segment dependency declaration.
    ///
    /// The `&'static` lifetime is deliberate: built-in segments return
    /// a `const &[DataDep]` at zero cost, and runtime-loaded plugin
    /// segments (e.g. `RhaiSegment`) promote their parsed
    /// `Vec<DataDep>` via `Vec::leak` once at plugin-load time. The
    /// plugin registry is built once per process and lives until exit,
    /// so the leak is bounded. If plugin hot-reload arrives (deferred
    /// feature), swap to an arena allocator or `Arc<[DataDep]>`.
    #[must_use]
    fn data_deps(&self) -> &'static [DataDep] {
        &[DataDep::Status]
    }

    /// Layout defaults (priority, width bounds, separator preference).
    /// User config may override each field via [`OverriddenSegment`].
    #[must_use]
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::default()
    }
}

// --- Built-in registry + config-driven override wrapper ----------------

/// Default segment order when no config supplies one. No rate-limit
/// segments are in the default line: a first-run user without any
/// config shouldn't trigger a macOS Keychain prompt or a network
/// request just to render the statusline. Users opt in by listing
/// the rate-limit segments explicitly in `[line.segments]`.
pub const DEFAULT_SEGMENT_IDS: &[&str] = &[
    "model",
    "context_window",
    "cost",
    "effort",
    "git_branch",
    "workspace",
];

/// Every built-in segment id. Used by [`PluginRegistry`] to reject
/// plugins whose `const ID` shadows a built-in. Add new built-ins
/// here AND to [`built_in_by_id`].
///
/// [`PluginRegistry`]: crate::plugins::PluginRegistry
pub const BUILT_IN_SEGMENT_IDS: &[&str] = &[
    "model",
    "context_window",
    "context_bar",
    "workspace",
    "cost",
    "effort",
    "git_branch",
    "rate_limit_5h",
    "rate_limit_7d",
    "rate_limit_5h_reset",
    "rate_limit_7d_reset",
    "extra_usage",
    "session_duration",
    "tokens_input",
    "tokens_output",
    "tokens_cached",
    "tokens_total",
];

/// Construct a built-in segment by its config id. Unknown ids return
/// `None` so config loaders can warn and skip. `extras` carries the
/// `[segments.<id>]` TOML bag; rate-limit segments parse their knobs
/// from it (`format`, `invert`, `compact`, `use_days`, `icon`,
/// `label`, `stale_marker`, `progress_width`). Other built-ins
/// currently ignore `extras`.
#[must_use]
pub fn built_in_by_id(
    id: &str,
    extras: Option<&std::collections::BTreeMap<String, toml::Value>>,
    warn: &mut impl FnMut(&str),
) -> Option<Box<dyn Segment>> {
    let empty: std::collections::BTreeMap<String, toml::Value> = std::collections::BTreeMap::new();
    let e = extras.unwrap_or(&empty);
    match id {
        "model" => Some(Box::new(model::ModelSegment)),
        "context_window" => Some(Box::new(context_window::ContextWindowSegment)),
        "context_bar" => Some(Box::new(context_bar::ContextBarSegment::from_extras(
            e, warn,
        ))),
        "workspace" => Some(Box::new(workspace::WorkspaceSegment)),
        "cost" => Some(Box::new(cost::CostSegment)),
        "effort" => Some(Box::new(effort::EffortSegment)),
        "git_branch" => Some(Box::new(git_branch::GitBranchSegment::from_extras(e, warn))),
        "rate_limit_5h" => Some(Box::new(rate_limit_5h::RateLimit5hSegment::from_extras(
            e, warn,
        ))),
        "rate_limit_7d" => Some(Box::new(rate_limit_7d::RateLimit7dSegment::from_extras(
            e, warn,
        ))),
        "rate_limit_5h_reset" => Some(Box::new(
            rate_limit_5h_reset::RateLimit5hResetSegment::from_extras(e, warn),
        )),
        "rate_limit_7d_reset" => Some(Box::new(
            rate_limit_7d_reset::RateLimit7dResetSegment::from_extras(e, warn),
        )),
        "extra_usage" => Some(Box::new(extra_usage::ExtraUsageSegment::from_extras(
            e, warn,
        ))),
        "session_duration" => Some(Box::new(session_duration::SessionDurationSegment)),
        "tokens_input" => Some(Box::new(tokens::TokensInputSegment)),
        "tokens_output" => Some(Box::new(tokens::TokensOutputSegment)),
        "tokens_cached" => Some(Box::new(tokens::TokensCachedSegment)),
        "tokens_total" => Some(Box::new(tokens::TokensTotalSegment)),
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
    user_style: Option<Style>,
}

impl OverriddenSegment {
    #[must_use]
    pub fn new(inner: Box<dyn Segment>) -> Self {
        Self {
            inner,
            priority: None,
            width: None,
            default_separator: None,
            user_style: None,
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

    /// Wholesale-replaces the inner segment's declared style at render
    /// time. See `docs/specs/theming.md` §Resolution precedence.
    #[must_use]
    pub fn with_user_style(mut self, style: Style) -> Self {
        self.user_style = Some(style);
        self
    }
}

impl Segment for OverriddenSegment {
    fn render(&self, ctx: &DataContext) -> RenderResult {
        let result = self.inner.render(ctx)?;
        Ok(result.map(|r| match self.user_style {
            Some(style) => r.with_style(style),
            None => r,
        }))
    }

    fn data_deps(&self) -> &'static [DataDep] {
        self.inner.data_deps()
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
                built_in_by_id(id, None, &mut |_| {}).is_some(),
                "expected built-in registry to know {id}"
            );
        }
    }

    #[test]
    fn built_in_by_id_resolves_additional_documented_ids() {
        for id in [
            "context_bar",
            "session_duration",
            "rate_limit_5h",
            "rate_limit_7d",
            "rate_limit_5h_reset",
            "rate_limit_7d_reset",
            "extra_usage",
            "tokens_input",
            "tokens_output",
            "tokens_cached",
            "tokens_total",
        ] {
            assert!(
                built_in_by_id(id, None, &mut |_| {}).is_some(),
                "expected {id} to resolve"
            );
        }
    }

    #[test]
    fn built_in_by_id_rejects_unknown() {
        assert!(built_in_by_id("nope", None, &mut |_| {}).is_none());
        assert!(built_in_by_id("", None, &mut |_| {}).is_none());
    }

    // --- OverriddenSegment ---

    #[test]
    fn overridden_segment_replaces_priority() {
        let base = built_in_by_id("workspace", None, &mut |_| {}).expect("known id");
        let base_priority = base.defaults().priority;
        let wrapped = OverriddenSegment::new(base).with_priority(200);
        assert_eq!(wrapped.defaults().priority, 200);
        assert_ne!(wrapped.defaults().priority, base_priority);
    }

    #[test]
    fn overridden_segment_replaces_width_bounds() {
        let base = built_in_by_id("workspace", None, &mut |_| {}).expect("known id");
        assert_eq!(base.defaults().width, None);
        let bounds = WidthBounds::new(5, 40).expect("valid");
        let wrapped = OverriddenSegment::new(base).with_width(bounds);
        assert_eq!(wrapped.defaults().width, Some(bounds));
    }

    #[test]
    fn overridden_segment_replaces_default_separator() {
        let base = built_in_by_id("workspace", None, &mut |_| {}).expect("known id");
        let wrapped = OverriddenSegment::new(base).with_default_separator(Separator::None);
        assert_eq!(wrapped.defaults().default_separator, Separator::None);
    }

    #[test]
    fn overridden_segment_delegates_render_to_inner() {
        let wrapped =
            OverriddenSegment::new(built_in_by_id("workspace", None, &mut |_| {}).unwrap())
                .with_priority(0);
        let rendered = wrapped.render(&stub_ctx()).unwrap().expect("rendered");
        assert_eq!(rendered.text(), "linesmith");
    }

    #[test]
    fn style_override_wholesale_replaces_inner_declared_style() {
        // A stub that declares Role::Accent + bold at render time. The
        // override must wipe both, not merge with them.
        struct Styled;
        impl Segment for Styled {
            fn render(&self, _: &DataContext) -> RenderResult {
                Ok(Some(
                    RenderedSegment::new("x")
                        .with_role(Role::Accent)
                        .with_style(Style {
                            bold: true,
                            ..Style::default()
                        }),
                ))
            }
            fn defaults(&self) -> SegmentDefaults {
                SegmentDefaults::with_priority(0)
            }
        }
        let override_style = Style {
            role: Some(Role::Primary),
            italic: true,
            ..Style::default()
        };
        let wrapped = OverriddenSegment::new(Box::new(Styled)).with_user_style(override_style);
        let rendered = wrapped.render(&stub_ctx()).unwrap().expect("rendered");
        assert_eq!(rendered.style, override_style);
    }

    #[test]
    fn style_override_preserves_inner_none_return() {
        struct Hidden;
        impl Segment for Hidden {
            fn render(&self, _: &DataContext) -> RenderResult {
                Ok(None)
            }
            fn defaults(&self) -> SegmentDefaults {
                SegmentDefaults::with_priority(0)
            }
        }
        let wrapped =
            OverriddenSegment::new(Box::new(Hidden)).with_user_style(Style::role(Role::Primary));
        assert_eq!(wrapped.render(&stub_ctx()).unwrap(), None);
    }

    fn stub_ctx() -> DataContext {
        use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
        use std::path::PathBuf;
        use std::sync::Arc;
        DataContext::new(StatusContext {
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
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }
}
