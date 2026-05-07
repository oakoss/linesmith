//! `Model` + pure `update` + `view` skeleton per ADR-0016.
//!
//! `update` is `(Model, Event) -> Model` so screen behavior is unit-
//! testable without ratatui in the loop. `view` renders the current
//! screen state into a ratatui `Frame`. The `AppScreen` enum is
//! `#[non_exhaustive]` so new screen variants don't churn match arms
//! in code that didn't need to change.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::config;
use crate::theme::{Capability, Theme};

use super::main_menu::{self, MainMenuState};
use super::placeholder::{self, PlaceholderState};
use super::preview;

/// Top-level UI state. Each variant carries its own state struct.
/// Add a screen by adding a variant + its state struct + a `match`
/// arm in [`view`] / [`update`].
#[non_exhaustive]
#[derive(Debug)]
pub(super) enum AppScreen {
    MainMenu(MainMenuState),
    Placeholder(PlaceholderState),
}

/// Top-level model. Carries the current screen, the parsed config,
/// the resolved theme, the detected color capability, and the
/// quit flag. Theme + capability are snapshot at boot so the
/// preview honors `config.theme` and `NO_COLOR` the same way the
/// production driver does.
pub(super) struct Model {
    pub(super) screen: AppScreen,
    // Held on `Model` so screens that need it can read it
    // directly; current screens (`MainMenu`, `Placeholder`) don't,
    // hence the dead-code allow.
    #[allow(dead_code)]
    pub(super) config: config::Config,
    pub(super) theme: Theme,
    pub(super) capability: Capability,
    pub(super) quit: bool,
}

impl Model {
    /// Construct a fresh `Model` against `config`, theme, and
    /// capability, opening on the `MainMenu` screen. The caller
    /// (`super::run`) handles config loading + parse-warning
    /// emission so it can write to stderr before the alt-screen
    /// takes over, plus theme registry construction and color
    /// capability detection.
    pub(super) fn new(config: config::Config, theme: Theme, capability: Capability) -> Self {
        Self {
            screen: AppScreen::MainMenu(MainMenuState::default()),
            config,
            theme,
            capability,
            quit: false,
        }
    }
}

/// Engine event dispatched into [`update`]. `Resize` carries no
/// payload — it only signals "the layout should redraw"; the
/// `view` path re-queries terminal size on each draw, so update
/// itself doesn't need the new dimensions.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub(super) enum Event {
    Key(KeyEvent),
    Resize,
}

/// Per-screen dispatch signal. Screens return one of these from
/// their `update` so `app::update` can apply the transition without
/// the screen code touching `Model` directly. Adding a new screen
/// means adding an `AppScreen` variant + a screen module that
/// returns the same outcome shape.
#[non_exhaustive]
#[derive(Debug)]
pub(super) enum ScreenOutcome {
    /// Screen handled the event internally (or didn't claim it);
    /// `Model` stays as-is.
    Stay,
    /// Replace `model.screen` with the supplied `AppScreen`. Used
    /// for menu activation and back-navigation.
    NavigateTo(AppScreen),
    /// Signal the event loop to leave the TUI.
    Quit,
}

/// Pure state transition. Unconditional-quit keys (`q`, Ctrl+C)
/// fire regardless of which screen is active; everything else
/// routes to the screen's own `update`, whose [`ScreenOutcome`] the
/// caller applies to `Model`.
///
/// `Event::Resize` is a no-op at the model layer; routing it
/// through [`update`] still triggers the post-update draw in the
/// event loop, which is the redraw the user wants.
#[must_use]
pub(super) fn update(mut model: Model, event: Event) -> Model {
    let key = match event {
        Event::Key(key) => key,
        Event::Resize => return model,
    };
    if is_unconditional_quit(&key) {
        model.quit = true;
        return model;
    }
    let outcome = match &mut model.screen {
        AppScreen::MainMenu(state) => main_menu::update(state, key),
        AppScreen::Placeholder(state) => placeholder::update(state, key),
    };
    match outcome {
        ScreenOutcome::Stay => {}
        ScreenOutcome::NavigateTo(screen) => model.screen = screen,
        ScreenOutcome::Quit => model.quit = true,
    }
    model
}

