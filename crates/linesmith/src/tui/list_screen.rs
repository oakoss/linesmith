//! Reusable `ListScreen` widget per ADR-0016.
//!
//! Owns cursor + move-mode state and exposes a pure `handle_key` so
//! screens can unit-test their dispatch tables without ratatui in
//! the loop. The render path uses ratatui's `List` + `ListState`
//! for free scrolling, with a custom layout around it for the
//! title, help row, and bottom description.
//!
//! Keymap:
//!
//! - ↑/↓ — wrap-around cursor in normal mode; swap-with-neighbor
//!   in move-mode (no wrap, since teleporting a row across the list
//!   is rarely the user's intent).
//! - Enter — when `move_mode_supported`, toggle move-mode; otherwise
//!   emit `Activate` so the caller opens the highlighted row.
//! - Esc — in move-mode, exit move-mode; otherwise `Unhandled` (so
//!   the global quit handler in [`super::app`] fires).
//! - lowercase ASCII letter with no modifiers, listed in
//!   `verb_letters`, in normal mode — `Action(letter)`. The widget
//!   doesn't know what each letter means; the caller maps it to its
//!   own action enum.

use std::borrow::Cow;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};
use ratatui::Frame;

/// Cursor + move-mode state owned by each screen that hosts a
/// `ListScreen`.
#[derive(Debug, Default, Clone)]
pub(super) struct ListScreenState {
    cursor: usize,
    move_mode: bool,
}

// `cursor()`, `move_mode()`, `set_cursor()`, and `new()` are
// exercised by tests but no production caller reads them yet.
// clippy's dead-code lint runs against the production build only,
// so test usage alone won't suppress the warning.
//
// `new()` is also kept to wrap `Default::default()` — without that
// indirection, `field_reassign_with_default` fires on the natural
// test pattern of constructing-then-tweaking a single field.
#[allow(dead_code)]
impl ListScreenState {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn cursor(&self) -> usize {
        self.cursor
    }

    pub(super) fn move_mode(&self) -> bool {
        self.move_mode
    }

    /// Force the cursor to a specific row, clamped to `[0, num_rows)`.
    /// Callers use this after operations that shrink `num_rows` so
    /// the cursor stays in range.
    pub(super) fn set_cursor(&mut self, idx: usize, num_rows: usize) {
        self.cursor = if num_rows == 0 {
            0
        } else {
            idx.min(num_rows - 1)
        };
    }
}

/// Outcome of one [`handle_key`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListOutcome {
    /// Widget handled the key internally; caller does nothing more.
    Consumed,
    /// Enter pressed in a non-move-mode-supporting screen — caller
    /// opens / activates the row at `state.cursor()`.
    Activate,
    /// A registered verb-letter was pressed in normal mode. Caller
    /// looks up the letter in its own action table.
    Action(char),
    /// In move-mode: caller must swap rows `from` and `to`. The
    /// widget has already advanced `state.cursor` to `to`.
    MoveSwap { from: usize, to: usize },
    /// Widget did not claim the key; caller can apply its own
    /// fallback (e.g. screen-specific keys) or let it bubble up.
    Unhandled,
}

/// One row in a [`render`] call.
#[derive(Debug, Clone)]
pub(super) struct ListRowData<'a> {
    pub(super) label: Cow<'a, str>,
    pub(super) description: Cow<'a, str>,
}

/// One verb in the help row, rendered as `letter label · letter
/// label · …` so the user always sees the active dispatch table
/// for the current screen.
///
/// `letter` must be an ASCII lowercase character and must appear
/// in the `verb_letters` slice passed to [`handle_key`]. Uppercase
/// or non-ASCII letters are silently ignored at dispatch time
/// (the widget gates `Action` on the same constraint), and a
/// letter not present in `verb_letters` produces a help row that
/// advertises a key that doesn't dispatch.
#[derive(Debug, Clone, Copy)]
pub(super) struct VerbHint<'a> {
    pub(super) letter: char,
    pub(super) label: &'a str,
}

