//! Main Menu screen — boot-time placeholder.
//!
//! Renders a centered title + quit hint + one-line config summary so
//! the boot path proves out end-to-end. The real menu (per ADR-0016
//! §Architecture: Items Editor, Theme Picker, Line Picker, Global
//! Overrides, Powerline Setup, Terminal Options, Install to Claude
//! Code, Quit) replaces this view once the `ListScreen` widget lands.

use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::app::Model;

#[derive(Debug, Default)]
pub(super) struct MainMenuState;

/// No-op for the placeholder; the cursor + verb dispatch arrive
/// with the `ListScreen` widget.
pub(super) fn update(_state: &mut MainMenuState, _key: KeyEvent) {}

/// Render a centered title, a quit hint, and a one-line config
/// summary so the placeholder confirms both boot and config-load
/// worked.
pub(super) fn view(_state: &MainMenuState, model: &Model, frame: &mut Frame) {
    let area = frame.area();
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " linesmith config ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Vertical layout: top spacer (40%), title (1 row), hint (1 row),
    // summary (1 row), bottom spacer. Centers the title cluster
    // without computing pixel offsets so the layout adapts to
    // whatever terminal size the user has.
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

    let title = Paragraph::new(Line::from(Span::styled(
        "Main Menu",
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(title, rows[1]);

    let hint = Paragraph::new(Line::from(vec![
        Span::raw("press "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" or "),
        Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" to quit"),
    ]))
    .alignment(Alignment::Center);
    frame.render_widget(hint, rows[2]);

    let summary =
        Paragraph::new(Line::from(Span::raw(config_summary(model)))).alignment(Alignment::Center);
    frame.render_widget(summary, rows[3]);
}

/// One-line summary of what the boot loaded: segment count from the
/// resolved config, or "no config file — defaults loaded" when the
/// path didn't exist. Lives here (not on `Model`) because it's
/// purely a placeholder-rendering concern.
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
