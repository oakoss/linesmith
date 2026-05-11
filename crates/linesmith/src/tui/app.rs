//! `Model` + pure `update` + `view` skeleton per ADR-0016.
//!
//! `update` is `(Model, Event) -> Model` so screen behavior is unit-
//! testable without ratatui in the loop. `view` renders the current
//! screen state into a ratatui `Frame`. The `AppScreen` enum is
//! `#[non_exhaustive]` so new screen variants don't churn match arms
//! in code that didn't need to change.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use toml_edit::DocumentMut;

use crate::config;
use crate::logging::CapturedSink;
use crate::theme::{Capability, Theme, ThemeRegistry};

use super::environment_warning::{prepend_env_warnings, EnvironmentSnapshot};
use super::install_screen::{self, InstallScreenState};
use super::items_editor::{self, ItemsEditorState};
use super::line_picker::{self, LinePickerState};
use super::main_menu::{self, MainMenuState};
use super::placeholder::{self, PlaceholderState};
use super::preview;
use super::raw_value_editor::{self, RawValueEditorState};
use super::theme_picker::{self, ThemePickerState};
use super::type_picker::{self, TypePickerState};

/// Top-level UI state. Each variant carries its own state struct.
/// Add a screen by adding a variant + its state struct + a `match`
/// arm in [`view`] / [`update`].
#[non_exhaustive]
#[derive(Debug)]
pub(super) enum AppScreen {
    MainMenu(MainMenuState),
    Placeholder(PlaceholderState),
    ItemsEditor(ItemsEditorState),
    LinePicker(LinePickerState),
    TypePicker(TypePickerState),
    RawValueEditor(RawValueEditorState),
    ThemePicker(ThemePickerState),
    InstallToClaudeCode(InstallScreenState),
}

impl AppScreen {
    /// Whether the active screen is consuming text input. Two
    /// global shortcuts are suppressed on these screens:
    ///
    /// - bare `q` quit — the user must be able to type a `q`
    ///   into the buffer without quitting the TUI.
    /// - Ctrl+S save — pending edits live in the screen's own
    ///   buffer, not yet in `model.document`; saving here would
    ///   write the pre-edit state and silently drop the visible
    ///   change. Suppression surfaces a warn telling the user to
    ///   commit (Enter) first.
    ///
    /// Ctrl+C still force-quits unconditionally — it's the
    /// universal escape hatch.
    fn captures_text_input(&self) -> bool {
        matches!(self, AppScreen::RawValueEditor(_))
    }

    /// Footer hint text for the active screen per ADR-0025. The
    /// hint is context-aware because the same key has different
    /// semantics across screens: on MainMenu `Esc` quits (no
    /// parent to back-nav to), on text-entry screens `q` is a
    /// literal character, etc. A single one-size-fits-all hint
    /// would advertise incorrect behavior on at least one screen.
    fn footer_hint(&self) -> &'static str {
        match self {
            AppScreen::MainMenu(_) => " [Enter] activate   [Esc] quit   [Ctrl+C] force-quit ",
            AppScreen::RawValueEditor(_) => " [Enter] commit   [Esc] cancel   [Ctrl+C] force-quit ",
            AppScreen::Placeholder(_)
            | AppScreen::ItemsEditor(_)
            | AppScreen::LinePicker(_)
            | AppScreen::TypePicker(_)
            | AppScreen::ThemePicker(_)
            | AppScreen::InstallToClaudeCode(_) => {
                " [Enter] confirm   [Esc] back   [q] quit   [Ctrl+C] force-quit "
            }
        }
    }
}

/// Save-feedback state surfaced in the preview pane per ADR-0025.
/// `Saved` is transient and clears on the next user input event so
/// the user sees confirmation but the toast doesn't linger. `Error`
/// persists until the next successful save replaces it; the user
/// needs the failure visible until they fix the underlying issue
/// (disk full, permissions, missing config path).
#[derive(Debug, Default)]
pub(super) enum SaveFeedback {
    /// No save attempt has run yet, or the most recent toast was
    /// cleared by the next user input event.
    #[default]
    None,
    /// Most recent commit's auto-save flushed successfully. Cleared
    /// at the top of [`update`] when the next key event fires.
    Saved,
    /// Most recent save attempt failed; banner stays until the
    /// next successful save (or a Clean commit that confirms
    /// in-memory state matches our last successful write).
    /// Carries the user-visible message.
    Error(String),
}

/// Top-level model. Carries the current screen, the parsed config,
/// the round-trip-preserving TOML document for write-back, the
/// resolved theme, the detected color capability, the captured log
/// sink (when running under the alt-screen), and the quit flag.
/// Theme + capability are snapshot at boot so the preview honors
/// `config.theme` and `NO_COLOR` the same way the production driver
/// does.
pub(super) struct Model {
    pub(super) screen: AppScreen,
    // Held on `Model` so screens that need it can read it
    // directly; current screens (`MainMenu`, `Placeholder`) don't,
    // hence the dead-code allow.
    #[allow(dead_code)]
    pub(super) config: config::Config,
    /// Mutable TOML document the editor mutates. Edit screens
    /// (items editor, line picker, theme picker, raw-value editor)
    /// take `&mut model.document` and apply scoped edits; the
    /// dispatcher auto-saves after every screen-level commit per
    /// ADR-0025, so `document` and the on-disk file converge on
    /// each `Enter`. `Model::save` re-checks `original_text` only
    /// to skip no-op writes (e.g. the implicit-default theme pick).
    pub(super) document: DocumentMut,
    /// Last-known on-disk bytes. Updated to the just-written
    /// document after every successful save. See [`Model::save`]
    /// for the skip-on-Clean rule.
    pub(super) original_text: String,
    /// Atomic-rename target. `None` means save is refused — either
    /// the user didn't supply a config path at all, or the file
    /// existed but parse-failed at load and overwriting it would
    /// clobber the user's broken-but-present TOML with defaults.
    pub(super) save_target: Option<PathBuf>,
    pub(super) theme: Theme,
    /// Full theme registry (built-in + user themes). Built at boot
    /// from `with_built_ins()` plus `with_user_themes(...)` when an
    /// XDG themes dir resolves. Held immutably; a quit-and-reopen
    /// picks up new user themes. Theme selection updates
    /// `model.theme` directly rather than touching the registry.
    theme_registry: ThemeRegistry,
    pub(super) capability: Capability,
    /// Process-wide log sink the boot path swapped in. The view
    /// passes a borrow to `preview::render_lines`, which drains
    /// macro emissions into the warnings vec so they paint into
    /// the warnings panel instead of corrupting the alt-screen.
    /// `None` for unit tests that bypass the boot path.
    pub(super) sink: Option<Arc<CapturedSink>>,
    /// Save-feedback toast/banner state per ADR-0025. Updated by the
    /// dispatcher after each `ScreenOutcome::Committed`; rendered by
    /// `view` inside the preview pane.
    pub(super) save_feedback: SaveFeedback,
    /// Resolved path to `~/.claude/settings.json` for the install
    /// screen. `None` when `$HOME` is unset; the install screen
    /// routes to a Placeholder in that case so the user gets a
    /// visible diagnostic instead of a no-op.
    pub(super) install_settings_path: Option<PathBuf>,
    /// `statusLine.command` value the install screen will write —
    /// `"linesmith"` for the default case, `"linesmith --config <p>"`
    /// when the TUI was invoked with `--config`. Pre-resolved at boot
    /// so the screen doesn't need to traverse `CliEnv` mid-dispatch.
    pub(super) install_command: String,
    pub(super) quit: bool,
}

