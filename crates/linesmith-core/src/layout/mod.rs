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
    text_width, LineItem, RenderContext, RenderedSegment, Segment, SegmentDefaults, Separator,
    WidthBounds,
};
use crate::theme::{self, Capability, Style, StyledRun, Theme};
use unicode_segmentation::UnicodeSegmentation;

/// Render `items` for `ctx` within `terminal_width` cells. Returns the
/// final line without a trailing newline. Segment render errors go
/// through [`crate::lsm_error!`] so a broken segment always surfaces,
/// even under `LINESMITH_LOG=off` — a blank statusline with zero
/// diagnostic is a bad UX even when the user opted into quiet mode.
/// Output is unstyled (callers that want theming use
/// [`render_with_warn`] with their own closure).
#[must_use]
pub fn render(items: &[LineItem], ctx: &DataContext, terminal_width: u16) -> String {
    let mut warn = |msg: &str| crate::lsm_error!("{msg}");
    render_with_warn(
        items,
        ctx,
        terminal_width,
        &mut warn,
        theme::default_theme(),
        Capability::None,
        false,
    )
}

/// Same as [`render`] but routes segment render-error diagnostics
/// through `warn` and emits ANSI SGR around each segment per `theme`
/// and `capability`. Used by [`crate::run_with_context`] so `cli_main`
/// tests can capture segment errors alongside exit codes while the
/// render path picks up theme colors.
///
/// `hyperlinks` gates OSC 8 emission for runs whose `Style.hyperlink`
/// is set. Pass `true` when the terminal advertises OSC 8 support
/// (e.g. via the `supports-hyperlinks` crate or an explicit user
/// override), `false` otherwise — capable terminals render the run
/// as a clickable link, others see plain text.
///
/// Thin wrapper over [`render_to_runs`] + [`runs_to_ansi`]; same
/// layout, same bytes. Callers that need the styled-run form (e.g.
/// the TUI preview pane) call [`render_to_runs`] directly.
#[must_use]
pub fn render_with_warn(
    items: &[LineItem],
    ctx: &DataContext,
    terminal_width: u16,
    warn: &mut dyn FnMut(&str),
    theme: &Theme,
    capability: Capability,
    hyperlinks: bool,
) -> String {
    let runs = render_to_runs(items, ctx, terminal_width, warn);
    runs_to_ansi(&runs, theme, capability, hyperlinks)
}

/// Render `items` into a flat [`StyledRun`] sequence. One run per
/// surviving segment, plus one run per non-empty surviving separator
/// (in render order). Layout decisions — priority-drop,
/// `shrink_to_fit`, truncatable reflow, width-bound truncation —
/// match [`render`] / [`render_with_warn`] exactly; only the emit
/// form differs.
///
/// `Separator::None` contributes no run; it would be an empty-text
/// run with no consumer use. Separator runs carry [`Style::default`];
/// separators inherit no styling from their flanking segments.
///
/// Segment render errors and `Ok(None)` go through `warn` exactly as
/// in the ANSI path; the run sequence reflects only segments that
/// survived to the layout pass, with separators surviving only
/// between two surviving segments.
#[must_use]
pub fn render_to_runs(
    items: &[LineItem],
    ctx: &DataContext,
    terminal_width: u16,
    warn: &mut dyn FnMut(&str),
) -> Vec<StyledRun> {
    let rc = RenderContext::new(terminal_width);
    let layout_items = collect_items_with(items, ctx, &rc, warn);
    let laid_out = apply_layout(layout_items, ctx, &rc, terminal_width);
    items_to_runs(&laid_out)
}

