use super::*;
use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
use crate::theme;
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

/// Stub `Segment` for layout tests that build [`LayoutItem`] literals
/// directly. The reflow loop's `shrink_to_fit` callback gets the
/// default `None`, so layout tests focused on priority-drop /
/// separators / truncatable behavior don't need to mint a fresh
/// segment per case.
struct NoopSegment;
impl Segment for NoopSegment {
    fn render(&self, _ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        Ok(None)
    }
}
static NOOP: NoopSegment = NoopSegment;
fn noop_segment() -> &'static dyn Segment {
    &NOOP
}

/// Stable id shared by layout test fixtures. Tests asserting distinct ids
/// per slot must define their own statics; copy-pasting `TEST_SEG_ID`
/// across two slots would compare `"test"` to `"test"` and silently pass.
static TEST_SEG_ID: Cow<'static, str> = Cow::Borrowed("test");

fn empty_ctx() -> DataContext {
    DataContext::new(StatusContext {
        tool: Tool::ClaudeCode,
        model: Some(ModelInfo {
            display_name: "X".into(),
        }),
        workspace: Some(WorkspaceInfo {
            project_dir: PathBuf::from("/"),
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

fn empty_rc() -> RenderContext {
    RenderContext::new(80)
}

/// Build a [`LayoutItem::Segment`] for a stub render. Most layout
/// tests use this; pair adjacent calls with [`space`], [`pl`], or
/// [`lit`] to interleave separator items the way the production
/// builder does.
fn item(text: &str, priority: u8) -> LayoutItem<'static> {
    LayoutItem::Segment(SegmentEntry {
        id: &TEST_SEG_ID,
        rendered: RenderedSegment::new(text),
        defaults: SegmentDefaults::with_priority(priority),
        segment: noop_segment(),
    })
}

fn space() -> LayoutItem<'static> {
    LayoutItem::Separator(Separator::Space)
}

fn pl() -> LayoutItem<'static> {
    LayoutItem::Separator(Separator::powerline())
}

fn lit(s: &'static str) -> LayoutItem<'static> {
    LayoutItem::Separator(Separator::Literal(Cow::Borrowed(s)))
}

fn none_sep() -> LayoutItem<'static> {
    LayoutItem::Separator(Separator::None)
}

/// Build a `Vec<LayoutItem>` with [`Separator::Space`] interleaved
/// between every adjacent `(text, priority)` pair.
fn spaced(specs: &[(&str, u8)]) -> Vec<LayoutItem<'static>> {
    interleaved(specs, space)
}

/// Build a `Vec<LayoutItem>` with [`Separator::powerline()`]
/// interleaved between every adjacent `(text, priority)` pair.
fn pl_spec(specs: &[(&str, u8)]) -> Vec<LayoutItem<'static>> {
    interleaved(specs, pl)
}

fn interleaved(
    specs: &[(&str, u8)],
    mut sep: impl FnMut() -> LayoutItem<'static>,
) -> Vec<LayoutItem<'static>> {
    let mut out = Vec::with_capacity(specs.len().saturating_mul(2));
    for (i, &(text, priority)) in specs.iter().enumerate() {
        out.push(item(text, priority));
        if i + 1 < specs.len() {
            out.push(sep());
        }
    }
    out
}

/// Wrap `segments` as a [`LineItem`] sequence with `sep` interleaved
/// between adjacent segments. For tests that drive the public render
/// entry points (`render_with_warn`, `render_to_runs`). Synthesizes
/// stable test ids (`"seg0"`, `"seg1"`, ...) so the `LineItem`
/// addressing contract from ADR-0026 stays exercised; tests that need
/// a specific id should build the `LineItem` literal directly.
fn line_items_with(segments: Vec<Box<dyn Segment>>, sep: Separator) -> Vec<LineItem> {
    let n = segments.len();
    let mut out = Vec::with_capacity(n.saturating_mul(2));
    for (i, segment) in segments.into_iter().enumerate() {
        out.push(LineItem::Segment {
            id: std::borrow::Cow::Owned(format!("seg{i}")),
            segment,
        });
        if i + 1 < n {
            out.push(LineItem::Separator(sep.clone()));
        }
    }
    out
}

fn line_items_spaced(segments: Vec<Box<dyn Segment>>) -> Vec<LineItem> {
    line_items_with(segments, Separator::Space)
}

/// Test helper: exercise `render_items` with the default theme and
/// no color capability so output is plain text — the invariant most
/// layout tests actually care about (priority-drop, separators,
/// truncation behavior) is independent of theming.
fn render_plain(items: Vec<LayoutItem<'_>>, terminal_width: u16) -> String {
    render_items(
        items,
        &empty_ctx(),
        &empty_rc(),
        terminal_width,
        theme::default_theme(),
        theme::Capability::None,
    )
}

#[test]
fn render_items_wraps_each_styled_segment_under_palette16() {
    // Plain + styled + plain layout: the styled one gets SGR
    // wrapping, the plain ones pass through. Confirms the layout
    // emits SGR *per segment* rather than globally, so decorations
    // don't leak across separators.
    use crate::theme::Role;
    let items = vec![
        item("a", 10),
        space(),
        LayoutItem::Segment(SegmentEntry {
            id: &TEST_SEG_ID,
            rendered: RenderedSegment::new("b").with_role(Role::Warning),
            defaults: SegmentDefaults::with_priority(10),
            segment: noop_segment(),
        }),
        space(),
        item("c", 10),
    ];
    let out = render_items(
        items,
        &empty_ctx(),
        &empty_rc(),
        100,
        theme::default_theme(),
        theme::Capability::Palette16,
    );
    // Warning → BrightYellow (SGR 93) on the default theme.
    assert_eq!(out, "a \x1b[93mb\x1b[0m c");
}

#[test]
fn total_width_sums_all_layout_items() {
    let items = spaced(&[("ab", 10), ("cd", 10), ("ef", 10)]);
    // widths 2+1+2+1+2 = 8.
    assert_eq!(total_width(&items), 8);
}

#[test]
fn total_width_zero_for_empty() {
    assert_eq!(total_width(&[]), 0);
}

#[test]
fn total_width_single_segment_has_no_separator() {
    let items = vec![item("abcde", 10)];
    assert_eq!(total_width(&items), 5);
}

#[test]
fn no_width_pressure_renders_all_with_separators() {
    let items = spaced(&[("one", 10), ("two", 20), ("three", 30)]);
    assert_eq!(render_plain(items, 100), "one two three");
}

