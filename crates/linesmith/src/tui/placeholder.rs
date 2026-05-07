//! Placeholder screen for menu items whose backing screens
//! haven't been built. Renders a centered "Coming soon. Press
//! Esc to return." panel and routes Esc back to the Main Menu
//! the user came from. Each entry that gains a real screen drops
//! its placeholder transition for the new `AppScreen` variant.

use std::mem;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::app::{AppScreen, ScreenOutcome};
use super::main_menu::MainMenuState;

/// Carries the menu-item label that triggered the placeholder plus
/// the previous Main Menu state, so Esc returns the user to the
/// same cursor position they activated from.
#[derive(Debug)]
pub(super) struct PlaceholderState {
    pub(super) name: &'static str,
    pub(super) prev: MainMenuState,
}

impl PlaceholderState {
    pub(super) fn new(name: &'static str, prev: MainMenuState) -> Self {
        Self { name, prev }
    }
}

/// Esc returns to the Main Menu (cursor preserved); every other
/// key is `Stay`. The unconditional-quit keys (`q`, Ctrl+C) are
/// already filtered upstream in `app::update`, so they never reach
/// this function.
///
/// Enter is intentionally inert: a placeholder has no rows to
/// activate. When a real screen replaces the placeholder, that
/// screen's `update` handles Enter as part of its own dispatch.
/// Treating Enter as back-nav-for-symmetry would silently break
/// the contract that Esc is the only back gesture.
pub(super) fn update(state: &mut PlaceholderState, key: KeyEvent) -> ScreenOutcome {
    if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Esc {
        let prev = mem::take(&mut state.prev);
        return ScreenOutcome::NavigateTo(AppScreen::MainMenu(prev));
    }
    ScreenOutcome::Stay
}

/// Bordered chrome with the menu-item label as the block title;
/// the body sits inside `inner` with a 40% top spacer that pushes
/// the "Coming soon" line and the keybinding hint toward visual
/// center, separated by a blank row. The `Min(0)` floor absorbs
/// the remaining height so the layout adapts to any terminal size
/// that fits the borders.
pub(super) fn view(state: &PlaceholderState, frame: &mut Frame, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        format!(" {} ", state.name),
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(40),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(0),
        ])
        .split(inner);

    let body = Paragraph::new(Line::from("Coming soon.")).alignment(Alignment::Center);
    frame.render_widget(body, rows[1]);

    let hint = Paragraph::new(Line::from(vec![
        Span::raw("Press "),
        Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" to return to the main menu, or "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" to quit."),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(hint, rows[3]);
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
    fn esc_returns_navigate_to_main_menu() {
        let mut state = PlaceholderState::new("Edit Lines", MainMenuState::default());
        let outcome = update(&mut state, key(KeyCode::Esc));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::MainMenu(_))
        ));
    }

    #[test]
    fn other_keys_stay_on_placeholder() {
        // Only Esc back-navigates; Enter / arrows / verbs are
        // no-ops on the placeholder. The unconditional-quit keys
        // (`q`, Ctrl+C) never reach this function — `app::update`
        // filters them upstream.
        let mut state = PlaceholderState::new("Edit Lines", MainMenuState::default());
        for code in [
            KeyCode::Enter,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Char('a'),
        ] {
            let outcome = update(&mut state, key(code));
            assert!(
                matches!(outcome, ScreenOutcome::Stay),
                "key {code:?} should not navigate",
            );
        }
    }

    #[test]
    fn esc_with_modifier_does_not_back_navigate() {
        // Shift+Esc / Ctrl+Esc shouldn't trigger back-nav — the user
        // pressing a chord meant something else. Pin so a future
        // change to `key.modifiers` matching doesn't silently catch
        // chord variants.
        let mut state = PlaceholderState::new("Edit Lines", MainMenuState::default());
        let outcome = update(&mut state, key_mod(KeyCode::Esc, KeyModifiers::SHIFT));
        assert!(matches!(outcome, ScreenOutcome::Stay));
    }
}