impl Model {
    /// Construct a fresh `Model` from the boot-path-resolved load
    /// outcome plus theme / capability / sink. Opens on the
    /// `MainMenu` screen.
    ///
    /// All write-back state — `document`, `original_text`,
    /// `save_target` — comes pre-resolved from `super::load_config`
    /// so this constructor is total: no fallible reads happen on
    /// `Model` itself, and tests don't need to stub a filesystem.
    // Eight `Model::new` parameters is one over clippy's default
    // threshold but each is load-bearing initialization the
    // constructor can't synthesize. Grouping into a `BootSnapshot`
    // struct would just shuffle the same values one indirection
    // deeper.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        config: config::Config,
        document: DocumentMut,
        original_text: String,
        save_target: Option<PathBuf>,
        theme: Theme,
        theme_registry: ThemeRegistry,
        capability: Capability,
        sink: Option<Arc<CapturedSink>>,
        install_settings_path: Option<PathBuf>,
        install_command: String,
    ) -> Self {
        Self {
            screen: AppScreen::MainMenu(MainMenuState::default()),
            config,
            document,
            original_text,
            save_target,
            theme,
            theme_registry,
            capability,
            sink,
            install_settings_path,
            install_command,
            save_feedback: SaveFeedback::None,
            quit: false,
        }
    }

    /// Persist the in-memory document to `save_target` via atomic
    /// rename. Returns a [`SaveOutcome`] describing what happened
    /// so the dispatcher can update `save_feedback` and emit
    /// diagnostics through the macro channel.
    ///
    /// Writes whenever the in-memory document doesn't match
    /// what's on disk: either the bytes differ from `original_text`
    /// OR the target file doesn't exist yet (the documented
    /// "first-run / create-a-config" flow on a `--config new.toml`
    /// invocation). The latter case lets a fresh session create the
    /// file from an unchanged document; it also rescues a config
    /// that was deleted externally between boot and save.
    ///
    /// On `Saved`, `original_text` is updated to the just-written
    /// stringified document so the next no-op commit (e.g. picking
    /// the implicit default again) returns `Clean` instead of
    /// retrying. On `Error`, `original_text` stays at the prior
    /// successful value so the next commit retries the write.
    pub(super) fn save(&mut self) -> SaveOutcome {
        let Some(path) = self.save_target.clone() else {
            return SaveOutcome::NoTarget;
        };
        let serialized = self.document.to_string();
        let dirty = serialized != self.original_text;
        if !dirty && path.exists() {
            return SaveOutcome::Clean;
        }
        match super::atomic_write(&path, &serialized) {
            Ok(()) => {
                self.original_text = serialized;
                SaveOutcome::Saved(path)
            }
            Err(error) => SaveOutcome::Error { path, error },
        }
    }
}

/// Result of [`Model::save`]. The caller emits diagnostics for the
/// non-`Clean` cases; `Clean` is silent because Ctrl+S spam on an
/// already-saved document shouldn't litter the warnings panel.
#[non_exhaustive]
#[derive(Debug)]
pub(super) enum SaveOutcome {
    /// `save_target == None` — either no `--config` was supplied
    /// or the file existed but parse-failed at load. Surface as a
    /// user-visible warning so they understand why Ctrl+S didn't
    /// do anything.
    NoTarget,
    /// In-memory document matches `original_text` AND the target
    /// file exists; no write happened. Silent — the user just
    /// wanted to save and there was nothing to save. A target
    /// file that's missing on disk takes the `Saved` arm even
    /// when the document is unchanged, so the first-run
    /// `--config new.toml` flow creates the file.
    Clean,
    /// Wrote `path` atomically.
    Saved(PathBuf),
    /// Write or rename failed. Carries the destination so the
    /// surfaced error names which file failed; carries the raw
    /// `io::Error` so future callers can match on `ErrorKind`
    /// (e.g., to suggest "chmod" on `PermissionDenied` vs
    /// "rerun with a different `--config`" on `NotFound`).
    Error { path: PathBuf, error: io::Error },
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
///
/// Per ADR-0025, "commit" (the document was mutated) and "navigate"
/// (the screen transitioned) are independent dimensions:
///
/// |                 | no nav          | nav                          |
/// | --------------- | --------------- | ---------------------------- |
/// | no commit       | `Stay`          | `NavigateTo(AppScreen)`      |
/// | commit          | `Committed`     | `CommitAndNavigate(AppScreen)` |
///
/// The dispatcher calls `model.save()` and updates `save_feedback`
/// on the two commit-bearing variants; the others leave save state
/// untouched.
#[non_exhaustive]
#[derive(Debug)]
pub(super) enum ScreenOutcome {
    /// Screen handled the event internally (or didn't claim it);
    /// `Model` stays as-is, no document mutation.
    Stay,
    /// Document was mutated but the screen stays put — used for
    /// in-screen verbs (items editor a/i/d/c/k/m, swap, separator
    /// insert). Dispatcher auto-saves and updates `save_feedback`.
    Committed,
    /// Replace `model.screen` with the supplied `AppScreen`,
    /// without mutating the document. Menu activation, Esc
    /// back-nav, and similar transitions.
    NavigateTo(AppScreen),
    /// Replace `model.screen` AND signal a document mutation —
    /// dispatcher auto-saves before applying the new screen.
    /// Pickers' Enter/Activate paths use this.
    CommitAndNavigate(AppScreen),
    /// Signal the event loop to leave the TUI.
    Quit,
}

/// Pure state transition. Per ADR-0025, screen-level commits
/// auto-save and surface a transient toast; `Ctrl+S` is a deprecated
/// no-op force-flush kept for one release as a muscle-memory
/// concession. Quit is unconditional — there's no dirty state under
/// instant-apply.
///
/// `Event::Resize` is a no-op at the model layer; routing it
/// through [`update`] still triggers the post-update draw in the
/// event loop, which is the redraw the user wants. Resize
/// deliberately doesn't clear `save_feedback` so an in-flight toast
/// survives terminal-resize redraws.
#[must_use]
pub(super) fn update(mut model: Model, event: Event) -> Model {
    let key = match event {
        Event::Key(key) => key,
        Event::Resize => return model,
    };
    // Transient toast clears on the next user input event: the
    // confirmation persists from the commit's render until the
    // user does anything else. Persistent error banners stay until
    // a successful save replaces them (or a Clean commit clears
    // the stale one).
    if matches!(model.save_feedback, SaveFeedback::Saved) {
        model.save_feedback = SaveFeedback::None;
    }
    // Ctrl+S is a deprecation no-op per ADR-0025: instant-apply
    // already wrote on the last commit, so there's nothing to
    // flush in the common case. Still calls `save()` to recover
    // from a prior commit whose write failed and left the document
    // ahead of disk. Suppressed on text-entry screens (Enter must
    // commit pending buffer edits before any save attempt makes
    // sense).
    if is_save_key(&key) {
        if model.screen.captures_text_input() {
            linesmith_core::lsm_warn!("press Enter to commit your edit before saving",);
            return model;
        }
        // Suppress the deprecation warn when there's an active save
        // error: Ctrl+S is the genuine recovery path in that case
        // (the document is ahead of disk and a force-flush retries
        // the write), and "no longer needed" would be misleading.
        if !matches!(model.save_feedback, SaveFeedback::Error(_)) {
            linesmith_core::lsm_warn!("Ctrl+S is no longer needed; changes save automatically",);
        }
        apply_commit_save(&mut model);
        return model;
    }
    if is_force_quit(&key) {
        // Ctrl+C is a user-initiated abandonment; surface the
        // unresolved error at `lsm_warn!` rather than `lsm_error!`
        // so a clean-intent quit-with-error stays distinguishable
        // from a force-quit at the log level.
        if let SaveFeedback::Error(msg) = &model.save_feedback {
            linesmith_core::lsm_warn!("force-quit with unresolved save error: {msg}");
        }
        model.quit = true;
        return model;
    }
    // Suppress the bare-`q` quit shortcut on text-entry screens so
    // the user can type a literal `q` into the buffer. Ctrl+C is
    // unaffected (handled by `is_force_quit` above) and remains
    // the universal escape hatch.
    if !model.screen.captures_text_input() && is_quit_attempt(&key) {
        log_quit_with_unresolved_error(&model, "quit");
        model.quit = true;
        return model;
    }
    let outcome = match &mut model.screen {
        AppScreen::MainMenu(state) => {
            let install_ctx = main_menu::InstallContext {
                settings_path: model.install_settings_path.as_deref(),
                install_command: &model.install_command,
            };
            main_menu::update(
                state,
                &model.config,
                &model.theme_registry,
                install_ctx,
                key,
            )
        }
        AppScreen::Placeholder(state) => placeholder::update(state, key),
        AppScreen::ItemsEditor(state) => {
            items_editor::update(state, &mut model.document, &mut model.config, key)
        }
        AppScreen::LinePicker(state) => {
            line_picker::update(state, &mut model.document, &mut model.config, key)
        }
        AppScreen::TypePicker(state) => {
            type_picker::update(state, &mut model.document, &mut model.config, key)
        }
        AppScreen::RawValueEditor(state) => {
            raw_value_editor::update(state, &mut model.document, &mut model.config, key)
        }
        AppScreen::ThemePicker(state) => theme_picker::update(
            state,
            &mut model.document,
            &mut model.config,
            &mut model.theme,
            key,
        ),
        AppScreen::InstallToClaudeCode(state) => install_screen::update(state, key),
    };
    match outcome {
        ScreenOutcome::Stay => {}
        ScreenOutcome::Committed => apply_commit_save(&mut model),
        ScreenOutcome::NavigateTo(screen) => model.screen = screen,
        ScreenOutcome::CommitAndNavigate(screen) => {
            apply_commit_save(&mut model);
            model.screen = screen;
        }
        ScreenOutcome::Quit => {
            // Screen-driven quits (MainMenu Esc, Exit row activation)
            // hit this arm. Mirror the global-q logging so the
            // captured sink preserves the failure trail post-exit
            // regardless of which keybind triggered the quit.
            log_quit_with_unresolved_error(&model, "quit");
            model.quit = true;
        }
    }
    model
}

/// Auto-save after a screen-level commit per ADR-0025. Updates
/// `save_feedback` so the view can render the toast or banner;
/// also emits the macro-channel diagnostic so the captured sink
/// gets the same signal in non-TUI contexts.
///
/// Per-arm semantics:
///
/// - `Saved` → toast (unconditionally replaces any prior None or
///   Error feedback) + `lsm_debug!`. The dedup logic only lives
///   in the `NoTarget` arm.
/// - `Clean` → no-op write (e.g. implicit-default re-pick); silent
///   on the diagnostic channel. If a prior `Error` banner is
///   active, clear it: in-memory state matches our last successful
///   write, so the prior failure is no longer the live state and
///   the banner would mislead. (External-edit collisions are
///   tracked separately — `Model::save` compares against
///   `original_text`, not the current disk bytes.) The toast is
///   suppressed too — nothing was actually saved.
/// - `NoTarget` → persistent banner + `lsm_warn!` (deduplicated
///   against an identical prior banner so repeated commits in a
///   no-target session don't spam the warnings panel).
/// - `Error` → persistent banner + `lsm_error!` with an
///   `ErrorKind`-aware remediation hint (PermissionDenied,
///   NotFound) appended where available.
fn apply_commit_save(model: &mut Model) {
    match model.save() {
        SaveOutcome::Saved(path) => {
            model.save_feedback = SaveFeedback::Saved;
            linesmith_core::lsm_debug!("saved {}", path.display());
        }
        SaveOutcome::Clean => {
            // No bytes to write. If a prior error left a banner
            // showing, in-memory state matches our last successful
            // write — so the prior failure is no longer the live
            // state and the stale banner would mislead. Clear it,
            // logging the resolution so the captured sink records
            // the recovery (the banner disappearing silently from
            // the UI would otherwise leave no trail). No toast,
            // since nothing was newly saved.
            if matches!(model.save_feedback, SaveFeedback::Error(_)) {
                model.save_feedback = SaveFeedback::None;
                linesmith_core::lsm_debug!("cleared stale save error banner");
            }
        }
        SaveOutcome::NoTarget => {
            // Dedup the warn so a no-target session that commits
            // repeatedly doesn't accumulate identical entries; the
            // banner stays as the steady-state signal, the warn
            // fires only on the first emission. Comparing against
            // the named constant makes the dedup invariant explicit
            // — a future banner-text edit must touch this constant,
            // not just an inline string literal somewhere.
            let already_shown = matches!(
                &model.save_feedback,
                SaveFeedback::Error(prior) if prior == NO_TARGET_BANNER,
            );
            if !already_shown {
                linesmith_core::lsm_warn!("{NO_TARGET_BANNER}");
            }
            model.save_feedback = SaveFeedback::Error(NO_TARGET_BANNER.to_string());
        }
        SaveOutcome::Error { path, error } => {
            let hint = save_error_hint(&error);
            let msg = format!("couldn't save to {}: {error}{hint}", path.display());
            linesmith_core::lsm_error!("{msg}");
            model.save_feedback = SaveFeedback::Error(msg);
        }
    }
}

/// User-visible banner when save_target is None. Hoisted to a
/// constant so the dedup check in `apply_commit_save`'s `NoTarget`
/// arm compares against a named invariant rather than an inline
/// runtime-built string — see the comment there.
const NO_TARGET_BANNER: &str =
    "save not available — no config path supplied or file failed to parse at load";

/// Map `PermissionDenied` and `NotFound` to a short remediation
/// hint for appending to the user-facing banner. Other kinds
/// return an empty string so the raw OS message stands alone;
/// extend the match arm when a new kind has actionable advice.
fn save_error_hint(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::PermissionDenied => {
            " (try `chmod` on the parent directory or rerun with a different `--config`)"
        }
        io::ErrorKind::NotFound => {
            " (parent directory missing — create it or rerun with a valid `--config`)"
        }
        _ => "",
    }
}