/// Match the keys that quit regardless of which screen is active:
/// `q` and Ctrl+C. Esc is intentionally screen-specific — sub-
/// screens use it for back-navigation; only the top-level menu
/// treats Esc as quit.
///
/// The Ctrl+C arm uses `contains(CONTROL)` rather than exact-match
/// because some terminals deliver Ctrl+Shift+C as `Char('C')` with
/// `CONTROL | SHIFT` set, and we want both shapes to quit.
fn is_unconditional_quit(key: &KeyEvent) -> bool {
    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), KeyModifiers::NONE) => true,
        (KeyCode::Char('c' | 'C'), m) if m.contains(KeyModifiers::CONTROL) => true,
        _ => false,
    }
}

/// Render the live-preview header above the active screen. The
/// preview lives at the top of every frame per ADR-0016. Height =
/// 2 border rows + max(1, line count) + one row per emitted
/// warning, with the total clamped to 16 rows so a many-line
/// config or noisy diagnostic stream can't crowd out the screen
/// below.
pub(super) fn view(model: &Model, frame: &mut Frame) {
    let area = frame.area();

    // The bordered preview block costs 2 columns horizontally;
    // the layout engine needs the *content* width so segments
    // shrink/drop against the surface that actually displays
    // them, not the outer frame width.
    let inner_width = area.width.saturating_sub(2);
    let (preview_lines, warnings) =
        preview::render_lines(&model.config, &model.theme, model.capability, inner_width);

    // Height: 2 border rows + at least 1 content row + 1 row per
    // warning (capped). Capped at 16 total so a pathological
    // multi-line config can't crowd out the screen below.
    let line_rows = u16::try_from(preview_lines.len().max(1)).unwrap_or(u16::MAX);
    let warn_rows = u16::try_from(warnings.len()).unwrap_or(u16::MAX);
    let preview_height = line_rows
        .saturating_add(warn_rows)
        .saturating_add(2)
        .min(16);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(preview_height), Constraint::Min(1)])
        .split(area);

    render_preview(&preview_lines, &warnings, chunks[0], frame);

    match &model.screen {
        AppScreen::MainMenu(state) => main_menu::view(state, frame, chunks[1]),
        AppScreen::Placeholder(state) => placeholder::view(state, frame, chunks[1]),
    }
}

