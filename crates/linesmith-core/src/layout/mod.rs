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
use crate::theme::{self, Capability, Style, StyledRun, Theme};
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
    segments: &[Box<dyn Segment>],
    ctx: &DataContext,
    terminal_width: u16,
    warn: &mut dyn FnMut(&str),
    theme: &Theme,
    capability: Capability,
    hyperlinks: bool,
) -> String {
    let runs = render_to_runs(segments, ctx, terminal_width, warn);
    runs_to_ansi(&runs, theme, capability, hyperlinks)
}

/// Render `segments` into a flat [`StyledRun`] sequence. One run per
/// surviving segment, plus one run per non-empty inter-segment
/// separator (in render order). Layout decisions — priority-drop,
/// `shrink_to_fit`, truncatable reflow, width-bound truncation —
/// match [`render`] / [`render_with_warn`] exactly; only the emit
/// form differs.
///
/// `Separator::None` between segments contributes no run; it would
/// be an empty-text run with no consumer use. Separator runs carry
/// [`Style::default`]; separators inherit no styling from their
/// flanking segments.
///
/// Segment render errors and `Ok(None)` go through `warn` exactly as
/// in the ANSI path; the run sequence reflects only segments that
/// survived to the layout pass.
#[must_use]
pub fn render_to_runs(
    segments: &[Box<dyn Segment>],
    ctx: &DataContext,
    terminal_width: u16,
    warn: &mut dyn FnMut(&str),
) -> Vec<StyledRun> {
    let rc = RenderContext::new(terminal_width);
    let items = collect_items_with(segments, ctx, &rc, warn);
    let laid_out = apply_layout(items, ctx, &rc, terminal_width);
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

/// Pure layout pass — no styling, no emission. Runs the
/// priority-drop / shrink / reflow loop and returns surviving items
/// in render order.
fn apply_layout<'a>(
    mut items: Vec<Item<'a>>,
    ctx: &DataContext,
    rc: &RenderContext,
    terminal_width: u16,
) -> Vec<Item<'a>> {
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
    items
}

/// Test-only helper that mirrors `render_with_warn`'s compose order.
/// Lets unit tests build `Item` literals directly without restating
/// the layout-then-emit dance per case.
#[cfg(test)]
fn render_items(
    items: Vec<Item<'_>>,
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
/// `Separator::None` is filtered here so consumers don't see
/// empty-text runs.
fn items_to_runs(items: &[Item<'_>]) -> Vec<StyledRun> {
    let mut runs = Vec::with_capacity(items.len().saturating_mul(2));
    for (i, item) in items.iter().enumerate() {
        runs.push(StyledRun {
            text: item.rendered.text.clone(),
            style: item.rendered.style.clone(),
        });
        if i + 1 < items.len() {
            let sep = effective_separator(item);
            let sep_text = sep.text();
            if !sep_text.is_empty() {
                runs.push(StyledRun {
                    text: sep_text.to_string(),
                    style: separator_style(sep),
                });
            }
        }
    }
    runs
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
mod tests;