#[test]
fn drops_highest_priority_under_pressure() {
    let items = spaced(&[
        ("aaaa", 10),
        ("bbbb", 200), // highest priority → drops first
        ("cccc", 50),
    ]);
    // Full: 4+1+4+1+4 = 14. Budget 10 forces one drop.
    let out = render_plain(items, 10);
    assert!(!out.contains("bbbb"));
    assert!(out.contains("aaaa"));
    assert!(out.contains("cccc"));
}

#[test]
fn drops_in_descending_priority_order() {
    let items = spaced(&[
        ("one", 10),
        ("two", 200), // drops first
        ("three", 20),
        ("four", 150), // drops second
        ("five", 30),
    ]);
    // Full: 3+1+3+1+5+1+4+1+4 = 23. Budget 15 forces two drops.
    assert_eq!(render_plain(items, 15), "one three five");
}

#[test]
fn priority_zero_never_drops_even_over_budget() {
    let items = spaced(&[("aaaa", 0), ("bbbb", 0)]);
    let out = render_plain(items, 3);
    assert_eq!(out, "aaaa bbbb");
}

#[test]
fn priority_drop_recomputes_budget_with_powerline_separators() {
    // Three priority-0 segments at width 4 with powerline chevrons
    // between them: full = 4 + chev + 4 + chev + 4. The middle
    // segment is the only droppable one (priority 200); after one
    // drop the layout becomes "aaaa <chev> cccc" (4 + chev + 4)
    // and fits the budget without a second drop. A regression that
    // forgot to subtract a chevron's cells when its preceding
    // segment dropped would over-drop or mis-budget.
    let items = pl_spec(&[("aaaa", 0), ("bbbb", 200), ("cccc", 0)]);
    // Full = 4 + 3 + 4 + 3 + 4 = 18; after drop = 4 + 3 + 4 = 11.
    let out = render_plain(items, 14);
    assert!(out.contains("aaaa"));
    assert!(!out.contains("bbbb"));
    assert!(out.contains("cccc"));
    assert!(
        out.contains('\u{E0B0}'),
        "chevron survives the drop: {out:?}"
    );
}

#[test]
fn mix_drops_positives_keeps_zeros() {
    let items = spaced(&[("keep-me", 0), ("droppable", 200), ("sticky", 0)]);
    // Budget forces drop; only the priority-200 segment is eligible.
    let out = render_plain(items, 20);
    assert_eq!(out, "keep-me sticky");
}

#[test]
fn no_trailing_separator() {
    let items = spaced(&[("a", 10), ("b", 10)]);
    assert_eq!(render_plain(items, 100), "a b");
}

#[test]
fn empty_input_renders_empty_string() {
    assert_eq!(render_plain(vec![], 100), "");
}

#[test]
fn respects_inline_literal_separator() {
    let items = vec![item("a", 10), lit(" | "), item("b", 10)];
    assert_eq!(render_plain(items, 100), "a | b");
}

#[test]
fn render_inline_none_separator_collapses_neighbors() {
    // Inline `Separator::None` between two segments produces no run
    // and no width — neighbors collapse against each other.
    let items = vec![item("a", 10), none_sep(), item("b", 10)];
    assert_eq!(render_plain(items, 100), "ab");
}

// --- width-bounds helpers ------------------------------------------

#[test]
fn apply_width_bounds_drops_below_min() {
    let bounds = WidthBounds::new(5, 10);
    let rendered = RenderedSegment::new("abc"); // width 3
    assert!(apply_width_bounds(rendered, bounds).is_none());
}

#[test]
fn apply_width_bounds_truncates_above_max() {
    let bounds = WidthBounds::new(0, 5);
    let rendered = RenderedSegment::new("abcdefghij"); // width 10
    let truncated = apply_width_bounds(rendered, bounds).expect("truncated");
    assert_eq!(truncated.width, 5);
    assert!(truncated.text.ends_with('…'));
    assert_eq!(truncated.text, "abcd…");
}

#[test]
fn apply_width_bounds_passthrough_within_range() {
    let bounds = WidthBounds::new(2, 10);
    let original = RenderedSegment::new("hello");
    let result = apply_width_bounds(original.clone(), bounds).expect("kept");
    assert_eq!(result, original);
}

#[test]
fn apply_width_bounds_none_is_passthrough() {
    let original = RenderedSegment::new("anything");
    let result = apply_width_bounds(original.clone(), None).expect("kept");
    assert_eq!(result, original);
}

#[test]
fn truncate_to_zero_yields_empty() {
    let out = truncate_to(RenderedSegment::new("abc"), 0);
    assert_eq!(out.text, "");
    assert_eq!(out.width, 0);
}

#[test]
fn truncate_handles_wide_grapheme_without_splitting() {
    // The middle-dot is 1 cell; truncating "42% · 200k" (10 cells) to
    // 6 cells should yield "42% ·…" (5 cells of content + ellipsis).
    let bounds = WidthBounds::new(0, 6);
    let truncated =
        apply_width_bounds(RenderedSegment::new("42% · 200k"), bounds).expect("truncated");
    assert_eq!(truncated.text, "42% ·…");
    assert_eq!(truncated.width, 6);
}

#[test]
fn truncate_preserves_combining_mark_with_base() {
    // "é" is U+0065 U+0301 (2 code points, 1 grapheme, 1 cell).
    // `abéde` is 5 cells, truncate to 4 should yield `abé…`.
    let r = RenderedSegment::new("ab\u{65}\u{301}de");
    assert_eq!(r.width, 5);
    let out = truncate_to(r, 4);
    assert_eq!(out.text, "ab\u{65}\u{301}…");
    assert_eq!(out.width, 4);
}

#[test]
fn truncate_does_not_split_zwj_emoji_sequence() {
    // 👨‍👩‍👦 is a ZWJ sequence (5 code points, 1 grapheme, 2 cells).
    // Total "a👨‍👩‍👦b" = 1 + 2 + 1 = 4 cells. Truncating to 3 cells:
    // budget for content is 2; we can fit "a" (1 cell) then the ZWJ
    // family (2 cells) would exceed budget, so output is "a…".
    let text = "a\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F466}b";
    let r = RenderedSegment::new(text);
    let out = truncate_to(r, 3);
    assert_eq!(out.text, "a…");
    assert_eq!(out.width, 2);
}

#[test]
fn truncate_to_max_cells_one_emits_only_ellipsis() {
    let r = RenderedSegment::new("anything");
    let out = truncate_to(r, 1);
    assert_eq!(out.text, "…");
    assert_eq!(out.width, 1);
}

#[test]
fn priority_ties_drop_rightmost_first() {
    let items = spaced(&[("left", 200), ("mid", 50), ("right", 200)]);
    // Full: 4+1+3+1+5 = 14. Budget 10 forces one drop; tied priorities
    // on "left" and "right" — right drops first.
    assert_eq!(render_plain(items, 10), "left mid");
}

