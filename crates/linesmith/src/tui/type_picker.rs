//! Type picker screen — horizontal list of segment IDs the user
//! can pick to add or insert into the items editor.
//!
//! Invoked from the items editor via the `a`/`i` verbs or ←/→.
//! Holds the prior `ItemsEditorState` so Esc returns to it
//! unchanged and Enter returns with the picked segment inserted at
//! the captured target position. All document mutation lives in
//! `super::items_editor::apply_insert`; this module is UI-only.

use std::mem;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use toml_edit::DocumentMut;

use crate::config;

use super::app::{AppScreen, ScreenOutcome};
use super::items_editor::{self, InsertTarget, ItemsEditorState};

/// Picker state. `candidates` is a static slice (no allocation);
/// `target` and `prev` are the picker's payload back to the editor.
///
/// Sourced from `BUILT_IN_SEGMENT_IDS` (the full set), not
/// `DEFAULT_SEGMENT_IDS` (just the startup layout) — so users can
/// reach `context_bar`, `output_style`, `rate_limit_*`, etc.
/// without hand-editing TOML.
#[derive(Debug)]
pub(super) struct TypePickerState {
    candidates: &'static [&'static str],
    cursor: usize,
    target: InsertTarget,
    prev: ItemsEditorState,
}

impl TypePickerState {
    pub(super) fn new(target: InsertTarget, prev: ItemsEditorState) -> Self {
        Self {
            candidates: linesmith_core::segments::BUILT_IN_SEGMENT_IDS,
            cursor: 0,
            target,
            prev,
        }
    }
}

/// How many candidates render in the centered window. Sized so
/// the row fits ~70-80 column terminals with the longest IDs
/// (`rate_limit_*_reset`).
const VISIBLE_WINDOW: usize = 5;

/// Slice of candidates rendered around the cursor, plus the start
/// offset so the renderer knows whether to draw `…` indicators.
/// Cursor is always within the returned window because `start` is
/// clamped to `[0, len - n]` and the window has `n` entries.
fn visible_window(state: &TypePickerState) -> (usize, &'static [&'static str]) {
    let len = state.candidates.len();
    if len == 0 {
        return (0, &[]);
    }
    let n = VISIBLE_WINDOW.min(len);
    let half = n / 2;
    let start = state.cursor.saturating_sub(half).min(len - n);
    (start, &state.candidates[start..start + n])
}

/// Drive the picker through one keypress. ←/→ navigate (with wrap)
/// within `candidates`; Enter applies the selection; Esc cancels.
/// Chord variants and unrecognized keys are inert so a typo can't
/// dismiss the picker silently.
pub(super) fn update(
    state: &mut TypePickerState,
    document: &mut DocumentMut,
    config: &mut config::Config,
    key: KeyEvent,
) -> ScreenOutcome {
    if key.modifiers != KeyModifiers::NONE {
        return ScreenOutcome::Stay;
    }
    match key.code {
        KeyCode::Esc => {
            let prev = mem::take(&mut state.prev);
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(prev))
        }
        KeyCode::Left => {
            navigate(state, Step::Left);
            ScreenOutcome::Stay
        }
        KeyCode::Right => {
            navigate(state, Step::Right);
            ScreenOutcome::Stay
        }
        KeyCode::Enter => {
            if state.candidates.is_empty() {
                let prev = mem::take(&mut state.prev);
                return ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(prev));
            }
            let segment = state.candidates[state.cursor];
            let target = state.target;
            let prev = mem::take(&mut state.prev);
            items_editor::apply_insert(prev, document, config, target, segment)
        }
        _ => ScreenOutcome::Stay,
    }
}

/// Navigation increment for the picker's cursor. Distinct from
/// `ratatui::layout::Direction` (which is orientation, not motion).
#[derive(Debug, Clone, Copy)]
enum Step {
    Left,
    Right,
}

