//! Raw value editor — single-line text input that replaces the
//! cursor segment with arbitrary content.
//!
//! Invoked from the items editor via the `r` verb. Useful for
//! segment IDs the picker doesn't expose (plugin IDs or future
//! built-ins). The TOML accepts whatever the user types and the
//! load layer warns on unknown IDs. The preview path doesn't
//! resolve plugin segments, so plugin IDs round-trip correctly
//! into the file but render as "unknown segment id" in the
//! preview. Document mutation lives in
//! `super::items_editor::apply_replace`; this module is UI-only.

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
use super::items_editor::{self, ItemsEditorState};

/// What field of the segments-array entry the editor is mutating.
/// Drives `apply_replace` dispatch: a separator-character commit
/// updates the inline table's `character` field; a segment-id
/// commit replaces the array entry with a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RawTarget {
    /// Replace the array entry at `target_idx` with the buffer as a
    /// TOML string. The default for non-separator rows.
    SegmentId,
    /// Update the `character` field of the inline-table separator
    /// at `target_idx`. An empty buffer clears the field (boundary
    /// falls back to `[layout_options].separator`).
    SeparatorCharacter,
}

/// Editor state. `text` is the buffer; `cursor` is a char index
/// into `text` (0..=text.chars().count()). `target_idx` is the
/// segments-array position being mutated; `target_kind` selects
/// which field (segment id vs separator character); `prev` is the
/// editor we return to.
#[derive(Debug)]
pub(super) struct RawValueEditorState {
    text: String,
    cursor: usize,
    target_idx: usize,
    target_kind: RawTarget,
    prev: ItemsEditorState,
}

impl RawValueEditorState {
    pub(super) fn new(
        initial: String,
        target_idx: usize,
        target_kind: RawTarget,
        prev: ItemsEditorState,
    ) -> Self {
        let cursor = initial.chars().count();
        Self {
            text: initial,
            cursor,
            target_idx,
            target_kind,
            prev,
        }
    }
}

/// Drive the raw editor through one keypress. Esc cancels, Enter
/// commits, character keys insert at cursor, navigation keys move
/// the cursor. Modifier-bearing keys other than plain Shift are
/// inert so terminal chord shortcuts don't insert garbage.
pub(super) fn update(
    state: &mut RawValueEditorState,
    document: &mut DocumentMut,
    config: &mut config::Config,
    key: KeyEvent,
) -> ScreenOutcome {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, KeyModifiers::NONE) => {
            let prev = mem::take(&mut state.prev);
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(prev))
        }
        (KeyCode::Enter, KeyModifiers::NONE) => {
            let target_idx = state.target_idx;
            let target_kind = state.target_kind;
            let new_value = mem::take(&mut state.text);
            let prev = mem::take(&mut state.prev);
            items_editor::apply_replace(prev, document, config, target_idx, target_kind, &new_value)
        }
        (KeyCode::Backspace, KeyModifiers::NONE) => {
            delete_before_cursor(state);
            ScreenOutcome::Stay
        }
        (KeyCode::Delete, KeyModifiers::NONE) => {
            delete_at_cursor(state);
            ScreenOutcome::Stay
        }
        (KeyCode::Left, KeyModifiers::NONE) => {
            state.cursor = state.cursor.saturating_sub(1);
            ScreenOutcome::Stay
        }
        (KeyCode::Right, KeyModifiers::NONE) => {
            state.cursor = (state.cursor + 1).min(state.text.chars().count());
            ScreenOutcome::Stay
        }
        (KeyCode::Home, KeyModifiers::NONE) => {
            state.cursor = 0;
            ScreenOutcome::Stay
        }
        (KeyCode::End, KeyModifiers::NONE) => {
            state.cursor = state.text.chars().count();
            ScreenOutcome::Stay
        }
        (KeyCode::Char(c), mods) if is_text_input_modifier(mods) => {
            insert_char(state, c);
            ScreenOutcome::Stay
        }
        _ => ScreenOutcome::Stay,
    }
}

/// Predicate for `KeyCode::Char` modifier sets we treat as text
/// input. Accepts no-modifier and Shift (uppercase letters), plus
/// the `CONTROL | ALT` AltGr combination (with optional Shift)
/// that produces characters like `@`, `{`, `}`, `€` on non-US
/// keyboard layouts. Ctrl-alone or Alt-alone is treated as a
/// chord shortcut and rejected so terminal hotkeys don't leak
/// text into the buffer.
fn is_text_input_modifier(mods: KeyModifiers) -> bool {
    if mods.is_empty() || mods == KeyModifiers::SHIFT {
        return true;
    }
    let altgr = KeyModifiers::CONTROL | KeyModifiers::ALT;
    mods == altgr || mods == altgr | KeyModifiers::SHIFT
}