fn render_preview(
    lines: &[Line<'static>],
    warnings: &[String],
    area: ratatui::layout::Rect,
    frame: &mut Frame,
) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " preview ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 {
        return;
    }

    // Vertical layout: lines fill the top, warnings (if any)
    // occupy the bottom rows in a dim italic style so they read
    // as advisory rather than primary content.
    let line_rows = u16::try_from(lines.len().max(1)).unwrap_or(u16::MAX);
    let warn_rows = u16::try_from(warnings.len()).unwrap_or(u16::MAX);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(line_rows), Constraint::Length(warn_rows)])
        .split(inner);

    if lines.is_empty() {
        let body = Paragraph::new(Line::from(
            "(no preview — `[line].segments` resolved to empty; check warnings below)",
        ));
        frame.render_widget(body, chunks[0]);
    } else {
        let body = Paragraph::new(lines.to_vec());
        frame.render_widget(body, chunks[0]);
    }

    if !warnings.is_empty() {
        let style = Style::default()
            .add_modifier(Modifier::DIM)
            .add_modifier(Modifier::ITALIC);
        let warn_lines: Vec<Line<'static>> = warnings
            .iter()
            .map(|w| Line::styled(format!("⚠ {w}"), style))
            .collect();
        let body = Paragraph::new(warn_lines);
        frame.render_widget(body, chunks[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    fn model() -> Model {
        Model::new(
            config::Config::default(),
            crate::theme::default_theme().clone(),
            Capability::None,
        )
    }

    #[test]
    fn esc_on_main_menu_quits() {
        // Esc is no longer in `is_unconditional_quit`; the quit
        // path now flows through the screen's `update`. Pin the
        // observable outcome (model.quit set) so the routing
        // change doesn't silently regress the Esc-quits-from-main
        // user contract.
        let m = update(model(), key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(m.quit);
    }

    #[test]
    fn lowercase_q_sets_quit() {
        let m = update(model(), key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(m.quit);
    }

    #[test]
    fn ctrl_c_sets_quit() {
        let m = update(model(), key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(m.quit);
    }

    #[test]
    fn unrelated_keys_do_not_quit() {
        // Pin: bare `c` (not Ctrl+C) and uppercase `Q` (modifier
        // mismatch — quit is gated to lowercase q with no modifiers)
        // both fall through to the screen's update without quitting.
        let m = update(model(), key(KeyCode::Char('c'), KeyModifiers::NONE));
        assert!(!m.quit);
        let m = update(model(), key(KeyCode::Char('Q'), KeyModifiers::SHIFT));
        assert!(!m.quit);
    }

    #[test]
    fn ctrl_c_uppercase_also_quits() {
        // Some terminals deliver Ctrl+C as KeyCode::Char('C') with the
        // SHIFT bit set alongside CONTROL. Pin both lowercase + uppercase
        // shapes so the quit predicate doesn't miss the variant a real
        // user actually generates.
        let m = update(
            model(),
            key(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ),
        );
        assert!(m.quit);
    }

    #[test]
    fn non_quit_key_routes_to_screen_without_quitting() {
        // The screen-dispatch arm in `update` is otherwise covered
        // only by quit short-circuits, which return before reaching
        // it. F12 is used (rather than a `Char` like `j`) because
        // it's guaranteed never to become a documented binding.
        let m = update(model(), key(KeyCode::F(12), KeyModifiers::NONE));
        assert!(!m.quit, "non-quit key must not set quit");
        assert!(
            matches!(m.screen, AppScreen::MainMenu(_)),
            "screen must remain MainMenu",
        );
    }

    #[test]
    fn resize_event_does_not_change_state() {
        // `Event::Resize` is a redraw signal that doesn't mutate the
        // model. Pin that update returns the model unchanged so the
        // event loop's post-update draw fires for free without any
        // screen-level routing.
        let m = update(model(), Event::Resize);
        assert!(!m.quit);
        assert!(matches!(m.screen, AppScreen::MainMenu(_)));
    }

    #[test]
    fn enter_on_main_menu_navigates_to_placeholder() {
        // Pin the dispatch chain: top-level update → screen
        // update → NavigateTo application. A regression in any
        // link breaks Enter-to-open across every menu row.
        let m = update(model(), key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!m.quit);
        assert!(
            matches!(m.screen, AppScreen::Placeholder(_)),
            "screen should transition to Placeholder",
        );
    }

    #[test]
    fn q_on_placeholder_quits() {
        // Pin that the unconditional-quit predicate runs *before*
        // screen dispatch, so `q` quits even from a sub-screen.
        // The placeholder's `update` only handles Esc; without
        // upstream filtering, `q` would no-op on the placeholder.
        let m = update(model(), key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::Placeholder(_)));
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(m.quit);
    }

    #[test]
    fn esc_on_placeholder_returns_to_main_menu() {
        // Activate from MainMenu to land on Placeholder, then Esc
        // navigates back. Pins both the screen restoration and the
        // top-level Esc handling (Esc must reach the screen's
        // update — `is_unconditional_quit` rejects it).
        let m = update(model(), key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::Placeholder(_)));
        let m = update(m, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!m.quit);
        assert!(matches!(m.screen, AppScreen::MainMenu(_)));
    }
}