/// Emit a flat [`StyledRun`] sequence as an ANSI SGR-wrapped string
/// suitable for terminal stdout. Each run with non-empty styling gets
/// its own `sgr_open` / `sgr_reset` pair so decorations don't leak
/// across boundaries; plain runs pass through unwrapped. When
/// `hyperlinks` is `true`, runs carrying `Style.hyperlink` are
/// additionally wrapped in OSC 8 open/close so capable terminals
/// render them as clickable links; the OSC 8 wrap sits *outside* the
/// SGR pair so the link survives the SGR reset. `hyperlinks = false`
/// drops the URL silently — the run still emits, just without the
/// link.
#[must_use]
pub fn runs_to_ansi(
    runs: &[StyledRun],
    theme: &Theme,
    capability: Capability,
    hyperlinks: bool,
) -> String {
    let mut out = String::new();
    for run in runs {
        let link = run.style.hyperlink.as_deref().filter(|_| hyperlinks);
        if let Some(url) = link {
            push_osc8_open(&mut out, url);
        }
        let open = theme::sgr_open(&run.style, theme, capability);
        if open.is_empty() {
            out.push_str(&run.text);
        } else {
            out.push_str(&open);
            out.push_str(&run.text);
            out.push_str(theme::sgr_reset());
        }
        if link.is_some() {
            push_osc8_close(&mut out);
        }
    }
    out
}

/// OSC 8 hyperlink open: `ESC ] 8 ; ; <url> ST`. Uses ESC `\` (the
/// canonical String Terminator) rather than the BEL alternative;
/// modern terminals accept both but ESC `\` is the spec form and
/// safer when output is piped through tools that interpret BEL.
///
/// Strips control characters from `url` before emission. Without
/// this, an embedded `ESC \` in a plugin- or repo-derived URL would
/// terminate the OSC 8 envelope early and turn the remainder into
/// raw terminal control sequences — the same escape-injection class
/// `RenderedSegment::new` strips from segment text.
fn push_osc8_open(out: &mut String, url: &str) {
    out.push_str("\x1b]8;;");
    for c in url.chars() {
        if !c.is_control() {
            out.push(c);
        }
    }
    out.push_str("\x1b\\");
}

/// OSC 8 hyperlink close: same envelope, empty URL.
fn push_osc8_close(out: &mut String) {
    out.push_str("\x1b]8;;\x1b\\");
}

/// One slot in the post-collect layout list. `Segment` carries the
/// rendered output, the defaults needed to place it (priority,
/// bounds, truncatable), and a back-reference to the trait object so
/// the reflow loop can call `shrink_to_fit` without re-walking the
/// input slice. `Separator` carries a resolved [`Separator`] value
/// ready for width math and emit; runtime overrides have already
/// been merged in.
enum LayoutItem<'a> {
    Segment(SegmentEntry<'a>),
    Separator(Separator),
}

struct SegmentEntry<'a> {
    /// User-facing config name threaded through for `LayoutDecision` events (ADR-0026).
    id: &'a std::borrow::Cow<'static, str>,
    rendered: RenderedSegment,
    defaults: SegmentDefaults,
    segment: &'a dyn Segment,
}

/// Walk the raw [`LineItem`] list, render each segment, and emit a
/// [`LayoutItem`] sequence ready for the layout pass.
///
/// Adjacency rules baked in here so downstream passes don't need to
/// know about them:
///
/// - A separator survives only when it sits between two surviving
///   segments. Leading separators, trailing separators, and
///   separators flanking a dropped segment are pruned.
/// - A segment's per-render `right_separator` override (the plugin
///   path) replaces the inline separator immediately to its right.
///   The override is applied here so width math and drop decisions
///   downstream see the post-override separator value.
fn collect_items_with<'a>(
    items: &'a [LineItem],
    ctx: &DataContext,
    rc: &RenderContext,
    warn: &mut dyn FnMut(&str),
) -> Vec<LayoutItem<'a>> {
    let mut out: Vec<LayoutItem<'a>> = Vec::with_capacity(items.len());
    for item in items {
        match item {
            LineItem::Segment { id, segment } => {
                let defaults = segment.defaults();
                let rendered = match segment.render(ctx, rc) {
                    Ok(Some(r)) => r,
                    Ok(None) => {
                        pop_trailing_separator(&mut out);
                        continue;
                    }
                    Err(err) => {
                        warn(&format!("segment error: {err}"));
                        pop_trailing_separator(&mut out);
                        continue;
                    }
                };
                let Some(rendered) = apply_width_bounds(rendered, defaults.width) else {
                    pop_trailing_separator(&mut out);
                    continue;
                };
                out.push(LayoutItem::Segment(SegmentEntry {
                    id,
                    rendered,
                    defaults,
                    segment: segment.as_ref(),
                }));
            }
            LineItem::Separator(sep) => {
                // Push only when directly preceded by a surviving
                // segment, so leading/orphaned separators drop.
                if matches!(out.last(), Some(LayoutItem::Segment(_))) {
                    out.push(LayoutItem::Separator(sep.clone()));
                }
            }
        }
    }
    pop_trailing_separator(&mut out);
    for i in 0..out.len() {
        apply_override_at(&mut out, i);
    }
    out
}