fn delete_before_cursor(state: &mut RawValueEditorState) {
    if state.cursor == 0 {
        return;
    }
    let new_text: String = state
        .text
        .chars()
        .enumerate()
        .filter_map(|(i, c)| (i + 1 != state.cursor).then_some(c))
        .collect();
    state.text = new_text;
    state.cursor -= 1;
}

fn delete_at_cursor(state: &mut RawValueEditorState) {
    if state.cursor >= state.text.chars().count() {
        return;
    }
    let new_text: String = state
        .text
        .chars()
        .enumerate()
        .filter_map(|(i, c)| (i != state.cursor).then_some(c))
        .collect();
    state.text = new_text;
}

fn insert_char(state: &mut RawValueEditorState, c: char) {
    let mut new_text = String::with_capacity(state.text.len() + c.len_utf8());
    let mut inserted = false;
    for (i, ch) in state.text.chars().enumerate() {
        if i == state.cursor {
            new_text.push(c);
            inserted = true;
        }
        new_text.push(ch);
    }
    if !inserted {
        new_text.push(c);
    }
    state.text = new_text;
    state.cursor += 1;
}

/// Render the editor. Layout: title row, help row, blank, the
/// text input row with cursor highlighted via reverse video on a
/// single character (or a trailing reverse-video space when the
/// cursor sits at end-of-text).
pub(super) fn view(state: &RawValueEditorState, frame: &mut Frame, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // help
            Constraint::Length(1), // blank
            Constraint::Length(1), // text input
            Constraint::Min(1),    // remainder
        ])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled(
        " edit raw value ",
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(title, chunks[0]);

    let help = Paragraph::new(Line::from(vec![
        Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" commit · "),
        Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" cancel · "),
        Span::styled("← →", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" cursor · "),
        Span::styled("Bksp", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" delete"),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(help, chunks[1]);

    let text_area = chunks[3];
    let mut spans: Vec<Span<'_>> = Vec::with_capacity(3);
    let chars: Vec<char> = state.text.chars().collect();
    let cursor = state.cursor.min(chars.len());
    let cursor_style = Style::default().add_modifier(Modifier::REVERSED);
    if cursor < chars.len() {
        if cursor > 0 {
            spans.push(Span::raw(chars[..cursor].iter().collect::<String>()));
        }
        spans.push(Span::styled(chars[cursor].to_string(), cursor_style));
        if cursor + 1 < chars.len() {
            spans.push(Span::raw(chars[cursor + 1..].iter().collect::<String>()));
        }
    } else {
        // Cursor sits past the last char — render a reverse-video
        // space so the user can see the insertion point.
        if !chars.is_empty() {
            spans.push(Span::raw(state.text.clone()));
        }
        spans.push(Span::styled(" ", cursor_style));
    }
    let body = Paragraph::new(Line::from(spans)).alignment(Alignment::Center);
    frame.render_widget(body, text_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::main_menu::MainMenuState;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn document(toml: &str) -> DocumentMut {
        toml.parse().expect("test toml must parse")
    }

    fn editor(initial: &str) -> RawValueEditorState {
        RawValueEditorState::new(
            initial.to_string(),
            0,
            RawTarget::SegmentId,
            ItemsEditorState::new(
                items_editor::LineKey::Single,
                items_editor::ItemsEditorPrev::MainMenu(MainMenuState::default()),
            ),
        )
    }

    #[test]
    fn enter_commits_text_and_returns_to_items_editor() {
        let mut s = editor("model");
        let mut doc = document("[line]\nsegments = [\"model\"]\n");
        let mut cfg = config::Config::default();
        for c in ['_', '2'] {
            update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char(c)));
        }
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(_))
        ));
        let line = cfg.line.expect("line reparsed");
        let ids: Vec<&str> = line
            .segments
            .iter()
            .filter_map(config::LineEntry::segment_id)
            .collect();
        assert_eq!(ids, vec!["model_2"]);
    }

    #[test]
    fn esc_cancels_and_leaves_document_untouched() {
        let raw = "[line]\nsegments = [\"model\"]\n";
        let mut s = editor("model");
        let mut doc = document(raw);
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('x')));
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Esc));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(_))
        ));
        assert_eq!(doc.to_string(), raw);
    }

    #[test]
    fn backspace_deletes_char_before_cursor() {
        let mut s = editor("abc");
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Backspace));
        assert_eq!(s.text, "ab");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn backspace_in_middle_removes_char_left_of_cursor() {
        // Cursor=2 in "abc"; Backspace removes 'b', cursor lands
        // at 1. Pins the mid-string deletion path that the
        // end-of-string test doesn't cover.
        let mut s = editor("abc");
        s.cursor = 2;
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Backspace));
        assert_eq!(s.text, "ac");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn backspace_on_multi_byte_char_does_not_panic() {
        // The chars()-based deletion path must handle multi-byte
        // characters without slicing through a UTF-8 boundary.
        // Insert 'é' then Backspace; observe empty buffer.
        let mut s = editor("");
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('é')));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Backspace));
        assert_eq!(s.text, "");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn backspace_at_zero_is_inert() {
        let mut s = editor("abc");
        s.cursor = 0;
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Backspace));
        assert_eq!(s.text, "abc");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn delete_removes_char_at_cursor() {
        let mut s = editor("abc");
        s.cursor = 1;
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Delete));
        assert_eq!(s.text, "ac");
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn arrows_move_cursor_within_bounds() {
        let mut s = editor("abc");
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Right));
        assert_eq!(s.cursor, 3, "Right at end is a no-op");
        for _ in 0..3 {
            update(&mut s, &mut doc, &mut cfg, key(KeyCode::Left));
        }
        assert_eq!(s.cursor, 0);
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Left));
        assert_eq!(s.cursor, 0, "Left at 0 is a no-op");
    }

    #[test]
    fn home_and_end_jump_to_bounds() {
        let mut s = editor("abc");
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Home));
        assert_eq!(s.cursor, 0);
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::End));
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn char_inserted_at_cursor_advances_cursor() {
        let mut s = editor("ac");
        s.cursor = 1;
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('b')));
        assert_eq!(s.text, "abc");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn shift_char_is_treated_as_text_input() {
        // Shift+a → uppercase A; the editor accepts it as text,
        // not as a chord.
        let mut s = editor("");
        let mut doc = document("");
        let mut cfg = config::Config::default();
        let chord = KeyEvent::new(KeyCode::Char('A'), KeyModifiers::SHIFT);
        update(&mut s, &mut doc, &mut cfg, chord);
        assert_eq!(s.text, "A");
    }

    #[test]
    fn altgr_modified_chars_insert_as_text() {
        // On non-US keyboard layouts, `@`, `{`, `}`, `€` etc.
        // arrive with CONTROL|ALT set (AltGr). These must insert
        // as text — rejecting them would make those characters
        // impossible to type, breaking edits for plugin IDs and
        // segment names that need them.
        let mut s = editor("");
        let mut doc = document("");
        let mut cfg = config::Config::default();
        let altgr = KeyEvent::new(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        update(&mut s, &mut doc, &mut cfg, altgr);
        assert_eq!(s.text, "@");
        // Shift+AltGr (some layouts) — also accepted.
        let shift_altgr = KeyEvent::new(
            KeyCode::Char('Œ'),
            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT,
        );
        update(&mut s, &mut doc, &mut cfg, shift_altgr);
        assert_eq!(s.text, "@Œ");
    }

    #[test]
    fn ctrl_and_alt_modified_chars_are_inert() {
        // Ctrl+S / Ctrl+C are intercepted at the app level and
        // never reach this update. Ctrl+other-letter and Alt+letter
        // chords shouldn't insert anything — pin so a future relax
        // of the modifier match doesn't let chord keys leak text.
        let mut s = editor("");
        let mut doc = document("");
        let mut cfg = config::Config::default();
        for mods in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
            let chord = KeyEvent::new(KeyCode::Char('a'), mods);
            update(&mut s, &mut doc, &mut cfg, chord);
            assert_eq!(
                s.text, "",
                "chord chars (mods={mods:?}) must not insert text"
            );
        }
    }

    #[test]
    fn unicode_char_inserts_correctly() {
        // Multi-byte char insertion. The cursor is a char index
        // (not byte), so this exercises the chars()-based
        // accounting rather than naive byte indexing.
        let mut s = editor("");
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('é')));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('日')));
        assert_eq!(s.text, "é日");
        assert_eq!(s.cursor, 2);
    }
}