#[test]
fn separator_none_not_charged_to_budget() {
    // Inline Separator::None between b and c collapses them against
    // each other. Widths 1+1+1 = 3; separators: Space (1) between
    // 0–1, None (0) between 1–2. Total = 4. Budget 4 must keep
    // everything and emit "a bc".
    let items = vec![
        item("a", 200),
        space(),
        item("b", 200),
        none_sep(),
        item("c", 200),
    ];
    assert_eq!(render_plain(items, 4), "a bc");
}

#[test]
fn total_width_returns_u32_beyond_u16_range() {
    // Three segments at u16::MAX each plus two Space separators:
    // sum = 3 * u16::MAX + 2. Must not wrap u32.
    fn wide(text: String) -> LayoutItem<'static> {
        LayoutItem::Segment(SegmentEntry {
            id: &TEST_SEG_ID,
            rendered: RenderedSegment::new(text),
            defaults: SegmentDefaults::with_priority(10),
            segment: noop_segment(),
        })
    }
    let items = vec![
        wide("x".repeat(u16::MAX as usize)),
        space(),
        wide("x".repeat(u16::MAX as usize)),
        space(),
        wide("x".repeat(u16::MAX as usize)),
    ];
    assert_eq!(total_width(&items), 3 * u32::from(u16::MAX) + 2);
}

#[test]
fn all_priority_zero_keeps_every_segment_even_when_overfull() {
    let items = spaced(&[("aaa", 0), ("bbb", 0), ("ccc", 0)]);
    // Full 3+1+3+1+3 = 11. Budget 4 is nowhere near; all three stay.
    assert_eq!(render_plain(items, 4), "aaa bbb ccc");
}

// --- error handling ---

use crate::segments::{RenderResult, SegmentError};

struct StubSegment(RenderResult);

impl Segment for StubSegment {
    fn render(&self, _ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        match &self.0 {
            Ok(Some(r)) => Ok(Some(r.clone())),
            Ok(None) => Ok(None),
            Err(e) => Err(SegmentError::new(e.message.clone())),
        }
    }
}

#[test]
fn segment_error_is_logged_and_hides_segment() {
    let line = line_items_spaced(vec![
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("ok-before"))))),
        Box::new(StubSegment(Err(SegmentError::new("boom")))),
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("ok-after"))))),
    ]);
    let mut warnings = Vec::new();
    let items = collect_items_with(&line, &empty_ctx(), &empty_rc(), &mut |msg| {
        warnings.push(msg.to_string());
    });
    // The Err segment vanishes; the separator that flanked it goes
    // with it (both adjacency rules at once). Two surviving segments
    // separated by one surviving Space = 3 LayoutItems.
    assert_eq!(items.len(), 3);
    assert_eq!(segment_text(&items[0]), "ok-before");
    assert!(matches!(items[1], LayoutItem::Separator(_)));
    assert_eq!(segment_text(&items[2]), "ok-after");
    // The error is surfaced to stderr exactly once.
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("segment error"));
    assert!(warnings[0].contains("boom"));
}

#[test]
fn ok_none_is_silently_hidden() {
    let line = line_items_spaced(vec![
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("visible"))))),
        Box::new(StubSegment(Ok(None))),
    ]);
    let mut warnings = Vec::new();
    let items = collect_items_with(&line, &empty_ctx(), &empty_rc(), &mut |msg| {
        warnings.push(msg.to_string());
    });
    // Hidden segment plus its trailing separator both prune away.
    assert_eq!(items.len(), 1);
    assert_eq!(segment_text(&items[0]), "visible");
    assert!(warnings.is_empty());
}

/// `WidthEcho` emits whatever `terminal_width` it receives — used
/// by both reflow-threading tests below.
struct WidthEcho;
impl Segment for WidthEcho {
    fn render(&self, _ctx: &DataContext, rc: &RenderContext) -> RenderResult {
        Ok(Some(RenderedSegment::new(rc.terminal_width.to_string())))
    }
}

#[test]
fn render_context_threads_terminal_width_into_segments() {
    // Asserts the engine threads `RenderContext::new(42)` to the
    // segment unmodified at the `collect_items_with` layer —
    // pinning runtime behavior, since type-signature compilation
    // alone doesn't prove the value moves.
    let line = line_items_spaced(vec![Box::new(WidthEcho)]);
    let mut warnings = Vec::new();
    let rc = RenderContext::new(42);
    let items = collect_items_with(&line, &empty_ctx(), &rc, &mut |msg| {
        warnings.push(msg.to_string());
    });
    assert_eq!(items.len(), 1);
    assert_eq!(segment_text(&items[0]), "42");
}

#[test]
fn render_with_warn_constructs_render_context_from_terminal_width_arg() {
    // Pins the construction line in `render_with_warn`: the public
    // entrypoint must build `RenderContext::new(terminal_width)`
    // from its argument and pass it to segments. A regression that
    // hard-coded a default would slip past the
    // `collect_items_with`-only test above.
    let line = line_items_spaced(vec![Box::new(WidthEcho)]);
    let mut warnings = Vec::new();
    let out = render_with_warn(
        &line,
        &empty_ctx(),
        137,
        &mut |msg| warnings.push(msg.to_string()),
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    assert!(out.contains("137"), "got {out:?}");
}

// --- truncate-before-drop (reflow) ---

fn truncatable_item(text: &str, priority: u8) -> LayoutItem<'static> {
    LayoutItem::Segment(SegmentEntry {
        id: &TEST_SEG_ID,
        rendered: RenderedSegment::new(text),
        defaults: SegmentDefaults::with_priority(priority).with_truncatable(true),
        segment: noop_segment(),
    })
}

/// Shorthand for the field-reach `out.rendered.text` pattern used in
/// many `collect_items_with` assertions: matches the `Segment` variant
/// and panics with a descriptive message when the slot is a separator.
#[track_caller]
fn segment_text<'a>(item: &'a LayoutItem<'_>) -> &'a str {
    match item {
        LayoutItem::Segment(seg) => &seg.rendered.text,
        LayoutItem::Separator(_) => panic!("expected segment, got separator"),
    }
}

