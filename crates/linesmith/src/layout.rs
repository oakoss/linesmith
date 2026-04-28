//! Layout engine. Takes a list of `Segment`s plus a `StatusContext` and
//! fits their renders into a terminal-width budget, dropping the
//! highest-priority (numerically largest) segments first — or, when a
//! segment opts in via `truncatable`, shrinking it to fit before drop.
//! Priority-0 segments are never dropped or truncated, even when that
//! overflows the budget.
//!
//! See `docs/specs/segment-system.md` §Layout algorithm.

use crate::data_context::DataContext;
use crate::segments::{
    text_width, RenderContext, RenderedSegment, Segment, SegmentDefaults, Separator, WidthBounds,
};
use crate::theme::{self, Capability, Theme};
use unicode_segmentation::UnicodeSegmentation;

/// Render `segments` for `ctx` within `terminal_width` cells. Returns the
/// final line without a trailing newline. Segment render errors go
/// through [`crate::lsm_error!`] so a broken segment always surfaces,
/// even under `LINESMITH_LOG=off` — a blank statusline with zero
/// diagnostic is a bad UX even when the user opted into quiet mode.
/// Output is unstyled (callers that want theming use
/// [`render_with_warn`] with their own closure).
#[must_use]
pub fn render(segments: &[Box<dyn Segment>], ctx: &DataContext, terminal_width: u16) -> String {
    let mut warn = |msg: &str| crate::lsm_error!("{msg}");
    render_with_warn(
        segments,
        ctx,
        terminal_width,
        &mut warn,
        theme::default_theme(),
        Capability::None,
    )
}

/// Same as [`render`] but routes segment render-error diagnostics
/// through `warn` and emits ANSI SGR around each segment per `theme`
/// and `capability`. Used by [`crate::run_with_context`] so `cli_main`
/// tests can capture segment errors alongside exit codes while the
/// render path picks up theme colors.
#[must_use]
pub fn render_with_warn(
    segments: &[Box<dyn Segment>],
    ctx: &DataContext,
    terminal_width: u16,
    warn: &mut dyn FnMut(&str),
    theme: &Theme,
    capability: Capability,
) -> String {
    let rc = RenderContext::new(terminal_width);
    let items = collect_items_with(segments, ctx, &rc, warn);
    render_items(items, ctx, &rc, terminal_width, theme, capability)
}

/// Rendered output paired with the defaults needed to place it (priority,
/// separator, bounds) and a back-reference to the segment so the reflow
/// loop can call `shrink_to_fit` without re-walking the input slice.
/// Bundled here so drop/emit passes don't re-query the trait.
struct Item<'a> {
    rendered: RenderedSegment,
    defaults: SegmentDefaults,
    segment: &'a dyn Segment,
}

fn collect_items_with<'a>(
    segments: &'a [Box<dyn Segment>],
    ctx: &DataContext,
    rc: &RenderContext,
    warn: &mut dyn FnMut(&str),
) -> Vec<Item<'a>> {
    segments
        .iter()
        .filter_map(|seg| {
            let defaults = seg.defaults();
            let rendered = match seg.render(ctx, rc) {
                Ok(Some(r)) => r,
                Ok(None) => return None,
                Err(err) => {
                    warn(&format!("segment error: {err}"));
                    return None;
                }
            };
            apply_width_bounds(rendered, defaults.width).map(|r| Item {
                rendered: r,
                defaults,
                segment: seg.as_ref(),
            })
        })
        .collect()
}

