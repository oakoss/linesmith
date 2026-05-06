//! `Model` + pure `update` + `view` skeleton per ADR-0016.
//!
//! `update` is `(Model, Event) -> Model` so screen behavior is unit-
//! testable without ratatui in the loop. `view` renders the current
//! screen state into a ratatui `Frame`. The `AppScreen` enum is
//! `#[non_exhaustive]` so new screen variants don't churn match arms
//! in code that didn't need to change.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;

use crate::config;

use super::main_menu::{self, MainMenuState};

/// Top-level UI state. Each variant carries its own state struct.
/// Add a screen by adding a variant + its state struct + a `match`
/// arm in [`view`] / [`update`].
#[non_exhaustive]
pub(super) enum AppScreen {
    MainMenu(MainMenuState),
}

/// Top-level model. Carries the current screen, the parsed config,
/// and the quit flag.
pub(super) struct Model {
    pub(super) screen: AppScreen,
    pub(super) config: config::Config,
    pub(super) quit: bool,
}

impl Model {
    /// Construct a fresh `Model` against `config`, opening on the
    /// `MainMenu` screen. The caller (`super::run`) handles config
    /// loading + parse-warning emission so it can write to stderr
    /// before the alt-screen takes over.
    pub(super) fn new(config: config::Config) -> Self {
        Self {
            screen: AppScreen::MainMenu(MainMenuState),
            config,
            quit: false,
        }
    }
}

/// Engine event dispatched into [`update`]. `Resize` carries no
/// payload — it only signals "the layout should redraw"; the
/// `view` path re-queries terminal size on each draw, so update
/// itself doesn't need the new dimensions. Mouse / paste land
/// with screens that need them.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub(super) enum Event {
    Key(KeyEvent),
    Resize,
}

/// Pure state transition. Global key handling (quit) runs first; if
/// the event isn't claimed it falls through to the current screen's
/// own update arm. Adding a screen means adding the `match
/// model.screen` arm and routing per-screen events to it.
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
    if is_global_quit(&key) {
        model.quit = true;
        return model;
    }
    match &mut model.screen {
        AppScreen::MainMenu(state) => main_menu::update(state, key),
    }
    model
}

/// Match the keys that always quit, regardless of which screen is
/// active: Esc, `q`, and Ctrl+C. Future screens that need to
/// override (e.g. text-input modes that consume `q`) will gate this
/// behind their own state check before [`update`] sees the event.
///
/// The Ctrl+C arm uses `contains(CONTROL)` rather than exact-match
/// because some terminals deliver Ctrl+Shift+C as `Char('C')` with
/// `CONTROL | SHIFT` set, and we want both shapes to quit.
fn is_global_quit(key: &KeyEvent) -> bool {
    match (key.code, key.modifiers) {
        (KeyCode::Esc, _) => true,
        (KeyCode::Char('q'), KeyModifiers::NONE) => true,
        (KeyCode::Char('c' | 'C'), m) if m.contains(KeyModifiers::CONTROL) => true,
        _ => false,
    }
}

/// Render the current screen. Each screen owns its own draw routine;
/// this function is a thin dispatcher. Screens read top-level state
/// (config, future preview runs) directly off `model`.
pub(super) fn view(model: &Model, frame: &mut Frame) {
    match &model.screen {
        AppScreen::MainMenu(state) => main_menu::view(state, model, frame),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, mods))
    }

    fn model() -> Model {
        Model::new(config::Config::default())
    }

    #[test]
    fn esc_sets_quit() {
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
        // The screen-dispatch arm in `update` (the `match model.screen`
        // block) is otherwise covered only by the global-quit short-
        // circuit, which returns before reaching it. A refactor that
        // flipped the dispatch arm to always-quit or to a no-op
        // would still pass the quit tests above; this pin catches
        // that. F12 is used (rather than a `Char` like `j`) because
        // it's guaranteed never to become a documented binding once
        // real screens land.
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
}
