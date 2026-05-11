//! Install-to-Claude-Code screen.
//!
//! Surfaces the current `~/.claude/settings.json` `statusLine`
//! installation state and exposes one-keystroke install/uninstall
//! verbs. Backed by [`crate::claude_settings`] so the underlying
//! atomic-write + .bak-backup logic is shared with the CLI
//! `linesmith install` / `linesmith uninstall` subcommands.
//!
//! The TUI surface refuses to clobber an `Other` (third-party)
//! statusLine outright; the CLI surface warns and proceeds. The split
//! is intentional: interactive users see the status indicator and can
//! uninstall first if they want the swap, while scripted CLI setup
//! needs to be able to migrate from another tool in one shot. Both
//! paths preserve the prior bytes in `<settings.json>.bak`.
//!
//! UX shape:
//!
//! ```text
//!  Install to Claude Code — currently: ✗ Not installed
//!
//!   ▶ Install     Add the linesmith statusLine to ~/.claude/settings.json
//!     Uninstall   Remove the linesmith statusLine
//!
//!  [i] install · [u] uninstall · [Enter] activate · [Esc] back
//! ```
//!
//! Verbs:
//! - `i` (or Enter on the Install row): writes/merges the linesmith
//!   statusLine into the settings file. Re-detects status after.
//! - `u` (or Enter on the Uninstall row): removes the statusLine.
//!   Bails if the existing statusLine points to a different tool.
//! - `Esc`: back-navigates to MainMenu.
//!
//! A future PropertyScreen widget will replace this with a richer
//! property-row layout. For now, a ListScreen-based screen is the
//! minimum that ships the auto-install UX without blocking on it.

use std::borrow::Cow;
use std::mem;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::claude_settings::{self, InstallOutcome, InstallationStatus, UninstallOutcome};

use super::app::{AppScreen, ScreenOutcome};
use super::list_screen::{
    self, ListOutcome, ListRowData, ListScreenState, ListScreenView, VerbHint,
};
use super::main_menu::MainMenuState;

const ROW_INSTALL: usize = 0;
const ROW_UNINSTALL: usize = 1;

/// Screen state. `prev` round-trips MainMenu state through Esc
/// back-nav; `status` is the most recently observed installation
/// state, refreshed on entry and after each verb. `home` is captured
/// at construction so the screen can re-resolve the settings path
/// without re-borrowing `CliEnv` through dispatch.
#[derive(Debug)]
pub(super) struct InstallScreenState {
    list: ListScreenState,
    prev: MainMenuState,
    status: InstallationStatus,
    settings_path: std::path::PathBuf,
    /// Resolved command string for `statusLine.command` — built at
    /// construction from `--config` / env so the install verb writes
    /// the same value the CLI `linesmith install` would.
    install_command: String,
}

impl InstallScreenState {
    /// Build the screen state. Takes a pre-resolved settings path
    /// (resolved at TUI boot from `$HOME/.claude/settings.json`) and
    /// install command (the `statusLine.command` value to write).
    /// Reads the current installation status synchronously.
    pub(super) fn new(
        prev: MainMenuState,
        settings_path: std::path::PathBuf,
        install_command: String,
    ) -> Self {
        let status =
            claude_settings::detect_installation_status(&settings_path).unwrap_or_else(|err| {
                linesmith_core::lsm_warn!(
                    "install screen: could not read {}: {err}",
                    settings_path.display(),
                );
                InstallationStatus::NotPresent
            });
        let mut list = ListScreenState::default();
        list.set_cursor(ROW_INSTALL, 2);
        Self {
            list,
            prev,
            status,
            settings_path,
            install_command,
        }
    }

    fn take_prev(&mut self) -> MainMenuState {
        mem::take(&mut self.prev)
    }

    fn re_detect(&mut self) {
        self.status = claude_settings::detect_installation_status(&self.settings_path)
            .unwrap_or_else(|err| {
                linesmith_core::lsm_warn!(
                    "install screen: could not re-read {}: {err}",
                    self.settings_path.display(),
                );
                InstallationStatus::NotPresent
            });
    }
}

const VERBS: &[VerbHint<'static>] = &[
    VerbHint {
        letter: 'i',
        label: "install",
    },
    VerbHint {
        letter: 'u',
        label: "uninstall",
    },
];

/// Verb letters fed to `list_screen::handle_key` for dispatch.
/// Mirror of [`VERBS`] reduced to just the letters since the widget's
/// `handle_key` signature wants `&[char]`, not the labeled-hint form.
const VERB_LETTERS: &[char] = &['i', 'u'];

pub(super) fn update(state: &mut InstallScreenState, key: KeyEvent) -> ScreenOutcome {
    if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Esc {
        return ScreenOutcome::NavigateTo(AppScreen::MainMenu(state.take_prev()));
    }
    match list_screen::handle_key(&mut state.list, key, 2, VERB_LETTERS, false) {
        ListOutcome::Activate => match state.list.cursor() {
            ROW_INSTALL => apply_install(state),
            ROW_UNINSTALL => apply_uninstall(state),
            _ => ScreenOutcome::Stay,
        },
        ListOutcome::Action('i') => apply_install(state),
        ListOutcome::Action('u') => apply_uninstall(state),
        ListOutcome::Action(_)
        | ListOutcome::MoveSwap { .. }
        | ListOutcome::Consumed
        | ListOutcome::Unhandled => ScreenOutcome::Stay,
    }
}