/// Caller-supplied configuration for one [`render`] call.
#[derive(Debug, Clone)]
pub(super) struct ListScreenView<'a> {
    pub(super) title: &'a str,
    pub(super) rows: &'a [ListRowData<'a>],
    pub(super) verbs: &'a [VerbHint<'a>],
    /// When true, Enter toggles move-mode (Items Editor, Line
    /// Picker). When false, Enter emits `Activate` so the caller
    /// can open the highlighted row (Main Menu, Theme Picker).
    pub(super) move_mode_supported: bool,
}

/// Pure key dispatch. Mutates `state` for cursor / move-mode
/// changes; returns the [`ListOutcome`] the caller should react to.
///
/// Cursor preprocessing: before any key is interpreted, the cursor
/// is clamped to `[0, num_rows)`. This protects the widget against
/// stale cursors that index past the end of a list that shrank
/// between renders (e.g. after a delete) when the caller forgot to
/// call `state.set_cursor`.
pub(super) fn handle_key(
    state: &mut ListScreenState,
    key: KeyEvent,
    num_rows: usize,
    verb_letters: &[char],
    move_mode_supported: bool,
) -> ListOutcome {
    if num_rows == 0 {
        state.cursor = 0;
    } else if state.cursor >= num_rows {
        state.cursor = num_rows - 1;
    }
    // Defend against a screen that previously rendered with
    // `move_mode_supported = true` (entering move-mode) and then
    // flips support back off. Without this clamp the dispatch
    // would still route through `handle_move_mode` even though the
    // help row no longer claims to support it.
    if !move_mode_supported {
        state.move_mode = false;
    }

    if state.move_mode {
        return handle_move_mode(state, key, num_rows);
    }
    handle_normal_mode(state, key, num_rows, verb_letters, move_mode_supported)
}

/// Move-mode dispatch. Keys outside the navigation set fall
/// through to `Unhandled` (not `Consumed`) so verb letters never
/// reorder rows by accident.
fn handle_move_mode(state: &mut ListScreenState, key: KeyEvent, num_rows: usize) -> ListOutcome {
    if key.modifiers != KeyModifiers::NONE {
        return ListOutcome::Unhandled;
    }
    match key.code {
        KeyCode::Esc | KeyCode::Enter => {
            state.move_mode = false;
            ListOutcome::Consumed
        }
        KeyCode::Up if num_rows >= 2 && state.cursor > 0 => {
            let from = state.cursor;
            let to = from - 1;
            state.cursor = to;
            ListOutcome::MoveSwap { from, to }
        }
        KeyCode::Down if num_rows >= 2 && state.cursor + 1 < num_rows => {
            let from = state.cursor;
            let to = from + 1;
            state.cursor = to;
            ListOutcome::MoveSwap { from, to }
        }
        KeyCode::Up | KeyCode::Down => ListOutcome::Consumed,
        _ => ListOutcome::Unhandled,
    }
}

fn handle_normal_mode(
    state: &mut ListScreenState,
    key: KeyEvent,
    num_rows: usize,
    verb_letters: &[char],
    move_mode_supported: bool,
) -> ListOutcome {
    match (key.code, key.modifiers) {
        (KeyCode::Up, KeyModifiers::NONE) => {
            if num_rows == 0 {
                return ListOutcome::Consumed;
            }
            state.cursor = if state.cursor == 0 {
                num_rows - 1
            } else {
                state.cursor - 1
            };
            ListOutcome::Consumed
        }
        (KeyCode::Down, KeyModifiers::NONE) => {
            if num_rows == 0 {
                return ListOutcome::Consumed;
            }
            state.cursor = if state.cursor + 1 >= num_rows {
                0
            } else {
                state.cursor + 1
            };
            ListOutcome::Consumed
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            if num_rows == 0 {
                return ListOutcome::Unhandled;
            }
            if move_mode_supported {
                state.move_mode = true;
                ListOutcome::Consumed
            } else {
                ListOutcome::Activate
            }
        }
        (KeyCode::Char(c), KeyModifiers::NONE)
            if num_rows > 0 && c.is_ascii_lowercase() && verb_letters.contains(&c) =>
        {
            ListOutcome::Action(c)
        }
        _ => ListOutcome::Unhandled,
    }
}