#[test]
fn reflow_truncates_highest_priority_before_dropping() {
    // Workspace-style scenario: long location plus a small fixed
    // segment. Without reflow the location would drop entirely;
    // with reflow it shrinks to fit so the user keeps orientation.
    let items = vec![
        truncatable_item("linesmith/very-long-feature-branch-name", 200),
        space(),
        item("Sonnet", 0),
    ];
    // Total: 39 + 1 + 6 = 46. Budget 30 → overflow 16.
    // Workspace truncates from 39 → 23 cells; result fits exactly.
    let out = render_plain(items, 30);
    assert!(out.starts_with("linesmith/very-long-fe"), "got {out:?}");
    assert!(out.ends_with("… Sonnet"), "got {out:?}");
    assert_eq!(text_width(&out), 30);
}

#[test]
fn reflow_drops_when_truncation_would_fall_below_floor() {
    // Budget so tight that truncating the workspace segment would
    // leave only the ellipsis (or less). Engine falls back to drop.
    let items = vec![
        truncatable_item("workspace-name", 200),
        space(),
        item("KEEP", 0),
    ];
    // Total: 14 + 1 + 4 = 19. Budget 4 → overflow 15.
    // workspace target = 14 - 15 < 0 → reflow returns None → drop.
    let out = render_plain(items, 4);
    assert_eq!(out, "KEEP");
}

fn truncatable_with_bounds(text: &str, priority: u8, bounds: WidthBounds) -> LayoutItem<'static> {
    LayoutItem::Segment(SegmentEntry {
        id: &TEST_SEG_ID,
        rendered: RenderedSegment::new(text),
        defaults: SegmentDefaults::with_priority(priority)
            .with_truncatable(true)
            .with_width(bounds),
        segment: noop_segment(),
    })
}

#[test]
fn reflow_respects_explicit_width_min_floor() {
    // Segment declares min=8; reflow must not shrink below that
    // even if a smaller truncation would fit the budget.
    let bounds = WidthBounds::new(8, u16::MAX).expect("valid");
    let items = vec![
        truncatable_with_bounds("abcdefghijklmnop", 200, bounds), // width 16
        space(),
        item("X", 0),
    ];
    // Total 16 + 1 + 1 = 18. Budget 10 → overflow 8 → target 8 ✓
    // (target equals floor; reflow proceeds).
    let out = render_plain(items, 10);
    assert!(out.contains('…'), "got {out:?}");
    assert!(out.ends_with(" X"), "got {out:?}");

    // Now budget 9 → overflow 9 → target 7 < floor 8 → drop.
    let items = vec![
        truncatable_with_bounds("abcdefghijklmnop", 200, bounds),
        space(),
        item("X", 0),
    ];
    let out = render_plain(items, 9);
    assert_eq!(out, "X");
}

#[test]
fn non_truncatable_drops_unchanged_under_pressure() {
    // Default `truncatable=false` keeps the legacy whole-segment
    // drop path so numeric segments don't suddenly start emitting
    // half-cut percentages or dollar figures.
    let items = spaced(&[("45% · 200k", 200), ("Sonnet", 0)]);
    // Total 10 + 1 + 6 = 17. Budget 10 → drop the wider one.
    let out = render_plain(items, 10);
    assert_eq!(out, "Sonnet");
}

#[test]
fn reflow_iterates_when_first_truncation_insufficient() {
    // Two truncatable segments, both same priority. After tying
    // priority we drop the right-most first; if that's still over
    // budget the loop comes back for the left one.
    let items = vec![
        truncatable_item("aaaaaaaaaa", 100),
        space(),
        truncatable_item("bbbbbbbbbb", 100),
        space(),
        item("KEEP", 0),
    ];
    // Total: 10 + 1 + 10 + 1 + 4 = 26. Budget 12 → overflow 14.
    // Right-most ("b...") is chosen first; truncating it to
    // 10-14 < 0 fails, so it drops. New total 10+1+4 = 15.
    // Loop continues; next iteration overflow=3, "a..." truncates
    // to 10-3 = 7 ("aaaaaa…").
    let out = render_plain(items, 12);
    assert_eq!(out, "aaaaaa… KEEP");
    assert_eq!(text_width(&out), 12);
}

#[test]
fn reflow_does_not_touch_priority_zero_even_when_truncatable() {
    // Priority 0 is "user said don't drop"; the reflow loop never
    // selects it (the existing droppable filter guards this).
    let items = vec![
        LayoutItem::Segment(SegmentEntry {
            id: &TEST_SEG_ID,
            rendered: RenderedSegment::new("untouchable-long-name"),
            defaults: SegmentDefaults::with_priority(0).with_truncatable(true),
            segment: noop_segment(),
        }),
        space(),
        item("Sonnet", 0),
    ];
    let out = render_plain(items, 5);
    assert_eq!(out, "untouchable-long-name Sonnet");
}

// --- shrink_to_fit (layout-pressure-aware compaction) ---

/// Stub segment whose `shrink_to_fit` returns the configured
/// compact form unconditionally — the engine's `target` check
/// gates whether it's accepted. Higher-than-default priority so
/// it's the one the reflow loop selects under pressure.
struct ShrinkableSegment {
    full: &'static str,
    compact: &'static str,
}
impl Segment for ShrinkableSegment {
    fn render(&self, _ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        Ok(Some(RenderedSegment::new(self.full)))
    }
    fn shrink_to_fit(
        &self,
        _ctx: &DataContext,
        _rc: &RenderContext,
        target: u16,
    ) -> Option<RenderedSegment> {
        let r = RenderedSegment::new(self.compact);
        (r.width <= target).then_some(r)
    }
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(200)
    }
}

/// Segment that always renders `text` and is priority-0 (never
/// dropped under pressure). Used as the "anchor" in shrink tests
/// so the reflow loop has only one droppable target.
struct AnchorSegment(&'static str);
impl Segment for AnchorSegment {
    fn render(&self, _ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        Ok(Some(RenderedSegment::new(self.0)))
    }
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(0)
    }
}

