//! Layout engine. Takes a list of `Segment`s plus a `StatusContext` and
//! fits their renders into a terminal-width budget, dropping the
//! highest-priority (numerically largest) segments first. Priority-0
//! segments are never dropped, even when that overflows the budget.
//!
//! See `docs/specs/segment-system.md` §Layout algorithm.

use crate::input::StatusContext;
use crate::segments::{
    text_width, RenderedSegment, Segment, SegmentDefaults, Separator, WidthBounds,
};
use std::io::{self, Write};
use unicode_segmentation::UnicodeSegmentation;

/// Render `segments` for `ctx` within `terminal_width` cells. Returns the
/// final line without a trailing newline. Segment render errors are
/// logged to the real process stderr; for injected-stderr testability,
/// use [`render_with_warn`] instead.
#[must_use]
pub fn render(segments: &[Box<dyn Segment>], ctx: &StatusContext, terminal_width: u16) -> String {
    let mut warn = |msg: &str| {
        let _ = writeln!(io::stderr().lock(), "linesmith: {msg}");
    };
    render_with_warn(segments, ctx, terminal_width, &mut warn)
}

/// Same as [`render`] but routes segment render-error diagnostics
/// through a caller-supplied warn sink. Used by
/// `run_with_segments_width_and_stderr` so `cli_main` tests can
/// capture segment errors alongside exit codes.
#[must_use]
pub fn render_with_warn(
    segments: &[Box<dyn Segment>],
    ctx: &StatusContext,
    terminal_width: u16,
    warn: &mut dyn FnMut(&str),
) -> String {
    let items = collect_items_with(segments, ctx, warn);
    render_items(items, terminal_width)
}

/// Rendered output paired with the defaults needed to place it (priority,
/// separator, bounds). Bundled here so drop/emit passes don't re-query the
/// trait.
struct Item {
    rendered: RenderedSegment,
    defaults: SegmentDefaults,
}

fn collect_items_with(
    segments: &[Box<dyn Segment>],
    ctx: &StatusContext,
    warn: &mut dyn FnMut(&str),
) -> Vec<Item> {
    segments
        .iter()
        .filter_map(|seg| {
            let defaults = seg.defaults();
            let rendered = match seg.render(ctx) {
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
            })
        })
        .collect()
}

fn render_items(mut items: Vec<Item>, terminal_width: u16) -> String {
    while total_width(&items) > u32::from(terminal_width) {
        let Some(drop_idx) = items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.defaults.priority > 0)
            .max_by_key(|(_, item)| item.defaults.priority)
            .map(|(i, _)| i)
        else {
            break;
        };
        items.remove(drop_idx);
    }

    let mut out = String::new();
    for (i, item) in items.iter().enumerate() {
        out.push_str(&item.rendered.text);
        if i + 1 < items.len() {
            out.push_str(effective_separator(item).text());
        }
    }
    out
}

