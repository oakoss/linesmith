//! Reusable `PropertyScreen` widget per ADR-0016, sibling of
//! [`super::list_screen`].
//!
//! Foundation for screens that aren't list-shaped: Global Overrides,
//! Powerline Setup, Terminal Options, Color Level, and the
//! per-segment editor templates. Rows are addressed by per-row letter
//! keys (no cursor, no navigation); each row carries its own letter
//! and a self-describing hint, so the help-row chrome ListScreen
//! needs is unnecessary here.
//!
//! Keymap:
//!
//! - lowercase ASCII letter with no modifiers, claimed by some row
//!   in `view.rows` — `Action(letter)`. The widget doesn't know
//!   what each letter means; the caller maps it to its own action
//!   enum, exactly like [`super::list_screen`].
//! - everything else (Esc, Up/Down, modified keys, unregistered
//!   letters) — `Unhandled`, so global handling in [`super::app`]
//!   (back-nav, quit) still runs.
//!
//! Unlike [`super::list_screen::handle_key`], the dispatch set is
//! derived from `view.rows` rather than a parallel `&[char]` arg —
//! that eliminates the "advertised letter doesn't dispatch" / "key
//! dispatches but no row claims it" drift class at the source.
//!
//! No production consumer wires this module in yet; the per-segment
//! editor templates (lsm-herx.10-.14), Terminal Options (lsm-herx.23),
//! Global Overrides (lsm-herx.27), and Powerline Setup (lsm-herx.30)
//! will. Hence the module-level dead-code allow — drop once any
//! consumer lands.
#![allow(dead_code)]

use std::borrow::Cow;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

/// Carries no per-screen state today. Kept as a distinct type so the
/// consumer pattern matches [`super::list_screen::ListScreenState`]:
/// `Default` for `mem::take` on screen transitions, with room to grow
/// without breaking call sites.
#[derive(Debug, Default, Clone)]
pub(super) struct PropertyScreenState {}

impl PropertyScreenState {
    pub(super) fn new() -> Self {
        Self::default()
    }
}

/// Outcome of one [`handle_key`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PropertyOutcome {
    /// A registered letter was pressed. Caller looks up the letter
    /// in its own action table and applies the mutation.
    Action(char),
    /// Widget did not claim the key; caller can apply its own
    /// fallback or let it bubble up to global handling.
    Unhandled,
}

/// One labeled row. The variant determines the render shape and
/// the inline hint text; dispatch is by letter alone.
#[derive(Debug, Clone)]
pub(super) enum PropertyRow<'a> {
    /// `"<label>: ✓ Enabled — Press (<letter>) to toggle"`
    Toggle {
        letter: char,
        label: Cow<'a, str>,
        enabled: bool,
    },
    /// `"<label>: <value_display> — (<cycle_letter>) cycle[, (<clear_letter>) clear]"`
    ValueCycle {
        cycle_letter: char,
        clear_letter: Option<char>,
        label: Cow<'a, str>,
        value_display: Cow<'a, str>,
    },
    /// `"<label>: <value_display> — Press (<letter>) to edit"`
    EditField {
        letter: char,
        label: Cow<'a, str>,
        value_display: Cow<'a, str>,
    },
    /// `"<label> [<indicator>] — Press (<letter>) to open"`
    Drilldown {
        letter: char,
        label: Cow<'a, str>,
        indicator: Cow<'a, str>,
    },
}

/// Optional one-line header rendered between title and rows.
/// Powerline Setup uses this for `"Font Status: ✓ Installed"` so
/// the setup state is visible without dipping into a row.
#[derive(Debug, Clone)]
pub(super) struct StatusHeader<'a> {
    pub(super) label: Cow<'a, str>,
    pub(super) value: Cow<'a, str>,
}

/// One paragraph in the footer `Note:` block. Rendered as
/// `"  • <bullet> — <text>"`.
#[derive(Debug, Clone)]
pub(super) struct NoteParagraph<'a> {
    pub(super) bullet: Cow<'a, str>,
    pub(super) text: Cow<'a, str>,
}