fn pop_trailing_separator(out: &mut Vec<LayoutItem<'_>>) {
    if matches!(out.last(), Some(LayoutItem::Separator(_))) {
        out.pop();
    }
}

/// Apply the runtime `right_separator` override for the segment at
/// `idx` to its right-edge inline separator (if any). Called from
/// [`collect_items_with`] across the whole list, and again from
/// [`apply_layout`] at a single index after `shrink_to_fit` / reflow
/// rewrites a segment's render — both paths can produce a different
/// `right_separator` than the pre-shrink value, and the inline slot
/// must track it.
///
/// `Some` overrides the inline value; `None` is a no-op (the
/// pre-existing inline value stays). The current implementation
/// can't distinguish "segment never had an override" from "segment
/// flipped from `Some` back to `None`" — the conservative behavior
/// keeps the most recently applied `Some`. Plugins that flip in
/// the latter direction are out of contract.
fn apply_override_at(items: &mut [LayoutItem<'_>], idx: usize) {
    let override_sep = match items.get(idx) {
        Some(LayoutItem::Segment(seg)) => seg.rendered.right_separator.clone(),
        _ => None,
    };
    if let Some(s) = override_sep {
        if let Some(LayoutItem::Separator(slot)) = items.get_mut(idx + 1) {
            *slot = s;
        }
    }
}

/// Pure layout pass — no styling, no emission. Runs the
/// priority-drop / shrink / reflow loop and returns surviving items
/// in render order. When a segment must be removed, the adjacent
/// separator goes with it (see [`drop_segment_and_adjacent_separator`]).
///
/// Per iteration, for the highest-priority droppable segment:
/// `shrink_to_fit` first, `truncatable` end-ellipsis reflow second,
/// drop the whole segment last. Each compaction path may produce a
/// `right_separator` different from the pre-shrink value, so the
/// inline override slot gets re-propagated after a rewrite.
fn apply_layout<'a>(
    mut items: Vec<LayoutItem<'a>>,
    ctx: &DataContext,
    rc: &RenderContext,
    terminal_width: u16,
) -> Vec<LayoutItem<'a>> {
    let budget = u32::from(terminal_width);
    loop {
        let total = total_width(&items);
        if total <= budget {
            break;
        }
        let Some(drop_idx) = highest_priority_droppable(&items) else {
            break;
        };
        let overflow = total - budget;
        // `highest_priority_droppable` only returns segment indices, so
        // this match always binds.
        let LayoutItem::Segment(seg) = &items[drop_idx] else {
            break;
        };
        if let Some(shrunk) = try_shrink(seg, ctx, rc, overflow) {
            if let LayoutItem::Segment(s) = &mut items[drop_idx] {
                s.rendered = shrunk;
            }
            apply_override_at(&mut items, drop_idx);
        } else if seg.defaults.truncatable {
            if let Some(reflowed) = try_reflow(seg, overflow) {
                items[drop_idx] = LayoutItem::Segment(reflowed);
                apply_override_at(&mut items, drop_idx);
            } else {
                drop_segment_and_adjacent_separator(&mut items, drop_idx);
            }
        } else {
            drop_segment_and_adjacent_separator(&mut items, drop_idx);
        }
    }
    items
}

