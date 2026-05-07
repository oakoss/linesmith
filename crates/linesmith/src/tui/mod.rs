//! `linesmith config` TUI — boot, terminal lifecycle, and event loop.
//!
//! Scope of this module per ADR-0015 (substrate) + ADR-0016 (screen
//! state machine): wires the ratatui terminal in raw-mode +
//! alternate-screen, installs a panic hook that restores terminal
//! state before the default panic handler runs (so a crash mid-screen
//! doesn't leave the user's shell in a broken state), polls
//! crossterm events, and dispatches them through the pure
//! `(Model, Event) -> Model` update function in [`app`]. Screens
//! (`main_menu`, `placeholder`, …) live in their own modules and
//! are dispatched through [`app::update`] / [`app::view`].
//!
//! This module is feature-gated behind `config-ui` (default-on per
//! ADR-0015); the daily render path never imports it.

mod app;
mod list_screen;
mod main_menu;
mod placeholder;

use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

use ratatui::crossterm::event::{self as cevent, Event as CtEvent};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;

use crate::config;

use app::{update, Event, Model};

/// Boot the TUI, run the event loop, and tear down. Returns a `u8`
/// exit code so [`crate::driver::cli_main`] doesn't have to translate.
/// Stderr-only diagnostics; stdout is owned by the alternate screen.
///
/// `redundant_pub_crate` and `unreachable_pub` collide on this entry
/// point: the former wants plain `pub` (the parent module is already
/// `pub(crate)`), the latter wants `pub(crate)` (no external
/// re-export). `pub(super)` is the most-restrictive shape that still
/// reaches `driver::config_action`; allow the redundancy lint here.
#[allow(clippy::redundant_pub_crate)]
pub(super) fn run(config_path: Option<&Path>, stderr: &mut dyn Write) -> u8 {
    let (config, parse_warning) = match load_config(config_path) {
        Ok(out) => out,
        Err(err) => {
            let _ = writeln!(stderr, "linesmith config: load: {err}");
            return 1;
        }
    };
    // Parse failures are non-fatal (the user can still edit through
    // the TUI), but the warning has to land before the alt screen
    // swallows stderr — otherwise the editor opens against a default
    // config and a future write-back silently shadows the user's
    // real broken TOML.
    if let Some(warning) = parse_warning {
        let _ = writeln!(stderr, "linesmith config: {warning}");
    }
    let model = Model::new(config);

    // Install the panic hook *before* enter_terminal so a panic
    // during `Terminal::new` or the first draw still routes through
    // `leave_terminal`. Pre-enter, the hook is a no-op (raw mode is
    // off, we're not in alt-screen yet); post-enter, it's the
    // safety net.
    install_panic_hook();
    if let Err(err) = enter_terminal() {
        let _ = writeln!(stderr, "linesmith config: terminal setup: {err}");
        return 1;
    }

    let outcome = run_loop(model);

    if let Err(err) = leave_terminal() {
        // Prefer surfacing the original outcome's exit code; restoring
        // the terminal is best-effort cleanup.
        let _ = writeln!(stderr, "linesmith config: terminal restore: {err}");
    }

    match outcome {
        Ok(()) => 0,
        Err(err) => {
            let _ = writeln!(stderr, "linesmith config: event loop: {err}");
            1
        }
    }
}

/// Maximum time `poll_event` blocks before returning `None`. Bounded
/// rather than infinite so a future timer-driven UI element (live
/// preview tick, countdown segment) has a wake budget without
/// rewriting the loop. Drawing only happens after a real event lands,
/// so an idle session at this interval costs one syscall every tick,
/// not a redraw.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Event-poll → update → draw loop. The initial draw paints the
/// screen once; subsequent draws fire only after `update` consumed
/// an event, so an idle session doesn't repaint at 10 Hz.
fn run_loop(mut model: Model) -> io::Result<()> {
    let backend = ratatui::backend::CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.draw(|frame| app::view(&model, frame))?;
    loop {
        let Some(event) = poll_event()? else {
            continue;
        };
        model = update(model, event);
        if model.quit {
            return Ok(());
        }
        terminal.draw(|frame| app::view(&model, frame))?;
    }
}