/// Caller-supplied configuration for one [`render`] call.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub(super) struct PropertyScreenView<'a> {
    pub(super) title: &'a str,
    pub(super) status_header: Option<StatusHeader<'a>>,
    pub(super) rows: &'a [PropertyRow<'a>],
    pub(super) note: &'a [NoteParagraph<'a>],
}

/// Every letter any row in `view` claims (including a
/// [`PropertyRow::ValueCycle`]'s `clear_letter` when present), in row
/// order. Single source of truth for both dispatch and the debug
/// uniqueness invariant in [`render`].
fn row_letters(view: &PropertyScreenView<'_>) -> Vec<char> {
    let mut out = Vec::with_capacity(view.rows.len() * 2);
    for row in view.rows {
        match row {
            PropertyRow::Toggle { letter, .. }
            | PropertyRow::EditField { letter, .. }
            | PropertyRow::Drilldown { letter, .. } => out.push(*letter),
            PropertyRow::ValueCycle {
                cycle_letter,
                clear_letter,
                ..
            } => {
                out.push(*cycle_letter);
                if let Some(c) = clear_letter {
                    out.push(*c);
                }
            }
        }
    }
    out
}

/// Pure key dispatch. `state` is inert today; it exists to keep the
/// contract symmetric with ListScreen and leave room to grow. The
/// dispatch set is derived from `view.rows` — letters and dispatcher
/// can't drift apart.
pub(super) fn handle_key(
    _state: &mut PropertyScreenState,
    key: KeyEvent,
    view: &PropertyScreenView<'_>,
) -> PropertyOutcome {
    if key.modifiers != KeyModifiers::NONE {
        return PropertyOutcome::Unhandled;
    }
    let KeyCode::Char(c) = key.code else {
        return PropertyOutcome::Unhandled;
    };
    if !c.is_ascii_lowercase() {
        return PropertyOutcome::Unhandled;
    }
    if row_letters(view).contains(&c) {
        PropertyOutcome::Action(c)
    } else {
        PropertyOutcome::Unhandled
    }
}

