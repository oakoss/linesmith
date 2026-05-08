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
mod items_editor;
mod list_screen;
mod main_menu;
mod placeholder;
mod preview;

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{self as cevent, Event as CtEvent, KeyEventKind};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::Terminal;
use toml_edit::DocumentMut;

use crate::config;
use crate::logging::{CapturedSink, SinkGuard};

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
    let load = match load_config(config_path) {
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
    if let Some(warning) = &load.warning {
        let _ = writeln!(stderr, "linesmith config: {warning}");
    }

    // Resolve theme + color capability so the preview tracks the
    // user's configured rendering. Theme registry is built from
    // XDG-derived dirs; capability honors `[layout_options].color`
    // and falls through to `Capability::detect` (which checks
    // `NO_COLOR` and probes the terminal). `FORCE_COLOR` and the
    // production driver's CLI flags aren't honored here — full
    // parity with the driver's resolve chain is a separate task.
    let xdg = crate::data_context::xdg::XdgEnv::from_process_env();
    let user_themes_dir = crate::runtime::themes::user_themes_dir(&xdg);
    let theme_registry =
        crate::runtime::themes::build_theme_registry(user_themes_dir.as_deref(), |msg| {
            let _ = writeln!(stderr, "linesmith config: {msg}");
        });
    let theme = resolve_theme(load.config.theme.as_deref(), &theme_registry, stderr).clone();
    let capability = resolve_capability(&load.config);

    // Install the captured-log sink *before* enter_terminal so any
    // macro emission that fires between sink install and the first
    // draw lands in the buffer (where the first frame's drain will
    // surface it) rather than on stderr (where the alt-screen would
    // paint over it). The `SinkGuard` restores `StderrSink` on drop
    // for the normal-return path; under the workspace's release
    // `panic = "abort"`, the panic hook (not Drop) is what restores
    // terminal state and stderr is owned by the alt-screen until
    // process exit.
    let captured_sink = Arc::new(CapturedSink::default());
    let _sink_guard = SinkGuard::install(captured_sink.clone());

    let model = Model::new(
        load.config,
        load.document,
        load.original_text,
        load.save_target,
        theme,
        capability,
        Some(Arc::clone(&captured_sink)),
    );

    // Install the panic hook *before* enter_terminal so a panic
    // during `Terminal::new` or the first draw still routes through
    // `leave_terminal`. Pre-enter, the hook is a no-op (raw mode is
    // off, we're not in alt-screen yet); post-enter, it's the
    // safety net.
    install_panic_hook();
    if let Err(err) = enter_terminal() {
        // Drain anything the sink captured between install and
        // failure (theme registry warnings, capability detection,
        // anything Model::new transitively emits) onto stderr —
        // the alt-screen never opened, so there's no warnings
        // panel to surface them in, and silently dropping
        // diagnostics from the boot-failure path is the worst
        // possible UX for "why didn't my TUI start".
        flush_captured_to_stderr(&captured_sink, stderr);
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
            // The event loop failed; surface anything macros emitted
            // between the last successful frame drain and the failure
            // point so the user sees the underlying diagnostic, not
            // just the I/O error code.
            flush_captured_to_stderr(&captured_sink, stderr);
            let _ = writeln!(stderr, "linesmith config: event loop: {err}");
            1
        }
    }
}

/// Atomically replace `path` with `contents`. Writes to a temp
/// file in the same directory, fsyncs, then renames over the
/// destination — so a crash mid-write leaves the original file
/// intact (and the temp file orphaned but harmless), and the
/// rename itself is atomic on both Unix (`rename(2)`) and Windows
/// (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING`, which `tempfile`
/// invokes via `persist`). Parent directory is created if missing
/// — covers the "user pointed `--config` at a path under
/// `~/.config/linesmith/` before that directory existed" case so
/// the first save isn't a `NotFound` failure.
// Same `redundant_pub_crate` / `unreachable_pub` clash as `run`
// above; same resolution.
#[allow(clippy::redundant_pub_crate)]
pub(super) fn atomic_write(path: &Path, contents: &str) -> io::Result<()> {
    // Resolve to absolute up front so the temp directory choice
    // can't drift if cwd changes between save attempts (a TUI host
    // process that chdirs after spawn would otherwise land the
    // temp on a different filesystem and lose `persist`'s atomic
    // contract via cross-device rename).
    let absolute: PathBuf = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write: path has no parent directory",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(&absolute).map_err(|e| {
        // Log the orphaned temp path so a user investigating
        // "why did save fail" doesn't also have to wonder where
        // the leftover `.tmpXXXXXX` next to their config came
        // from. The `Drop` on `e.file` cleans up if it can, but a
        // partial-persist on Windows ("destination open by another
        // process") can leave the temp behind.
        linesmith_core::lsm_warn!(
            "atomic_write: persist failed; orphaned temp at {} may be removed manually",
            e.file.path().display(),
        );
        e.error
    })?;
    Ok(())
}

