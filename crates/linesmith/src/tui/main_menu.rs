//! Main Menu screen — boot-time placeholder.
//!
//! Renders the reusable [`super::list_screen`] widget against a
//! single-row stub list so the boot path proves the widget out
//! end-to-end. The target menu is in ADR-0016 §Architecture.

use std::borrow::Cow;

use ratatui::crossterm::event::KeyEvent;
use ratatui::Frame;

use super::app::Model;
use super::list_screen::{
    self, ListOutcome, ListRowData, ListScreenState, ListScreenView, VerbHint,
};

#[derive(Debug, Default)]
pub(super) struct MainMenuState {
    list: ListScreenState,
}

/// Drive the placeholder list through the shared widget.
///
/// The placeholder configures `verbs = &[]` and
/// `move_mode_supported = false`, so the legitimate outcomes are
/// `Consumed`, `Unhandled`, and `Activate` (Enter on the single
/// row). `Activate` is a no-op until the navigation deliverable
/// wires real menu targets. `Action(_)` and `MoveSwap` are
/// unreachable given that config; if either fires, the placeholder
/// has been mis-configured (e.g. a stray verb registered, or the
/// move-mode flag flipped) and the `debug_assert!` turns the
/// silent no-op into a CI failure.
pub(super) fn update(state: &mut MainMenuState, key: KeyEvent) {
    let rows = placeholder_rows();
    match list_screen::handle_key(&mut state.list, key, rows.len(), &[], false) {
        ListOutcome::Consumed | ListOutcome::Unhandled | ListOutcome::Activate => {}
        outcome @ (ListOutcome::Action(_) | ListOutcome::MoveSwap { .. }) => {
            debug_assert!(
                false,
                "main menu placeholder: unexpected outcome {outcome:?}",
            );
        }
    }
}

pub(super) fn view(state: &MainMenuState, model: &Model, frame: &mut Frame) {
    let rows = placeholder_rows();
    let summary = config_summary(model);
    let label = rows.first().copied().unwrap_or("");
    let row_data = [ListRowData {
        label: Cow::Borrowed(label),
        description: Cow::Owned(summary),
    }];
    let verbs: [VerbHint<'_>; 0] = [];
    let view = ListScreenView {
        title: " linesmith config (placeholder) ",
        rows: &row_data,
        verbs: &verbs,
        move_mode_supported: false,
    };
    list_screen::render(&state.list, &view, frame.area(), frame);
}

fn placeholder_rows() -> &'static [&'static str] {
    &["Press q or Esc to quit"]
}

/// One-line summary of what the boot loaded: either a segment
/// count or a "no segments configured" hint when the resolved
/// config has no `[line]` table.
fn config_summary(model: &Model) -> String {
    let line = model.config.line.as_ref();
    let segments = line.map(|l| l.segments.len()).unwrap_or(0);
    if segments == 0 {
        "no segments configured (defaults will render)".to_string()
    } else {
        format!("{segments} segments configured")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    #[test]
    fn config_summary_zero_segments_branch() {
        // Default Config has no `[line]` table; the summary takes the
        // unwrap_or(0) path. Pin the user-facing text so a copy tweak
        // becomes a deliberate edit.
        let model = Model::new(config::Config::default());
        assert_eq!(
            config_summary(&model),
            "no segments configured (defaults will render)",
        );
    }

    #[test]
    fn enter_on_placeholder_does_not_panic_in_debug() {
        // Regression pin: the placeholder has one row and
        // `move_mode_supported = false`, so Enter returns
        // `ListOutcome::Activate`. An earlier draft sent that
        // outcome to `debug_assert!(false, ...)`, which panicked
        // on every Enter keypress in debug builds. Activate must
        // be in the no-op arm.
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut state = MainMenuState::default();
        update(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
    }

    #[test]
    fn config_summary_populated_segments_branch() {
        // A two-segment line takes the `format!` branch with the
        // segment count rendered as a plain number.
        let cfg: config::Config = "[line]\nsegments = [\"model\", \"workspace\"]\n"
            .parse()
            .expect("parse");
        let model = Model::new(cfg);
        assert_eq!(config_summary(&model), "2 segments configured");
    }
}