/// Render the property screen into `area`. Layout (top to bottom):
/// title, optional status header, blank, rows, blank, optional
/// note paragraphs. Title always renders; the optional sections
/// collapse to zero-height when absent.
pub(super) fn render(
    _state: &PropertyScreenState,
    view: &PropertyScreenView<'_>,
    area: Rect,
    frame: &mut Frame,
) {
    // Two rows claiming the same letter would silently steal each
    // other's dispatch; a non-lowercase letter renders a hint for a
    // key `handle_key` refuses. Catch both at first render in debug.
    debug_assert!(
        {
            let letters = row_letters(view);
            let mut seen = std::collections::BTreeSet::new();
            letters
                .iter()
                .all(|c| c.is_ascii_lowercase() && seen.insert(*c))
        },
        "PropertyScreen row letters must be unique ASCII-lowercase: {:?}",
        row_letters(view),
    );

    let status_height: u16 = u16::from(view.status_header.is_some());
    let note_height = u16::try_from(view.note.len()).unwrap_or(u16::MAX);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(status_height),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(note_height),
        ])
        .split(area);

    let title = Paragraph::new(Line::from(Span::styled(
        view.title,
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(title, chunks[0]);

    if let Some(header) = view.status_header.as_ref() {
        let line = Line::from(vec![
            Span::raw(header.label.as_ref()),
            Span::raw(": "),
            Span::styled(
                header.value.as_ref(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(Paragraph::new(line).alignment(Alignment::Center), chunks[1]);
    }

    let row_lines: Vec<Line<'_>> = view.rows.iter().map(render_row).collect();
    frame.render_widget(Paragraph::new(row_lines), chunks[3]);

    if !view.note.is_empty() {
        let note_lines: Vec<Line<'_>> = view
            .note
            .iter()
            .map(|p| {
                Line::from(vec![
                    Span::raw("  • "),
                    Span::styled(
                        p.bullet.as_ref(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" — "),
                    Span::raw(p.text.as_ref()),
                ])
            })
            .collect();
        frame.render_widget(Paragraph::new(note_lines), chunks[5]);
    }
}

fn render_row<'a>(row: &'a PropertyRow<'a>) -> Line<'a> {
    match row {
        PropertyRow::Toggle {
            letter,
            label,
            enabled,
        } => {
            let state_text = if *enabled {
                "✓ Enabled"
            } else {
                "✗ Disabled"
            };
            Line::from(vec![
                Span::raw(label.as_ref()),
                Span::raw(": "),
                Span::styled(state_text, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(" — Press ("),
                Span::styled(
                    letter.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(") to toggle"),
            ])
        }
        PropertyRow::ValueCycle {
            cycle_letter,
            clear_letter,
            label,
            value_display,
        } => {
            let mut spans = vec![
                Span::raw(label.as_ref()),
                Span::raw(": "),
                Span::styled(
                    value_display.as_ref(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(" — ("),
                Span::styled(
                    cycle_letter.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(") cycle"),
            ];
            if let Some(clear) = clear_letter {
                spans.push(Span::raw(", ("));
                spans.push(Span::styled(
                    clear.to_string(),
                    Style::default().add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::raw(") clear"));
            }
            Line::from(spans)
        }
        PropertyRow::EditField {
            letter,
            label,
            value_display,
        } => Line::from(vec![
            Span::raw(label.as_ref()),
            Span::raw(": "),
            Span::styled(
                value_display.as_ref(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" — Press ("),
            Span::styled(
                letter.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(") to edit"),
        ]),
        PropertyRow::Drilldown {
            letter,
            label,
            indicator,
        } => Line::from(vec![
            Span::raw(label.as_ref()),
            Span::raw(" ["),
            Span::styled(
                indicator.as_ref(),
                Style::default().add_modifier(Modifier::ITALIC),
            ),
            Span::raw("] — Press ("),
            Span::styled(
                letter.to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(") to open"),
        ]),
    }
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

    /// One `Toggle` row per letter, with placeholder copy. Tests can
    /// state their dispatch set declaratively while exercising the
    /// real letter-derivation path.
    fn toggle_rows(letters: &[char]) -> Vec<PropertyRow<'static>> {
        letters
            .iter()
            .map(|&c| PropertyRow::Toggle {
                letter: c,
                label: Cow::Borrowed("x"),
                enabled: false,
            })
            .collect()
    }

    fn view_for<'a>(rows: &'a [PropertyRow<'a>]) -> PropertyScreenView<'a> {
        PropertyScreenView {
            title: "t",
            status_header: None,
            rows,
            note: &[],
        }
    }

    #[test]
    fn row_letter_dispatches_action() {
        let mut s = PropertyScreenState::new();
        let rows = toggle_rows(&['o', 'f']);
        let out = handle_key(&mut s, key(KeyCode::Char('o')), &view_for(&rows));
        assert_eq!(out, PropertyOutcome::Action('o'));
    }

    #[test]
    fn unclaimed_lowercase_letter_is_unhandled() {
        // Defends against a screen advertising letter X then later
        // dropping the row that owns X: a stale keypress must not
        // silently dispatch to a no-longer-present row.
        let mut s = PropertyScreenState::new();
        let rows = toggle_rows(&['o', 'f']);
        let out = handle_key(&mut s, key(KeyCode::Char('z')), &view_for(&rows));
        assert_eq!(out, PropertyOutcome::Unhandled);
    }

    #[test]
    fn empty_rows_make_every_letter_unhandled() {
        let mut s = PropertyScreenState::new();
        let rows: Vec<PropertyRow<'_>> = vec![];
        let out = handle_key(&mut s, key(KeyCode::Char('o')), &view_for(&rows));
        assert_eq!(out, PropertyOutcome::Unhandled);
    }

    #[test]
    fn value_cycle_clear_letter_dispatches_alongside_cycle_letter() {
        // `clear_letter` claims the key in addition to `cycle_letter`.
        // A regression that derived only `cycle_letter` would drop
        // the clear action silently.
        let mut s = PropertyScreenState::new();
        let rows = vec![PropertyRow::ValueCycle {
            cycle_letter: 'f',
            clear_letter: Some('g'),
            label: Cow::Borrowed("Color"),
            value_display: Cow::Borrowed("(none)"),
        }];
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Char('f')), &view_for(&rows)),
            PropertyOutcome::Action('f')
        );
        assert_eq!(
            handle_key(&mut s, key(KeyCode::Char('g')), &view_for(&rows)),
            PropertyOutcome::Action('g')
        );
    }

    #[test]
    fn uppercase_letter_does_not_dispatch_lowercase_action() {
        // Shift+O is a different key from `o`; only unmodified
        // ASCII-lowercase is in the dispatch set.
        let mut s = PropertyScreenState::new();
        let rows = toggle_rows(&['o']);
        let out = handle_key(&mut s, key(KeyCode::Char('O')), &view_for(&rows));
        assert_eq!(out, PropertyOutcome::Unhandled);
    }

    #[test]
    fn modified_letter_is_unhandled() {
        // Ctrl+O is a control sequence, not a property toggle;
        // widgets must not steal modifier keys.
        let mut s = PropertyScreenState::new();
        let rows = toggle_rows(&['o']);
        for mods in [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
        ] {
            let out = handle_key(&mut s, key_mod(KeyCode::Char('o'), mods), &view_for(&rows));
            assert_eq!(out, PropertyOutcome::Unhandled, "modifier {mods:?}");
        }
    }

    #[test]
    fn navigation_and_function_keys_bubble_to_global() {
        // PropertyScreen owns no navigation; every such key must
        // reach `app::update` for back-nav, quit, and global chords.
        let mut s = PropertyScreenState::new();
        let rows = toggle_rows(&['o']);
        for k in [
            KeyCode::Esc,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Enter,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Backspace,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::F(1),
        ] {
            assert_eq!(
                handle_key(&mut s, key(k), &view_for(&rows)),
                PropertyOutcome::Unhandled,
                "{k:?} must bubble"
            );
        }
    }

    #[test]
    fn non_ascii_letter_is_unhandled() {
        // `is_ascii_lowercase` gates dispatch — keeps the widget out
        // of the locale-sensitive uppercase/lowercase business.
        let mut s = PropertyScreenState::new();
        let rows = toggle_rows(&['o']);
        let out = handle_key(&mut s, key(KeyCode::Char('ö')), &view_for(&rows));
        assert_eq!(out, PropertyOutcome::Unhandled);
    }

    #[test]
    fn digit_is_unhandled_even_if_row_letter_collides_numerically() {
        // `is_ascii_lowercase` rejects digits; a regression that
        // swapped to `is_ascii_alphanumeric` would leak digit keys.
        let mut s = PropertyScreenState::new();
        let rows = toggle_rows(&['o']);
        let out = handle_key(&mut s, key(KeyCode::Char('1')), &view_for(&rows));
        assert_eq!(out, PropertyOutcome::Unhandled);
    }

    fn rows_fixture() -> Vec<PropertyRow<'static>> {
        vec![
            PropertyRow::Toggle {
                letter: 'o',
                label: Cow::Borrowed("Global Bold"),
                enabled: true,
            },
            PropertyRow::ValueCycle {
                cycle_letter: 'f',
                clear_letter: Some('g'),
                label: Cow::Borrowed("Override FG Color"),
                value_display: Cow::Borrowed("(none)"),
            },
            PropertyRow::EditField {
                letter: 'p',
                label: Cow::Borrowed("Default Padding"),
                value_display: Cow::Borrowed("(none)"),
            },
            PropertyRow::Drilldown {
                letter: 't',
                label: Cow::Borrowed("Terminal Width"),
                indicator: Cow::Borrowed("drilldown"),
            },
        ]
    }

    fn dump(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_all_four_row_variants_with_inline_hints() {
        // Combined render: title + status_header + rows + note in
        // one frame, so a chunk-index regression that swaps the
        // rows region with the notes region (or drops a chunk slot)
        // shows up as cross-region content leakage in the asserts.
        let rows = rows_fixture();
        let note = vec![NoteParagraph {
            bullet: Cow::Borrowed("Drilldown"),
            text: Cow::Borrowed("opens a sub-screen"),
        }];
        let header = StatusHeader {
            label: Cow::Borrowed("Font Status"),
            value: Cow::Borrowed("✓ Installed"),
        };
        let view = PropertyScreenView {
            title: "Global Overrides",
            status_header: Some(header),
            rows: &rows,
            note: &note,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
        let frame = dump(&terminal);
        assert!(
            frame.contains("Global Overrides"),
            "title missing:\n{frame}"
        );
        assert!(
            frame.contains("Font Status: ✓ Installed"),
            "status header missing:\n{frame}"
        );
        assert!(
            frame.contains("Global Bold: ✓ Enabled"),
            "toggle on-state missing:\n{frame}"
        );
        assert!(
            frame.contains("Press (o) to toggle"),
            "toggle hint missing:\n{frame}"
        );
        assert!(
            frame.contains("Override FG Color: (none)"),
            "value-cycle label/value missing:\n{frame}"
        );
        assert!(
            frame.contains("(f) cycle, (g) clear"),
            "value-cycle hint missing:\n{frame}"
        );
        assert!(
            frame.contains("Default Padding: (none)"),
            "edit-field label/value missing:\n{frame}"
        );
        assert!(
            frame.contains("Press (p) to edit"),
            "edit-field hint missing:\n{frame}"
        );
        assert!(
            frame.contains("Terminal Width [drilldown]"),
            "drilldown label/indicator missing:\n{frame}"
        );
        assert!(
            frame.contains("Press (t) to open"),
            "drilldown hint missing:\n{frame}"
        );
        assert!(
            frame.contains("• Drilldown — opens a sub-screen"),
            "note paragraph missing:\n{frame}"
        );
    }

    #[test]
    fn toggle_disabled_renders_x_glyph() {
        let rows = vec![PropertyRow::Toggle {
            letter: 'o',
            label: Cow::Borrowed("Global Bold"),
            enabled: false,
        }];
        let view = PropertyScreenView {
            title: "Title",
            status_header: None,
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
        let frame = dump(&terminal);
        assert!(
            frame.contains("Global Bold: ✗ Disabled"),
            "disabled state missing:\n{frame}"
        );
    }

    #[test]
    fn value_cycle_without_clear_omits_clear_hint() {
        let rows = vec![PropertyRow::ValueCycle {
            cycle_letter: 'f',
            clear_letter: None,
            label: Cow::Borrowed("Color"),
            value_display: Cow::Borrowed("red"),
        }];
        let view = PropertyScreenView {
            title: "Title",
            status_header: None,
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
        let frame = dump(&terminal);
        assert!(frame.contains("(f) cycle"), "cycle hint missing:\n{frame}");
        assert!(
            !frame.contains("clear"),
            "clear hint must not render:\n{frame}"
        );
    }

    #[test]
    fn status_header_renders_when_present_and_absent_when_none() {
        let rows: Vec<PropertyRow<'_>> = vec![];
        let header = StatusHeader {
            label: Cow::Borrowed("Font Status"),
            value: Cow::Borrowed("✓ Installed"),
        };
        let view = PropertyScreenView {
            title: "Powerline Setup",
            status_header: Some(header),
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
        assert!(
            dump(&terminal).contains("Font Status: ✓ Installed"),
            "status header missing"
        );

        let view_no_header = PropertyScreenView {
            status_header: None,
            ..view
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 8)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view_no_header, f.area(), f))
            .expect("draw");
        assert!(
            !dump(&terminal).contains("Font Status"),
            "status header must not render when None"
        );
    }

    #[test]
    fn note_paragraphs_render_as_bulleted_block() {
        let note = vec![
            NoteParagraph {
                bullet: Cow::Borrowed("Drilldown"),
                text: Cow::Borrowed("opens a sub-screen for compound settings"),
            },
            NoteParagraph {
                bullet: Cow::Borrowed("Cycle"),
                text: Cow::Borrowed("steps through preset values"),
            },
        ];
        let rows: Vec<PropertyRow<'_>> = vec![];
        let view = PropertyScreenView {
            title: "Title",
            status_header: None,
            rows: &rows,
            note: &note,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
        let frame = dump(&terminal);
        assert!(
            frame.contains("• Drilldown — opens a sub-screen"),
            "first note paragraph missing:\n{frame}"
        );
        assert!(
            frame.contains("• Cycle — steps through preset"),
            "second note paragraph missing:\n{frame}"
        );
    }

    #[test]
    fn empty_rows_still_paints_title_chrome() {
        let rows: Vec<PropertyRow<'_>> = vec![];
        let view = PropertyScreenView {
            title: "Nothing Here",
            status_header: None,
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
        assert!(dump(&terminal).contains("Nothing Here"));
    }

    #[test]
    fn long_label_does_not_panic_in_narrow_terminal() {
        // Wide labels in a narrow terminal are truncated by ratatui's
        // Paragraph; verify no panic.
        let rows = vec![PropertyRow::Toggle {
            letter: 'o',
            label: Cow::Owned("a".repeat(200)),
            enabled: true,
        }];
        let view = PropertyScreenView {
            title: "Title",
            status_header: None,
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(20, 8)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
    }

    #[test]
    fn cjk_label_renders_without_panic_or_partial_grapheme() {
        // CJK characters cost 2 cells each. `dump` walks one cell at
        // a time; the trailing continuation cell of a wide glyph is
        // `""` (ratatui convention) or `" "` (buffer pre-fill), so
        // assert each codepoint individually rather than as a
        // concatenated substring.
        let rows = vec![PropertyRow::Toggle {
            letter: 'o',
            label: Cow::Borrowed("中文标签"),
            enabled: true,
        }];
        let view = PropertyScreenView {
            title: "標題",
            status_header: None,
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
        let frame = dump(&terminal);
        for ch in ["標", "題", "中", "文", "标", "签"] {
            assert!(frame.contains(ch), "wide grapheme {ch:?} missing:\n{frame}");
        }
    }

    #[test]
    fn render_in_height_one_terminal_does_not_panic() {
        // Six-constraint layout chain must not panic when the area
        // can only fit the title; ratatui truncates lower chunks to
        // zero height. Guards against Constraint::Length underflow.
        let rows = rows_fixture();
        let view = PropertyScreenView {
            title: "T",
            status_header: None,
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 1)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
    }

    #[test]
    fn render_in_height_zero_terminal_does_not_panic() {
        // Resize race: a terminal can briefly report a zero-height
        // area before settling. Render must no-op rather than panic.
        let rows = rows_fixture();
        let view = PropertyScreenView {
            title: "T",
            status_header: None,
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 1)).expect("test backend");
        terminal
            .draw(|f| {
                let area = Rect::new(0, 0, f.area().width, 0);
                render(&PropertyScreenState::new(), &view, area, f);
            })
            .expect("draw");
    }

    #[test]
    #[should_panic(expected = "PropertyScreen row letters must be unique")]
    fn duplicate_row_letters_panic_in_debug() {
        // Two rows both claiming `'o'` would dispatch to the first
        // match and silently drop the second. debug_assert! in
        // render catches the misuse at the first frame; release
        // builds skip the check.
        let rows = vec![
            PropertyRow::Toggle {
                letter: 'o',
                label: Cow::Borrowed("First"),
                enabled: true,
            },
            PropertyRow::EditField {
                letter: 'o',
                label: Cow::Borrowed("Second"),
                value_display: Cow::Borrowed("x"),
            },
        ];
        let view = PropertyScreenView {
            title: "T",
            status_header: None,
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
    }

    #[test]
    #[should_panic(expected = "PropertyScreen row letters must be unique")]
    fn uppercase_row_letter_panics_in_debug() {
        // A row claiming `'O'` would render `"Press (O) to toggle"`
        // but `handle_key` rejects non-lowercase, so the user sees
        // a dead key. The same debug_assert that gates uniqueness
        // gates lowercase.
        let rows = vec![PropertyRow::Toggle {
            letter: 'O',
            label: Cow::Borrowed("Bad"),
            enabled: true,
        }];
        let view = PropertyScreenView {
            title: "T",
            status_header: None,
            rows: &rows,
            note: &[],
        };
        let mut terminal = Terminal::new(TestBackend::new(40, 8)).expect("test backend");
        terminal
            .draw(|f| render(&PropertyScreenState::new(), &view, f.area(), f))
            .expect("draw");
    }
}
