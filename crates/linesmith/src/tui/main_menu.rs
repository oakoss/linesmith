//! Main Menu screen: top-level navigation per ADR-0016.
//!
//! Renders seven rows (Edit Lines, Edit Colors, Powerline Setup,
//! Terminal Options, Global Overrides, Install to Claude Code,
//! Exit) through the shared [`super::list_screen`] widget. Enter
//! on any non-Exit row navigates to a placeholder; Enter on Exit
//! quits. Esc on this screen also quits — sub-screens use Esc for
//! back-navigation, so the top-level handler is the only place
//! Esc shortcut-quits.

use std::borrow::Cow;
use std::mem;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;

use super::app::{AppScreen, ScreenOutcome};
use super::list_screen::{
    self, ListOutcome, ListRowData, ListScreenState, ListScreenView, VerbHint,
};
use super::placeholder::PlaceholderState;

#[derive(Debug, Default, Clone)]
pub(super) struct MainMenuState {
    list: ListScreenState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MainMenuItem {
    EditLines,
    EditColors,
    PowerlineSetup,
    TerminalOptions,
    GlobalOverrides,
    InstallToClaudeCode,
    Exit,
}

impl MainMenuItem {
    fn label(self) -> &'static str {
        match self {
            Self::EditLines => "Edit Lines",
            Self::EditColors => "Edit Colors",
            Self::PowerlineSetup => "Powerline Setup",
            Self::TerminalOptions => "Terminal Options",
            Self::GlobalOverrides => "Global Overrides",
            Self::InstallToClaudeCode => "Install to Claude Code",
            Self::Exit => "Exit",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::EditLines => "Add, remove, and reorder segments",
            Self::EditColors => "Customize colors per segment or via theme",
            Self::PowerlineSetup => "Configure powerline-style separators",
            Self::TerminalOptions => "Terminal width detection and color level",
            Self::GlobalOverrides => "Engine knobs (Inherit Colors, Bold, etc.)",
            Self::InstallToClaudeCode => "Wire linesmith into Claude Code settings",
            Self::Exit => "Quit the configuration editor",
        }
    }
}

/// Display order. Index is the cursor position; the slice and the
/// [`MainMenuItem`] enum are paired sources of truth, pinned by a
/// test that asserts the slice equals an explicit literal.
const MENU_ITEMS: &[MainMenuItem] = &[
    MainMenuItem::EditLines,
    MainMenuItem::EditColors,
    MainMenuItem::PowerlineSetup,
    MainMenuItem::TerminalOptions,
    MainMenuItem::GlobalOverrides,
    MainMenuItem::InstallToClaudeCode,
    MainMenuItem::Exit,
];

/// Drive the menu through the shared list widget. Esc and the
/// `Exit` row both quit; every other row navigates to a placeholder.
///
/// `Action(_)` and `MoveSwap` are unreachable given the
/// configuration (`verbs = &[]`, `move_mode_supported = false`);
/// they fall through to `unreachable!` so a misconfiguration that
/// would silently swallow keypresses fails loudly instead.
pub(super) fn update(state: &mut MainMenuState, key: KeyEvent) -> ScreenOutcome {
    if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Esc {
        return ScreenOutcome::Quit;
    }
    match list_screen::handle_key(&mut state.list, key, MENU_ITEMS.len(), &[], false) {
        ListOutcome::Activate => activate(state),
        ListOutcome::Consumed | ListOutcome::Unhandled => ScreenOutcome::Stay,
        outcome @ (ListOutcome::Action(_) | ListOutcome::MoveSwap { .. }) => {
            unreachable!(
                "main menu: list_screen returned {outcome:?} despite verbs=&[] \
                 and move_mode_supported=false; update this dispatch arm if \
                 those args changed",
            )
        }
    }
}

/// Resolve the highlighted menu item to a `ScreenOutcome`. Exit
/// short-circuits to `Quit`; every other item packs the current
/// state into a `Placeholder` so Esc back-nav can restore the
/// cursor row.
///
/// The cursor is always in range here: `handle_key` clamps it
/// before returning `Activate`.
fn activate(state: &mut MainMenuState) -> ScreenOutcome {
    debug_assert!(
        state.list.cursor() < MENU_ITEMS.len(),
        "list_screen::handle_key must clamp the cursor before Activate",
    );
    let item = MENU_ITEMS[state.list.cursor()];
    if matches!(item, MainMenuItem::Exit) {
        return ScreenOutcome::Quit;
    }
    // `mem::take` swaps the in-place state for a default that the
    // caller's `NavigateTo` immediately overwrites. The defaulted
    // MainMenuState is never observable: `app::update` applies the
    // outcome synchronously before the event loop yields back to
    // `view`, so no render path can see it.
    let prev = mem::take(state);
    ScreenOutcome::NavigateTo(AppScreen::Placeholder(PlaceholderState::new(
        item.label(),
        prev,
    )))
}