fn render_items(
    mut items: Vec<Item<'_>>,
    ctx: &DataContext,
    rc: &RenderContext,
    terminal_width: u16,
    theme: &Theme,
    capability: Capability,
) -> String {
    let budget = u32::from(terminal_width);
    loop {
        let total = total_width(&items);
        if total <= budget {
            break;
        }
        let Some(drop_idx) = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.defaults.priority > 0)
            .max_by_key(|(_, item)| item.defaults.priority)
            .map(|(i, _)| i)
        else {
            break;
        };
        let overflow = total - budget;
        // Try segment-side compaction first; the segment knows things
        // the engine doesn't (which decoration is signal-bearing,
        // which prefix to keep). Falls through to generic end-ellipsis
        // truncation only when shrink_to_fit declines.
        if let Some(shrunk) = try_shrink(&items[drop_idx], ctx, rc, overflow) {
            items[drop_idx].rendered = shrunk;
            continue;
        }
        if items[drop_idx].defaults.truncatable {
            if let Some(reflowed) = try_reflow(&items[drop_idx], overflow) {
                items[drop_idx] = reflowed;
                continue;
            }
        }
        items.remove(drop_idx);
    }

    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        let style = &item.rendered.style;
        let open = theme::sgr_open(style, theme, capability);
        if !open.is_empty() {
            out.push_str(&open);
            out.push_str(&item.rendered.text);
            out.push_str(theme::sgr_reset());
        } else {
            out.push_str(&item.rendered.text);
        }
        if i + 1 < items.len() {
            out.push_str(effective_separator(item).text());
        }
    }
    out
}

/// Sum of segment widths plus the separators that sit *between* segments
/// (no trailing separator). `u32` prevents `u16` overflow on many wide
/// segments.
fn total_width(items: &[Item<'_>]) -> u32 {
    if items.is_empty() {
        return 0;
    }
    let seg_sum: u32 = items.iter().map(|i| u32::from(i.rendered.width)).sum();
    let sep_sum: u32 = items
        .iter()
        .take(items.len() - 1)
        .map(|item| u32::from(effective_separator(item).width()))
        .sum();
    seg_sum + sep_sum
}

fn effective_separator<'i>(item: &'i Item<'_>) -> &'i Separator {
    item.rendered
        .right_separator
        .as_ref()
        .unwrap_or(&item.defaults.default_separator)
}

/// Applies `bounds`: under-min drops the segment, over-max truncates with
/// a trailing ellipsis and a recomputed width. `None` bounds is an
/// explicit passthrough — the segment carries no constraints.
fn apply_width_bounds(
    rendered: RenderedSegment,
    bounds: Option<WidthBounds>,
) -> Option<RenderedSegment> {
    let Some(bounds) = bounds else {
        return Some(rendered);
    };
    if rendered.width < bounds.min() {
        return None;
    }
    if rendered.width > bounds.max() {
        return Some(truncate_to(rendered, bounds.max()));
    }
    Some(rendered)
}

/// Shrink `item` by `overflow` cells so the layout fits, or return
/// `None` when the result would fall below `max(width.min, 2)` cells
/// (one content grapheme plus the ellipsis), so the caller can drop the
/// segment whole.
///
/// Subtracting exactly `overflow` lands total width on the budget so
/// the reflow loop exits on its next check; a wide grapheme straddling
/// the boundary may yield a slightly narrower result, which still
/// meets the `overflow` requirement.
fn try_reflow<'a>(item: &Item<'a>, overflow: u32) -> Option<Item<'a>> {
    let floor = item.defaults.width.map_or(2, |b| b.min().max(2));
    let cur = item.rendered.width;
    let target = u32::from(cur).checked_sub(overflow)?;
    let target_u16 = u16::try_from(target).ok()?;
    if target_u16 < floor {
        return None;
    }
    let truncated = truncate_to(item.rendered.clone(), target_u16);
    if truncated.width < floor {
        return None;
    }
    Some(Item {
        rendered: truncated,
        defaults: item.defaults.clone(),
        segment: item.segment,
    })
}