/// Sum of segment widths plus the separators that sit *between* segments
/// (no trailing separator). `u32` prevents `u16` overflow on many wide
/// segments.
fn total_width(items: &[Item]) -> u32 {
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

fn effective_separator(item: &Item) -> &Separator {
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

/// Truncate `rendered` to at most `max_cells` terminal cells, appending
/// `…` (U+2026, 1 cell) as a continuation marker. Iterates by grapheme
/// cluster so combining marks, ZWJ sequences, and emoji stay intact.
pub(crate) fn truncate_to(rendered: RenderedSegment, max_cells: u16) -> RenderedSegment {
    if max_cells == 0 {
        return RenderedSegment::from_parts(String::new(), 0, rendered.right_separator);
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
    RenderedSegment::from_parts(out, used.saturating_add(1), rendered.right_separator)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn item(text: &str, priority: u8) -> Item {
        Item {
            rendered: RenderedSegment::new(text),
            defaults: SegmentDefaults::with_priority(priority),
        }
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
        assert_eq!(render_items(items, 100), "one two three");
    }

    #[test]
    fn drops_highest_priority_under_pressure() {
        let items = vec![
            item("aaaa", 10),
            item("bbbb", 200), // highest priority → drops first
            item("cccc", 50),
        ];
        // Full: 4+1+4+1+4 = 14. Budget 10 forces one drop.
        let out = render_items(items, 10);
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
        assert_eq!(render_items(items, 15), "one three five");
    }

    #[test]
    fn priority_zero_never_drops_even_over_budget() {
        let items = vec![item("aaaa", 0), item("bbbb", 0)];
        let out = render_items(items, 3);
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
        let out = render_items(items, 20);
        assert_eq!(out, "keep-me sticky");
    }

    #[test]
    fn no_trailing_separator() {
        let items = vec![item("a", 10), item("b", 10)];
        assert_eq!(render_items(items, 100), "a b");
    }

    #[test]
    fn empty_input_renders_empty_string() {
        assert_eq!(render_items(vec![], 100), "");
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
                },
            },
            Item {
                rendered: RenderedSegment::new("b"),
                defaults: SegmentDefaults::with_priority(10),
            },
        ];
        assert_eq!(render_items(items, 100), "a | b");
    }

    #[test]
    fn render_override_separator_beats_default() {
        let items = vec![
            Item {
                rendered: RenderedSegment::with_separator("a", Separator::None),
                defaults: SegmentDefaults::with_priority(10),
            },
            Item {
                rendered: RenderedSegment::new("b"),
                defaults: SegmentDefaults::with_priority(10),
            },
        ];
        assert_eq!(render_items(items, 100), "ab");
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
        assert_eq!(render_items(items, 10), "left mid");
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
            },
            Item {
                rendered: RenderedSegment::with_separator("b", Separator::None),
                defaults: SegmentDefaults::with_priority(200),
            },
            Item {
                rendered: RenderedSegment::new("c"),
                defaults: SegmentDefaults::with_priority(200),
            },
        ];
        assert_eq!(render_items(items, 4), "a bc");
    }

    #[test]
    fn total_width_returns_u32_beyond_u16_range() {
        // Three segments at u16::MAX each: sum = 3 * u16::MAX plus two
        // separator cells. Must not wrap.
        let items = vec![
            Item {
                rendered: RenderedSegment::new("x".repeat(u16::MAX as usize)),
                defaults: SegmentDefaults::with_priority(10),
            },
            Item {
                rendered: RenderedSegment::new("x".repeat(u16::MAX as usize)),
                defaults: SegmentDefaults::with_priority(10),
            },
            Item {
                rendered: RenderedSegment::new("x".repeat(u16::MAX as usize)),
                defaults: SegmentDefaults::with_priority(10),
            },
        ];
        assert_eq!(total_width(&items), 3 * u32::from(u16::MAX) + 2);
    }

    #[test]
    fn all_priority_zero_keeps_every_segment_even_when_overfull() {
        let items = vec![item("aaa", 0), item("bbb", 0), item("ccc", 0)];
        // Full 3+1+3+1+3 = 11. Budget 4 is nowhere near; all three stay.
        assert_eq!(render_items(items, 4), "aaa bbb ccc");
    }

    // --- error handling ---

    use crate::input::{ModelInfo, Tool, WorkspaceInfo};
    use crate::segments::{RenderResult, SegmentError};
    use std::path::PathBuf;
    use std::sync::Arc;

    struct StubSegment(RenderResult);

    impl Segment for StubSegment {
        fn render(&self, _ctx: &StatusContext) -> RenderResult {
            match &self.0 {
                Ok(Some(r)) => Ok(Some(r.clone())),
                Ok(None) => Ok(None),
                Err(e) => Err(SegmentError::new(e.message.clone())),
            }
        }
    }

    fn empty_ctx() -> StatusContext {
        StatusContext {
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
            rate_limits: None,
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
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
        let items = collect_items_with(&segments, &empty_ctx(), &mut |msg| {
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
        let items = collect_items_with(&segments, &empty_ctx(), &mut |msg| {
            warnings.push(msg.to_string());
        });
        assert_eq!(items.len(), 1);
        assert!(warnings.is_empty());
    }
}