#[test]
fn shrink_to_fit_replaces_full_render_when_compact_form_fits() {
    // Engine-level pin: the reflow loop calls shrink_to_fit
    // before considering drop. Full = "longbranch * ↑2 ↓1" (18
    // cells), compact = "longbranch" (10 cells). KEEP is
    // priority-0 so it can't be the drop target — only the
    // shrinkable segment is eligible.
    let items = line_items_spaced(vec![
        Box::new(ShrinkableSegment {
            full: "longbranch * ↑2 ↓1",
            compact: "longbranch",
        }),
        Box::new(AnchorSegment("KEEP")),
    ]);
    let mut warnings = Vec::new();
    let line = render_with_warn(
        &items,
        &empty_ctx(),
        17,
        &mut |m| warnings.push(m.to_string()),
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    // Full 18 + sep 1 + KEEP 4 = 23. Budget 17 → overflow 6.
    // shrink target = 18 - 6 = 12. Compact "longbranch" (10)
    // fits → "longbranch KEEP" (15 cells).
    assert_eq!(line, "longbranch KEEP");
}

#[test]
fn shrink_to_fit_falls_back_to_drop_when_compact_form_too_wide() {
    // Compact form is wider than target → engine rejects it,
    // falls through to drop (segment isn't truncatable).
    let items = line_items_spaced(vec![
        Box::new(ShrinkableSegment {
            full: "longbranch",
            compact: "stilltoolongtruly",
        }),
        Box::new(AnchorSegment("X")),
    ]);
    let mut warnings = Vec::new();
    let line = render_with_warn(
        &items,
        &empty_ctx(),
        5,
        &mut |m| warnings.push(m.to_string()),
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    // Compact form 17 cells > target → reject → drop. Only the
    // anchor remains.
    assert_eq!(line, "X");
}

#[test]
fn shrink_to_fit_honors_configured_width_min_floor() {
    // A segment with `width.min = 8` configured: even though its
    // compact form is 5 cells (would otherwise fit a target ≥ 5),
    // the engine must reject the shrunk render and drop the
    // segment because the user contracted "at least 8 cells or
    // hide me." Pins parity with `apply_width_bounds` /
    // `try_reflow`.
    struct LowFloorShrink;
    impl Segment for LowFloorShrink {
        fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
            Ok(Some(RenderedSegment::new("longerprefix")))
        }
        fn shrink_to_fit(
            &self,
            _: &DataContext,
            _: &RenderContext,
            _target: u16,
        ) -> Option<RenderedSegment> {
            Some(RenderedSegment::new("five5"))
        }
        fn defaults(&self) -> SegmentDefaults {
            SegmentDefaults::with_priority(200)
                .with_width(WidthBounds::new(8, u16::MAX).expect("valid"))
        }
    }
    let items = line_items_spaced(vec![Box::new(LowFloorShrink), Box::new(AnchorSegment("X"))]);
    let line = render_with_warn(
        &items,
        &empty_ctx(),
        7,
        &mut |_| {},
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    // shrunk would deliver 5 cells, but width.min=8 → rejected,
    // segment drops. Only anchor remains.
    assert_eq!(line, "X");
}

#[test]
fn shrink_to_fit_rejects_too_wide_response_and_drops() {
    // A misbehaving segment ignores `target` and emits a render
    // wider than the engine asked for. The engine must reject
    // the response (preserving the layout-fit invariant) and
    // fall through to drop. The contract violation also fires
    // `lsm_warn!` (visible on stderr during test runs); that
    // side effect isn't captured by the warn closure passed to
    // `render_with_warn`, which only carries segment-render
    // errors — asserting layout outcome is the testable
    // contract here.
    struct MisbehavingSegment;
    impl Segment for MisbehavingSegment {
        fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
            Ok(Some(RenderedSegment::new("longbranch")))
        }
        fn shrink_to_fit(
            &self,
            _: &DataContext,
            _: &RenderContext,
            _target: u16,
        ) -> Option<RenderedSegment> {
            Some(RenderedSegment::new("stilltoolongtruly"))
        }
        fn defaults(&self) -> SegmentDefaults {
            SegmentDefaults::with_priority(200)
        }
    }
    let items = line_items_spaced(vec![
        Box::new(MisbehavingSegment),
        Box::new(AnchorSegment("X")),
    ]);
    let line = render_with_warn(
        &items,
        &empty_ctx(),
        5,
        &mut |_| {},
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    assert_eq!(line, "X");
}

#[test]
fn shrink_to_fit_runs_before_truncatable_end_ellipsis() {
    // A segment that's both truncatable AND has shrink_to_fit:
    // segment-side intelligence wins. The compact form replaces
    // the full render before generic end-ellipsis fires.
    struct DualSegment;
    impl Segment for DualSegment {
        fn render(&self, _ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
            Ok(Some(RenderedSegment::new("longprefix-with-tail")))
        }
        fn shrink_to_fit(
            &self,
            _ctx: &DataContext,
            _rc: &RenderContext,
            target: u16,
        ) -> Option<RenderedSegment> {
            let r = RenderedSegment::new("longprefix");
            (r.width <= target).then_some(r)
        }
        fn defaults(&self) -> SegmentDefaults {
            SegmentDefaults::with_priority(200).with_truncatable(true)
        }
    }
    let items = line_items_spaced(vec![
        Box::new(DualSegment),
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("X"))))),
    ]);
    let mut warnings = Vec::new();
    let line = render_with_warn(
        &items,
        &empty_ctx(),
        13,
        &mut |m| warnings.push(m.to_string()),
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    // Full = 20, X = 1, separator = 1 → total 22. Budget 13 →
    // overflow 9. shrink target = 20 - 9 = 11. Compact
    // "longprefix" (10) fits → "longprefix X" (12 cells).
    // No "…" appears because shrink_to_fit ran first.
    assert!(line.contains("longprefix"), "got {line:?}");
    assert!(!line.contains('…'), "no end-ellipsis: {line:?}");
}

// --- render_to_runs ---------------------------------------------------

#[test]
fn render_to_runs_empty_input_yields_no_runs() {
    let items: Vec<LineItem> = vec![];
    let runs = render_to_runs(&items, &empty_ctx(), 100, &mut |_| {});
    assert!(runs.is_empty());
}

#[test]
fn render_to_runs_emits_segment_then_separator_then_segment() {
    // Neither segment requested a role, so all three emitted runs
    // carry Style::default().
    let items = line_items_spaced(vec![
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("a"))))),
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("b"))))),
    ]);
    let runs = render_to_runs(&items, &empty_ctx(), 100, &mut |_| {});
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0].text, "a");
    assert_eq!(runs[0].style, Style::default());
    assert_eq!(runs[1].text, " ");
    assert_eq!(runs[1].style, Style::default());
    assert_eq!(runs[2].text, "b");
    assert_eq!(runs[2].style, Style::default());
}

#[test]
fn render_to_runs_preserves_segment_style() {
    // The styled segment's role lands on its run unchanged; the
    // TUI consumer maps role → ratatui Color, so anything dropped
    // here would silently break themed preview.
    use crate::theme::Role;
    let items = line_items_spaced(vec![
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("plain"))))),
        Box::new(StubSegment(Ok(Some(
            RenderedSegment::new("warn").with_role(Role::Warning),
        )))),
    ]);
    let runs = render_to_runs(&items, &empty_ctx(), 100, &mut |_| {});
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[2].text, "warn");
    assert_eq!(runs[2].style.role, Some(Role::Warning));
}