/// Index of the highest-priority droppable segment, or `None` when
/// every segment is priority-0 (pinned).
fn highest_priority_droppable(items: &[LayoutItem<'_>]) -> Option<usize> {
    items
        .iter()
        .enumerate()
        .filter_map(|(i, item)| match item {
            LayoutItem::Segment(seg) if seg.defaults.priority > 0 => {
                Some((i, seg.defaults.priority))
            }
            _ => None,
        })
        .max_by_key(|(_, pri)| *pri)
        .map(|(i, _)| i)
}

/// Drop the segment at `idx` along with one adjacent separator: the
/// right-edge separator first, falling back to the left-edge when
/// the segment was the last in the line.
fn drop_segment_and_adjacent_separator(items: &mut Vec<LayoutItem<'_>>, idx: usize) {
    let next_is_sep = matches!(items.get(idx + 1), Some(LayoutItem::Separator(_)));
    let prev_is_sep = idx > 0 && matches!(items.get(idx - 1), Some(LayoutItem::Separator(_)));
    if next_is_sep {
        items.remove(idx);
        items.remove(idx);
    } else if prev_is_sep {
        items.remove(idx);
        items.remove(idx - 1);
    } else {
        items.remove(idx);
    }
}

/// Test-only helper that mirrors `render_with_warn`'s compose order.
/// Lets unit tests build [`LayoutItem`] literals directly without
/// restating the layout-then-emit dance per case.
#[cfg(test)]
fn render_items(
    items: Vec<LayoutItem<'_>>,
    ctx: &DataContext,
    rc: &RenderContext,
    terminal_width: u16,
    theme: &Theme,
    capability: Capability,
) -> String {
    let laid_out = apply_layout(items, ctx, rc, terminal_width);
    let runs = items_to_runs(&laid_out);
    runs_to_ansi(&runs, theme, capability, false)
}

/// Flatten step for [`render_to_runs`]: see that function for the
/// emit contract. Separator runs carry [`Style::default`];
/// `Separator::None` (text == "") is filtered here so consumers
/// don't see empty-text runs.
fn items_to_runs(items: &[LayoutItem<'_>]) -> Vec<StyledRun> {
    items
        .iter()
        .filter_map(|item| match item {
            LayoutItem::Segment(seg) => Some(StyledRun {
                text: seg.rendered.text.clone(),
                style: seg.rendered.style.clone(),
            }),
            LayoutItem::Separator(sep) => {
                let text = sep.text();
                if text.is_empty() {
                    None
                } else {
                    Some(StyledRun {
                        text: text.to_string(),
                        style: separator_style(sep),
                    })
                }
            }
        })
        .collect()
}

/// Style for an inter-segment separator run. Plain separators carry
/// `Style::default()`; powerline chevrons get `Role::Muted` so the
/// chevron reads as readable secondary text rather than dropping into
/// the dim divider/border shade (which on most dark themes renders too
/// close to the background to be legible without bg fill).
fn separator_style(sep: &Separator) -> Style {
    match sep {
        Separator::Powerline { .. } => Style::role(theme::Role::Muted),
        _ => Style::default(),
    }
}

/// Sum of every layout item's width — segments and separators alike.
/// `u32` prevents `u16` overflow on many wide segments.
fn total_width(items: &[LayoutItem<'_>]) -> u32 {
    items
        .iter()
        .map(|item| match item {
            LayoutItem::Segment(seg) => u32::from(seg.rendered.width),
            LayoutItem::Separator(sep) => u32::from(sep.width()),
        })
        .sum()
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
fn try_reflow<'a>(item: &SegmentEntry<'a>, overflow: u32) -> Option<SegmentEntry<'a>> {
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
    Some(SegmentEntry {
        id: item.id,
        rendered: truncated,
        defaults: item.defaults,
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
    item: &SegmentEntry<'_>,
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
mod tests;