fn apply_install(state: &mut InstallScreenState) -> ScreenOutcome {
    if let InstallationStatus::Other { command } = &state.status {
        // Refuse to silently clobber a third-party statusLine. The
        // user invokes uninstall explicitly to acknowledge the
        // replacement, or edits ~/.claude/settings.json by hand.
        linesmith_core::lsm_warn!(
            "install: existing statusLine points to {command:?}; remove it first via your editor of choice",
        );
        return ScreenOutcome::Stay;
    }
    match claude_settings::install(&state.settings_path, &state.install_command) {
        Ok(InstallOutcome::Created) => {
            linesmith_core::lsm_debug!(
                "installed: wrote {} (created)",
                state.settings_path.display(),
            );
        }
        Ok(InstallOutcome::Updated) => {
            linesmith_core::lsm_debug!(
                "installed: updated {} (prior contents backed up to .bak)",
                state.settings_path.display(),
            );
        }
        Err(err) => {
            linesmith_core::lsm_error!(
                "install: could not write {}: {err}",
                state.settings_path.display(),
            );
        }
    }
    state.re_detect();
    ScreenOutcome::Stay
}

fn apply_uninstall(state: &mut InstallScreenState) -> ScreenOutcome {
    if let InstallationStatus::Other { command } = &state.status {
        linesmith_core::lsm_warn!(
            "uninstall: statusLine points to {command:?}, not linesmith; refusing to remove",
        );
        return ScreenOutcome::Stay;
    }
    match claude_settings::uninstall(&state.settings_path) {
        Ok(UninstallOutcome::Removed) => {
            linesmith_core::lsm_debug!(
                "uninstalled: removed statusLine from {} (prior contents backed up to .bak)",
                state.settings_path.display(),
            );
        }
        Ok(UninstallOutcome::NoFile) => {
            linesmith_core::lsm_debug!(
                "uninstall: no file at {} — nothing to remove",
                state.settings_path.display(),
            );
        }
        Ok(UninstallOutcome::NoStatusLine) => {
            linesmith_core::lsm_debug!(
                "uninstall: {} has no statusLine — nothing to remove",
                state.settings_path.display(),
            );
        }
        Err(err) => {
            linesmith_core::lsm_error!(
                "uninstall: could not write {}: {err}",
                state.settings_path.display(),
            );
        }
    }
    state.re_detect();
    ScreenOutcome::Stay
}