#[test]
fn render_to_runs_skips_separator_none_between_segments() {
    // `Separator::None` is "no gap"; the runs view skips it
    // entirely so consumers don't have to filter empty-text runs.
    let items = line_items_with(
        vec![
            Box::new(StubSegment(Ok(Some(RenderedSegment::with_separator(
                "a",
                Separator::None,
            ))))),
            Box::new(StubSegment(Ok(Some(RenderedSegment::new("b"))))),
        ],
        Separator::Space,
    );
    // The plugin per-render override on segment "a" replaces the
    // inline Space with Separator::None at that boundary.
    let runs = render_to_runs(&items, &empty_ctx(), 100, &mut |_| {});
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].text, "a");
    assert_eq!(runs[1].text, "b");
}

#[test]
fn render_to_runs_drops_segments_under_width_pressure() {
    // The runs view reflects post-layout state: dropped segments
    // produce no run, and the separator that would have followed
    // a dropped segment also vanishes.
    let items = line_items_spaced(vec![
        Box::new(StubSegment(Ok(Some(
            RenderedSegment::new("keep").with_role(crate::theme::Role::Primary),
        )))),
        Box::new(DroppableStub("droppable")),
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("anchor"))))),
    ]);
    // Total: 4 + 1 + 9 + 1 + 6 = 21. Budget 12 forces the
    // priority-200 middle segment to drop.
    let runs = render_to_runs(&items, &empty_ctx(), 12, &mut |_| {});
    let texts: Vec<&str> = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(texts, vec!["keep", " ", "anchor"]);
}

/// Build a styled multi-segment line for round-trip tests: roled
/// segments + a plain literal in the middle so both styled and
/// unstyled run paths are exercised.
fn round_trip_line() -> Vec<LineItem> {
    use crate::theme::Role;
    line_items_spaced(vec![
        Box::new(StubSegment(Ok(Some(
            RenderedSegment::new("ctx").with_role(Role::Info),
        )))),
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("|"))))),
        Box::new(StubSegment(Ok(Some(
            RenderedSegment::new("err").with_role(Role::Error),
        )))),
    ])
}

fn round_trip_assert(terminal_width: u16, capability: theme::Capability, hyperlinks: bool) {
    let items = round_trip_line();
    let direct = render_with_warn(
        &items,
        &empty_ctx(),
        terminal_width,
        &mut |_| {},
        theme::default_theme(),
        capability,
        hyperlinks,
    );
    let runs = render_to_runs(&items, &empty_ctx(), terminal_width, &mut |_| {});
    let recomposed = runs_to_ansi(&runs, theme::default_theme(), capability, hyperlinks);
    assert_eq!(
        direct, recomposed,
        "cap={capability:?} width={terminal_width} hyperlinks={hyperlinks}"
    );
}

#[test]
fn render_to_runs_then_runs_to_ansi_matches_render_with_warn() {
    // Round-trip pin: `render_to_runs` → `runs_to_ansi` must match
    // `render_with_warn` byte-for-byte. The contract that lets
    // `render_with_warn` stay a thin wrapper.
    round_trip_assert(100, theme::Capability::Palette16, false);
}

#[test]
fn render_to_runs_round_trip_holds_under_capability_none() {
    // No-color path: every run goes through the `open.is_empty()`
    // branch in `runs_to_ansi`. A future change to `sgr_open`
    // returning a non-empty string for `Capability::None` would
    // silently leak escapes; this pins it.
    round_trip_assert(100, theme::Capability::None, false);
}

#[test]
fn render_to_runs_round_trip_holds_under_width_pressure() {
    // Width pressure forces `apply_layout` to drop a segment;
    // both emit paths must produce the same post-drop output.
    // `round_trip_segments` totals 9 cells; budget 5 drops the
    // rightmost priority-128 tie ("err"), leaving "ctx |".
    round_trip_assert(5, theme::Capability::Palette16, false);
}

#[test]
fn render_to_runs_round_trip_holds_with_hyperlinks_enabled() {
    // The `hyperlinks` bool must thread identically through both
    // emit paths. `round_trip_segments` carries no hyperlinks
    // today, so the equivalence is structural — a regression
    // where one path silently dropped the bool would still match
    // here. Adding a hyperlinked segment to the round-trip set
    // is a follow-up; this test names the bool-thread contract.
    round_trip_assert(100, theme::Capability::Palette16, true);
}

#[test]
fn render_to_runs_with_one_survivor_emits_no_trailing_separator() {
    // Drop pressure leaves a single segment. `pop_trailing_separator`
    // (run when the priority-drop loop removes a segment whose
    // right-edge separator was already adjacent) must remove the
    // surviving separator so the runs view doesn't end with a stray
    // " " run.
    let items = line_items_spaced(vec![
        Box::new(StubSegment(Ok(Some(RenderedSegment::new("a"))))),
        Box::new(DroppableStub("droppable")),
    ]);
    // Total: 1 + 1 + 9 = 11. Budget 1 drops the priority-200
    // segment; "a" survives alone with no trailing separator.
    let runs = render_to_runs(&items, &empty_ctx(), 1, &mut |_| {});
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].text, "a");
}

#[test]
fn render_to_runs_emits_powerline_chevron_with_muted_role() {
    // Pins both the glyph and the `Role::Muted` style — a future
    // bg-transition restyle should land as an intentional update
    // to this assertion.
    use crate::theme::Role;
    let items = line_items_with(
        vec![
            Box::new(StubSegment(Ok(Some(
                RenderedSegment::new("a").with_role(Role::Primary),
            )))),
            Box::new(StubSegment(Ok(Some(RenderedSegment::new("b"))))),
        ],
        Separator::powerline(),
    );
    let runs = render_to_runs(&items, &empty_ctx(), 100, &mut |_| {});
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[1].text, " \u{E0B0} ");
    assert_eq!(runs[1].style.role, Some(Role::Muted));
}

#[test]
fn powerline_separator_emits_padded_chevron_with_correct_width() {
    // The chevron is a Nerd Font glyph in the private-use range;
    // unicode-width doesn't know its cell count, so the layout's
    // total_width math depends on `Separator::width()`'s answer.
    // Pin both the emitted text (single-space pad on each side of
    // the chevron) and the reported width (1-cell chevron + 2
    // padding cells = 3).
    assert_eq!(Separator::powerline().width(), 3);
    assert_eq!(Separator::powerline().text(), " \u{E0B0} ");
}

#[test]
fn powerline_chevrons_are_charged_to_total_width_in_layout() {
    // total_width sums every layout item. Three priority-0 segments
    // at width 4 plus two powerline chevrons between them = 4 + chev
    // + 4 + chev + 4. A regression that stopped counting Powerline
    // width would silently push lines past budget. Computed (not
    // hardcoded) so a future change to the chevron's padding-cell
    // count fails this assertion at the right line.
    let items = pl_spec(&[("aaaa", 0), ("bbbb", 0), ("cccc", 0)]);
    let chev = u32::from(Separator::powerline().width());
    assert_eq!(total_width(&items), 4 + chev + 4 + chev + 4);
}