/// Log policy on quit-with-Error per ADR-0025: three exit paths,
/// two levels.
///
/// - `q` (bare key) and screen-driven `ScreenOutcome::Quit`
///   (MainMenu Esc, Exit row) are clean-intent quits — the user
///   thinks they're done. Both call this helper, which emits
///   `lsm_error!` so monitoring channels weight the unresolved
///   failure appropriately.
/// - Ctrl+C is a user-initiated abandonment and emits `lsm_warn!`
///   inline in the dispatcher (not through this helper) so its
///   log level stays distinguishable from the clean-intent paths.
///
/// All three preserve the failure trail in the captured sink so
/// the alt-screen teardown doesn't lose the banner's context.
fn log_quit_with_unresolved_error(model: &Model, kind: &str) {
    if let SaveFeedback::Error(msg) = &model.save_feedback {
        linesmith_core::lsm_error!("{kind} with unresolved save error: {msg}");
    }
}

/// `q` only. Esc isn't here because Esc has screen-specific
/// semantics — Placeholder's Esc backs out to MainMenu, ListScreen's
/// Esc exits move-mode. MainMenu's Esc returns `ScreenOutcome::Quit`
/// directly. Ctrl+C is split out to [`is_force_quit`] because it
/// quits unconditionally on every screen including text-entry.
fn is_quit_attempt(key: &KeyEvent) -> bool {
    matches!(
        (key.code, key.modifiers),
        (KeyCode::Char('q'), KeyModifiers::NONE),
    )
}

/// Ctrl+C is the universal escape hatch — quits unconditionally.
/// Some terminals deliver Ctrl+Shift+C as `Char('C')` with
/// `CONTROL | SHIFT` set; `contains(CONTROL)` accepts both shapes.
fn is_force_quit(key: &KeyEvent) -> bool {
    matches!(
        (key.code, key.modifiers),
        (KeyCode::Char('c' | 'C'), m) if m.contains(KeyModifiers::CONTROL),
    )
}

/// Ctrl+S — deprecated per ADR-0025; kept for one release as a
/// no-op force-flush plus a one-line warn so muscle-memory presses
/// don't surprise the user.
fn is_save_key(key: &KeyEvent) -> bool {
    matches!(
        (key.code, key.modifiers),
        (KeyCode::Char('s' | 'S'), m) if m.contains(KeyModifiers::CONTROL),
    )
}