/// Render the list screen into `area`. Layout (top to bottom):
/// title, help row, blank, scrolling list, blank, description.
/// An empty rows slice still paints the title and help-row
/// chrome; the list and description slots stay blank.
pub(super) fn render(
    state: &ListScreenState,
    view: &ListScreenView<'_>,
    area: Rect,
    frame: &mut Frame,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // help row
            Constraint::Length(1), // blank
            Constraint::Min(1),    // list body
            Constraint::Length(1), // blank
            Constraint::Length(1), // description
        ])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled(
        view.title,
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(title, chunks[0]);

    let help = Paragraph::new(help_line(
        view.verbs,
        state.move_mode,
        view.move_mode_supported,
    ))
    .alignment(Alignment::Center);
    frame.render_widget(help, chunks[1]);

    // Clamp once — `handle_key` clamps on entry, but `render` can
    // run between a data mutation and the next key event (e.g. an
    // async data load that resolves mid-frame). Without one
    // clamped value driving both the highlight and the description,
    // a stale cursor highlights row N while the description shows
    // empty.
    let cursor = if view.rows.is_empty() {
        0
    } else {
        state.cursor.min(view.rows.len() - 1)
    };

    let items: Vec<ListItem<'_>> = view
        .rows
        .iter()
        .map(|row| ListItem::new(Line::from(row.label.as_ref())))
        .collect();
    let highlight_style = if state.move_mode {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    };
    let list = List::new(items)
        .highlight_symbol("▶ ")
        .highlight_style(highlight_style);

    let mut list_state = ListState::default();
    if !view.rows.is_empty() {
        list_state.select(Some(cursor));
    }
    frame.render_stateful_widget(list, chunks[3], &mut list_state);

    let description = view
        .rows
        .get(cursor)
        .map(|row| row.description.as_ref())
        .unwrap_or("");
    let description =
        Paragraph::new(Line::from(Span::raw(description))).alignment(Alignment::Center);
    frame.render_widget(description, chunks[5]);
}