/// Drain any entries the captured sink picked up during boot and
/// write them to `stderr`. Used by the early-return arm when
/// `enter_terminal` fails: the alt-screen never opened, so the
/// warnings panel never gets a chance to surface them, and silently
/// dropping diagnostics from the boot-failure path is the worst
/// possible UX for "why didn't my TUI start".
///
/// Lines come out prefixed with `linesmith config:` to match the
/// other boot-path stderr writes (parse warnings, terminal setup
/// errors). The captured `[<level>] <msg>` body is preserved as-is
/// so the level tag is visible to the user.
fn flush_captured_to_stderr(captured: &CapturedSink, stderr: &mut dyn Write) {
    for entry in captured.drain() {
        let _ = writeln!(stderr, "linesmith config: {entry}");
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
    Ok(classify_event(cevent::read()?))
}

/// Map a raw crossterm event to our internal [`Event`] enum.
///
/// `KeyEventKind::Press` is the only key kind we forward — Windows
/// crossterm reports both Press and Release for every physical
/// keystroke (macOS/Linux only emit Press by default), so accepting
/// every kind would double-fire `Action` verbs and double-toggle
/// move-mode on Enter under Windows. OS-level autorepeat already
/// produces a stream of Press events, so filtering Repeat doesn't
/// break held-key navigation.
fn classify_event(event: CtEvent) -> Option<Event> {
    match event {
        CtEvent::Key(key) if key.kind == KeyEventKind::Press => Some(Event::Key(key)),
        CtEvent::Resize(_, _) => Some(Event::Resize),
        // Mouse / FocusGained/Lost / Paste — ignored for v0.1.
        // Non-Press key kinds (Release, Repeat) — filtered above.
        _ => None,
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

/// Resolve the color capability for the preview from
/// `[layout_options].color`: Never → strip color, Always →
/// richest the terminal supports (with TrueColor rescue when not
/// a TTY), Auto → `Capability::detect` (which checks `NO_COLOR`
/// and probes the terminal). `FORCE_COLOR` and the CLI's
/// `--force-color` / `--no-color` flags aren't honored here; the
/// editor doesn't take args.
fn resolve_capability(config: &config::Config) -> crate::theme::Capability {
    use crate::theme::Capability;
    match config.layout_options.as_ref().map(|l| l.color) {
        Some(config::ColorPolicy::Never) => Capability::None,
        Some(config::ColorPolicy::Always) => {
            // `from_terminal` strips `NO_COLOR` and runs the
            // supports-color probe; if the probe says "no TTY"
            // we still want color (the user asked for it).
            let probed = Capability::from_terminal();
            if probed == Capability::None {
                Capability::TrueColor
            } else {
                probed
            }
        }
        _ => Capability::detect(),
    }
}

/// Resolve the user's configured theme name against the registry.
/// Empty / unset name → `default`. Unknown name → `default` with a
/// stderr warning emitted before the alt-screen takes over so the
/// user sees the typo without hunting in scrollback.
fn resolve_theme<'a>(
    name: Option<&str>,
    registry: &'a crate::theme::ThemeRegistry,
    stderr: &mut dyn Write,
) -> &'a crate::theme::Theme {
    let Some(name) = name.filter(|n| !n.is_empty()) else {
        return registry
            .lookup("default")
            .expect("default theme is always in the registry");
    };
    match registry.lookup(name) {
        Some(t) => t,
        None => {
            let _ = writeln!(
                stderr,
                "linesmith config: unknown theme '{name}'; using 'default'",
            );
            registry
                .lookup("default")
                .expect("default theme is always in the registry")
        }
    }
}

/// Result of [`load_config`]. Bundles the typed [`config::Config`]
/// the render pipeline needs with the round-trip-preserving
/// [`DocumentMut`] the editor mutates and saves, plus enough state
/// to drive the save-allowed / save-refused decision.
// Same `redundant_pub_crate` / `unreachable_pub` clash as `run`
// above; same resolution.
#[allow(clippy::redundant_pub_crate)]
#[derive(Debug)]
pub(super) struct LoadOutcome {
    pub(super) config: config::Config,
    pub(super) document: DocumentMut,
    /// The exact bytes read from disk (or `String::new()` for the
    /// no-file / parse-error paths). Used by the dirty-check: a
    /// stringify of `document` that equals this means no edits.
    pub(super) original_text: String,
    /// Where Ctrl+S should write. `Some` when save is allowed —
    /// either the file existed and parsed cleanly, or the file
    /// didn't exist but the user supplied a path so save creates
    /// it. `None` when save is refused: no path provided, or the
    /// file existed but parse-failed (overwriting it would clobber
    /// the user's broken-but-present config with defaults).
    pub(super) save_target: Option<PathBuf>,
    /// Optional human-readable warning the caller surfaces on
    /// stderr *before* the alt-screen takes over. Carries unknown-
    /// key diagnostics on a clean parse, or the parse-error
    /// message on a malformed file.
    pub(super) warning: Option<String>,
}

/// Load the config for the boot path.
///
/// Outcomes:
///
/// - `path == None` → empty document, `save_target = None`. No
///   target to save to; Ctrl+S will surface a "save not available"
///   message when triggered.
/// - File absent (path provided, `NotFound`) → empty document,
///   `save_target = Some(path)`. Save creates the file.
/// - File present and parses cleanly → loaded document,
///   `save_target = Some(path)`, optional unknown-key warning.
/// - File present but malformed → empty document,
///   `save_target = None`, parse-error warning. Save is refused
///   because the user's broken-but-present file would otherwise
///   get clobbered with defaults on the first Ctrl+S.
///
/// I/O errors other than `NotFound` propagate so the boot path
/// surfaces them as a load failure exit, not as a silent fallback.
fn load_config(path: Option<&Path>) -> io::Result<LoadOutcome> {
    let Some(path) = path else {
        return Ok(LoadOutcome {
            config: config::Config::default(),
            document: DocumentMut::new(),
            original_text: String::new(),
            save_target: None,
            warning: None,
        });
    };
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut warnings: Vec<String> = Vec::new();
            match config::Config::from_str_validated(&text, |w| warnings.push(w.to_string())) {
                Ok(cfg) => {
                    // toml + toml_edit share a parser at the same
                    // major version, so a disagreement here means
                    // the two crates have skewed (most likely a
                    // future Cargo update bumped one but not the
                    // other). Don't panic — that crashes the TUI
                    // mid-edit. Don't fall back to an empty
                    // document either — that would hand the user
                    // an editor that round-trips defaults and
                    // silently drops their existing keys on
                    // Ctrl+S. Treat as save-disabled (same posture
                    // as a malformed file) so the user can browse
                    // through the parsed Config but can't clobber.
                    match text.parse::<DocumentMut>() {
                        Ok(document) => {
                            let warning = (!warnings.is_empty()).then(|| warnings.join("\n"));
                            Ok(LoadOutcome {
                                config: cfg,
                                document,
                                original_text: text,
                                save_target: Some(path.to_path_buf()),
                                warning,
                            })
                        }
                        Err(err) => Ok(LoadOutcome {
                            config: cfg,
                            document: DocumentMut::new(),
                            original_text: String::new(),
                            save_target: None,
                            warning: Some(format!(
                                "TOML parser skew in {}: {err} — editor opened read-only (save disabled)",
                                path.display()
                            )),
                        }),
                    }
                }
                Err(err) => Ok(LoadOutcome {
                    config: config::Config::default(),
                    document: DocumentMut::new(),
                    original_text: String::new(),
                    save_target: None,
                    warning: Some(format!(
                        "parse error in {}: {err} — opening with defaults (save disabled)",
                        path.display()
                    )),
                }),
            }
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(LoadOutcome {
            config: config::Config::default(),
            document: DocumentMut::new(),
            original_text: String::new(),
            save_target: Some(path.to_path_buf()),
            warning: None,
        }),
        Err(err) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventState, KeyModifiers};
    use std::fs;
    use tempfile::TempDir;

    fn key_event(kind: KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind_and_state(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            kind,
            KeyEventState::NONE,
        )
    }

    #[test]
    fn atomic_write_creates_new_file_with_contents() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        atomic_write(&path, "[line]\nsegments = []\n").expect("write");
        let read = fs::read_to_string(&path).expect("read");
        assert_eq!(read, "[line]\nsegments = []\n");
    }

    #[test]
    fn atomic_write_replaces_existing_file_atomically() {
        // Pin the round-trip: writing fresh contents over an
        // existing file leaves the destination with the new
        // bytes, not concatenated. Failure mode would be a
        // non-atomic write that appends or partially overwrites.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        fs::write(&path, "old contents that should disappear").expect("seed");
        atomic_write(&path, "new bytes").expect("write");
        assert_eq!(fs::read_to_string(&path).expect("read"), "new bytes");
    }

    #[test]
    fn atomic_write_creates_missing_parent_directory() {
        // Pin: pointing --config at a path under a directory
        // that doesn't exist (e.g., `~/.config/linesmith/` when
        // the user has never run linesmith) creates the parent
        // before writing. Without this, the first save would
        // NotFound and the user wouldn't understand why.
        let tmp = TempDir::new().expect("tempdir");
        let nested = tmp.path().join("nested/subdir/config.toml");
        atomic_write(&nested, "x").expect("write");
        assert_eq!(fs::read_to_string(&nested).expect("read"), "x");
    }

    #[test]
    fn atomic_write_does_not_leave_temp_file_on_success() {
        // The tempfile crate's persist() consumes the temp; pin
        // that no `*.tmp` siblings linger after a successful
        // write. Without this, a future refactor that drops the
        // persist() call would silently leave temps around.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        atomic_write(&path, "x").expect("write");
        let entries: Vec<_> = fs::read_dir(tmp.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected only the destination file, got {entries:?}",
        );
    }

    #[test]
    fn flush_captured_to_stderr_drains_with_boot_path_prefix() {
        // Pin the boot-failure drain format: each entry comes out
        // prefixed with `linesmith config: ` to match the other
        // boot-path stderr writes (parse warnings, terminal-setup
        // errors), and the captured `[<level>] <msg>` body stays
        // intact so the user sees the level tag. Drain is
        // consume-once: a second call returns nothing.
        use crate::logging::{Level, LogSink};

        let _serial = crate::logging::_test_serial_lock();
        let captured = CapturedSink::default();
        captured.emit(Level::Warn, "first");
        captured.emit_error("oops");
        let mut stderr = Vec::<u8>::new();
        flush_captured_to_stderr(&captured, &mut stderr);
        let written = String::from_utf8(stderr).expect("utf8");
        assert!(
            written.contains("linesmith config: [warn] first"),
            "missing warn prefix in {written:?}",
        );
        assert!(
            written.contains("linesmith config: [error] oops"),
            "missing error prefix in {written:?}",
        );
        // Drain consumed both entries; second flush is a no-op.
        let mut second = Vec::<u8>::new();
        flush_captured_to_stderr(&captured, &mut second);
        assert!(second.is_empty(), "second flush leaked: {second:?}");
    }

    #[test]
    fn classify_press_key_routes_to_event_key() {
        let outcome = classify_event(CtEvent::Key(key_event(KeyEventKind::Press)));
        assert!(matches!(outcome, Some(Event::Key(_))));
    }

    #[test]
    fn classify_release_key_is_filtered() {
        // Crossterm on Windows emits both Press AND Release for
        // every keystroke; macOS/Linux only emit Press by default.
        // Without this filter, Windows users would double-fire
        // every `Action` verb and double-toggle move-mode on
        // Enter. Pin the filter so a future "match all key kinds"
        // refactor regresses noisily instead of just on Windows.
        let outcome = classify_event(CtEvent::Key(key_event(KeyEventKind::Release)));
        assert!(outcome.is_none());
    }

    #[test]
    fn classify_repeat_key_is_filtered() {
        // OS-level autorepeat already produces a stream of Press
        // events for held keys, so we don't need to handle Repeat
        // separately. Filtering it out keeps held-key behavior
        // identical across platforms — autorepeat cadence comes
        // from the OS, not from us.
        let outcome = classify_event(CtEvent::Key(key_event(KeyEventKind::Repeat)));
        assert!(outcome.is_none());
    }

    #[test]
    fn classify_resize_routes_to_event_resize() {
        let outcome = classify_event(CtEvent::Resize(80, 24));
        assert!(matches!(outcome, Some(Event::Resize)));
    }

    #[test]
    fn classify_focus_and_paste_are_filtered() {
        // Mouse / FocusGained / FocusLost / Paste land with the
        // screens that need them; today none do, so they fall
        // through to the catchall and produce no event.
        assert!(classify_event(CtEvent::FocusGained).is_none());
        assert!(classify_event(CtEvent::FocusLost).is_none());
        assert!(classify_event(CtEvent::Paste("ignored".to_string())).is_none());
    }

    #[test]
    fn load_config_none_path_refuses_save() {
        // No --config / no XDG fallback → no save target at all.
        // Ctrl+S surfaces "save not available"; the editor is
        // effectively read-only.
        let out = load_config(None).expect("ok");
        assert!(out.warning.is_none());
        assert_eq!(out.config, config::Config::default());
        assert!(out.save_target.is_none(), "no path → no save target");
        assert!(out.original_text.is_empty());
    }

    #[test]
    fn load_config_missing_file_allows_save_to_path() {
        // Path provided, file absent → defaults loaded, but save
        // target is the user-supplied path so Ctrl+S creates it.
        // Distinguishes "no path at all" (refuse save) from "path
        // provided but file doesn't exist yet" (create on save).
        let tmp = TempDir::new().expect("tempdir");
        let missing = tmp.path().join("does_not_exist.toml");
        let out = load_config(Some(&missing)).expect("ok");
        assert!(out.warning.is_none());
        assert_eq!(out.config, config::Config::default());
        assert_eq!(out.save_target.as_deref(), Some(missing.as_path()));
        assert!(out.original_text.is_empty());
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
        let out = load_config(Some(&path)).expect("ok");
        // Parse still succeeds — the editor opens against the user's
        // real config, not defaults. Save remains allowed: forward-
        // compat unknown keys aren't a reason to refuse round-trip.
        let segments = out
            .config
            .line
            .as_ref()
            .map(|l| l.segments.clone())
            .unwrap_or_default();
        assert_eq!(segments, vec!["model".to_string()]);
        let msg = out.warning.expect("unknown-key warning present");
        assert!(msg.contains("bogus_top_level_key"), "got {msg:?}");
        assert_eq!(out.save_target.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn load_config_valid_toml_carries_document_and_original_text() {
        // Pin the round-trip foundation: a clean parse populates
        // both `original_text` (the exact bytes we read) and
        // `document` (the toml_edit DocumentMut). Without these,
        // dirty-detection has nothing to compare against and save
        // has nothing to write.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let raw = "# header comment kept\n[line]\nsegments = [\"model\"]\n";
        fs::write(&path, raw).expect("write");
        let out = load_config(Some(&path)).expect("ok");
        assert!(out.warning.is_none(), "valid TOML emits no warning");
        let segments = out
            .config
            .line
            .as_ref()
            .map(|l| l.segments.clone())
            .unwrap_or_default();
        assert_eq!(segments, vec!["model".to_string()]);
        assert_eq!(out.original_text, raw);
        // toml_edit round-trips byte-for-byte on a clean parse —
        // pin that the loaded document is initially identical to
        // the source so dirty-check starts at False.
        assert_eq!(out.document.to_string(), raw);
        assert_eq!(out.save_target.as_deref(), Some(path.as_path()));
    }

    #[test]
    fn load_config_malformed_toml_disables_save_with_warning() {
        // Pin the v0.1 contract: parse error → default config + a
        // warning string the boot path emits to stderr before the
        // alt-screen takes over. Save is REFUSED — overwriting a
        // broken-but-present file with defaults on the first
        // Ctrl+S would silently destroy whatever the user was
        // mid-edit on.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("broken.toml");
        fs::write(&path, "this = is not = valid TOML\n").expect("write");
        let out = load_config(Some(&path)).expect("ok");
        assert_eq!(out.config, config::Config::default());
        let msg = out.warning.expect("warning present");
        assert!(msg.contains("parse error"), "got {msg:?}");
        assert!(
            msg.contains("broken.toml"),
            "warning names the path: {msg:?}"
        );
        assert!(msg.contains("opening with defaults"), "got {msg:?}");
        assert!(msg.contains("save disabled"), "got {msg:?}");
        assert!(
            out.save_target.is_none(),
            "parse error must refuse save target",
        );
        assert!(out.original_text.is_empty());
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