pub(super) fn view(state: &MainMenuState, frame: &mut Frame, area: Rect) {
    let row_data: Vec<ListRowData<'static>> = MENU_ITEMS
        .iter()
        .map(|item| ListRowData {
            label: Cow::Borrowed(item.label()),
            description: Cow::Borrowed(item.description()),
        })
        .collect();
    let verbs: [VerbHint<'_>; 0] = [];
    let view = ListScreenView {
        title: " linesmith config ",
        rows: &row_data,
        verbs: &verbs,
        move_mode_supported: false,
    };
    list_screen::render(&state.list, &view, area, frame);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_mod(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn esc_quits() {
        let mut state = MainMenuState::default();
        let outcome = update(&mut state, key(KeyCode::Esc));
        assert!(matches!(outcome, ScreenOutcome::Quit));
    }

    #[test]
    fn esc_with_modifier_does_not_quit() {
        // Mirror of placeholder's `esc_with_modifier_does_not_back_navigate`.
        // The strict `key.modifiers == NONE` gate is intentional —
        // chord Esc variants should fall through to the list widget
        // (which treats them as `Unhandled`) rather than quit. A
        // future change to `(KeyCode::Esc, _)` would silently quit
        // on Shift+Esc / Ctrl+Esc; this test makes that change a
        // deliberate edit. The cursor assertion pins the full
        // no-op contract so a regression that mutates cursor state
        // mid-fall-through still fails.
        for mods in [KeyModifiers::SHIFT, KeyModifiers::CONTROL] {
            let mut state = MainMenuState::default();
            let outcome = update(&mut state, key_mod(KeyCode::Esc, mods));
            assert!(
                matches!(outcome, ScreenOutcome::Stay),
                "mods={mods:?} should fall through to Stay, got {outcome:?}",
            );
            assert_eq!(state.list.cursor(), 0, "mods={mods:?} cursor moved");
        }
    }

    #[test]
    fn menu_items_matches_expected_layout() {
        // Pin `MENU_ITEMS` to the literal slice so any drift —
        // adding a variant, removing one, reordering, duplicating
        // — becomes a deliberate edit to this test. Iterating
        // `MENU_ITEMS` and pattern-matching each item only catches
        // variants already in the slice; equality against the
        // literal is the structural pin.
        assert_eq!(
            MENU_ITEMS,
            &[
                MainMenuItem::EditLines,
                MainMenuItem::EditColors,
                MainMenuItem::PowerlineSetup,
                MainMenuItem::TerminalOptions,
                MainMenuItem::GlobalOverrides,
                MainMenuItem::InstallToClaudeCode,
                MainMenuItem::Exit,
            ],
        );
    }

    #[test]
    fn cursor_preserved_across_activate_esc_activate_round_trip() {
        // Walk Activate → (placeholder) → Esc-back → Activate
        // again and pin that the second placeholder carries the
        // same cursor as the first. Catches a regression that
        // resets the MainMenuState on Esc back-nav — most likely
        // shape would be replacing the `mem::take` round-trip
        // with a fresh `MainMenuState::default()` somewhere along
        // the back-nav path.
        use super::super::app::{update as app_update, AppScreen, Event};
        let mut model = super::super::app::Model::new(
            crate::config::Config::default(),
            crate::theme::default_theme().clone(),
            crate::theme::Capability::None,
            None,
        );
        // Down twice → cursor on row 2 (Powerline Setup).
        model = app_update(model, Event::Key(key(KeyCode::Down)));
        model = app_update(model, Event::Key(key(KeyCode::Down)));
        // Activate → Placeholder.
        model = app_update(model, Event::Key(key(KeyCode::Enter)));
        let cursor_first = match &model.screen {
            AppScreen::Placeholder(p) => {
                assert_eq!(p.name, "Powerline Setup");
                p.prev.list.cursor()
            }
            other => panic!("expected Placeholder after first activate, got {other:?}"),
        };
        assert_eq!(cursor_first, 2);
        // Esc → MainMenu. Pin the cursor on the restored MainMenu
        // *before* the second Activate, so a regression that
        // resets cursor here fails this assertion directly rather
        // than getting masked by the subsequent name-mismatch.
        model = app_update(model, Event::Key(key(KeyCode::Esc)));
        match &model.screen {
            AppScreen::MainMenu(state) => {
                assert_eq!(state.list.cursor(), 2, "cursor must survive Esc back-nav",)
            }
            other => panic!("expected MainMenu after Esc, got {other:?}"),
        }
        // Second Activate → Placeholder with the same cursor packed
        // into prev. Confirms the dispatch chain wires the restored
        // cursor through to a fresh placeholder.
        model = app_update(model, Event::Key(key(KeyCode::Enter)));
        match &model.screen {
            AppScreen::Placeholder(p) => {
                assert_eq!(p.name, "Powerline Setup");
                assert_eq!(p.prev.list.cursor(), 2);
            }
            other => panic!("expected Placeholder on second activate, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_default_cursor_navigates_to_edit_lines_placeholder() {
        // Default cursor is row 0 = "Edit Lines". Pin both the
        // outcome shape and the placeholder name so a row reorder
        // becomes a deliberate edit, not a silent navigation
        // change.
        let mut state = MainMenuState::default();
        let outcome = update(&mut state, key(KeyCode::Enter));
        match outcome {
            ScreenOutcome::NavigateTo(AppScreen::Placeholder(p)) => {
                assert_eq!(p.name, "Edit Lines");
            }
            other => panic!("expected Placeholder(Edit Lines), got {other:?}"),
        }
    }

    #[test]
    fn enter_on_exit_row_quits() {
        // Walking the cursor down to the Exit row (last) and
        // pressing Enter must emit `Quit`, not navigate.
        let mut state = MainMenuState::default();
        for _ in 0..(MENU_ITEMS.len() - 1) {
            let outcome = update(&mut state, key(KeyCode::Down));
            assert!(matches!(outcome, ScreenOutcome::Stay));
        }
        assert_eq!(MENU_ITEMS[state.list.cursor()], MainMenuItem::Exit);
        let outcome = update(&mut state, key(KeyCode::Enter));
        assert!(matches!(outcome, ScreenOutcome::Quit));
    }

    #[test]
    fn enter_on_each_non_exit_row_carries_correct_placeholder_name() {
        // Walks the menu and asserts every non-Exit row routes to
        // a placeholder named after the item's label. Catches a
        // copy-paste bug in `activate` where the wrong item label
        // could end up in `PlaceholderState::name`.
        for (idx, item) in MENU_ITEMS.iter().enumerate() {
            if matches!(item, MainMenuItem::Exit) {
                continue;
            }
            let mut state = MainMenuState::default();
            for _ in 0..idx {
                update(&mut state, key(KeyCode::Down));
            }
            // Pin that the cursor walked to row `idx` before
            // pressing Enter. Without this, a regression in Down
            // navigation would surface as "wrong placeholder name"
            // instead of "cursor didn't move", which misleads
            // debugging.
            assert_eq!(state.list.cursor(), idx);
            let outcome = update(&mut state, key(KeyCode::Enter));
            match outcome {
                ScreenOutcome::NavigateTo(AppScreen::Placeholder(p)) => {
                    assert_eq!(p.name, item.label(), "row {idx}");
                }
                other => panic!("row {idx}: expected Placeholder, got {other:?}"),
            }
        }
    }

    #[test]
    fn placeholder_carries_main_menu_state_for_back_nav() {
        // After activating, the previous MainMenuState (with cursor
        // position) lives inside the Placeholder so Esc can restore
        // it. Pin that the cursor index is preserved across the
        // transition.
        let mut state = MainMenuState::default();
        update(&mut state, key(KeyCode::Down));
        update(&mut state, key(KeyCode::Down));
        // Cursor now on row 2 (Powerline Setup). Activating it
        // should pack a MainMenuState with cursor=2 into the
        // Placeholder.
        let outcome = update(&mut state, key(KeyCode::Enter));
        match outcome {
            ScreenOutcome::NavigateTo(AppScreen::Placeholder(p)) => {
                assert_eq!(p.name, "Powerline Setup");
                assert_eq!(p.prev.list.cursor(), 2);
            }
            other => panic!("expected Placeholder, got {other:?}"),
        }
    }

    #[test]
    fn down_advances_cursor() {
        let mut state = MainMenuState::default();
        update(&mut state, key(KeyCode::Down));
        assert_eq!(state.list.cursor(), 1);
    }

    #[test]
    fn up_at_top_wraps_to_last() {
        let mut state = MainMenuState::default();
        update(&mut state, key(KeyCode::Up));
        assert_eq!(state.list.cursor(), MENU_ITEMS.len() - 1);
    }
}