/// Block up to [`POLL_INTERVAL`] for a crossterm event. Returns
/// `None` on timeout so the loop can re-poll without a redraw.
/// Resize routes through [`Event::Resize`] specifically because the
/// loop only redraws on real events — discarding resize would leave
/// a stale frame until the next keypress.
fn poll_event() -> io::Result<Option<Event>> {
    if !cevent::poll(POLL_INTERVAL)? {
        return Ok(None);
    }
    match cevent::read()? {
        CtEvent::Key(key) => Ok(Some(Event::Key(key))),
        CtEvent::Resize(_, _) => Ok(Some(Event::Resize)),
        // Mouse / FocusGained/Lost / Paste — ignored for v0.1.
        _ => Ok(None),
    }
}

/// Enable raw mode + alternate screen + cursor hide. Symmetric with
/// [`leave_terminal`]. The cleanup-on-failure path runs every
/// reverse step regardless of which one tripped: `execute!`
/// processes its commands sequentially, so an `EnterAlternateScreen`
/// success followed by a `cursor::Hide` failure would leave the
/// alt-screen active under raw mode. Roll both back unconditionally.
fn enter_terminal() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    if let Err(err) = execute!(
        stdout,
        EnterAlternateScreen,
        ratatui::crossterm::cursor::Hide,
    ) {
        let _ = execute!(
            stdout,
            LeaveAlternateScreen,
            ratatui::crossterm::cursor::Show,
        );
        let _ = disable_raw_mode();
        return Err(err);
    }
    Ok(())
}

/// Restore raw mode + leave alternate screen + show cursor. Idempotent
/// best-effort: the panic hook also calls this so a crash leaves the
/// user with a usable shell.
///
/// `disable_raw_mode` runs even when the alt-screen / cursor restore
/// write fails, otherwise an I/O error during shutdown would leave
/// the shell stuck in raw mode — the exact failure mode this
/// function is meant to prevent. The first error encountered
/// propagates; later errors are dropped.
fn leave_terminal() -> io::Result<()> {
    let mut stdout = io::stdout();
    let screen = execute!(
        stdout,
        LeaveAlternateScreen,
        ratatui::crossterm::cursor::Show,
    );
    let raw = disable_raw_mode();
    screen.and(raw)
}

/// Wrap the existing panic hook with one that restores terminal state
/// before delegating. Without this, a panic mid-render leaves the
/// terminal in raw mode + alt screen and the user's prompt is
/// effectively unusable.
fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = leave_terminal();
        prev(info);
    }));
}