fn navigate(state: &mut TypePickerState, step: Step) {
    let len = state.candidates.len();
    if len == 0 {
        return;
    }
    state.cursor = match step {
        Step::Left if state.cursor == 0 => len - 1,
        Step::Left => state.cursor - 1,
        Step::Right if state.cursor + 1 >= len => 0,
        Step::Right => state.cursor + 1,
    };
}

/// Render the picker. Layout: title row, help row, blank, then the
/// candidates row centered with the cursor highlighted via reverse
/// video.
pub(super) fn view(state: &TypePickerState, frame: &mut Frame, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // help
            Constraint::Length(1), // blank
            Constraint::Length(1), // candidates
            Constraint::Min(1),    // remainder
        ])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled(
        " add segment ",
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(title, chunks[0]);

    let help = Paragraph::new(Line::from(vec![
        Span::styled("←→", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" navigate · "),
        Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" select · "),
        Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" cancel"),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(help, chunks[1]);

    let (start, visible) = visible_window(state);
    let len = state.candidates.len();
    let dim_style = Style::default().add_modifier(Modifier::DIM);
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(visible.len() * 2 + 2);
    if start > 0 {
        spans.push(Span::styled("…  ", dim_style));
    }
    for (offset, candidate) in visible.iter().enumerate() {
        if offset > 0 {
            spans.push(Span::raw("  "));
        }
        let original_idx = start + offset;
        let style = if original_idx == state.cursor {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        spans.push(Span::styled(*candidate, style));
    }
    if start + visible.len() < len {
        spans.push(Span::styled("  …", dim_style));
    }
    let candidates = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
    frame.render_widget(candidates, chunks[3]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn document(toml: &str) -> DocumentMut {
        toml.parse().expect("test toml must parse")
    }

    fn picker_state(target: InsertTarget) -> TypePickerState {
        TypePickerState::new(target, ItemsEditorState::default())
    }

    #[test]
    fn left_wraps_from_first_to_last_candidate() {
        let mut s = picker_state(InsertTarget::After(0));
        let mut doc = document("");
        let mut cfg = config::Config::default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Left));
        assert!(matches!(outcome, ScreenOutcome::Stay));
        assert_eq!(s.cursor, s.candidates.len() - 1);
    }

    #[test]
    fn right_wraps_from_last_to_first_candidate() {
        let mut s = picker_state(InsertTarget::After(0));
        s.cursor = s.candidates.len() - 1;
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Right));
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn esc_returns_to_items_editor_without_mutating_document() {
        let raw = "[line]\nsegments = [\"a\"]\n";
        let mut s = picker_state(InsertTarget::After(0));
        let mut doc = document(raw);
        let mut cfg = config::Config::default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Esc));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(_))
        ));
        assert_eq!(doc.to_string(), raw);
    }

    #[test]
    fn esc_round_trips_prev_editor_cursor_through_mem_take() {
        // A regression that constructs `ItemsEditorState::default()`
        // for the cancel path silently resets the editor's items-
        // list cursor. Pin that the picker's `prev` round-trips
        // intact: set a non-default cursor, Esc, observe the
        // restored cursor unchanged.
        let mut prev = ItemsEditorState::default();
        prev.set_cursor(2, 3);
        let mut s = TypePickerState::new(InsertTarget::After(2), prev);
        let mut doc = document("[line]\nsegments = [\"a\", \"b\", \"c\"]\n");
        let mut cfg = config::Config::default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Esc));
        match outcome {
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(restored)) => {
                assert_eq!(restored.cursor(), 2);
            }
            other => panic!("expected NavigateTo(ItemsEditor), got {other:?}"),
        }
    }

    #[test]
    fn candidates_include_non_default_built_in_segments() {
        // The picker sources from the full BUILT_IN_SEGMENT_IDS,
        // not the smaller DEFAULT_SEGMENT_IDS startup list — so
        // segments like `context_bar` and `rate_limit_*` are
        // reachable from the UI without hand-editing TOML.
        // `context_bar` is in the full set but not the defaults;
        // its presence in `candidates` proves the wider sourcing.
        let s = picker_state(InsertTarget::After(0));
        assert!(
            s.candidates.contains(&"context_bar"),
            "picker should expose all built-ins, not just defaults: {:?}",
            s.candidates,
        );
        assert!(
            s.candidates.len() >= linesmith_core::segments::BUILT_IN_SEGMENT_IDS.len(),
            "candidate count should match BUILT_IN_SEGMENT_IDS",
        );
    }

    #[test]
    fn visible_window_keeps_cursor_in_view_at_each_position() {
        // Walk the cursor across every candidate and assert the
        // returned window contains the cursor's index. Without
        // this invariant, a cursor near the right edge could
        // render off-screen.
        let mut s = picker_state(InsertTarget::After(0));
        for cursor in 0..s.candidates.len() {
            s.cursor = cursor;
            let (start, visible) = visible_window(&s);
            assert!(
                cursor >= start && cursor < start + visible.len(),
                "cursor={cursor} not in window [{start}, {})",
                start + visible.len(),
            );
            assert!(visible.len() <= VISIBLE_WINDOW);
        }
    }

    #[test]
    fn navigate_advances_cursor_by_one_in_the_middle() {
        // Non-wrap arms of the navigate match — pin so a refactor
        // that collapses them into the wrap arms (and thus skips
        // adjacent indices) trips here.
        let mut s = picker_state(InsertTarget::After(0));
        s.cursor = 2;
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Right));
        assert_eq!(s.cursor, 3);
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Left));
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn chord_modifier_keys_are_inert() {
        // Shift+← / Ctrl+Esc must not navigate or cancel — pin so a
        // future relax of the modifier check doesn't let chord
        // arrows reorder candidates or stray Ctrl+Esc dismiss the
        // picker.
        let mut s = picker_state(InsertTarget::After(0));
        let mut doc = document("");
        let mut cfg = config::Config::default();
        let chord_left = KeyEvent::new(KeyCode::Left, KeyModifiers::SHIFT);
        update(&mut s, &mut doc, &mut cfg, chord_left);
        assert_eq!(s.cursor, 0, "chord arrow must not advance cursor");
        let chord_esc = KeyEvent::new(KeyCode::Esc, KeyModifiers::CONTROL);
        let outcome = update(&mut s, &mut doc, &mut cfg, chord_esc);
        assert!(
            matches!(outcome, ScreenOutcome::Stay),
            "chord Esc must not cancel: {outcome:?}"
        );
    }

    #[test]
    fn enter_inserts_after_cursor_and_returns_to_editor() {
        let mut prev = ItemsEditorState::default();
        // Walk the editor's cursor to row 1 so we exercise non-zero
        // After(idx) handling.
        prev.set_cursor(1, 2);
        let mut s = TypePickerState::new(InsertTarget::After(1), prev);
        s.cursor = 0;
        let pick = s.candidates[0];
        let mut doc = document(
            r#"[line]
segments = ["a", "b"]
"#,
        );
        let mut cfg = config::Config::default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        match outcome {
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(_)) => {}
            other => panic!("expected NavigateTo(ItemsEditor), got {other:?}"),
        }
        let written = doc.to_string();
        assert!(
            written.contains(&format!("\"{pick}\"")),
            "picked segment must be in document: {written}",
        );
    }

    #[test]
    fn enter_inserts_before_cursor_at_index_zero() {
        // Edge: insert at index 0 of a non-empty array. The picked
        // segment lands at index 0 and the existing entries shift
        // right by one.
        let mut s = TypePickerState::new(InsertTarget::Before(0), ItemsEditorState::default());
        s.cursor = 0;
        let pick = s.candidates[0];
        let mut doc = document(
            r#"[line]
segments = ["a", "b"]
"#,
        );
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        let segments = cfg.line.expect("line reparsed").segments;
        assert_eq!(segments[0].segment_id(), Some(pick));
        assert_eq!(segments[1].segment_id(), Some("a"));
        assert_eq!(segments[2].segment_id(), Some("b"));
    }
}