/// Render the live-preview header above the active screen, the
/// screen body in the middle, and a one-line keybind footer at the
/// bottom. The preview height equals 2 border rows plus max(1, line
/// count) plus one row per emitted warning plus one row for save
/// feedback (when present), clamped to 16 total so a noisy
/// diagnostic stream can't crowd out the screen below.
pub(super) fn view(model: &Model, frame: &mut Frame) {
    let area = frame.area();

    // The bordered preview block costs 2 columns horizontally;
    // the layout engine needs the *content* width so segments
    // shrink/drop against the surface that actually displays
    // them, not the outer frame width.
    let inner_width = area.width.saturating_sub(2);
    // While the theme picker is active, render the preview header
    // with the cursor's theme rather than the committed one. The
    // user moves up/down and sees each theme's effect on their
    // current segments without first having to commit + re-open.
    // Esc reverts (model.theme is unchanged) and the next render
    // falls back to the committed theme. Enter commits both
    // document state AND model.theme via theme_picker::update.
    let preview_theme: &Theme = match &model.screen {
        AppScreen::ThemePicker(state) => state.cursor_theme(),
        _ => &model.theme,
    };
    let (preview_lines, mut warnings) = preview::render_lines(
        &model.config,
        preview_theme,
        model.capability,
        inner_width,
        model.sink.as_deref(),
    );

    // Prepend environment warnings (NO_COLOR, TTY status, palette
    // tier mismatch, VSCode contrast shim, tmux passthrough) to
    // the runtime warnings panel. These describe the user's
    // terminal setup rather than the current render, so they read
    // first as context for everything below. Env is re-snapshotted
    // per render — cheap (a handful of env lookups, all libc-local)
    // and avoids a Model field for a session-constant. Color policy
    // is read from the parsed config so user overrides
    // (`color = "always"` / `"never"`) suppress ladder warnings
    // that would otherwise misattribute the cause.
    let env_snapshot = EnvironmentSnapshot::from_process();
    let color_policy = model
        .config
        .layout_options
        .as_ref()
        .map_or(config::ColorPolicy::Auto, |lo| lo.color);
    prepend_env_warnings(&mut warnings, model.capability, color_policy, &env_snapshot);

    let feedback_row = match &model.save_feedback {
        SaveFeedback::None => 0u16,
        SaveFeedback::Saved | SaveFeedback::Error(_) => 1u16,
    };

    // Height: 2 border rows + at least 1 content row + 1 row per
    // warning + feedback (capped). Capped at 16 total so a
    // pathological multi-line config can't crowd out the screen
    // below.
    let line_rows = u16::try_from(preview_lines.len().max(1)).unwrap_or(u16::MAX);
    let warn_rows = u16::try_from(warnings.len()).unwrap_or(u16::MAX);
    let preview_height = line_rows
        .saturating_add(warn_rows)
        .saturating_add(feedback_row)
        .saturating_add(2)
        .min(16);

    // Footer keybind hint bar gets a fixed single row at the
    // bottom of the screen per ADR-0025.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(preview_height),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(area);

    render_preview(
        &preview_lines,
        &warnings,
        &model.save_feedback,
        chunks[0],
        frame,
    );

    match &model.screen {
        AppScreen::MainMenu(state) => main_menu::view(state, frame, chunks[1]),
        AppScreen::Placeholder(state) => placeholder::view(state, frame, chunks[1]),
        AppScreen::ItemsEditor(state) => {
            items_editor::view(state, &model.document, frame, chunks[1])
        }
        AppScreen::LinePicker(state) => line_picker::view(state, &model.document, frame, chunks[1]),
        AppScreen::TypePicker(state) => type_picker::view(state, frame, chunks[1]),
        AppScreen::RawValueEditor(state) => raw_value_editor::view(state, frame, chunks[1]),
        AppScreen::ThemePicker(state) => theme_picker::view(state, frame, chunks[1]),
        AppScreen::InstallToClaudeCode(state) => install_screen::view(state, frame, chunks[1]),
    }

    render_footer_hints(&model.screen, frame, chunks[2]);
}

/// One-line keybind hint bar at the bottom of every frame per
/// ADR-0025's discoverability commitment. Hint text is delegated
/// to `AppScreen::footer_hint` because keys carry different
/// semantics per screen (MainMenu Esc quits, text-entry q types
/// a literal character, etc.).
fn render_footer_hints(screen: &AppScreen, frame: &mut Frame, area: ratatui::layout::Rect) {
    if area.height == 0 {
        return;
    }
    let hint = Paragraph::new(Line::styled(
        screen.footer_hint(),
        Style::default().add_modifier(Modifier::DIM),
    ));
    frame.render_widget(hint, area);
}