/// Build the help-row line. In move-mode, the verb table is
/// suppressed in favor of a one-line reminder of what move-mode
/// does, since verbs don't dispatch in move-mode and advertising
/// them would mislead the user. When `move_mode_supported` is true
/// and the user isn't yet in move-mode, append an "Enter move-mode"
/// hint so the keypress is discoverable.
fn help_line<'a>(
    verbs: &'a [VerbHint<'a>],
    move_mode: bool,
    move_mode_supported: bool,
) -> Line<'a> {
    if move_mode {
        return Line::from(Span::styled(
            "move-mode: ↑↓ reorder · Esc/Enter exit",
            Style::default().add_modifier(Modifier::ITALIC),
        ));
    }
    let mut spans: Vec<Span<'a>> = Vec::with_capacity(verbs.len() * 4 + 2);
    for (idx, verb) in verbs.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::raw(" · "));
        }
        spans.push(Span::styled(
            verb.letter.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
        spans.push(Span::raw(verb.label));
    }
    if move_mode_supported {
        if !spans.is_empty() {
            spans.push(Span::raw(" · "));
        }
        spans.push(Span::styled(
            "Enter",
            Style::default().add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" move-mode"));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn default_state_is_cursor_zero_normal_mode() {
        let s = ListScreenState::new();
        assert_eq!(s.cursor(), 0);
        assert!(!s.move_mode());
    }

    #[test]
    fn down_advances_cursor() {
        let mut s = ListScreenState::new();
        let out = handle_key(&mut s, key(KeyCode::Down), 3, &[], false);
        assert_eq!(out, ListOutcome::Consumed);
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn up_at_top_wraps_to_bottom() {
        // ↑↓ wrap so the user never has to page back through the
        // list to reach the other end.
        let mut s = ListScreenState::new();
        let out = handle_key(&mut s, key(KeyCode::Up), 3, &[], false);
        assert_eq!(out, ListOutcome::Consumed);
        assert_eq!(s.cursor(), 2);
    }

    #[test]
    fn down_at_bottom_wraps_to_top() {
        let mut s = ListScreenState::new();
        s.set_cursor(2, 3);
        let out = handle_key(&mut s, key(KeyCode::Down), 3, &[], false);
        assert_eq!(out, ListOutcome::Consumed);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn arrows_on_empty_list_are_no_op() {
        // num_rows=0 has no valid cursor; the widget eats the keys
        // without panicking on a bad index.
        let mut s = ListScreenState::new();
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Up), 0, &[], false),
            ListOutcome::Consumed,
        );
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Down), 0, &[], false),
            ListOutcome::Consumed,
        );
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn enter_with_move_mode_supported_toggles_into_move_mode() {
        let mut s = ListScreenState::new();
        let out = handle_key(&mut s, key(KeyCode::Enter), 3, &[], true);
        assert_eq!(out, ListOutcome::Consumed);
        assert!(s.move_mode());
    }

    #[test]
    fn enter_without_move_mode_returns_activate() {
        // Main Menu shape: Enter on a row opens that screen, so the
        // outcome has to surface as a distinct caller signal — not
        // `Consumed` (which would silently do nothing).
        let mut s = ListScreenState::new();
        let out = handle_key(&mut s, key(KeyCode::Enter), 3, &[], false);
        assert_eq!(out, ListOutcome::Activate);
        assert!(!s.move_mode());
    }

    #[test]
    fn enter_on_empty_list_is_unhandled_regardless_of_move_mode_supported() {
        // No cursor target → no `Activate` and no point entering
        // move-mode against zero rows. Bubble up so the caller can
        // show its own empty-state guidance.
        let mut s = ListScreenState::new();
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Enter), 0, &[], true),
            ListOutcome::Unhandled,
        );
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Enter), 0, &[], false),
            ListOutcome::Unhandled,
        );
    }

    #[test]
    fn esc_in_normal_mode_is_unhandled_for_global_quit() {
        // The global Esc=quit handler in `app::update` runs only
        // when the screen returns the key unconsumed; if the widget
        // ate Esc here, the user couldn't quit from a list screen.
        let mut s = ListScreenState::new();
        let out = handle_key(&mut s, key(KeyCode::Esc), 3, &[], false);
        assert_eq!(out, ListOutcome::Unhandled);
    }

    #[test]
    fn esc_in_move_mode_exits_move_mode() {
        let mut s = ListScreenState::new();
        s.move_mode = true;
        let out = handle_key(&mut s, key(KeyCode::Esc), 3, &[], true);
        assert_eq!(out, ListOutcome::Consumed);
        assert!(!s.move_mode());
    }

    #[test]
    fn enter_in_move_mode_exits_move_mode() {
        // Symmetric with Esc — either key is accepted so whichever
        // one is closer to the user's hands works.
        let mut s = ListScreenState::new();
        s.move_mode = true;
        let out = handle_key(&mut s, key(KeyCode::Enter), 3, &[], true);
        assert_eq!(out, ListOutcome::Consumed);
        assert!(!s.move_mode());
    }

    #[test]
    fn move_mode_down_swaps_with_neighbor() {
        let mut s = ListScreenState::new();
        s.move_mode = true;
        s.set_cursor(0, 3);
        let out = handle_key(&mut s, key(KeyCode::Down), 3, &[], true);
        assert_eq!(out, ListOutcome::MoveSwap { from: 0, to: 1 });
        assert_eq!(s.cursor(), 1);
        assert!(s.move_mode());
    }

    #[test]
    fn move_mode_up_swaps_with_neighbor() {
        let mut s = ListScreenState::new();
        s.move_mode = true;
        s.set_cursor(2, 3);
        let out = handle_key(&mut s, key(KeyCode::Up), 3, &[], true);
        assert_eq!(out, ListOutcome::MoveSwap { from: 2, to: 1 });
        assert_eq!(s.cursor(), 1);
        assert!(s.move_mode());
    }

    #[test]
    fn move_mode_up_at_top_does_not_swap_or_wrap() {
        // Wrapping during move-mode would teleport a row across the
        // list — confusing UX. Stay put without emitting a swap.
        let mut s = ListScreenState::new();
        s.move_mode = true;
        s.set_cursor(0, 3);
        let out = handle_key(&mut s, key(KeyCode::Up), 3, &[], true);
        assert_eq!(out, ListOutcome::Consumed);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn move_mode_down_at_bottom_does_not_swap_or_wrap() {
        let mut s = ListScreenState::new();
        s.move_mode = true;
        s.set_cursor(2, 3);
        let out = handle_key(&mut s, key(KeyCode::Down), 3, &[], true);
        assert_eq!(out, ListOutcome::Consumed);
        assert_eq!(s.cursor(), 2);
    }

    #[test]
    fn move_mode_with_one_row_is_consumed_no_swap() {
        // num_rows<2 means a swap is impossible. Don't emit a
        // MoveSwap with from==to (caller would do a no-op clone)
        // and don't panic on the index math.
        let mut s = ListScreenState::new();
        s.move_mode = true;
        let out = handle_key(&mut s, key(KeyCode::Down), 1, &[], true);
        assert_eq!(out, ListOutcome::Consumed);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn move_mode_verb_letter_is_unhandled() {
        // Verbs are gated to normal mode so pressing one (e.g. 'd'
        // for delete) while reordering can't accidentally trigger
        // a destructive action.
        let mut s = ListScreenState::new();
        s.move_mode = true;
        let out = handle_key(&mut s, key(KeyCode::Char('d')), 3, &['d'], true);
        assert_eq!(out, ListOutcome::Unhandled);
        assert!(s.move_mode());
    }

    #[test]
    fn registered_verb_letter_emits_action() {
        let mut s = ListScreenState::new();
        let out = handle_key(&mut s, key(KeyCode::Char('a')), 3, &['a', 'd'], false);
        assert_eq!(out, ListOutcome::Action('a'));
    }

    #[test]
    fn unregistered_letter_is_unhandled() {
        // Only letters in the registered verb list emit actions;
        // everything else bubbles up so the screen / global
        // dispatcher can choose its own fallback.
        let mut s = ListScreenState::new();
        let out = handle_key(&mut s, key(KeyCode::Char('z')), 3, &['a', 'd'], false);
        assert_eq!(out, ListOutcome::Unhandled);
    }

    #[test]
    fn verb_letter_with_modifier_is_unhandled() {
        // Shift+a or Ctrl+a should NOT match the registered 'a'
        // verb — the user pressing a chord never meant to trigger
        // the bare-letter action.
        let mut s = ListScreenState::new();
        let shift = handle_key(
            &mut s,
            key_mod(KeyCode::Char('a'), KeyModifiers::SHIFT),
            3,
            &['a'],
            false,
        );
        assert_eq!(shift, ListOutcome::Unhandled);
        let ctrl = handle_key(
            &mut s,
            key_mod(KeyCode::Char('a'), KeyModifiers::CONTROL),
            3,
            &['a'],
            false,
        );
        assert_eq!(ctrl, ListOutcome::Unhandled);
    }

    #[test]
    fn uppercase_letter_in_verb_list_still_does_not_dispatch() {
        // Defense against a caller that violates the
        // lowercase-only `VerbHint::letter` contract: even if
        // 'A' is registered, the dispatch arm rejects it via the
        // ascii-lowercase guard. Without that guard, a typo'd
        // VerbHint would let users trigger actions through chord
        // keys that produce uppercase Chars.
        let mut s = ListScreenState::new();
        let out = handle_key(&mut s, key(KeyCode::Char('A')), 3, &['A'], false);
        assert_eq!(out, ListOutcome::Unhandled);
    }

    #[test]
    fn verb_letter_on_empty_list_returns_unhandled() {
        // Empty-list defenses are uniform across Enter and verb
        // letters; both bubble up so a caller's `Action` handler
        // that reads `state.cursor()` never sees an out-of-range
        // index.
        let mut s = ListScreenState::new();
        let out = handle_key(&mut s, key(KeyCode::Char('a')), 0, &['a'], false);
        assert_eq!(out, ListOutcome::Unhandled);
    }

    #[test]
    fn move_mode_supported_false_clears_stale_state_move_mode() {
        // A screen that previously rendered with move-mode support
        // (entering move-mode) and then flips support back off
        // shouldn't keep dispatching through `handle_move_mode` —
        // the help row already stopped advertising move-mode, so
        // keeping the dispatch live is a split-brain bug.
        let mut s = ListScreenState::new();
        s.move_mode = true;
        let out = handle_key(&mut s, key(KeyCode::Down), 3, &[], false);
        assert!(!s.move_mode());
        // Subsequent dispatch ran through normal-mode → ↓ wraps
        // from cursor 0 to cursor 1 instead of swapping rows.
        assert_eq!(out, ListOutcome::Consumed);
        assert_eq!(s.cursor(), 1);
    }

    #[test]
    fn enter_with_stale_cursor_clamps_before_activate() {
        // The clamp at the top of `handle_key` runs before the
        // Enter→Activate arm, so a caller reading `state.cursor()`
        // after Activate gets a valid index even when the list
        // shrank under the cursor between events.
        let mut s = ListScreenState::new();
        s.cursor = 5;
        let out = handle_key(&mut s, key(KeyCode::Enter), 3, &[], false);
        assert_eq!(out, ListOutcome::Activate);
        assert_eq!(s.cursor(), 2);
    }

    #[test]
    fn move_mode_arrows_with_modifier_are_unhandled() {
        // The chord guard at the top of `handle_move_mode` is the
        // entire defense against accidental swaps from chord keys.
        // A simplification pass that drops the guard would silently
        // make Shift+Down reorder rows. Ctrl variant covers a
        // future modifier-set typo that special-cases SHIFT.
        for mods in [KeyModifiers::SHIFT, KeyModifiers::CONTROL] {
            let mut s = ListScreenState::new();
            s.move_mode = true;
            s.set_cursor(1, 3);
            let out = handle_key(&mut s, key_mod(KeyCode::Down, mods), 3, &[], true);
            assert_eq!(out, ListOutcome::Unhandled, "mods={mods:?}");
            assert_eq!(
                s.cursor(),
                1,
                "chord arrow must not move cursor (mods={mods:?})"
            );
        }
    }

    #[test]
    fn arrows_with_modifier_in_normal_mode_are_unhandled() {
        // ↑/↓ match `(KeyCode::Up, KeyModifiers::NONE)` exactly;
        // chord arrows fall through to the `_` arm. A future
        // change to `(KeyCode::Up, _)` would silently eat chord
        // arrows that a parent screen might want to interpret.
        for mods in [KeyModifiers::SHIFT, KeyModifiers::CONTROL] {
            let mut s = ListScreenState::new();
            let out = handle_key(&mut s, key_mod(KeyCode::Down, mods), 3, &[], false);
            assert_eq!(out, ListOutcome::Unhandled, "mods={mods:?}");
            assert_eq!(s.cursor(), 0);
        }
    }

    #[test]
    fn handle_key_clamps_cursor_and_move_mode_in_one_call() {
        // Combined stale state: cursor past end, move_mode flag
        // stale, and `move_mode_supported = false`. Pin the
        // post-clamp outcome so a refactor that consolidates the
        // two clamps into a helper but forgets to call one of
        // them still trips this test.
        let mut s = ListScreenState::new();
        s.cursor = 99;
        s.move_mode = true;
        let out = handle_key(&mut s, key(KeyCode::Down), 3, &[], false);
        assert!(!s.move_mode(), "move_mode should clear");
        // Cursor was clamped from 99 → 2 (last row); ↓ in normal
        // mode wraps from 2 → 0.
        assert_eq!(out, ListOutcome::Consumed);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn cursor_is_clamped_when_list_shrinks_under_it() {
        // A delete that drops the row count without the caller
        // calling set_cursor leaves the cursor past the end. The
        // widget defends by clamping on the next handle_key.
        let mut s = ListScreenState::new();
        s.cursor = 5; // pretend we deleted from a 6-row list
        let out = handle_key(&mut s, key(KeyCode::Down), 3, &[], false);
        assert_eq!(out, ListOutcome::Consumed);
        // After clamp, cursor was 2 (last row); ↓ wraps to 0.
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn set_cursor_clamps_to_valid_range() {
        let mut s = ListScreenState::new();
        s.set_cursor(99, 4);
        assert_eq!(s.cursor(), 3);
        s.set_cursor(99, 0);
        assert_eq!(s.cursor(), 0);
    }

    #[test]
    fn render_smoke_paints_title_cursor_and_description() {
        // End-to-end render check via TestBackend. The frame
        // contains the title, the cursor symbol on the highlighted
        // row, the rendered row labels, and the highlighted row's
        // description. Catches regressions in layout chunk math
        // and in the highlight_symbol/list-state wiring that pure
        // `handle_key` tests can't.
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).expect("backend");
        let mut s = ListScreenState::new();
        s.set_cursor(1, 3);
        let rows = [
            ListRowData {
                label: Cow::Borrowed("First"),
                description: Cow::Borrowed("desc-A"),
            },
            ListRowData {
                label: Cow::Borrowed("Second"),
                description: Cow::Borrowed("desc-B"),
            },
            ListRowData {
                label: Cow::Borrowed("Third"),
                description: Cow::Borrowed("desc-C"),
            },
        ];
        let verbs = [VerbHint {
            letter: 'a',
            label: "add",
        }];
        let view = ListScreenView {
            title: "Demo",
            rows: &rows,
            verbs: &verbs,
            move_mode_supported: false,
        };
        terminal
            .draw(|frame| render(&s, &view, frame.area(), frame))
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        let dump: String = (0..buf.area.height)
            .map(|y| {
                let row: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect();
                format!("{row}\n")
            })
            .collect();
        assert!(dump.contains("Demo"), "title missing:\n{dump}");
        assert!(dump.contains("a add"), "help row missing 'a add':\n{dump}");
        assert!(
            dump.contains("▶ Second"),
            "cursor on row 1 missing:\n{dump}"
        );
        assert!(dump.contains("First"), "row 0 label missing:\n{dump}");
        assert!(dump.contains("Third"), "row 2 label missing:\n{dump}");
        assert!(
            dump.contains("desc-B"),
            "highlighted row's description missing:\n{dump}",
        );
        assert!(
            !dump.contains("desc-A"),
            "non-highlighted description leaked:\n{dump}",
        );
    }

    #[test]
    fn render_in_move_mode_shows_move_mode_help_row() {
        // Pin the help-row swap: in move-mode the verb table is
        // hidden in favor of a move-mode reminder. Without this the
        // help row would advertise verbs that are gated off.
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).expect("backend");
        let mut s = ListScreenState::new();
        s.move_mode = true;
        let rows = [ListRowData {
            label: Cow::Borrowed("Only"),
            description: Cow::Borrowed("desc"),
        }];
        let verbs = [VerbHint {
            letter: 'a',
            label: "add",
        }];
        let view = ListScreenView {
            title: "Demo",
            rows: &rows,
            verbs: &verbs,
            move_mode_supported: true,
        };
        terminal
            .draw(|frame| render(&s, &view, frame.area(), frame))
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        let dump: String = (0..buf.area.height)
            .map(|y| {
                let row: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect();
                format!("{row}\n")
            })
            .collect();
        assert!(
            dump.contains("move-mode"),
            "move-mode help row missing:\n{dump}",
        );
        assert!(
            !dump.contains("a add"),
            "verb-table help row leaked into move-mode:\n{dump}",
        );
    }

    #[test]
    fn render_advertises_enter_move_mode_when_supported_but_not_active() {
        // Discoverability: a list that supports move-mode but isn't
        // currently in move-mode should mention Enter in the help
        // row. Without this hint the user has to guess that Enter
        // does anything different than on a non-reorderable list.
        let backend = TestBackend::new(60, 12);
        let mut terminal = Terminal::new(backend).expect("backend");
        let s = ListScreenState::new();
        let rows = [ListRowData {
            label: Cow::Borrowed("Only"),
            description: Cow::Borrowed("desc"),
        }];
        let verbs = [VerbHint {
            letter: 'a',
            label: "add",
        }];
        let view = ListScreenView {
            title: "Demo",
            rows: &rows,
            verbs: &verbs,
            move_mode_supported: true,
        };
        terminal
            .draw(|frame| render(&s, &view, frame.area(), frame))
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        let dump: String = (0..buf.area.height)
            .map(|y| {
                let row: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect();
                format!("{row}\n")
            })
            .collect();
        assert!(dump.contains("a add"), "verbs still listed:\n{dump}");
        assert!(
            dump.contains("Enter move-mode"),
            "Enter hint missing:\n{dump}",
        );
    }

    #[test]
    fn render_clamps_stale_cursor_for_both_highlight_and_description() {
        // `handle_key` clamps on entry, but `render` can run
        // between a data mutation and the next event. The
        // description lookup is the load-bearing side: without
        // the clamp, `view.rows.get(99)` returns `None` and the
        // slot goes blank while ratatui's own internal clamp
        // still highlights the last row. Pinning both halves here
        // catches a regression that drops the description clamp
        // and pretends the description is "supposed to be empty
        // at the edge."
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).expect("backend");
        let mut s = ListScreenState::new();
        s.cursor = 99;
        let rows = [
            ListRowData {
                label: Cow::Borrowed("First"),
                description: Cow::Borrowed("desc-A"),
            },
            ListRowData {
                label: Cow::Borrowed("Second"),
                description: Cow::Borrowed("desc-B"),
            },
            ListRowData {
                label: Cow::Borrowed("Third"),
                description: Cow::Borrowed("desc-C"),
            },
        ];
        let verbs: [VerbHint<'_>; 0] = [];
        let view = ListScreenView {
            title: "Demo",
            rows: &rows,
            verbs: &verbs,
            move_mode_supported: false,
        };
        terminal
            .draw(|frame| render(&s, &view, frame.area(), frame))
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        let dump: String = (0..buf.area.height)
            .map(|y| {
                let row: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect();
                format!("{row}\n")
            })
            .collect();
        assert!(
            dump.contains("▶ Third"),
            "highlight should clamp to last row:\n{dump}",
        );
        assert!(
            dump.contains("desc-C"),
            "description should clamp to same row as highlight:\n{dump}",
        );
    }

    #[test]
    fn render_with_empty_rows_does_not_panic() {
        // A list with zero rows still has to render the chrome
        // (title, help) without indexing into an empty rows slice.
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("backend");
        let s = ListScreenState::new();
        let rows: [ListRowData<'_>; 0] = [];
        let verbs: [VerbHint<'_>; 0] = [];
        let view = ListScreenView {
            title: "Empty",
            rows: &rows,
            verbs: &verbs,
            move_mode_supported: false,
        };
        terminal
            .draw(|frame| render(&s, &view, frame.area(), frame))
            .expect("draw");
        let buf = terminal.backend().buffer().clone();
        let dump: String = (0..buf.area.height)
            .map(|y| {
                let row: String = (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect();
                format!("{row}\n")
            })
            .collect();
        assert!(dump.contains("Empty"), "title still renders:\n{dump}");
    }
}