/// Ask the segment to produce a render at most `cur_width - overflow`
/// cells wide. Returns `None` when `shrink_to_fit` itself returns
/// `None` (default impl, or the segment declined). A segment that
/// returns `Some(r)` with `r.width > target` violates the documented
/// contract — the engine rejects the response (to preserve the
/// layout-fit invariant) and routes the violation through
/// [`crate::lsm_warn!`] so the misbehavior is visible to the segment
/// author. The caller falls through to `truncatable` end-ellipsis or
/// drop on any of these outcomes.
fn try_shrink(
    item: &Item<'_>,
    ctx: &DataContext,
    rc: &RenderContext,
    overflow: u32,
) -> Option<RenderedSegment> {
    let cur = item.rendered.width;
    // `cur < overflow` is reachable: one segment frequently can't
    // absorb the whole overflow alone (e.g. cost=6 when total
    // overshoots by 12). `checked_sub` returns `None` and the engine
    // drops the segment so the loop iterates with a smaller total.
    let target = u16::try_from(u32::from(cur).checked_sub(overflow)?).ok()?;
    // Honor the user's declared `width.min` floor on the shrunk
    // render the same way `apply_width_bounds` and `try_reflow` do —
    // a configured min is a contract that a too-narrow render is
    // worse than no render. No `+ 2` like `try_reflow`'s floor
    // because `shrink_to_fit` produces an arbitrary string, not
    // text + ellipsis.
    let min_floor = item.defaults.width.map_or(0, |b| b.min());
    if target < min_floor {
        return None;
    }
    let shrunk = item.segment.shrink_to_fit(ctx, rc, target)?;
    if shrunk.width > target {
        crate::lsm_warn!(
            "segment shrink_to_fit returned width {} > target {}; rejecting",
            shrunk.width,
            target,
        );
        return None;
    }
    if shrunk.width < min_floor {
        return None;
    }
    Some(shrunk)
}