/// Load the config for the boot path. Returns the parsed config plus
/// an optional human-readable warning the caller surfaces *before*
/// taking over the screen.
///
/// Outcomes:
///
/// - `path == None` or file absent → `(Config::default(), None)`.
///   The user is operating without a config file; defaults are the
///   correct starting point with no warning.
/// - File present and parses cleanly → `(parsed, None)`.
/// - File present and parses with unknown-key warnings (typo'd
///   section header, forward-compat key) → `(parsed, Some(joined))`.
///   Surfaces the same warnings the render path emits on stderr so
///   the user knows about stale or typo'd keys before opening the
///   editor. Goes through `Config::from_str_validated` rather than
///   the plain `FromStr` impl because the latter explicitly drops
///   unknown-key diagnostics.
/// - File present but malformed → `(Config::default(), Some(msg))`.
///   The editor opens against defaults so the user can fix the
///   broken file interactively. Without the warning the user
///   wouldn't notice their real config is being shadowed.
///
/// I/O errors other than `NotFound` propagate so the boot path
/// surfaces them as a load failure exit, not as a silent fallback.
fn load_config(path: Option<&Path>) -> io::Result<(config::Config, Option<String>)> {
    let Some(path) = path else {
        return Ok((config::Config::default(), None));
    };
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut warnings: Vec<String> = Vec::new();
            match config::Config::from_str_validated(&text, |w| warnings.push(w.to_string())) {
                Ok(cfg) => {
                    let warning = (!warnings.is_empty()).then(|| warnings.join("\n"));
                    Ok((cfg, warning))
                }
                Err(err) => Ok((
                    config::Config::default(),
                    Some(format!(
                        "parse error in {}: {err} — opening with defaults",
                        path.display()
                    )),
                )),
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok((config::Config::default(), None)),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn load_config_none_path_returns_default_no_warning() {
        let (cfg, warning) = load_config(None).expect("ok");
        assert!(warning.is_none());
        assert_eq!(cfg, config::Config::default());
    }

    #[test]
    fn load_config_missing_file_returns_default_no_warning() {
        // The user is operating without a config file. Boot picks up
        // defaults silently — the editor is the right place to start
        // a config from scratch.
        let tmp = TempDir::new().expect("tempdir");
        let missing = tmp.path().join("does_not_exist.toml");
        let (cfg, warning) = load_config(Some(&missing)).expect("ok");
        assert!(warning.is_none());
        assert_eq!(cfg, config::Config::default());
    }

    #[test]
    fn load_config_unknown_keys_surface_as_warning() {
        // Pin the from_str_validated wiring: a config with an
        // unknown top-level key parses successfully but emits a
        // warning the boot path surfaces on stderr. Without this,
        // the user would get the same silent-shadowing behavior the
        // malformed-TOML branch was fixed to prevent — typo'd
        // section headers (e.g. `[lines]`) would parse as forward-
        // compat unknown keys and never reach the user.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("typo.toml");
        // Top-level bogus key (before the `[line]` section, so TOML
        // scope keeps it at the root). Inside a section, the
        // unknown-key validator doesn't fire on top-level allow-list
        // mismatches.
        fs::write(
            &path,
            "bogus_top_level_key = 42\n[line]\nsegments = [\"model\"]\n",
        )
        .expect("write");
        let (cfg, warning) = load_config(Some(&path)).expect("ok");
        // Parse still succeeds — the editor opens against the user's
        // real config, not defaults.
        let segments = cfg
            .line
            .as_ref()
            .map(|l| l.segments.clone())
            .unwrap_or_default();
        assert_eq!(segments, vec!["model".to_string()]);
        // Warning surfaces the unknown key.
        let msg = warning.expect("unknown-key warning present");
        assert!(msg.contains("bogus_top_level_key"), "got {msg:?}");
    }

    #[test]
    fn load_config_valid_toml_returns_parsed_no_warning() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        fs::write(&path, "[line]\nsegments = [\"model\"]\n").expect("write");
        let (cfg, warning) = load_config(Some(&path)).expect("ok");
        assert!(warning.is_none(), "valid TOML emits no warning");
        let segments = cfg
            .line
            .as_ref()
            .map(|l| l.segments.clone())
            .unwrap_or_default();
        assert_eq!(segments, vec!["model".to_string()]);
    }

    #[test]
    fn load_config_malformed_toml_returns_default_with_warning() {
        // Pin the v0.1 contract: parse error → default config + a
        // warning string the boot path emits to stderr before the
        // alt-screen takes over. Without the warning the user
        // wouldn't notice their real config is being shadowed.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("broken.toml");
        fs::write(&path, "this = is not = valid TOML\n").expect("write");
        let (cfg, warning) = load_config(Some(&path)).expect("ok");
        assert_eq!(cfg, config::Config::default());
        let msg = warning.expect("warning present");
        assert!(msg.contains("parse error"), "got {msg:?}");
        assert!(
            msg.contains("broken.toml"),
            "warning names the path: {msg:?}"
        );
        assert!(msg.contains("opening with defaults"), "got {msg:?}");
    }

    #[cfg(unix)]
    #[test]
    fn load_config_unreadable_file_propagates_error() {
        // Permission-denied isn't a silent fallback; it surfaces as
        // a load-failure exit. Unix-only (chmod 000 isn't portable),
        // and assumes a non-root test runner — root bypasses the
        // permission check.
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("locked.toml");
        fs::write(&path, "irrelevant").expect("write");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o000);
        fs::set_permissions(&path, perms).expect("chmod");
        let outcome = load_config(Some(&path));
        // Restore perms so TempDir's drop can clean up.
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
        assert!(outcome.is_err(), "expected error, got {outcome:?}");
    }
}