#[test]
fn render_with_warn_emits_powerline_chevron_wrapped_in_muted_sgr() {
    // End-to-end pin: drive two segments through `render_with_warn`
    // under Palette16 with powerline separators between them. The
    // output must contain the padded chevron wrapped in *some* SGR
    // open + reset; the exact bytes are computed from
    // `theme::sgr_open` for the Muted role on the default theme,
    // so this test adapts if the default theme's Muted color is
    // ever retuned. Decouples "chevron emits styled" from "the
    // exact ANSI code for Muted on theme X."
    struct RoledSeg(&'static str, theme::Role);
    impl Segment for RoledSeg {
        fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
            Ok(Some(RenderedSegment::new(self.0).with_role(self.1)))
        }
        fn defaults(&self) -> SegmentDefaults {
            SegmentDefaults::with_priority(10)
        }
    }
    let items = line_items_with(
        vec![
            Box::new(RoledSeg("a", theme::Role::Primary)),
            Box::new(RoledSeg("b", theme::Role::Info)),
        ],
        Separator::powerline(),
    );
    let line = render_with_warn(
        &items,
        &empty_ctx(),
        100,
        &mut |_| {},
        theme::default_theme(),
        theme::Capability::Palette16,
        false,
    );
    let muted_sgr = theme::sgr_open(
        &Style::role(theme::Role::Muted),
        theme::default_theme(),
        theme::Capability::Palette16,
    );
    let expected = format!("{muted_sgr} \u{E0B0} \x1b[0m");
    assert!(
        line.contains(&expected),
        "padded chevron with Muted SGR not in line: {line:?} (expected substring: {expected:?})"
    );
}

#[test]
fn render_to_runs_emits_literal_separator_with_default_style() {
    // An inline `Separator::Literal(" | ")` becomes a separator run
    // with that exact text and Style::default() — separators don't
    // inherit segment styling, even when the flanking segment carries
    // a role.
    let items = line_items_with(
        vec![
            Box::new(StubSegment(Ok(Some(
                RenderedSegment::new("a").with_role(crate::theme::Role::Warning),
            )))),
            Box::new(StubSegment(Ok(Some(RenderedSegment::new("b"))))),
        ],
        Separator::Literal(Cow::Borrowed(" | ")),
    );
    let runs = render_to_runs(&items, &empty_ctx(), 100, &mut |_| {});
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[1].text, " | ");
    assert_eq!(runs[1].style, Style::default());
}

#[test]
fn runs_to_ansi_emits_osc8_around_styled_run_when_hyperlinks_supported() {
    // Pin the OSC 8 envelope and its order: the link wraps
    // *outside* the SGR pair so the link survives `sgr_reset`.
    // Bytes asserted explicitly so a future change to the OSC 8
    // emitter (BEL terminator, different escape) is caught.
    use crate::theme::Role;
    let runs = vec![StyledRun::new(
        "branch",
        Style::role(Role::Primary).with_hyperlink("https://example.com/b"),
    )];
    let out = runs_to_ansi(
        &runs,
        theme::default_theme(),
        theme::Capability::Palette16,
        true,
    );
    assert_eq!(
        out, "\x1b]8;;https://example.com/b\x1b\\\x1b[95mbranch\x1b[0m\x1b]8;;\x1b\\",
        "got {out:?}"
    );
}

#[test]
fn runs_to_ansi_drops_hyperlink_when_not_supported() {
    // `hyperlinks = false` must produce zero OSC 8 bytes; the
    // run still emits with its SGR styling. The URL is dropped
    // silently — capable terminals get the link, others get the
    // text.
    use crate::theme::Role;
    let runs = vec![StyledRun::new(
        "branch",
        Style::role(Role::Primary).with_hyperlink("https://example.com/b"),
    )];
    let out = runs_to_ansi(
        &runs,
        theme::default_theme(),
        theme::Capability::Palette16,
        false,
    );
    assert_eq!(out, "\x1b[95mbranch\x1b[0m");
    assert!(!out.contains("\x1b]8"), "no OSC 8: {out:?}");
}

#[test]
fn runs_to_ansi_emits_no_osc8_when_style_has_no_hyperlink() {
    // `hyperlinks = true` is permission, not obligation: a run
    // with `Style.hyperlink = None` emits no OSC 8 even when the
    // terminal supports it.
    let runs = vec![StyledRun::new("plain", Style::default())];
    let out = runs_to_ansi(&runs, theme::default_theme(), theme::Capability::None, true);
    assert_eq!(out, "plain");
    assert!(!out.contains("\x1b]8"), "no OSC 8: {out:?}");
}

#[test]
fn runs_to_ansi_emits_osc8_around_unstyled_run() {
    // An unstyled run with a hyperlink still gets OSC 8: the link
    // is independent of color/decoration. The text passes through
    // without an SGR pair.
    let runs = vec![StyledRun::new(
        "click",
        Style::default().with_hyperlink("https://example.com"),
    )];
    let out = runs_to_ansi(&runs, theme::default_theme(), theme::Capability::None, true);
    assert_eq!(out, "\x1b]8;;https://example.com\x1b\\click\x1b]8;;\x1b\\");
}

#[test]
fn osc8_pair_balanced_when_hyperlinked_run_is_truncated() {
    // Truncation rewrites the run's text; the OSC 8 wrapper sits
    // outside text in `runs_to_ansi`, so truncated text still
    // emits a balanced OSC 8 open/close. Pins the design
    // contract: hyperlinks live on `Style`, never in `text`, so
    // there's no escape-sequence inside the string for
    // truncation to split.
    let mut rendered = RenderedSegment::new("very-long-branch-name")
        .with_style(Style::default().with_hyperlink("https://example.com/branch"));
    rendered = truncate_to(rendered, 8);
    // Truncation produces "very-lo…" (7 graphemes + ellipsis = 8 cells).
    let runs = vec![StyledRun::new(
        rendered.text().to_string(),
        rendered.style.clone(),
    )];
    let out = runs_to_ansi(&runs, theme::default_theme(), theme::Capability::None, true);
    assert!(
        out.starts_with("\x1b]8;;https://example.com/branch\x1b\\"),
        "OSC 8 open present: {out:?}"
    );
    assert!(
        out.ends_with("\x1b]8;;\x1b\\"),
        "OSC 8 close present: {out:?}"
    );
    assert!(out.contains('…'), "truncation marker preserved: {out:?}");
    assert_eq!(
        out.matches("\x1b]8;;").count(),
        2,
        "exactly one open and one close: {out:?}"
    );
}