pub(super) fn view(state: &InstallScreenState, frame: &mut Frame, area: Rect) {
    let title: Cow<'_, str> = match &state.status {
        InstallationStatus::NotPresent => {
            Cow::Borrowed(" install to Claude Code — ✗ no settings.json ")
        }
        InstallationStatus::NotInstalled => {
            Cow::Borrowed(" install to Claude Code — ✗ not installed ")
        }
        InstallationStatus::Installed { command } => Cow::Owned(format!(
            " install to Claude Code — ✓ installed as {command:?} ",
        )),
        InstallationStatus::Other { command } => Cow::Owned(format!(
            " install to Claude Code — ⚠ statusLine points to {command:?} ",
        )),
    };

    let rows = [
        ListRowData {
            label: Cow::Borrowed("Install"),
            description: Cow::Borrowed("Add the linesmith statusLine to ~/.claude/settings.json"),
        },
        ListRowData {
            label: Cow::Borrowed("Uninstall"),
            description: Cow::Borrowed("Remove the linesmith statusLine"),
        },
    ];

    let view = ListScreenView {
        title: title.as_ref(),
        rows: &rows,
        verbs: VERBS,
        move_mode_supported: false,
    };
    list_screen::render(&state.list, &view, area, frame);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_settings::InstallationStatus;
    use std::fs;
    use tempfile::TempDir;

    fn state_with_settings(tmp: &TempDir) -> InstallScreenState {
        InstallScreenState {
            list: ListScreenState::default(),
            prev: MainMenuState::default(),
            status: InstallationStatus::NotPresent,
            settings_path: tmp.path().join("settings.json"),
            install_command: "linesmith".to_string(),
        }
    }

    #[test]
    fn install_verb_creates_settings_file_when_absent() {
        // Pin the end-to-end install flow through the screen: `i`
        // on an absent settings.json creates the file with the
        // linesmith statusLine block. The status re-detects after.
        let tmp = TempDir::new().expect("tempdir");
        let mut s = state_with_settings(&tmp);
        let key = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        let outcome = update(&mut s, key);
        assert!(matches!(outcome, ScreenOutcome::Stay));
        assert!(
            s.settings_path.exists(),
            "install must create settings.json"
        );
        match &s.status {
            InstallationStatus::Installed { command } => {
                assert_eq!(command, "linesmith");
            }
            other => panic!("expected Installed after `i`, got {other:?}"),
        }
    }

    #[test]
    fn uninstall_verb_removes_status_line_and_re_detects() {
        let tmp = TempDir::new().expect("tempdir");
        let mut s = state_with_settings(&tmp);
        let key_i = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        update(&mut s, key_i);
        let key_u = KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE);
        let outcome = update(&mut s, key_u);
        assert!(matches!(outcome, ScreenOutcome::Stay));
        assert_eq!(s.status, InstallationStatus::NotInstalled);
    }

    #[test]
    fn install_verb_updates_existing_linesmith_settings() {
        // Pre-existing linesmith install: pressing `i` re-writes the
        // statusLine block (e.g. after the user changed the command
        // by hand) and backs up the prior bytes. The screen sees the
        // `InstallOutcome::Updated` arm, distinct from `Created`.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("settings.json");
        fs::write(
            &path,
            r#"{"statusLine":{"type":"command","command":"linesmith --config /old"}}"#,
        )
        .expect("seed");
        let mut s = InstallScreenState {
            list: ListScreenState::default(),
            prev: MainMenuState::default(),
            status: claude_settings::detect_installation_status(&path).expect("detect"),
            settings_path: path.clone(),
            install_command: "linesmith".to_string(),
        };
        let outcome = update(
            &mut s,
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
        );
        assert!(matches!(outcome, ScreenOutcome::Stay));
        assert_eq!(
            s.status,
            InstallationStatus::Installed {
                command: "linesmith".to_string()
            }
        );
        assert!(
            tmp.path().join("settings.json.bak").exists(),
            "Updated outcome must back up the prior bytes",
        );
    }

    #[test]
    fn uninstall_verb_is_idempotent_when_no_status_line() {
        // File exists with non-statusLine keys: `u` is a clean no-op,
        // no backup written, unrelated keys preserved.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("settings.json");
        fs::write(&path, r#"{"model":"sonnet"}"#).expect("seed");
        let mut s = InstallScreenState {
            list: ListScreenState::default(),
            prev: MainMenuState::default(),
            status: claude_settings::detect_installation_status(&path).expect("detect"),
            settings_path: path.clone(),
            install_command: "linesmith".to_string(),
        };
        let outcome = update(
            &mut s,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
        );
        assert!(matches!(outcome, ScreenOutcome::Stay));
        assert_eq!(s.status, InstallationStatus::NotInstalled);
        assert!(!tmp.path().join("settings.json.bak").exists());
        let preserved = fs::read_to_string(&path).expect("read");
        assert!(preserved.contains("sonnet"));
    }

    #[test]
    fn uninstall_verb_is_idempotent_when_no_file() {
        let tmp = TempDir::new().expect("tempdir");
        let mut s = state_with_settings(&tmp);
        let outcome = update(
            &mut s,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
        );
        assert!(matches!(outcome, ScreenOutcome::Stay));
        assert_eq!(s.status, InstallationStatus::NotPresent);
        assert!(!s.settings_path.exists());
    }

    #[test]
    fn uninstall_refuses_to_remove_other_tool() {
        // Mirror of `install_refuses_to_clobber_other_tool`: the
        // uninstall verb on a third-party statusLine must also be
        // inert so an accidental `u` doesn't strip ccstatusline.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("settings.json");
        fs::write(
            &path,
            r#"{"statusLine":{"type":"command","command":"ccstatusline"}}"#,
        )
        .expect("seed");
        let mut s = InstallScreenState {
            list: ListScreenState::default(),
            prev: MainMenuState::default(),
            status: claude_settings::detect_installation_status(&path).expect("detect"),
            settings_path: path.clone(),
            install_command: "linesmith".to_string(),
        };
        let outcome = update(
            &mut s,
            KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE),
        );
        assert!(matches!(outcome, ScreenOutcome::Stay));
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("ccstatusline"));
    }

    #[test]
    fn install_refuses_to_clobber_other_tool() {
        // Pin: if statusLine points to ccstatusline (or any non-
        // linesmith tool), pressing `i` is inert and emits a warn
        // — the user must remove the existing entry first. Without
        // this, the auto-install would silently replace the user's
        // working setup.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("settings.json");
        fs::write(
            &path,
            r#"{ "statusLine": { "type": "command", "command": "ccstatusline" } }"#,
        )
        .expect("seed");
        let mut s = InstallScreenState {
            list: ListScreenState::default(),
            prev: MainMenuState::default(),
            status: claude_settings::detect_installation_status(&path).expect("detect"),
            settings_path: path.clone(),
            install_command: "linesmith".to_string(),
        };
        let outcome = update(
            &mut s,
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
        );
        assert!(matches!(outcome, ScreenOutcome::Stay));
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("ccstatusline"));
        assert!(!raw.contains("linesmith"));
    }

    #[test]
    fn esc_back_navigates_to_main_menu_without_acting() {
        let tmp = TempDir::new().expect("tempdir");
        let mut s = state_with_settings(&tmp);
        let outcome = update(&mut s, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::MainMenu(_))
        ));
        assert!(
            !s.settings_path.exists(),
            "Esc must not have created the settings file",
        );
    }
}