/// Truncate `rendered` to at most `max_cells` terminal cells, appending
/// `…` (U+2026, 1 cell) as a continuation marker. Iterates by grapheme
/// cluster so combining marks, ZWJ sequences, and emoji stay intact.
pub(crate) fn truncate_to(rendered: RenderedSegment, max_cells: u16) -> RenderedSegment {
    if max_cells == 0 {
        return RenderedSegment::from_parts(
            String::new(),
            0,
            rendered.right_separator,
            rendered.style,
        );
    }
    // Reserve one cell for the ellipsis.
    let budget = max_cells.saturating_sub(1);
    let mut out = String::new();
    let mut used: u16 = 0;
    for cluster in rendered.text.graphemes(true) {
        let w = text_width(cluster);
        if used.saturating_add(w) > budget {
            break;
        }
        out.push_str(cluster);
        used = used.saturating_add(w);
    }
    out.push('…');
    RenderedSegment::from_parts(
        out,
        used.saturating_add(1),
        rendered.right_separator,
        rendered.style,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use crate::theme;
    use std::borrow::Cow;
    use std::path::PathBuf;
    use std::sync::Arc;

    /// Stub `Segment` for layout tests that build `Item` literals
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

    fn empty_ctx() -> DataContext {
        DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "X".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/"),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    fn empty_rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn item(text: &str, priority: u8) -> Item<'static> {
        Item {
            rendered: RenderedSegment::new(text),
            defaults: SegmentDefaults::with_priority(priority),
            segment: noop_segment(),
        }
    }

    /// Test helper: exercise `render_items` with the default theme and
    /// no color capability so output is plain text — the invariant most
    /// layout tests actually care about (priority-drop, separators,
    /// truncation behavior) is independent of theming.
    fn render_plain(items: Vec<Item<'_>>, terminal_width: u16) -> String {
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
            Item {
                rendered: RenderedSegment::new("a"),
                defaults: SegmentDefaults::with_priority(10),
                segment: noop_segment(),
            },
            Item {
                rendered: RenderedSegment::new("b").with_role(Role::Warning),
                defaults: SegmentDefaults::with_priority(10),
                segment: noop_segment(),
            },
            Item {
                rendered: RenderedSegment::new("c"),
                defaults: SegmentDefaults::with_priority(10),
                segment: noop_segment(),
            },
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
    fn total_width_counts_inter_segment_separators_only() {
        let items = vec![item("ab", 10), item("cd", 10), item("ef", 10)];
        // widths 2+2+2 = 6, separators between: 2 * 1 = 2, total 8.
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
        let items = vec![item("one", 10), item("two", 20), item("three", 30)];
        assert_eq!(render_plain(items, 100), "one two three");
    }

    #[test]
    fn drops_highest_priority_under_pressure() {
        let items = vec![
            item("aaaa", 10),
            item("bbbb", 200), // highest priority → drops first
            item("cccc", 50),
        ];
        // Full: 4+1+4+1+4 = 14. Budget 10 forces one drop.
        let out = render_plain(items, 10);
        assert!(!out.contains("bbbb"));
        assert!(out.contains("aaaa"));
        assert!(out.contains("cccc"));
    }

    #[test]
    fn drops_in_descending_priority_order() {
        let items = vec![
            item("one", 10),
            item("two", 200), // drops first
            item("three", 20),
            item("four", 150), // drops second
            item("five", 30),
        ];
        // Full: 3+1+3+1+5+1+4+1+4 = 23. Budget 15 forces two drops.
        assert_eq!(render_plain(items, 15), "one three five");
    }

    #[test]
    fn priority_zero_never_drops_even_over_budget() {
        let items = vec![item("aaaa", 0), item("bbbb", 0)];
        let out = render_plain(items, 3);
        assert_eq!(out, "aaaa bbbb");
    }

    #[test]
    fn mix_drops_positives_keeps_zeros() {
        let items = vec![
            item("keep-me", 0),
            item("droppable", 200),
            item("sticky", 0),
        ];
        // Budget forces drop; only the priority-200 segment is eligible.
        let out = render_plain(items, 20);
        assert_eq!(out, "keep-me sticky");
    }

    #[test]
    fn no_trailing_separator() {
        let items = vec![item("a", 10), item("b", 10)];
        assert_eq!(render_plain(items, 100), "a b");
    }

    #[test]
    fn empty_input_renders_empty_string() {
        assert_eq!(render_plain(vec![], 100), "");
    }

    #[test]
    fn respects_custom_separator_from_defaults() {
        let items = vec![
            Item {
                rendered: RenderedSegment::new("a"),
                defaults: SegmentDefaults {
                    priority: 10,
                    width: None,
                    default_separator: Separator::Literal(Cow::Borrowed(" | ")),
                    truncatable: false,
                },
                segment: noop_segment(),
            },
            Item {
                rendered: RenderedSegment::new("b"),
                defaults: SegmentDefaults::with_priority(10),
                segment: noop_segment(),
            },
        ];
        assert_eq!(render_plain(items, 100), "a | b");
    }

    #[test]
    fn render_override_separator_beats_default() {
        let items = vec![
            Item {
                rendered: RenderedSegment::with_separator("a", Separator::None),
                defaults: SegmentDefaults::with_priority(10),
                segment: noop_segment(),
            },
            Item {
                rendered: RenderedSegment::new("b"),
                defaults: SegmentDefaults::with_priority(10),
                segment: noop_segment(),
            },
        ];
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
        let items = vec![item("left", 200), item("mid", 50), item("right", 200)];
        // Full: 4+1+3+1+5 = 14. Budget 10 forces one drop; tied priorities
        // on "left" and "right" — right drops first.
        assert_eq!(render_plain(items, 10), "left mid");
    }

    #[test]
    fn separator_none_not_charged_to_budget() {
        // Three segments; middle one declares Separator::None on its right
        // edge, collapsing against the next. Widths 1+1+1 = 3; separators
        // are Space (1) between 0-1, None (0) between 1-2. Total = 4.
        // Any budget ≥ 4 must keep everything and emit "a bc".
        let items = vec![
            Item {
                rendered: RenderedSegment::new("a"),
                defaults: SegmentDefaults::with_priority(200),
                segment: noop_segment(),
            },
            Item {
                rendered: RenderedSegment::with_separator("b", Separator::None),
                defaults: SegmentDefaults::with_priority(200),
                segment: noop_segment(),
            },
            Item {
                rendered: RenderedSegment::new("c"),
                defaults: SegmentDefaults::with_priority(200),
                segment: noop_segment(),
            },
        ];
        assert_eq!(render_plain(items, 4), "a bc");
    }

    #[test]
    fn total_width_returns_u32_beyond_u16_range() {
        // Three segments at u16::MAX each: sum = 3 * u16::MAX plus two
        // separator cells. Must not wrap.
        let items = vec![
            Item {
                rendered: RenderedSegment::new("x".repeat(u16::MAX as usize)),
                defaults: SegmentDefaults::with_priority(10),
                segment: noop_segment(),
            },
            Item {
                rendered: RenderedSegment::new("x".repeat(u16::MAX as usize)),
                defaults: SegmentDefaults::with_priority(10),
                segment: noop_segment(),
            },
            Item {
                rendered: RenderedSegment::new("x".repeat(u16::MAX as usize)),
                defaults: SegmentDefaults::with_priority(10),
                segment: noop_segment(),
            },
        ];
        assert_eq!(total_width(&items), 3 * u32::from(u16::MAX) + 2);
    }

    #[test]
    fn all_priority_zero_keeps_every_segment_even_when_overfull() {
        let items = vec![item("aaa", 0), item("bbb", 0), item("ccc", 0)];
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
        let segments: Vec<Box<dyn Segment>> = vec![
            Box::new(StubSegment(Ok(Some(RenderedSegment::new("ok-before"))))),
            Box::new(StubSegment(Err(SegmentError::new("boom")))),
            Box::new(StubSegment(Ok(Some(RenderedSegment::new("ok-after"))))),
        ];
        let mut warnings = Vec::new();
        let items = collect_items_with(&segments, &empty_ctx(), &empty_rc(), &mut |msg| {
            warnings.push(msg.to_string());
        });
        // The Err segment vanishes from layout; neighbors survive.
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].rendered.text, "ok-before");
        assert_eq!(items[1].rendered.text, "ok-after");
        // The error is surfaced to stderr exactly once.
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("segment error"));
        assert!(warnings[0].contains("boom"));
    }

    #[test]
    fn ok_none_is_silently_hidden() {
        let segments: Vec<Box<dyn Segment>> = vec![
            Box::new(StubSegment(Ok(Some(RenderedSegment::new("visible"))))),
            Box::new(StubSegment(Ok(None))),
        ];
        let mut warnings = Vec::new();
        let items = collect_items_with(&segments, &empty_ctx(), &empty_rc(), &mut |msg| {
            warnings.push(msg.to_string());
        });
        assert_eq!(items.len(), 1);
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
        let segments: Vec<Box<dyn Segment>> = vec![Box::new(WidthEcho)];
        let mut warnings = Vec::new();
        let rc = RenderContext::new(42);
        let items = collect_items_with(&segments, &empty_ctx(), &rc, &mut |msg| {
            warnings.push(msg.to_string());
        });
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].rendered.text, "42");
    }

    #[test]
    fn render_with_warn_constructs_render_context_from_terminal_width_arg() {
        // Pins the construction line in `render_with_warn`: the public
        // entrypoint must build `RenderContext::new(terminal_width)`
        // from its argument and pass it to segments. A regression that
        // hard-coded a default would slip past the
        // `collect_items_with`-only test above.
        let segments: Vec<Box<dyn Segment>> = vec![Box::new(WidthEcho)];
        let mut warnings = Vec::new();
        let line = render_with_warn(
            &segments,
            &empty_ctx(),
            137,
            &mut |msg| warnings.push(msg.to_string()),
            theme::default_theme(),
            theme::Capability::None,
        );
        assert!(line.contains("137"), "got {line:?}");
    }

    // --- truncate-before-drop (reflow) ---

    fn truncatable_item(text: &str, priority: u8) -> Item<'static> {
        Item {
            rendered: RenderedSegment::new(text),
            defaults: SegmentDefaults::with_priority(priority).with_truncatable(true),
            segment: noop_segment(),
        }
    }

    #[test]
    fn reflow_truncates_highest_priority_before_dropping() {
        // Workspace-style scenario: long location plus a small fixed
        // segment. Without reflow the location would drop entirely;
        // with reflow it shrinks to fit so the user keeps orientation.
        let items = vec![
            truncatable_item("linesmith/very-long-feature-branch-name", 200),
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
        let items = vec![truncatable_item("workspace-name", 200), item("KEEP", 0)];
        // Total: 14 + 1 + 4 = 19. Budget 4 → overflow 15.
        // workspace target = 14 - 15 < 0 → reflow returns None → drop.
        let out = render_plain(items, 4);
        assert_eq!(out, "KEEP");
    }

    #[test]
    fn reflow_respects_explicit_width_min_floor() {
        // Segment declares min=8; reflow must not shrink below that
        // even if a smaller truncation would fit the budget.
        let bounds = WidthBounds::new(8, u16::MAX).expect("valid");
        let mut wide = truncatable_item("abcdefghijklmnop", 200); // width 16
        wide.defaults.width = Some(bounds);
        let items = vec![wide, item("X", 0)];
        // Total 16 + 1 + 1 = 18. Budget 10 → overflow 8 → target 8 ✓
        // (target equals floor; reflow proceeds).
        let out = render_plain(items, 10);
        assert!(out.contains('…'), "got {out:?}");
        assert!(out.ends_with(" X"), "got {out:?}");

        // Now budget 9 → overflow 9 → target 7 < floor 8 → drop.
        let bounds = WidthBounds::new(8, u16::MAX).expect("valid");
        let mut wide = truncatable_item("abcdefghijklmnop", 200);
        wide.defaults.width = Some(bounds);
        let items = vec![wide, item("X", 0)];
        let out = render_plain(items, 9);
        assert_eq!(out, "X");
    }

    #[test]
    fn non_truncatable_drops_unchanged_under_pressure() {
        // Default `truncatable=false` keeps the legacy whole-segment
        // drop path so numeric segments don't suddenly start emitting
        // half-cut percentages or dollar figures.
        let items = vec![item("45% · 200k", 200), item("Sonnet", 0)];
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
            truncatable_item("bbbbbbbbbb", 100),
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
            Item {
                rendered: RenderedSegment::new("untouchable-long-name"),
                defaults: SegmentDefaults::with_priority(0).with_truncatable(true),
                segment: noop_segment(),
            },
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
        let segments: Vec<Box<dyn Segment>> = vec![
            Box::new(ShrinkableSegment {
                full: "longbranch * ↑2 ↓1",
                compact: "longbranch",
            }),
            Box::new(AnchorSegment("KEEP")),
        ];
        let mut warnings = Vec::new();
        let line = render_with_warn(
            &segments,
            &empty_ctx(),
            17,
            &mut |m| warnings.push(m.to_string()),
            theme::default_theme(),
            theme::Capability::None,
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
        let segments: Vec<Box<dyn Segment>> = vec![
            Box::new(ShrinkableSegment {
                full: "longbranch",
                compact: "stilltoolongtruly",
            }),
            Box::new(AnchorSegment("X")),
        ];
        let mut warnings = Vec::new();
        let line = render_with_warn(
            &segments,
            &empty_ctx(),
            5,
            &mut |m| warnings.push(m.to_string()),
            theme::default_theme(),
            theme::Capability::None,
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
        let segments: Vec<Box<dyn Segment>> =
            vec![Box::new(LowFloorShrink), Box::new(AnchorSegment("X"))];
        let line = render_with_warn(
            &segments,
            &empty_ctx(),
            7,
            &mut |_| {},
            theme::default_theme(),
            theme::Capability::None,
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
        let segments: Vec<Box<dyn Segment>> =
            vec![Box::new(MisbehavingSegment), Box::new(AnchorSegment("X"))];
        let line = render_with_warn(
            &segments,
            &empty_ctx(),
            5,
            &mut |_| {},
            theme::default_theme(),
            theme::Capability::None,
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
        let segments: Vec<Box<dyn Segment>> = vec![
            Box::new(DualSegment),
            Box::new(StubSegment(Ok(Some(RenderedSegment::new("X"))))),
        ];
        let mut warnings = Vec::new();
        let line = render_with_warn(
            &segments,
            &empty_ctx(),
            13,
            &mut |m| warnings.push(m.to_string()),
            theme::default_theme(),
            theme::Capability::None,
        );
        // Full = 20, X = 1, separator = 1 → total 22. Budget 13 →
        // overflow 9. shrink target = 20 - 9 = 11. Compact
        // "longprefix" (10) fits → "longprefix X" (12 cells).
        // No "…" appears because shrink_to_fit ran first.
        assert!(line.contains("longprefix"), "got {line:?}");
        assert!(!line.contains('…'), "no end-ellipsis: {line:?}");
    }
}