fn render_preview(
    lines: &[Line<'static>],
    warnings: &[String],
    feedback: &SaveFeedback,
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

    // Vertical layout: lines on top, warnings advisory below in
    // dim italic, save feedback in its own row at the bottom (when
    // present) so a "✓ saved" toast or "✗ ..." error banner is
    // visually distinct from the warnings stream.
    let line_rows = u16::try_from(lines.len().max(1)).unwrap_or(u16::MAX);
    let warn_rows = u16::try_from(warnings.len()).unwrap_or(u16::MAX);
    let feedback_rows = match feedback {
        SaveFeedback::None => 0u16,
        SaveFeedback::Saved | SaveFeedback::Error(_) => 1u16,
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(line_rows),
            Constraint::Length(warn_rows),
            Constraint::Length(feedback_rows),
        ])
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

    match feedback {
        SaveFeedback::None => {}
        SaveFeedback::Saved => {
            let body = Paragraph::new(Line::styled(
                "✓ saved",
                Style::default().add_modifier(Modifier::BOLD),
            ));
            frame.render_widget(body, chunks[2]);
        }
        SaveFeedback::Error(msg) => {
            let body = Paragraph::new(Line::styled(
                format!("✗ {msg}"),
                Style::default().add_modifier(Modifier::BOLD),
            ));
            frame.render_widget(body, chunks[2]);
        }
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
            DocumentMut::new(),
            String::new(),
            None,
            crate::theme::default_theme().clone(),
            ThemeRegistry::with_built_ins(),
            Capability::None,
            None,
            None,
            "linesmith".to_string(),
        )
    }

    /// Build a model loaded from `original` with `save_target` set
    /// to a tempfile path. Mutating `model.document` afterward
    /// puts the document ahead of `original_text` so the next
    /// `Model::save` actually writes something.
    fn model_with_loaded_text(original: &str, save_target: PathBuf) -> Model {
        let document: DocumentMut = original.parse().expect("test text must parse");
        Model::new(
            config::Config::default(),
            document,
            original.to_string(),
            Some(save_target),
            crate::theme::default_theme().clone(),
            ThemeRegistry::with_built_ins(),
            Capability::None,
            None,
            None,
            "linesmith".to_string(),
        )
    }

    #[test]
    fn save_returns_no_target_when_save_target_unset() {
        // Pin the no-target refusal: a model whose `save_target`
        // is None (loaded from defaults with no path, or load
        // parse-error) refuses save so the user gets visible
        // feedback instead of a silent no-op.
        let mut m = model();
        // Fake a dirty state so we'd otherwise try to write.
        m.original_text = "old".to_string();
        let outcome = m.save();
        assert!(matches!(outcome, SaveOutcome::NoTarget));
    }

    #[test]
    fn save_creates_missing_file_even_when_document_unchanged() {
        // The first-run flow (`linesmith config --config new.toml`
        // where new.toml doesn't exist yet) loads with an empty
        // document and an empty `original_text`, so the dirty
        // check would otherwise return false and Ctrl+S would
        // fall through to `SaveOutcome::Clean` — never creating
        // the file the user asked the editor to open. Pin that
        // an unedited save against a missing target still writes,
        // matching the doc on `load_config` ("Save creates the
        // file") and the `SaveOutcome::Clean` doc which carves
        // out this exact case.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("new.toml");
        assert!(!path.exists(), "test setup must start with no file");
        let mut m = Model::new(
            config::Config::default(),
            DocumentMut::new(),
            String::new(),
            Some(path.clone()),
            crate::theme::default_theme().clone(),
            ThemeRegistry::with_built_ins(),
            Capability::None,
            None,
            None,
            "linesmith".to_string(),
        );
        let outcome = m.save();
        assert!(
            matches!(&outcome, SaveOutcome::Saved(p) if p == &path),
            "missing-file save must take the Saved arm, got {outcome:?}",
        );
        assert!(path.exists(), "Saved must have created the file");
        // Idempotent: a second save with no edits and the file
        // now present takes the Clean arm.
        let outcome = m.save();
        assert!(
            matches!(outcome, SaveOutcome::Clean),
            "second save on unchanged document with file present must be Clean, got {outcome:?}",
        );
    }

    #[test]
    fn save_recreates_file_deleted_externally() {
        // Defense-in-depth pin for the same predicate: if a clean
        // load was followed by an external `rm` of the config
        // (cron job, tmp cleanup, user accident), Ctrl+S writes
        // the in-memory document back. Without the existence
        // check in `save`, this would silently no-op and the
        // user's editor would keep showing data they couldn't
        // persist.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path.clone());
        // The helper builds the Model directly; seed the file on
        // disk to model "loaded from disk, then deleted".
        std::fs::write(&path, raw).expect("seed");
        assert_eq!(
            m.document.to_string(),
            m.original_text,
            "loaded model must start with document matching original_text",
        );
        std::fs::remove_file(&path).expect("rm");
        let outcome = m.save();
        assert!(
            matches!(&outcome, SaveOutcome::Saved(p) if p == &path),
            "deleted-target save must recreate, got {outcome:?}",
        );
        let written = std::fs::read_to_string(&path).expect("read");
        assert_eq!(
            written, raw,
            "recreated file must match the loaded document"
        );
    }

    #[test]
    fn save_returns_clean_when_no_edits_and_file_present() {
        // Pin the `Clean` arm's full predicate: document matches
        // `original_text` AND target file exists on disk. Without
        // the file-existence half, a missing target would
        // silently no-op on Ctrl+S even though `load_config`'s
        // doc promises save creates the file — the regression
        // surfaces as "I opened `--config new.toml`, pressed
        // Ctrl+S, and the file never appeared".
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        // Seed the file so save's existence check sees it.
        std::fs::write(&path, raw).expect("seed");
        let metadata_before = std::fs::metadata(&path).expect("stat");
        let mut m = model_with_loaded_text(raw, path.clone());
        let outcome = m.save();
        assert!(matches!(outcome, SaveOutcome::Clean), "got {outcome:?}");
        // Clean must not rewrite the file — preserves the
        // original mtime/inode, no side effect.
        let metadata_after = std::fs::metadata(&path).expect("stat");
        assert_eq!(
            metadata_before.modified().expect("mtime"),
            metadata_after.modified().expect("mtime"),
            "Clean must not touch the file",
        );
    }

    #[test]
    fn save_writes_dirty_document_atomically_and_clears_dirty_flag() {
        // Pin the three-way invariant a successful save must
        // satisfy: the bytes on disk == the stringified in-memory
        // document == the post-save `original_text`. Substring
        // assertions alone would let a regression slip where save
        // wrote `original_text` (the pre-edit bytes) instead of
        // `serialized` — the file would still contain `[line]`
        // and lose `theme`, which is detectable, but a "wrote
        // pre-edit bytes AND updated original_text to match"
        // ordering bug would leave `original_text == serialized`
        // (in-memory invariant satisfied) while losing the edit on
        // disk; only the byte-equality assertion catches that.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path.clone());
        m.document["theme"] = toml_edit::value("dracula");
        let expected_serialized = m.document.to_string();
        let outcome = m.save();
        assert!(
            matches!(&outcome, SaveOutcome::Saved(p) if p == &path),
            "got {outcome:?}",
        );
        assert_eq!(
            m.original_text, expected_serialized,
            "post-save original_text must advance to the just-written bytes",
        );
        let written = std::fs::read_to_string(&path).expect("read");
        assert_eq!(
            written, expected_serialized,
            "on-disk bytes must match in-memory document",
        );
        assert_eq!(
            written, m.original_text,
            "original_text must be the just-written bytes",
        );
    }

    #[test]
    fn save_preserves_comments_and_blank_lines_on_round_trip() {
        // The whole point of toml_edit over toml: editing one key
        // doesn't strip user comments and formatting elsewhere.
        // Pin both.
        let raw = "# top comment\n\n[line]  # inline section comment\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path.clone());
        m.document["theme"] = toml_edit::value("dracula");
        let _ = m.save();
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("# top comment"),
            "lost top comment: {written:?}",
        );
        assert!(
            written.contains("# inline section comment"),
            "lost inline comment: {written:?}",
        );
    }

    #[test]
    fn save_returns_error_when_path_unwritable() {
        // Pin the I/O error path: pointing save_target at a path
        // whose parent isn't writable (a file masquerading as a
        // directory) returns SaveOutcome::Error and leaves the
        // dirty flag set so the user can retry.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        // Create a regular file and try to write into it as if
        // it were a directory — atomic_write's create_dir_all
        // refuses (NotADirectory or similar).
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, "x").expect("write");
        let path = blocker.join("config.toml");
        let raw = "[line]\nsegments = [\"model\"]\n";
        let mut m = model_with_loaded_text(raw, path.clone());
        m.document["theme"] = toml_edit::value("dracula");
        let outcome = m.save();
        assert!(
            matches!(&outcome, SaveOutcome::Error { path: p, .. } if p == &path),
            "got {outcome:?}",
        );
        assert_eq!(
            m.original_text, raw,
            "error path must not advance original_text — that would silently mark the failed write as the new baseline so the next commit wouldn't retry",
        );
    }

    #[test]
    fn apply_commit_save_sets_error_feedback_on_no_target() {
        // Pin the persistent-banner UX from ADR-0025: a save with
        // no target writes the failure message into save_feedback
        // so the preview pane renders the banner until the next
        // successful save replaces it. The captured sink also gets
        // the warn so non-TUI driver paths see the same signal.
        use crate::logging::{self, Level};

        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let mut m = model();
        m.original_text = "old".to_string();
        apply_commit_save(&mut m);

        match &m.save_feedback {
            SaveFeedback::Error(msg) => assert!(
                msg.contains("save not available"),
                "banner must name the failure reason: {msg}",
            ),
            other => panic!("expected Error feedback, got {other:?}"),
        }
        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[warn]") && e.contains("save not available")),
            "captured sink must mirror the banner via lsm_warn! in {entries:?}",
        );
    }

    #[test]
    fn no_target_dedup_emits_warn_only_on_first_emission() {
        // Pin the dedup invariant: a no-target session that commits
        // repeatedly emits the lsm_warn! exactly once, while the
        // banner persists across all subsequent commits. A
        // regression that drops the `prior == NO_TARGET_BANNER`
        // check (or flips the equality) would re-spam the warnings
        // panel on every commit.
        use crate::logging::{self, Level};
        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let mut m = model();
        // Two consecutive NoTarget commits — model() has save_target=None
        // and a non-empty document, so save() returns NoTarget.
        m.original_text = "old".to_string();
        apply_commit_save(&mut m);
        apply_commit_save(&mut m);

        let warn_count = captured
            .drain()
            .iter()
            .filter(|e| e.starts_with("[warn]") && e.contains("save not available"))
            .count();
        assert_eq!(
            warn_count, 1,
            "lsm_warn! must fire exactly once across N consecutive NoTarget commits",
        );
        assert!(
            matches!(m.save_feedback, SaveFeedback::Error(_)),
            "banner must persist across both commits: {:?}",
            m.save_feedback,
        );
    }

    #[test]
    fn save_error_hint_maps_known_kinds_and_falls_through_for_others() {
        // Pin the ErrorKind → remediation mapping. PermissionDenied
        // and NotFound have specific advice users can act on;
        // everything else returns empty so the raw OS message
        // carries the load alone. Catches regressions that swap
        // the arms or drop a hint string.
        use std::io::{Error, ErrorKind};
        let perm = save_error_hint(&Error::from(ErrorKind::PermissionDenied));
        assert!(
            perm.contains("chmod") || perm.contains("--config"),
            "PermissionDenied hint must mention chmod or --config: {perm:?}",
        );
        let nf = save_error_hint(&Error::from(ErrorKind::NotFound));
        assert!(
            nf.contains("parent directory") || nf.contains("--config"),
            "NotFound hint must mention parent directory or --config: {nf:?}",
        );
        let other = save_error_hint(&Error::from(ErrorKind::Other));
        assert_eq!(
            other, "",
            "unmapped kinds must return empty so the raw OS message stands alone: {other:?}",
        );
    }

    #[test]
    fn apply_commit_save_sets_error_feedback_on_io_failure() {
        // I/O failure during save sets a persistent error banner
        // AND fires `lsm_error!` so non-TUI driver paths see the
        // same signal. The in-memory edit is preserved
        // (original_text unchanged) so the next commit retries.
        use crate::logging::{self, Level};

        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, "x").expect("write");
        let path = blocker.join("config.toml");
        let mut m = model_with_loaded_text(raw, path);
        m.document["theme"] = toml_edit::value("dracula");
        apply_commit_save(&mut m);

        match &m.save_feedback {
            SaveFeedback::Error(msg) => assert!(
                msg.contains("couldn't save"),
                "banner must lead with the failure: {msg}",
            ),
            other => panic!("expected Error feedback, got {other:?}"),
        }
        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[error]") && e.contains("couldn't save")),
            "captured sink must mirror the banner via lsm_error! in {entries:?}",
        );
    }

    #[test]
    fn ctrl_s_after_failure_retries_and_lands_saved() {
        // End-to-end retry contract: first commit hits an
        // unwritable parent and surfaces Error; the user fixes the
        // underlying issue (test simulates by swapping the blocker
        // for a real dir) and presses Ctrl+S; the force-flush
        // re-attempts via `apply_commit_save` and flips feedback
        // to `Saved`. Catches a regression where the dispatcher
        // skips apply_commit_save when feedback is Error, or where
        // Model::save's `original_text` advance gets short-
        // circuited on retry.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, "x").expect("seed blocker");
        let path = blocker.join("config.toml");
        let mut m = model_with_loaded_text(raw, path.clone());
        m.document["theme"] = toml_edit::value("dracula");
        // First commit: writes through `apply_commit_save` because
        // the parent isn't a directory → SaveOutcome::Error.
        apply_commit_save(&mut m);
        assert!(
            matches!(m.save_feedback, SaveFeedback::Error(_)),
            "first commit must surface Error",
        );
        // Unblock: replace the file-as-blocker with an actual
        // directory, simulating the user resolving the underlying
        // issue (chmod, mkdir, etc.).
        std::fs::remove_file(&blocker).expect("rm blocker");
        std::fs::create_dir(&blocker).expect("mkdir blocker");
        // Retry via Ctrl+S routes back through apply_commit_save
        // (the deprecation warn is suppressed because Error is
        // active). The write now succeeds → SaveFeedback::Saved.
        let m = update(m, key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(
            matches!(m.save_feedback, SaveFeedback::Saved),
            "Ctrl+S after failure must flip feedback to Saved on success: {:?}",
            m.save_feedback,
        );
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("dracula"),
            "retry must persist the dracula edit: {written}",
        );
    }

    #[test]
    fn apply_commit_save_sets_saved_feedback_on_success() {
        // Successful save sets the transient `Saved` toast and
        // fires `lsm_debug!` so a `LINESMITH_LOG=debug` user sees
        // the confirmation. At default level the macro entry is
        // filtered, but the toast is still rendered by `view`.
        use crate::logging::{self, Level};

        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Debug);

        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path);
        m.document["theme"] = toml_edit::value("dracula");
        apply_commit_save(&mut m);

        assert!(
            matches!(m.save_feedback, SaveFeedback::Saved),
            "successful save must flip feedback to Saved",
        );
        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[debug]") && e.contains("saved")),
            "expected saved debug in {entries:?}",
        );
        logging::set_level(Level::Warn);
    }

    #[test]
    fn successful_commit_after_error_clears_error_banner() {
        // ADR-0025's failure-recovery contract: after a save error,
        // the banner stays until the next successful save replaces
        // it. Pin that state transition so a transient failure
        // (NFS hiccup, brief disk-full) self-heals on the next
        // commit instead of leaving a stale banner forever.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path);
        m.save_feedback = SaveFeedback::Error("simulated prior failure".to_string());
        m.document["theme"] = toml_edit::value("dracula");
        apply_commit_save(&mut m);
        assert!(
            matches!(m.save_feedback, SaveFeedback::Saved),
            "successful save must replace error banner with saved toast",
        );
    }

    #[test]
    fn next_event_clears_saved_toast_but_not_error_banner() {
        // Toast is transient (clears on next event); banner is
        // persistent (stays until cleared by a successful save).
        // Pin both halves so a future refactor can't accidentally
        // collapse them into one lifetime.
        let mut m = model();
        m.save_feedback = SaveFeedback::Saved;
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            matches!(m.save_feedback, SaveFeedback::None),
            "Saved toast must clear on next key event",
        );

        let mut m = model();
        m.save_feedback = SaveFeedback::Error("disk full".to_string());
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        assert!(
            matches!(m.save_feedback, SaveFeedback::Error(_)),
            "Error banner must persist across events: {:?}",
            m.save_feedback,
        );
    }

    #[test]
    fn ctrl_s_emits_deprecation_warn_and_force_flushes() {
        // ADR-0025 keeps Ctrl+S for one release as a deprecated
        // no-op force-flush so muscle-memory presses don't surprise
        // the user. Pin: the deprecation warn fires AND save is
        // attempted (so an out-of-sync document gets caught up).
        use crate::logging::{self, Level};

        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path.clone());
        // Pretend a prior save failed so the document is ahead of
        // disk — the Ctrl+S force-flush should write it.
        m.document["theme"] = toml_edit::value("dracula");
        let _ = update(m, key(KeyCode::Char('s'), KeyModifiers::CONTROL));

        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[warn]") && e.contains("save automatically")),
            "expected deprecation warn in {entries:?}",
        );
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("dracula"),
            "force-flush must write the in-memory document: {written}",
        );
    }

    #[test]
    fn quit_is_unconditional_under_instant_apply() {
        // ADR-0025 drops the dirty-gated ConfirmQuit modal: under
        // instant-apply every screen-level commit already saved,
        // so there's no dirty state at quit time and no need to
        // confirm. Pin the unconditional behavior — `q` quits
        // immediately whether the document was just edited or not.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path);
        m.document["theme"] = toml_edit::value("dracula");
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(
            m.quit,
            "q must quit immediately, no modal under instant-apply",
        );
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
    fn implicit_default_commit_keeps_feedback_none() {
        // The implicit-default no-op (set_theme_in_document skips
        // when name == "default" AND document has no theme key)
        // should produce a Clean SaveOutcome from Model::save. The
        // dispatcher must NOT flash a "Saved" toast for what the
        // user perceives as a no-op. Pin that save_feedback stays
        // None across the full MainMenu → ThemePicker → Enter flow.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, raw).expect("seed");
        let m = model_with_loaded_text(raw, path);
        // EditColors → ThemePicker → Enter on cursor (which is on
        // "default", since config.theme is absent and the picker
        // falls back to the first registered theme).
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(m.save_feedback, SaveFeedback::None),
            "implicit-default commit must NOT set Saved feedback: {:?}",
            m.save_feedback,
        );
    }

    #[test]
    fn clean_commit_clears_prior_error_banner() {
        // ADR-0025 failure-recovery contract: if a prior commit's
        // save failed and left an Error banner, a subsequent no-op
        // (Clean) commit means in-memory state matches our last
        // successful write — the prior failure is no longer the
        // live state and the stale banner must clear. Otherwise
        // the user sees a misleading error after reverting their
        // edit.
        //
        // `model_with_loaded_text` builds a Model whose `save()`
        // returns Clean while the document matches `original_text`
        // and the file exists; that's the trigger we need.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, raw).expect("seed");
        let mut m = model_with_loaded_text(raw, path);
        m.save_feedback = SaveFeedback::Error("simulated prior failure".to_string());
        apply_commit_save(&mut m);
        assert!(
            matches!(m.save_feedback, SaveFeedback::None),
            "Clean must clear stale Error banner: {:?}",
            m.save_feedback,
        );
    }

    #[test]
    fn ctrl_s_suppresses_deprecation_warn_when_save_error_active() {
        // When the user has a stale Error banner (e.g. from a
        // PermissionDenied), Ctrl+S is the genuine recovery path —
        // not a deprecated muscle-memory press. Pin both halves:
        // (a) the "Ctrl+S is no longer needed" warn is suppressed
        // while Error is active, and (b) the force-flush still
        // runs (here it lands on the Clean arm since the doc
        // matches disk; the test's job is to prove the dispatcher
        // didn't skip apply_commit_save entirely).
        use crate::logging::{self, Level};
        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, raw).expect("seed");
        let mut m = model_with_loaded_text(raw, path);
        m.save_feedback = SaveFeedback::Error("prior failure".to_string());
        let m = update(m, key(KeyCode::Char('s'), KeyModifiers::CONTROL));

        let entries = captured.drain();
        assert!(
            !entries
                .iter()
                .any(|e| e.contains("Ctrl+S is no longer needed")),
            "deprecation warn must be suppressed while Error active: {entries:?}",
        );
        // Ctrl+S routed through apply_commit_save and saw Clean
        // (doc matches disk); that cleared the stale banner per
        // the failure-recovery contract.
        assert!(
            matches!(m.save_feedback, SaveFeedback::None),
            "Ctrl+S with Clean SaveOutcome must clear the stale Error banner: {:?}",
            m.save_feedback,
        );
    }

    #[test]
    fn quit_with_unresolved_error_emits_lsm_error_for_post_exit_trail() {
        // Once Ctrl+S is deprecated, the only remediation channel
        // for a save failure is the persistent banner — but quit
        // tears down the alt-screen and the banner disappears.
        // Pin: quitting via `q` with an Error banner active emits
        // a captured-sink `lsm_error!` (intent-was-clean-exit) so
        // the failure trail survives into the post-exit stderr
        // drain.
        use crate::logging::{self, Level};
        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let mut m = model();
        m.save_feedback = SaveFeedback::Error("simulated PermissionDenied".to_string());
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(m.quit, "q must still quit unconditionally");
        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[error]") && e.contains("unresolved save error")),
            "q-with-Error must log lsm_error! to captured sink: {entries:?}",
        );
    }

    #[test]
    fn ctrl_c_with_unresolved_error_emits_lsm_warn_not_lsm_error() {
        // Ctrl+C is a user-initiated abandonment (not "I thought I
        // saved cleanly"), so the post-exit trail logs at
        // `lsm_warn!` — error aggregators shouldn't weight it the
        // same as a clean-exit-with-unsaved-changes.
        use crate::logging::{self, Level};
        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let mut m = model();
        m.save_feedback = SaveFeedback::Error("simulated failure".to_string());
        let m = update(m, key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(m.quit);
        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[warn]")
                    && e.contains("force-quit with unresolved save error")),
            "Ctrl+C-with-Error must log lsm_warn! (not lsm_error!): {entries:?}",
        );
        assert!(
            !entries
                .iter()
                .any(|e| e.starts_with("[error]") && e.contains("unresolved save error")),
            "Ctrl+C path must NOT emit lsm_error! — that's reserved for clean-intent quits: {entries:?}",
        );
    }

    #[test]
    fn screen_driven_quit_with_unresolved_error_also_logs() {
        // The `ScreenOutcome::Quit` arm in the dispatcher must
        // mirror the global-q logging policy. MainMenu's Esc
        // returns `ScreenOutcome::Quit` (not the bare-q path), so
        // without this Esc-quitting with an Error banner would
        // silently lose the trail.
        use crate::logging::{self, Level};
        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let mut m = model();
        m.save_feedback = SaveFeedback::Error("simulated failure".to_string());
        // Esc on MainMenu → main_menu::update returns ScreenOutcome::Quit.
        let m = update(m, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(m.quit, "MainMenu Esc must quit");
        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[error]") && e.contains("unresolved save error")),
            "screen-driven Quit with Error must log to captured sink: {entries:?}",
        );
    }

    #[test]
    fn footer_hint_text_matches_screen_keybind_semantics() {
        // Per ADR-0025 discoverability + Codex round-3 finding: the
        // footer must not advertise keys whose behavior varies by
        // screen. MainMenu's Esc quits (no parent to back-nav to),
        // text-entry screens consume `q` as a literal character.
        // A single one-size-fits-all hint would mislead on at
        // least one screen.
        let main = AppScreen::MainMenu(MainMenuState::default());
        let hint = main.footer_hint();
        assert!(
            hint.contains("[Esc] quit") && !hint.contains("[Esc] back"),
            "MainMenu footer must say Esc quits, not back-navs: {hint}",
        );

        let editor = AppScreen::RawValueEditor(super::raw_value_editor::RawValueEditorState::new(
            String::new(),
            0,
            super::raw_value_editor::RawTarget::SegmentId,
            ItemsEditorState::default(),
        ));
        let hint = editor.footer_hint();
        assert!(
            !hint.contains("[q] quit"),
            "RawValueEditor footer must NOT advertise [q] quit — q is a literal character: {hint}",
        );
        assert!(
            hint.contains("Ctrl+C"),
            "RawValueEditor footer must surface Ctrl+C as the universal escape: {hint}",
        );

        let items = AppScreen::ItemsEditor(ItemsEditorState::default());
        let hint = items.footer_hint();
        assert!(
            hint.contains("[Esc] back") && hint.contains("[q] quit"),
            "ItemsEditor footer must advertise Esc-back + q-quit: {hint}",
        );
    }

    #[test]
    fn resize_event_preserves_saved_toast() {
        // ADR-0025 contract: Resize "deliberately doesn't clear
        // `save_feedback` so an in-flight toast survives terminal-
        // resize redraws." A regression that mirrors the key-event
        // toast-clear into the Resize path would silently strip
        // the user's confirmation on the next redraw.
        let mut m = model();
        m.save_feedback = SaveFeedback::Saved;
        let m = update(m, Event::Resize);
        assert!(
            matches!(m.save_feedback, SaveFeedback::Saved),
            "Resize must NOT clear the Saved toast: {:?}",
            m.save_feedback,
        );
    }

    #[test]
    fn theme_picker_cursor_diverges_from_committed_theme_on_navigation() {
        // Unit-level pin for the live-preview override's data half:
        // while the ThemePicker is active, `state.cursor_theme()`
        // returns a different theme from `model.theme` after the
        // user navigates away from the committed selection. This
        // is what `app::view`'s preview branch reads, but this
        // test does NOT assert that `view` actually consumes it —
        // it only pins that the picker's snapshot tracks the cursor
        // independently of `model.theme`. The end-to-end regression
        // (`view` drops the match arm and renders the committed
        // theme despite picker state) needs an integration test
        // against rendered output (TestBackend buffer assertion).
        let m = update(model(), key(KeyCode::Down, KeyModifiers::NONE)); // EditColors
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE)); // → ThemePicker
        match &m.screen {
            AppScreen::ThemePicker(_) => {}
            other => panic!("expected ThemePicker, got {other:?}"),
        }
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        let cursor_theme_name = match &m.screen {
            AppScreen::ThemePicker(state) => state.cursor_theme().name().to_string(),
            other => panic!("expected ThemePicker, got {other:?}"),
        };
        assert_ne!(
            cursor_theme_name,
            m.theme.name(),
            "cursor must move off the committed theme on Down",
        );
    }

    #[test]
    fn enter_on_main_menu_navigates_to_placeholder() {
        // Pin the dispatch chain: top-level update → screen
        // update → NavigateTo application. Walks past EditLines
        // (ItemsEditor) and EditColors (ThemePicker) to a row that
        // still uses Placeholder, so this test stays focused on the
        // dispatch chain rather than which screen variant a specific
        // row happens to open.
        let m = update(model(), key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
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
        // Walks past EditLines (ItemsEditor) and EditColors
        // (ThemePicker) to PowerlineSetup which still placeholders.
        let m = update(model(), key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::Placeholder(_)));
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(m.quit);
    }

    #[test]
    fn items_editor_swap_through_app_dispatch_mutates_document_and_config() {
        // Pins the full dispatch chain for the ItemsEditor variant:
        // top-level `update` → `items_editor::update` → document
        // mutation → `refresh_config`. A regression that omits the
        // new match arm or wires it to the wrong state would only
        // fail items_editor's in-module tests (which call its
        // `update` directly); this catches the chain.
        let raw = "[line]\nsegments = [\"a\", \"b\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let m = model_with_loaded_text(raw, path);
        // Default cursor=0 = EditLines → ItemsEditor.
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::ItemsEditor(_)));
        // Enter toggles move-mode; ↓ requests the swap.
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        let line = m.config.line.clone().expect("line config reparsed");
        let ids: Vec<&str> = line
            .segments
            .iter()
            .filter_map(linesmith_core::config::LineEntry::segment_id)
            .collect();
        assert_eq!(ids, vec!["b", "a"]);
        let written = m.document.to_string();
        assert!(
            written.contains("\"b\"") && written.contains("\"a\""),
            "document should retain both segments: {written}",
        );
    }

    #[test]
    fn q_keypress_on_raw_value_editor_inserts_text_does_not_quit() {
        // The bare-`q` quit shortcut must be suppressed on text-
        // entry screens so the user can type `q` into the buffer.
        // Pin: open raw editor via 'r', press 'q', observe no
        // quit and the buffer mutated.
        let raw = "[line]\nsegments = [\"alpha\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let m = model_with_loaded_text(raw, path);
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::RawValueEditor(_)));
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!m.quit, "q on text-entry screen must not quit");
        assert!(matches!(m.screen, AppScreen::RawValueEditor(_)));
        // Commit and assert the buffer landed in the document.
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let line = m.config.line.clone().expect("line reparsed");
        let ids: Vec<&str> = line
            .segments
            .iter()
            .filter_map(linesmith_core::config::LineEntry::segment_id)
            .collect();
        assert_eq!(ids, vec!["alphaq"]);
    }

    #[test]
    fn ctrl_s_on_raw_value_editor_suppresses_save_and_warns() {
        // Real data-loss bug if not suppressed: the raw editor
        // keeps pending edits in its own buffer until Enter, so
        // `model.save()` would write the pre-edit document and
        // silently drop the user's visible change. Pin: Ctrl+S on
        // the editor leaves the document untouched, the editor
        // active, and surfaces a warn telling the user to commit
        // first.
        use crate::logging::{self, Level};
        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let raw = "[line]\nsegments = [\"alpha\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let m = model_with_loaded_text(raw, path.clone());
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::RawValueEditor(_)));
        let m = update(m, key(KeyCode::Char('x'), KeyModifiers::NONE));
        let pre_save_doc = m.document.to_string();
        let m = update(m, key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        // Editor still active; document unchanged; no file written.
        assert!(
            matches!(m.screen, AppScreen::RawValueEditor(_)),
            "Ctrl+S must not navigate away from the text-entry screen",
        );
        assert_eq!(
            m.document.to_string(),
            pre_save_doc,
            "document must not change — buffer hasn't been committed",
        );
        assert!(
            !path.exists(),
            "Ctrl+S on a text-entry screen must not write to disk",
        );
        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[warn]") && e.contains("commit")),
            "expected commit-warning in {entries:?}",
        );
    }

    #[test]
    fn raw_verb_on_default_segment_seeds_with_runtime_default() {
        // When `[line].segments` is absent, segment_count falls
        // back to DEFAULT_SEGMENT_IDS so the user sees the runtime
        // defaults. The raw editor must seed from the matching
        // default — pressing `r` on row 0 of a fresh config seeds
        // with "model" (the first default), and Enter-without-
        // typing preserves it after the materialize-on-first-edit
        // path commits the explicit array. A regression that drops
        // the runtime-defaults fallback in `open_raw_value_editor`
        // would silently surface as an empty seed and erase the
        // default on commit.
        let raw = "";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let m = model_with_loaded_text(raw, path);
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::RawValueEditor(_)));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let line = m.config.line.clone().expect("line reparsed");
        let first_id = line.segments[0]
            .segment_id()
            .expect("first entry is a segment id");
        assert_eq!(
            first_id, "model",
            "first runtime default must round-trip through r → Enter",
        );
        assert_eq!(line.segments.len(), 6);
    }

    #[test]
    fn raw_verb_preserves_literal_non_string_string_id() {
        // A real segment ID literally equal to "<non-string>" is
        // valid TOML and must round-trip through the raw editor
        // — the synthetic placeholder check has to inspect the
        // TOML value's type, not the rendered label, or this
        // string gets erased on Enter-without-typing.
        let raw = "[line]\nsegments = [\"<non-string>\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let m = model_with_loaded_text(raw, path);
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::RawValueEditor(_)));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let line = m.config.line.clone().expect("line reparsed");
        let ids: Vec<&str> = line
            .segments
            .iter()
            .filter_map(linesmith_core::config::LineEntry::segment_id)
            .collect();
        assert_eq!(
            ids,
            vec!["<non-string>"],
            "literal '<non-string>' must round-trip; placeholder check must inspect TOML type",
        );
    }

    #[test]
    fn raw_verb_on_non_string_entry_seeds_empty_buffer_through_dispatch() {
        // Pin the empty-seed contract end-to-end: a non-string
        // segment renders as `<non-string>` in segment_labels but
        // the raw editor must NOT inherit that placeholder as the
        // seed — otherwise pressing Enter without typing would
        // commit the literal "<non-string>" as a segment ID.
        // Verifies via Enter-without-typing → commit lands empty.
        let raw = "[line]\nsegments = [\"a\", 42]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let m = model_with_loaded_text(raw, path);
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Char('r'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::RawValueEditor(_)));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let line = m.config.line.clone().expect("line reparsed");
        let ids: Vec<&str> = line
            .segments
            .iter()
            .filter_map(linesmith_core::config::LineEntry::segment_id)
            .collect();
        assert_eq!(
            ids,
            vec!["a", ""],
            "non-string entry replaced with empty seed",
        );
    }

    #[test]
    fn add_verb_through_app_dispatch_opens_picker_and_inserts_on_enter() {
        // End-to-end pin for the new `AppScreen::TypePicker` arm:
        // top-level update → items_editor::update → TypePicker
        // → type_picker::update → items_editor::apply_insert →
        // ItemsEditor (with new entry). A regression that omits
        // either dispatch arm (update or view) only fails at
        // runtime; this catches the chain.
        let raw = "[line]\nsegments = [\"a\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let m = model_with_loaded_text(raw, path);
        // Activate EditLines → ItemsEditor.
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::ItemsEditor(_)));
        // 'a' opens the picker.
        let m = update(m, key(KeyCode::Char('a'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::TypePicker(_)));
        // Enter selects the first candidate ("model" by default
        // ordering of `DEFAULT_SEGMENT_IDS`).
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::ItemsEditor(_)));
        let line = m.config.line.clone().expect("line reparsed");
        // After(0) inserts at index 1 → ["a", picked].
        assert_eq!(line.segments.len(), 2);
        assert_eq!(line.segments[0].segment_id(), Some("a"));
    }

    #[test]
    fn items_editor_swap_auto_saves_without_ctrl_s() {
        // Pin the load-bearing instant-apply contract: an in-screen
        // commit (here, items editor's MoveSwap from move-mode +
        // Down) auto-saves to disk without the user touching Ctrl+S.
        // A regression that drops the dispatcher's `Committed` →
        // `apply_commit_save` wiring would leave the file ahead of
        // memory until quit, defeating the ADR's whole point.
        let raw = "[line]\nsegments = [\"a\", \"b\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let m = model_with_loaded_text(raw, path.clone());
        // EditLines → ItemsEditor → Enter (move-mode) → Down (swap).
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("\"b\", \"a\"") || written.contains("\"b\",\"a\""),
            "swap must auto-save to disk; got: {written:?}",
        );
        assert!(
            matches!(m.save_feedback, SaveFeedback::Saved),
            "save_feedback must flip to Saved after the auto-save",
        );
    }

    #[test]
    fn esc_on_placeholder_returns_to_main_menu() {
        // Activate from MainMenu to land on Placeholder, then Esc
        // navigates back. Pins both the screen restoration and the
        // top-level Esc handling (Esc must reach the screen's
        // update — `is_unconditional_quit` rejects it). Walks past
        // EditLines and EditColors to a row that still uses
        // Placeholder (PowerlineSetup, row 2).
        let m = update(model(), key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::Placeholder(_)));
        let m = update(m, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!m.quit);
        assert!(matches!(m.screen, AppScreen::MainMenu(_)));
    }
}