#[test]
fn osc8_pair_balanced_when_hyperlinked_run_truncated_to_zero() {
    // truncate_to(_, 0) yields empty text + preserved style. The
    // OSC 8 pair must still be balanced — emitting a half-open
    // envelope would break every later byte on the line.
    let rendered = RenderedSegment::new("anything")
        .with_style(Style::default().with_hyperlink("https://example.com"));
    let truncated = truncate_to(rendered, 0);
    let runs = vec![StyledRun::new(
        truncated.text().to_string(),
        truncated.style.clone(),
    )];
    let out = runs_to_ansi(&runs, theme::default_theme(), theme::Capability::None, true);
    assert_eq!(
        out, "\x1b]8;;https://example.com\x1b\\\x1b]8;;\x1b\\",
        "empty-text run still emits balanced OSC 8 pair: {out:?}"
    );
}

#[test]
fn runs_to_ansi_emits_independent_osc8_pairs_for_adjacent_hyperlinked_runs() {
    // Adjacent runs with different links must each get their own
    // open/close pair — no nesting, no leak across the boundary.
    // Pins the per-run scoping of OSC 8 emission.
    let runs = vec![
        StyledRun::new("a", Style::default().with_hyperlink("https://a.example")),
        StyledRun::new("b", Style::default().with_hyperlink("https://b.example")),
    ];
    let out = runs_to_ansi(&runs, theme::default_theme(), theme::Capability::None, true);
    assert_eq!(
        out,
        "\x1b]8;;https://a.example\x1b\\a\x1b]8;;\x1b\\\x1b]8;;https://b.example\x1b\\b\x1b]8;;\x1b\\"
    );
    assert_eq!(out.matches("\x1b]8;;").count(), 4, "two opens + two closes");
}

#[test]
fn push_osc8_open_strips_control_chars_from_url() {
    // Security regression: a URL with embedded ESC `\` would
    // close the OSC 8 envelope early, turning the rest of the
    // line into raw control sequences. `push_osc8_open` strips
    // control bytes before emit. The bare `\` survives but
    // cannot reconstitute a String Terminator without the
    // stripped ESC.
    let runs = vec![StyledRun::new(
        "x",
        Style::default().with_hyperlink("https://example.com\x1b\\evil\x07more"),
    )];
    let out = runs_to_ansi(&runs, theme::default_theme(), theme::Capability::None, true);
    // Exactly one OSC 8 open and one close — the embedded ESC `\`
    // can't smuggle a second close into the output.
    assert_eq!(
        out.matches("\x1b]8;;").count(),
        2,
        "exactly one pair: {out:?}"
    );
    assert!(!out.contains("\x1b\\evil"), "ESC \\ stripped: {out:?}");
    assert!(!out.contains('\x07'), "BEL stripped: {out:?}");
    assert!(
        out.contains("https://example.com\\evilmore"),
        "non-control chars survive: {out:?}"
    );
}

#[test]
fn push_osc8_open_strips_c1_string_terminator_and_nul() {
    // `char::is_control()` covers C0 (0x00-0x1F, 0x7F) and C1
    // (0x80-0x9F). The most plausible bypass via the C1 range is
    // 0x9C (single-byte ST in 8-bit terminals); NUL and DEL are
    // the other classics. Pin that all three are stripped so a
    // future change to the sanitizer can't quietly narrow the
    // filter.
    let runs = vec![StyledRun::new(
        "x",
        Style::default().with_hyperlink("https://a.example\x00b\x7fc\u{009C}d"),
    )];
    let out = runs_to_ansi(&runs, theme::default_theme(), theme::Capability::None, true);
    assert_eq!(out.matches("\x1b]8;;").count(), 2, "single pair: {out:?}");
    assert!(!out.contains('\x00'), "NUL stripped: {out:?}");
    assert!(!out.contains('\x7f'), "DEL stripped: {out:?}");
    assert!(!out.contains('\u{009C}'), "C1 ST stripped: {out:?}");
    assert!(out.contains("https://a.examplebcd"));
}

#[test]
fn runs_to_ansi_capability_none_emits_unwrapped_text() {
    // Pin the no-color emit path independent of layout: a run
    // with a styled role + Capability::None must produce zero
    // ANSI escapes. Catches a regression where `sgr_open` would
    // start emitting decoration codes for the no-color tier.
    use crate::theme::Role;
    let runs = vec![
        StyledRun::new("plain", Style::default()),
        StyledRun::new(" ", Style::default()),
        StyledRun::new("warn", Style::role(Role::Warning)),
    ];
    let out = runs_to_ansi(
        &runs,
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    assert_eq!(out, "plain warn");
    assert!(!out.contains('\x1b'), "unexpected ANSI escape: {out:?}");
}

/// Stub for the drop-under-pressure run test: priority-200 so it
/// becomes the layout's first drop target. `StubSegment`'s default
/// priority (128) wouldn't be eligible against the anchors.
struct DroppableStub(&'static str);
impl Segment for DroppableStub {
    fn render(&self, _: &DataContext, _: &RenderContext) -> RenderResult {
        Ok(Some(RenderedSegment::new(self.0)))
    }
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(200)
    }
}

#[test]
fn try_reflow_preserves_segment_id_reference() {
    // ADR-0026 contract: try_reflow rebuilds the SegmentEntry but
    // must thread the original id reference through, otherwise the
    // emit sites in apply_layout (lsm-b00q) would lose the user's
    // config name across reflow. Pointer-equality pins that the
    // borrow chain survives — a regression that re-borrows from a
    // different Cow (or worse, clones it) would fail here.
    static ALT_ID: Cow<'static, str> = Cow::Borrowed("alt");
    let entry = SegmentEntry {
        id: &ALT_ID,
        rendered: RenderedSegment::new("workspace-with-extra-content"),
        defaults: SegmentDefaults::with_priority(100).with_truncatable(true),
        segment: noop_segment(),
    };
    let original_id_ptr = std::ptr::from_ref(entry.id);
    let reflowed =
        super::try_reflow(&entry, 10).expect("reflow must succeed at target 18 / floor 2");
    assert!(
        std::ptr::eq(std::ptr::from_ref(reflowed.id), original_id_ptr),
        "try_reflow must preserve the id reference, not clone it",
    );
    assert_eq!(
        reflowed.id.as_ref(),
        "alt",
        "id content must survive — defense-in-depth alongside ptr::eq",
    );
}
