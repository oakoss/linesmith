//! Segment trait and layout-intent types. Full contract lives in
//! `docs/specs/segment-system.md`; this module carries the subset the
//! layout engine uses today: visibility, cell width, priority,
//! separator preference, and theme role.

use crate::data_context::{DataContext, DataDep};
use crate::theme::{Color, Role, Style, StyledRun};
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

pub mod agent;
pub mod builder;
pub mod context_bar;
pub mod context_window;
pub mod cost;
pub mod effort;
pub mod extra_usage;
pub mod extras;
pub mod git_branch;
pub mod model;
pub mod output_style;
pub mod progress_bar;
pub mod rate_limit;
pub mod session_duration;
pub mod tokens;
pub mod version;
pub mod vim;
pub mod workspace;

/// The color a group satellite inherits from its lead ([ADR-0028]): the
/// `role` and `fg` projection of a [`Style`]. Named so the
/// [`RenderedSegment::group_lead_color`] /
/// [`RenderedSegment::recolor_for_group`] pair has a typed contract
/// instead of a positional `(role, fg)` tuple.
///
/// [ADR-0028]: https://github.com/oakoss/linesmith/blob/main/docs/adrs/0028-group-lead-coloring-and-role-vocabulary.md
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GroupColor {
    pub(crate) role: Option<Role>,
    pub(crate) fg: Option<Color>,
}

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
    /// Optional intra-segment color spans. When `Some`, the layout
    /// engine fans the segment into one [`StyledRun`] per span instead
    /// of a single run, letting one segment paint multiple colors
    /// (e.g. a progress bar's filled cells vs its dim trough). `text`
    /// and `width` stay the authoritative concatenation for layout
    /// math; the spans must agree with them (the [`Self::from_spans`]
    /// constructor enforces this). `style` is the whole-segment
    /// fallback, used when spans is `None` and when a width-bound
    /// truncation drops the spans (see [`crate::layout::truncate_to`]).
    pub(crate) spans: Option<Vec<StyledRun>>,
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
            spans: None,
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
            spans: None,
        }
    }

    /// Build a multi-color segment from an ordered list of styled
    /// spans. Each span's text is sanitized like [`Self::new`]; the
    /// segment's `text`/`width` become the concatenation, so layout
    /// math is unaffected by the split. Adjacent spans that share a
    /// style are merged so the emitted run sequence carries no redundant
    /// SGR pairs.
    ///
    /// `spans` is `Some` only when two or more distinct-style runs
    /// survive: an empty input or a single effective run folds to `None`
    /// with that run's style as the whole-segment [`Self::style`]. So
    /// `Some` always means genuinely multi-color, a single-run segment
    /// compares equal to the identical bare one, and the width-bound
    /// truncation fallback (which drops spans) renders the same color it
    /// would un-truncated.
    #[must_use]
    pub fn from_spans(spans: impl IntoIterator<Item = StyledRun>) -> Self {
        let mut merged: Vec<StyledRun> = Vec::new();
        for span in spans {
            let text = sanitize_control_chars(span.text().to_string());
            if text.is_empty() {
                continue;
            }
            match merged.last_mut() {
                Some(last) if last.style() == span.style() => last.text.push_str(&text),
                _ => merged.push(StyledRun::new(text, span.style().clone())),
            }
        }
        let text: String = merged.iter().map(StyledRun::text).collect();
        let width = text_width(&text);
        let (style, spans) = match merged.len() {
            0 => (Style::default(), None),
            1 => (merged.pop().expect("len == 1").style, None),
            _ => (Style::default(), Some(merged)),
        };
        Self {
            text,
            width,
            right_separator: None,
            style,
            spans,
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

    /// Intra-segment color spans, if this segment paints more than one
    /// color. `None` means the whole segment renders in [`Self::style`]
    /// as a single run.
    #[must_use]
    pub fn spans(&self) -> Option<&[StyledRun]> {
        self.spans.as_deref()
    }

    /// Drop any intra-segment spans, collapsing the segment back to a
    /// single run in [`Self::style`]. Used when a user style override
    /// wins over the segment's own per-span coloring (the override is
    /// declared "wholesale-replaces", so it flattens the spans too).
    #[must_use]
    pub(crate) fn without_spans(mut self) -> Self {
        self.spans = None;
        self
    }

    /// Reads the whole-segment [`Self::style`], not the per-span colors:
    /// a multi-color segment sets a canonical whole-segment role
    /// (`with_role` after `from_spans`) that is its group color ([ADR-0028]).
    ///
    /// [ADR-0028]: https://github.com/oakoss/linesmith/blob/main/docs/adrs/0028-group-lead-coloring-and-role-vocabulary.md
    #[must_use]
    pub(crate) fn group_lead_color(&self) -> GroupColor {
        GroupColor {
            role: self.style.role,
            fg: self.style.fg,
        }
    }

    /// Repaint this satellite in its group lead's resolved color
    /// ([ADR-0028]). Decorations (bold/italic/underline/dim) and hyperlinks
    /// are preserved. Spans are repainted too: the emit path reads spans
    /// in preference to the whole-segment style.
    ///
    /// [ADR-0028]: https://github.com/oakoss/linesmith/blob/main/docs/adrs/0028-group-lead-coloring-and-role-vocabulary.md
    pub(crate) fn recolor_for_group(&mut self, color: GroupColor) {
        // Whole-segment style is the color source when spans is None and
        // for `group_lead_color`; keep it coherent even though spans, when
        // present, win on the emit path and are repainted below.
        self.style.role = color.role;
        self.style.fg = color.fg;
        if let Some(spans) = self.spans.as_mut() {
            for span in spans {
                span.style.role = color.role;
                span.style.fg = color.fg;
            }
        }
    }

    /// Prepend `icon` plus a separating space to the rendered text,
    /// recomputing width. When the segment carries spans, the icon
    /// becomes a leading span in the whole-segment [`Self::style`] so
    /// the existing spans keep their own colors. `icon` is sanitized
    /// here because it arrives from untrusted user config.
    #[must_use]
    pub(crate) fn with_icon_prefix(mut self, icon: &str) -> Self {
        let prefix = sanitize_control_chars(format!("{icon} "));
        if let Some(spans) = self.spans.as_mut() {
            // Coalesce with the first span when styles match (keeps the
            // same-style-runs-are-merged invariant `from_spans` upholds),
            // else prepend a leading icon span in the whole-segment style.
            match spans.first_mut() {
                Some(first) if first.style() == &self.style => {
                    first.text.insert_str(0, &prefix);
                }
                _ => spans.insert(0, StyledRun::new(prefix.clone(), self.style.clone())),
            }
        }
        self.text = format!("{prefix}{}", self.text);
        self.width = text_width(&self.text);
        self
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
            spans: None,
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
/// allocates once. `Powerline { width }` emits the Nerd Font
/// right-arrow chevron (U+E0B0) flanked by single-space padding;
/// `width` is the chevron's own cell count (1 or 2 — see
/// `[layout_options].powerline_width`), and the reported [`width()`]
/// includes the 2 padding cells. Chevron styling lives in
/// [`crate::layout::separator_style`].
///
/// [`width()`]: Separator::width
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Separator {
    Space,
    Theme,
    Literal(Cow<'static, str>),
    Powerline { width: PowerlineWidth },
    None,
}

/// Cell-count for the Nerd Font powerline chevron (U+E0B0). Most
/// modern fonts at standard sizes render the chevron as a single cell;
/// some larger sizes / older Nerd Font builds render it as two. The
/// type makes any other value unrepresentable so layout-width math
/// can't drift into invalid territory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PowerlineWidth {
    #[default]
    One,
    Two,
}

impl PowerlineWidth {
    /// Cell count this width represents (1 or 2).
    #[must_use]
    pub const fn cells(self) -> u16 {
        match self {
            Self::One => 1,
            Self::Two => 2,
        }
    }
}

/// Nerd Font right-arrow chevron (U+E0B0) with single-space padding
/// on each side.
const POWERLINE_CHEVRON_PADDED: &str = " \u{E0B0} ";

impl Separator {
    /// Default 1-cell powerline chevron. Use this for the common case
    /// (most modern Nerd Fonts render U+E0B0 as 1 cell at standard
    /// sizes); pass `Powerline { width: PowerlineWidth::Two }` for
    /// fonts/sizes that render 2 cells.
    #[must_use]
    pub const fn powerline() -> Self {
        Self::Powerline {
            width: PowerlineWidth::One,
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Space | Self::Theme => " ",
            Self::Literal(s) => s,
            Self::Powerline { .. } => POWERLINE_CHEVRON_PADDED,
            Self::None => "",
        }
    }

    #[must_use]
    pub fn width(&self) -> u16 {
        match self {
            Self::Space | Self::Theme => 1,
            Self::Literal(s) => text_width(s),
            Self::Powerline { width } => width.cells() + 2,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct SegmentDefaults {
    pub priority: u8,
    pub width: Option<WidthBounds>,
    pub icon: Option<&'static str>,
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
    /// [`Self::with_width`] and [`Self::with_truncatable`].
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

    /// Chainable setter for a built-in icon default.
    #[must_use]
    pub fn with_icon(mut self, icon: &'static str) -> Self {
        self.icon = Some(icon);
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
            icon: None,
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

/// Per-render layout state the engine builds once per call and threads
/// into every [`Segment::render`]. Distinct from [`DataContext`], which
/// is the data layer (one instance per process invocation, shared
/// across segments). `RenderContext` is the layout layer: terminal
/// width today, room for line index / capability / neighbor info as
/// dynamic-segment work lands.
///
/// `#[non_exhaustive]` keeps future additions SemVer-safe; segments
/// that don't read the field accept it as `_rc: &RenderContext` and
/// pay nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct RenderContext {
    /// Total cells available to this line. Sourced from the terminal,
    /// or the schema-defined fallback (200) when stdout is detached.
    pub terminal_width: u16,
}

impl RenderContext {
    #[must_use]
    pub fn new(terminal_width: u16) -> Self {
        Self { terminal_width }
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
    /// [`data_deps`](Self::data_deps). `rc` is per-render layout
    /// state — terminal width today — for segments that pick their
    /// own shape based on available room.
    fn render(&self, ctx: &DataContext, rc: &RenderContext) -> RenderResult;

    /// Layout-pressure-aware compaction hook. The reflow loop calls
    /// this on any segment under width pressure (truncatable or
    /// not), asking whether it can produce a render at most `target`
    /// cells wide. It runs before `truncatable` end-ellipsis
    /// truncation, so segment-side intelligence beats generic
    /// string clipping when both apply. Default returns `None` (no
    /// compact form available; engine falls through to truncatable
    /// or drop). Segments with structured tail content override to
    /// shed decoration while keeping the signal-bearing prefix.
    ///
    /// The returned render must lie in `[width.min, target]` cells:
    /// wider violates the layout-fit invariant (engine rejects and
    /// warns), narrower violates the user's `width.min` contract
    /// (engine rejects silently, same as `apply_width_bounds`).
    /// Implementations should return `None` rather than emit a
    /// render outside this range.
    ///
    /// See `docs/specs/segment-system.md` §Layout algorithm for
    /// the reflow loop's full ordering and target derivation.
    #[allow(unused_variables)]
    #[must_use]
    fn shrink_to_fit(
        &self,
        ctx: &DataContext,
        rc: &RenderContext,
        target: u16,
    ) -> Option<RenderedSegment> {
        None
    }

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

    /// Layout defaults (priority, width bounds, truncatable opt-in).
    /// User config may override each field via [`OverriddenSegment`].
    /// Implementations must be O(1), do no I/O, and avoid allocation:
    /// the layout engine snapshots this at collect time and the
    /// [`LineItem::Debug`] impl reads it for `dbg!` / panic-backtrace
    /// formatting.
    #[must_use]
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::default()
    }

    /// True when user config pinned this segment's color via a
    /// `[segments.<id>] style` override that sets a role or `fg`.
    /// Group-lead coloring ([ADR-0028]) checks this to honor resolution
    /// precedence: user override (step 1) beats the group color (step 2).
    /// A plugin's own declared `fg` is step-3 and still yields to the group.
    ///
    /// [ADR-0028]: https://github.com/oakoss/linesmith/blob/main/docs/adrs/0028-group-lead-coloring-and-role-vocabulary.md
    #[must_use]
    fn user_color_pinned(&self) -> bool {
        false
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
/// Also used by `resolve_segment_id` (O(n) per segment at build time,
/// not render time) to pin built-in ids to `Cow::Borrowed` per ADR-0026.
/// If the list grows past ~50 entries, swap to a `phf::Set`.
///
/// [`PluginRegistry`]: linesmith_plugin::PluginRegistry
pub const BUILT_IN_SEGMENT_IDS: &[&str] = &[
    "model",
    "context_window",
    "context_bar",
    "workspace",
    "cost",
    "effort",
    "output_style",
    "vim",
    "agent",
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
    "version",
];

/// Construct a built-in segment by its config id. Unknown ids return
/// `None` so config loaders can warn and skip. `extras` carries the
/// `[segments.<id>]` TOML bag; rate-limit segments parse their knobs
/// from it (`format`, `invert`, `compact`, `use_days`, `label`,
/// `stale_marker`, `progress_width`). Other built-ins
/// currently ignore `extras`.
///
/// Every arm in this `match` must have a corresponding entry in
/// [`BUILT_IN_SEGMENT_IDS`] and vice versa. The forward direction is
/// covered by `built_in_by_id_resolves_every_id_in_built_in_segment_ids`;
/// a match arm missing from the const would silently let a plugin shadow
/// the built-in and degrade its `Cow::Borrowed` short-circuit to
/// `Cow::Owned`. Add new built-ins to both lists together.
#[must_use]
pub fn built_in_by_id(
    id: &str,
    extras: Option<&std::collections::BTreeMap<String, toml::Value>>,
    warn: &mut impl FnMut(&str),
) -> Option<Box<dyn Segment>> {
    let empty: std::collections::BTreeMap<String, toml::Value> = std::collections::BTreeMap::new();
    let e = extras.unwrap_or(&empty);
    match id {
        "model" => Some(Box::new(model::ModelSegment::from_extras(e, warn))),
        "context_window" => Some(Box::new(context_window::ContextWindowSegment)),
        "context_bar" => Some(Box::new(context_bar::ContextBarSegment::from_extras(
            e, warn,
        ))),
        "workspace" => Some(Box::new(workspace::WorkspaceSegment)),
        "cost" => Some(Box::new(cost::CostSegment)),
        "effort" => Some(Box::new(effort::EffortSegment)),
        "output_style" => Some(Box::new(output_style::OutputStyleSegment)),
        "vim" => Some(Box::new(vim::VimSegment)),
        "agent" => Some(Box::new(agent::AgentSegment)),
        "git_branch" => Some(Box::new(git_branch::GitBranchSegment::from_extras(e, warn))),
        "rate_limit_5h" => Some(Box::new(
            rate_limit::five_hour::RateLimit5hSegment::from_extras(e, warn),
        )),
        "rate_limit_7d" => Some(Box::new(
            rate_limit::seven_day::RateLimit7dSegment::from_extras(e, warn),
        )),
        "rate_limit_5h_reset" => Some(Box::new(
            rate_limit::five_hour::RateLimit5hResetSegment::from_extras(e, warn),
        )),
        "rate_limit_7d_reset" => Some(Box::new(
            rate_limit::seven_day::RateLimit7dResetSegment::from_extras(e, warn),
        )),
        "extra_usage" => Some(Box::new(extra_usage::ExtraUsageSegment::from_extras(
            e, warn,
        ))),
        "session_duration" => Some(Box::new(session_duration::SessionDurationSegment)),
        "tokens_input" => Some(Box::new(tokens::TokensInputSegment)),
        "tokens_output" => Some(Box::new(tokens::TokensOutputSegment)),
        "tokens_cached" => Some(Box::new(tokens::TokensCachedSegment)),
        "tokens_total" => Some(Box::new(tokens::TokensTotalSegment)),
        "version" => Some(Box::new(version::VersionSegment::from_extras(e, warn))),
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
    user_style: Option<Style>,
    user_icon: Option<String>,
}

impl OverriddenSegment {
    #[must_use]
    pub fn new(inner: Box<dyn Segment>) -> Self {
        Self {
            inner,
            priority: None,
            width: None,
            user_style: None,
            user_icon: None,
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

    /// Wholesale-replaces the inner segment's declared style at render
    /// time. See `docs/specs/theming.md` §Resolution precedence.
    #[must_use]
    pub fn with_user_style(mut self, style: Style) -> Self {
        self.user_style = Some(style);
        self
    }

    #[must_use]
    pub fn with_icon(mut self, icon: String) -> Self {
        self.user_icon = Some(icon);
        self
    }

    /// The icon to prepend, if one is set and non-empty. An empty
    /// override means "no icon" (the disable form), so it never
    /// produces a stray leading space regardless of how the wrapper
    /// was constructed.
    fn effective_icon(&self) -> Option<&str> {
        self.user_icon.as_deref().filter(|i| !i.is_empty())
    }

    /// Cells the icon prefix (`icon` + one space) adds to the rendered
    /// width. `shrink_to_fit` reserves this before delegating so the
    /// prepended result still satisfies the layout engine's target.
    fn icon_prefix_width(&self) -> u16 {
        self.effective_icon()
            .map_or(0, |icon| text_width(icon).saturating_add(1))
    }

    fn apply_render_overrides(&self, mut rendered: RenderedSegment) -> RenderedSegment {
        if let Some(override_style) = &self.user_style {
            let merged = merge_user_override(rendered.style(), override_style);
            rendered = rendered.without_spans().with_style(merged);
        }
        if let Some(icon) = self.effective_icon() {
            rendered = rendered.with_icon_prefix(icon);
        }
        rendered
    }
}

impl Segment for OverriddenSegment {
    fn render(&self, ctx: &DataContext, rc: &RenderContext) -> RenderResult {
        let result = self.inner.render(ctx, rc)?;
        Ok(result.map(|r| self.apply_render_overrides(r)))
    }

    fn shrink_to_fit(
        &self,
        ctx: &DataContext,
        rc: &RenderContext,
        target: u16,
    ) -> Option<RenderedSegment> {
        // Reserve the icon prefix so the prepended result still fits
        // `target`; if even the prefix won't fit, the segment can't be
        // shown in compact form (the engine would reject the overflow
        // and drop it whole).
        let inner_target = target.checked_sub(self.icon_prefix_width())?;
        let inner = self.inner.shrink_to_fit(ctx, rc, inner_target)?;
        Some(self.apply_render_overrides(inner))
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
        d
    }

    fn user_color_pinned(&self) -> bool {
        self.user_style
            .as_ref()
            .is_some_and(|s| s.role.is_some() || s.fg.is_some())
    }
}

/// Merge a user-config style override onto the inner segment's style.
/// Visual fields (role, fg, bold, italic, underline, dim) take the
/// override's value — that's the documented "user wholesale-replaces
/// segment styling" behavior. `hyperlink` is the exception: it carries
/// segment behavior (the link target) rather than appearance, and the
/// user-style TOML syntax doesn't expose a way to set it, so the
/// override always arrives with `hyperlink: None`. Inheriting the
/// inner segment's hyperlink keeps `[segments.X] color = "red"` from
/// silently stripping links the segment emits.
fn merge_user_override(inner: &Style, override_style: &Style) -> Style {
    let mut merged = override_style.clone();
    if merged.hyperlink.is_none() {
        merged.hyperlink = inner.hyperlink.clone();
    }
    merged
}

/// One slot in a line layout: a configured segment or an inline
/// separator between segments. The builder (`build_segments` /
/// `build_lines`) interleaves separators between adjacent segments
/// from `[layout_options].separator`; the renderer walks this list
/// directly. See `docs/specs/segment-system.md` §Data model.
///
/// A plugin's per-render override ([`RenderedSegment::with_separator`])
/// beats the inline `Separator` only when an inline-separator slot
/// exists immediately to the segment's right. An override on the
/// rightmost segment, or a segment whose right-neighbor separator
/// has already been pruned, has no boundary to apply to and is
/// silently discarded.
///
/// Per-variant `#[non_exhaustive]` is omitted from `LineItem::Segment`
/// because consumers pattern-match `{ id, segment }` directly and the
/// consumer set is narrow (builder + tests + benches). Contrast
/// `LayoutDecision`'s per-variant `#[non_exhaustive]` (ADR-0026 §C):
/// those events are observability surfaces with an unknown consumer set,
/// so field-additive forward-compat justifies the `, ..` pattern cost.
#[non_exhaustive]
pub enum LineItem {
    /// A segment paired with the user-facing config id that names it
    /// (per ADR-0026). Sourced from `LineEntry::segment_id()` (the TOML key).
    ///
    /// `id` is a label, not an identity: the layout engine threads it
    /// through `LayoutDecision` events but does not verify it against the
    /// inner segment's type. External constructors must keep the two in sync.
    ///
    /// `Cow::Borrowed` vs `Cow::Owned` is a per-emit allocation trade-off,
    /// not a correctness invariant. Built-in ids land as `Cow::Borrowed`;
    /// plugin and user-config ids land as `Cow::Owned`. External
    /// constructors that don't preserve this partition are correct but pay
    /// one extra allocation per built-in emit.
    Segment {
        id: std::borrow::Cow<'static, str>,
        segment: Box<dyn Segment>,
        /// True when this segment shares a color group with the segment
        /// immediately to its left ([ADR-0029]): a maximal run of
        /// `fuses_left` segments plus their leftmost lead form one color
        /// group, rendered in the lead's resolved color. The builder sets
        /// it from the entry's `group` flag (and `merge`'s implied
        /// grouping); the group-lead color pass consumes it. `false` for
        /// an ungrouped segment and for a group's lead.
        ///
        /// [ADR-0029]: https://github.com/oakoss/linesmith/blob/main/docs/adrs/0029-group-boundary-marker-and-merge-reconciliation.md
        fuses_left: bool,
    },
    Separator(Separator),
}

impl std::fmt::Debug for LineItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // The trait has no `Debug` bound, so surface the id +
            // layout intent (priority, width hints) — that's what's
            // load-bearing in panic dumps and `dbg!` output anyway.
            Self::Segment {
                id,
                segment,
                fuses_left,
            } => f
                .debug_struct("Segment")
                .field("id", id)
                .field("defaults", &segment.defaults())
                .field("fuses_left", fuses_left)
                .finish(),
            Self::Separator(sep) => f.debug_tuple("Separator").field(sep).finish(),
        }
    }
}

#[cfg(test)]
impl LineItem {
    /// Centralizes the `LineItem::Segment { .. }` fixture literal so a
    /// new field on the variant touches this one site, not every test.
    pub(crate) fn seg(
        id: impl Into<std::borrow::Cow<'static, str>>,
        segment: Box<dyn Segment>,
    ) -> Self {
        Self::Segment {
            id: id.into(),
            segment,
            fuses_left: false,
        }
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
        // Powerline is configurable: width 1 (Nerd Font default) or
        // width 2 (some fonts/sizes render the chevron as 2 cells).
        // The reported width adds 2 cells of padding (one space on
        // each side of the chevron) since `text()` emits " ▶ ".
        assert_eq!(Separator::powerline().width(), 3);
        assert_eq!(
            Separator::Powerline {
                width: PowerlineWidth::Two,
            }
            .width(),
            4
        );
    }

    #[test]
    fn width_bounds_rejects_inverted_range() {
        assert!(WidthBounds::new(20, 10).is_none());
        assert!(WidthBounds::new(10, 10).is_some());
        assert!(WidthBounds::new(0, u16::MAX).is_some());
    }

    #[test]
    fn line_item_debug_renders_each_variant() {
        // The hand-written `Debug` impl on `LineItem` exists because
        // `Box<dyn Segment>` blocks `derive(Debug)`. Pin that both
        // variants format without panicking and that the variant
        // tag + id are visible in the output so panic backtraces
        // and `dbg!` calls identify the slot.
        struct StubSeg;
        impl Segment for StubSeg {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(None)
            }
        }
        let seg = LineItem::seg(std::borrow::Cow::Borrowed("stub"), Box::new(StubSeg));
        let sep = LineItem::Separator(Separator::powerline());
        let seg_dbg = format!("{seg:?}");
        let sep_dbg = format!("{sep:?}");
        assert!(seg_dbg.starts_with("Segment {"), "got {seg_dbg:?}");
        assert!(sep_dbg.starts_with("Separator("), "got {sep_dbg:?}");
        // The Segment-variant body surfaces id + defaults so panic
        // dumps carry the slot name + priority/width context.
        // Field-named `id:` + `defaults:` defend against a regression
        // that renames either field while preserving the body content.
        assert!(seg_dbg.contains("id:"), "got {seg_dbg:?}");
        assert!(seg_dbg.contains("defaults:"), "got {seg_dbg:?}");
        assert!(seg_dbg.contains("stub"), "got {seg_dbg:?}");
        assert!(seg_dbg.contains("priority"), "got {seg_dbg:?}");
        assert!(seg_dbg.contains("fuses_left"), "got {seg_dbg:?}");
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
        assert!(!d.truncatable);
    }

    #[test]
    fn builders_chain_on_segment_defaults() {
        let bounds = WidthBounds::new(4, 40).expect("valid bounds");
        let d = SegmentDefaults::with_priority(32)
            .with_width(bounds)
            .with_truncatable(true);
        assert_eq!(d.priority, 32);
        assert_eq!(d.width, Some(bounds));
        assert!(d.truncatable);
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
            "output_style",
            "vim",
            "agent",
        ] {
            assert!(
                built_in_by_id(id, None, &mut |_| {}).is_some(),
                "expected {id} to resolve"
            );
        }
    }

    #[test]
    fn built_in_by_id_resolves_every_id_in_built_in_segment_ids() {
        // Anchors the contract documented at `BUILT_IN_SEGMENT_IDS`:
        // every id in the const must round-trip through the registry.
        // Catches drift between the const and the match arms.
        for id in BUILT_IN_SEGMENT_IDS {
            assert!(
                built_in_by_id(id, None, &mut |_| {}).is_some(),
                "BUILT_IN_SEGMENT_IDS lists {id} but built_in_by_id can't construct it"
            );
        }
    }

    #[test]
    fn built_in_by_id_rejects_unknown() {
        assert!(built_in_by_id("nope", None, &mut |_| {}).is_none());
        assert!(built_in_by_id("", None, &mut |_| {}).is_none());
    }

    #[test]
    fn built_in_by_id_threads_extras_to_version_segment() {
        // Pin the registry → from_extras wiring for `version`. A
        // future refactor that drops `from_extras` and constructs
        // `VersionSegment::default()` would silently break user
        // configs that set `[segments.version].prefix = "CC "`.
        use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
        use std::collections::BTreeMap;
        use std::path::PathBuf;
        use std::sync::Arc;

        let mut extras = BTreeMap::new();
        extras.insert("prefix".to_string(), toml::Value::String("CC ".to_string()));
        let seg = built_in_by_id("version", Some(&extras), &mut |_| {})
            .expect("version segment resolves");

        let ctx = DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: Some(ModelInfo {
                display_name: "X".into(),
            }),
            workspace: Some(WorkspaceInfo {
                project_dir: PathBuf::from("/r"),
                git_worktree: None,
            }),
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: Some("2.1.90".into()),
            raw: Arc::new(serde_json::Value::Null),
        });
        let rc = RenderContext::new(80);
        let rendered = seg.render(&ctx, &rc).unwrap().expect("renders");
        assert_eq!(rendered.text(), "CC 2.1.90");
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
    fn overridden_segment_delegates_render_to_inner() {
        let wrapped =
            OverriddenSegment::new(built_in_by_id("workspace", None, &mut |_| {}).unwrap())
                .with_priority(0);
        let rendered = wrapped
            .render(&stub_ctx(), &stub_rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "linesmith");
    }

    #[test]
    fn style_override_wholesale_replaces_inner_declared_style() {
        // A stub that declares Role::Accent + bold at render time. The
        // override must wipe both, not merge with them.
        struct Styled;
        impl Segment for Styled {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
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
        let wrapped =
            OverriddenSegment::new(Box::new(Styled)).with_user_style(override_style.clone());
        let rendered = wrapped
            .render(&stub_ctx(), &stub_rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.style, override_style);
    }

    #[test]
    fn user_color_pinned_reflects_color_bearing_override_only() {
        struct Plain;
        impl Segment for Plain {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(Some(RenderedSegment::new("x")))
            }
        }
        assert!(!Plain.user_color_pinned());
        let by_role =
            OverriddenSegment::new(Box::new(Plain)).with_user_style(Style::role(Role::Info));
        assert!(by_role.user_color_pinned());
        let by_fg = OverriddenSegment::new(Box::new(Plain)).with_user_style(Style {
            fg: Some(Color::Palette256(5)),
            ..Style::default()
        });
        assert!(by_fg.user_color_pinned());
        // Decorations without color don't pin, so the group color still
        // applies and keeps those decorations.
        let bold_only = OverriddenSegment::new(Box::new(Plain)).with_user_style(Style {
            bold: true,
            ..Style::default()
        });
        assert!(!bold_only.user_color_pinned());
        // An icon override is not a color pin.
        let icon_only = OverriddenSegment::new(Box::new(Plain)).with_icon("⎇".to_string());
        assert!(!icon_only.user_color_pinned());
    }

    #[test]
    fn recolor_for_group_repaints_style_and_spans_keeping_decorations() {
        let mut multi = RenderedSegment::from_spans([
            StyledRun::new(
                "a",
                Style {
                    role: Some(Role::Success),
                    bold: true,
                    ..Style::default()
                }
                .with_hyperlink("https://example.com/a"),
            ),
            StyledRun::new("b", Style::role(Role::Muted)),
        ]);
        multi.recolor_for_group(GroupColor {
            role: Some(Role::Primary),
            fg: None,
        });
        let spans = multi.spans().expect("multi-color render keeps spans");
        assert_eq!(spans[0].style().role, Some(Role::Primary));
        assert!(
            spans[0].style().bold,
            "per-span decoration survives recolor"
        );
        assert_eq!(
            spans[0].style().hyperlink.as_deref(),
            Some("https://example.com/a"),
            "hyperlink (link behavior, not color) survives recolor"
        );
        assert_eq!(spans[1].style().role, Some(Role::Primary));
        assert_eq!(
            multi.group_lead_color(),
            GroupColor {
                role: Some(Role::Primary),
                fg: None,
            }
        );
    }

    #[test]
    fn user_style_override_preserves_inner_hyperlink() {
        // Pin the merge contract: visual override fields wholesale-
        // replace, but the inner segment's hyperlink survives so a
        // user `[segments.X] color = "red"` doesn't silently strip
        // links the segment emits. The user-style TOML syntax has no
        // hyperlink slot today, so the override's hyperlink is
        // always None — inheriting from the inner is lossless.
        struct Linked;
        impl Segment for Linked {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(Some(RenderedSegment::new("x").with_style(
                    Style::default().with_hyperlink("https://example.com"),
                )))
            }
            fn defaults(&self) -> SegmentDefaults {
                SegmentDefaults::with_priority(0)
            }
        }
        let override_style = Style::role(Role::Error);
        let wrapped =
            OverriddenSegment::new(Box::new(Linked)).with_user_style(override_style.clone());
        let rendered = wrapped
            .render(&stub_ctx(), &stub_rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.style.role, Some(Role::Error));
        assert_eq!(
            rendered.style.hyperlink.as_deref(),
            Some("https://example.com"),
        );
    }

    #[test]
    fn style_override_preserves_inner_none_return() {
        struct Hidden;
        impl Segment for Hidden {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(None)
            }
            fn defaults(&self) -> SegmentDefaults {
                SegmentDefaults::with_priority(0)
            }
        }
        let wrapped =
            OverriddenSegment::new(Box::new(Hidden)).with_user_style(Style::role(Role::Primary));
        assert_eq!(wrapped.render(&stub_ctx(), &stub_rc()).unwrap(), None);
    }

    #[test]
    fn shrink_to_fit_passthrough_reaches_inner_with_user_style_applied() {
        // The OverriddenSegment wrapper must forward shrink_to_fit to
        // the inner segment so user-overridden segments retain their
        // layout-pressure compaction. The wrapper also has to apply
        // user_style to the shrunk render the same way it does on
        // render — otherwise a styled override loses its theme on the
        // compact path.
        struct Shrinkable;
        impl Segment for Shrinkable {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(Some(RenderedSegment::new("full")))
            }
            fn shrink_to_fit(
                &self,
                _: &DataContext,
                _: &RenderContext,
                target: u16,
            ) -> Option<RenderedSegment> {
                let r = RenderedSegment::new("c");
                (r.width <= target).then_some(r)
            }
        }
        let override_style = Style {
            role: Some(Role::Primary),
            italic: true,
            ..Style::default()
        };
        let wrapped =
            OverriddenSegment::new(Box::new(Shrinkable)).with_user_style(override_style.clone());
        let shrunk = wrapped
            .shrink_to_fit(&stub_ctx(), &stub_rc(), 5)
            .expect("inner returned compact form");
        assert_eq!(shrunk.text, "c");
        assert_eq!(shrunk.style, override_style);
    }

    #[test]
    fn shrink_to_fit_passthrough_keeps_inner_style_when_no_user_override() {
        // The `None` arm of `match self.user_style` must pass the
        // inner shrunk render through unchanged. A regression that
        // unconditionally applies a default style would clobber the
        // inner segment's role (e.g. `git_branch`'s `Role::Accent`
        // would silently drop on the compact path for any user
        // without a configured style override).
        struct ShrinkableWithRole;
        impl Segment for ShrinkableWithRole {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(Some(RenderedSegment::new("full").with_role(Role::Accent)))
            }
            fn shrink_to_fit(
                &self,
                _: &DataContext,
                _: &RenderContext,
                _target: u16,
            ) -> Option<RenderedSegment> {
                Some(RenderedSegment::new("c").with_role(Role::Accent))
            }
        }
        // No `with_user_style` call — wrapper carries no override.
        let wrapped = OverriddenSegment::new(Box::new(ShrinkableWithRole));
        let shrunk = wrapped
            .shrink_to_fit(&stub_ctx(), &stub_rc(), 10)
            .expect("inner returned compact form");
        assert_eq!(shrunk.style.role, Some(Role::Accent));
    }

    #[test]
    fn shrink_to_fit_passthrough_returns_none_when_inner_declines() {
        // Default trait impl returns None; the wrapper must forward
        // None unchanged rather than emit a stub render of its own.
        struct Plain;
        impl Segment for Plain {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(Some(RenderedSegment::new("plain")))
            }
        }
        let wrapped =
            OverriddenSegment::new(Box::new(Plain)).with_user_style(Style::role(Role::Primary));
        assert!(wrapped
            .shrink_to_fit(&stub_ctx(), &stub_rc(), 100)
            .is_none());
    }

    #[test]
    fn shrink_to_fit_reserves_icon_width_so_compact_form_fits_target() {
        // The icon is prepended AFTER the inner compacts, so the wrapper
        // reserves its width up front. Without that, a segment that fills
        // its budget overflows once `"{icon} "` is added and the layout
        // engine drops it instead of showing the compact form.
        struct FillsTarget;
        impl Segment for FillsTarget {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(Some(RenderedSegment::new("full")))
            }
            fn shrink_to_fit(
                &self,
                _: &DataContext,
                _: &RenderContext,
                target: u16,
            ) -> Option<RenderedSegment> {
                Some(RenderedSegment::new("x".repeat(target as usize)))
            }
        }
        let wrapped = OverriddenSegment::new(Box::new(FillsTarget)).with_icon("I".to_string());
        let shrunk = wrapped
            .shrink_to_fit(&stub_ctx(), &stub_rc(), 10)
            .expect("compact form fits");
        assert!(
            shrunk.width <= 10,
            "icon-prefixed compact render must fit target, got width {}",
            shrunk.width
        );
        assert!(shrunk.text.starts_with("I "));
        // When even the icon prefix can't fit, decline rather than overflow.
        assert!(wrapped.shrink_to_fit(&stub_ctx(), &stub_rc(), 1).is_none());
    }

    #[test]
    fn icon_override_strips_control_chars() {
        // A user-config icon is untrusted input; the prepend goes through
        // `from_parts` (no built-in sanitization), so control chars must
        // be stripped here or a config could smuggle terminal escapes.
        struct Plain;
        impl Segment for Plain {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(Some(RenderedSegment::new("text")))
            }
        }
        let wrapped = OverriddenSegment::new(Box::new(Plain)).with_icon("\u{1b}[2Jx".to_string());
        let r = wrapped
            .render(&stub_ctx(), &stub_rc())
            .unwrap()
            .expect("visible");
        assert!(
            !r.text.contains('\u{1b}'),
            "ESC must be stripped from icon, got {:?}",
            r.text
        );

        // An empty override is the disable form: no icon, no leading space.
        let disabled = OverriddenSegment::new(Box::new(Plain)).with_icon(String::new());
        let d = disabled
            .render(&stub_ctx(), &stub_rc())
            .unwrap()
            .expect("visible");
        assert_eq!(d.text, "text");
    }

    #[test]
    fn segments_declare_their_default_icons() {
        // Guards the shipped DEFAULT_ICON codepoints, especially for
        // git_branch and the rate_limit family whose `assemble`/`wrap`
        // were reworked by the unify (icon now sourced via the generic
        // wrapper, not the segment's own formatter).
        use crate::segments::{
            context_bar::ContextBarSegment,
            git_branch::GitBranchSegment,
            rate_limit::{
                RateLimit5hResetSegment, RateLimit5hSegment, RateLimit7dResetSegment,
                RateLimit7dSegment,
            },
            session_duration::SessionDurationSegment,
        };
        assert_eq!(
            GitBranchSegment::default().defaults().icon,
            Some("\u{f126}")
        );
        assert_eq!(
            ContextBarSegment::default().defaults().icon,
            Some("\u{f035b}")
        );
        assert_eq!(SessionDurationSegment.defaults().icon, Some("\u{f252}"));
        assert_eq!(
            RateLimit5hSegment::default().defaults().icon,
            Some("\u{f017}")
        );
        assert_eq!(
            RateLimit7dSegment::default().defaults().icon,
            Some("\u{f073}")
        );
        assert_eq!(
            RateLimit5hResetSegment::default().defaults().icon,
            Some("\u{21bb}")
        );
        assert_eq!(
            RateLimit7dResetSegment::default().defaults().icon,
            Some("\u{21bb}")
        );
    }

    #[test]
    fn from_spans_concatenates_text_and_merges_adjacent_same_style() {
        // Two Primary spans flanking an Accent span: the Primary runs
        // stay distinct (the Accent breaks the run), and text is the
        // straight concatenation driving layout width.
        let seg = RenderedSegment::from_spans([
            StyledRun::new("ab", Style::role(Role::Primary)),
            StyledRun::new("cd", Style::role(Role::Primary)),
            StyledRun::new("ef", Style::role(Role::Accent)),
        ]);
        assert_eq!(seg.text(), "abcdef");
        assert_eq!(seg.width(), 6);
        let spans = seg.spans().expect("spans present");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text(), "abcd");
        assert_eq!(spans[0].style().role, Some(Role::Primary));
        assert_eq!(spans[1].text(), "ef");
        assert_eq!(spans[1].style().role, Some(Role::Accent));
    }

    #[test]
    fn from_spans_skips_empty_spans_and_sanitizes_control_chars() {
        // One span empties out and the other sanitizes to "ab"; a single
        // effective run folds to None with its style as the whole-segment
        // style (see from_spans_single_run_folds_to_none).
        let seg = RenderedSegment::from_spans([
            StyledRun::new("", Style::role(Role::Primary)),
            StyledRun::new("a\u{7}b", Style::role(Role::Accent)),
        ]);
        assert_eq!(seg.text(), "ab");
        assert!(seg.spans().is_none());
        assert_eq!(seg.style().role, Some(Role::Accent));
    }

    #[test]
    fn from_spans_single_run_folds_to_none_lifting_its_style() {
        // A lone effective run isn't "multi-color"; it folds to None so
        // `Some` always means >=2 distinct-style runs, and the run's
        // style becomes the whole-segment style (matching what the normal
        // and truncation render paths both emit).
        let single =
            RenderedSegment::from_spans([StyledRun::new("ab", Style::role(Role::Primary))]);
        assert!(single.spans().is_none());
        assert_eq!(single.text(), "ab");
        assert_eq!(single.style().role, Some(Role::Primary));

        // Multiple spans that coalesce to one style also fold.
        let coalesced = RenderedSegment::from_spans([
            StyledRun::new("ab", Style::role(Role::Primary)),
            StyledRun::new("cd", Style::role(Role::Primary)),
        ]);
        assert!(coalesced.spans().is_none());
        assert_eq!(coalesced.text(), "abcd");
        assert_eq!(coalesced.style().role, Some(Role::Primary));
    }

    #[test]
    fn from_spans_empty_input_normalizes_to_none() {
        // All-empty input → `None`, not `Some(vec![])`, so `Some` always
        // means multi-color and an empty segment compares equal to a
        // bare single-run one.
        let empty = RenderedSegment::from_spans([] as [StyledRun; 0]);
        assert!(empty.spans().is_none());
        assert_eq!(empty.text(), "");
        let all_filtered =
            RenderedSegment::from_spans([StyledRun::new("", Style::role(Role::Primary))]);
        assert!(all_filtered.spans().is_none());
    }

    #[test]
    fn from_spans_does_not_merge_same_role_different_modifiers() {
        // Coalescing keys on full `Style` equality, not role alone: two
        // Primary spans differing by `bold` must stay distinct.
        let bold = Style {
            bold: true,
            ..Style::role(Role::Primary)
        };
        let seg = RenderedSegment::from_spans([
            StyledRun::new("ab", Style::role(Role::Primary)),
            StyledRun::new("cd", bold),
        ]);
        let spans = seg.spans().expect("spans present");
        assert_eq!(spans.len(), 2);
        assert!(!spans[0].style().bold);
        assert!(spans[1].style().bold);
    }

    #[test]
    fn with_icon_prefix_prepends_separate_span_when_style_differs() {
        // First span's style (Muted) differs from the whole-segment
        // fallback (Success), so the icon prepends as its own span.
        let seg = RenderedSegment::from_spans([
            StyledRun::new("AB", Style::role(Role::Muted)),
            StyledRun::new("CD", Style::role(Role::Success)),
        ])
        .with_role(Role::Success)
        .with_icon_prefix("I");
        assert_eq!(seg.text(), "I ABCD");
        let spans = seg.spans().expect("spans present");
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].text(), "I ");
        assert_eq!(spans[0].style().role, Some(Role::Success));
        assert_eq!(spans[1].text(), "AB");
    }

    #[test]
    fn with_icon_prefix_coalesces_into_first_span_when_style_matches() {
        // First span shares the whole-segment fallback style (Success),
        // so the icon merges into it rather than emitting a redundant
        // same-style run.
        let seg = RenderedSegment::from_spans([
            StyledRun::new("AB", Style::role(Role::Success)),
            StyledRun::new("CD", Style::role(Role::Muted)),
        ])
        .with_role(Role::Success)
        .with_icon_prefix("I");
        assert_eq!(seg.text(), "I ABCD");
        let spans = seg.spans().expect("spans present");
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text(), "I AB");
        assert_eq!(spans[0].style().role, Some(Role::Success));
        assert_eq!(spans[1].text(), "CD");
    }

    #[test]
    fn icon_prefixed_spanned_segment_degrades_on_truncation() {
        // After the icon span is inserted, a width-bound truncation must
        // drop ALL spans (icon included) and keep text/width authoritative.
        let seg = RenderedSegment::from_spans([
            StyledRun::new("ABCD", Style::role(Role::Success)),
            StyledRun::new("EFGH", Style::role(Role::Muted)),
        ])
        .with_role(Role::Success)
        .with_icon_prefix("I");
        assert!(seg.spans().is_some());
        let out = crate::layout::truncate_to(seg, 4);
        assert!(out.spans().is_none());
        assert_eq!(out.text(), "I A…");
        assert_eq!(out.style().role, Some(Role::Success));
    }

    #[test]
    fn without_spans_collapses_to_single_run() {
        let seg = RenderedSegment::from_spans([
            StyledRun::new("AB", Style::role(Role::Success)),
            StyledRun::new("CD", Style::role(Role::Muted)),
        ])
        .without_spans();
        assert!(seg.spans().is_none());
        assert_eq!(seg.text(), "ABCD");
    }

    #[test]
    fn user_style_override_flattens_spans_then_icon_prepends() {
        // A spanned segment + a user style override: the override is
        // "wholesale-replaces", so the per-span coloring collapses to a
        // single run in the override style. The icon still prepends.
        struct Spanned;
        impl Segment for Spanned {
            fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
                Ok(Some(
                    RenderedSegment::from_spans([
                        StyledRun::new("AB", Style::role(Role::Success)),
                        StyledRun::new("CD", Style::role(Role::Muted)),
                    ])
                    .with_role(Role::Success),
                ))
            }
        }
        let wrapped = OverriddenSegment::new(Box::new(Spanned))
            .with_user_style(Style::role(Role::Accent))
            .with_icon("I".to_string());
        let r = wrapped
            .render(&stub_ctx(), &stub_rc())
            .unwrap()
            .expect("visible");
        assert_eq!(r.text(), "I ABCD");
        assert!(r.spans().is_none(), "user override must flatten spans");
        assert_eq!(r.style().role, Some(Role::Accent));
    }

    fn stub_ctx() -> DataContext {
        use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
        use std::path::PathBuf;
        use std::sync::Arc;
        DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: Some(ModelInfo {
                display_name: "Claude".into(),
            }),
            workspace: Some(WorkspaceInfo {
                project_dir: PathBuf::from("/repo/linesmith"),
                git_worktree: None,
            }),
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    fn stub_rc() -> RenderContext {
        RenderContext::new(80)
    }
}
