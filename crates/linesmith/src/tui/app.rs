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
use crate::theme::{Capability, Theme};

use super::items_editor::{self, ItemsEditorState};
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
    ConfirmQuit(ConfirmQuitState),
    ItemsEditor(ItemsEditorState),
}

/// State for the modal "you have unsaved changes" prompt that
/// gates the quit path when [`Model::is_dirty`] is true. Holds the
/// screen to return to on cancel — boxed so `AppScreen` doesn't
/// grow unboundedly via self-reference.
#[derive(Debug)]
pub(super) struct ConfirmQuitState {
    pub(super) prior: Box<AppScreen>,
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
    /// Mutable TOML document the editor mutates and Ctrl+S writes
    /// back. Edit screens (lsm-herx.7+ items editor, format-enum
    /// editor, etc.) take `&mut model.document` and apply scoped
    /// edits; `is_dirty()` compares the stringified form against
    /// `original_text` to gate the save / confirm-on-quit prompts.
    pub(super) document: DocumentMut,
    /// Exact bytes the boot path read from disk (or empty when no
    /// file existed / no path was provided). The dirty-check
    /// stringifies `document` and compares to this; matching means
    /// no edits, so save is a no-op and quit doesn't prompt.
    pub(super) original_text: String,
    /// Where Ctrl+S writes. `None` means save is refused — either
    /// the user didn't supply a config path at all, or the file
    /// existed but parse-failed and overwriting it would clobber
    /// the user's broken-but-present TOML with defaults.
    pub(super) save_target: Option<PathBuf>,
    pub(super) theme: Theme,
    pub(super) capability: Capability,
    /// Process-wide log sink the boot path swapped in. The view
    /// passes a borrow to `preview::render_lines`, which drains
    /// macro emissions into the warnings vec so they paint into
    /// the warnings panel instead of corrupting the alt-screen.
    /// `None` for unit tests that bypass the boot path.
    pub(super) sink: Option<Arc<CapturedSink>>,
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
    pub(super) fn new(
        config: config::Config,
        document: DocumentMut,
        original_text: String,
        save_target: Option<PathBuf>,
        theme: Theme,
        capability: Capability,
        sink: Option<Arc<CapturedSink>>,
    ) -> Self {
        Self {
            screen: AppScreen::MainMenu(MainMenuState::default()),
            config,
            document,
            original_text,
            save_target,
            theme,
            capability,
            sink,
            quit: false,
        }
    }

    /// `true` when the in-memory document has diverged from what
    /// was loaded. Implemented as a stringify-diff against
    /// `original_text` because `toml_edit` doesn't expose a
    /// cheaper change-tracking handle, and configs are small
    /// enough (low KB) that the per-call cost is irrelevant
    /// compared to the cost of a missed-edit silent data loss
    /// from a flag-based approach.
    ///
    /// Called only on quit-attempt and save-attempt today; not on
    /// every keystroke.
    #[must_use]
    pub(super) fn is_dirty(&self) -> bool {
        self.document.to_string() != self.original_text
    }

    /// Persist the in-memory document to `save_target` via atomic
    /// rename. Returns a [`SaveOutcome`] describing what happened
    /// so the caller can emit appropriate diagnostics — success is
    /// quiet by design (the user pressed Ctrl+S; no news is good
    /// news), refused saves and I/O errors fire through the
    /// `lsm_warn!` / `lsm_error!` macros into the warnings panel.
    ///
    /// Writes whenever the in-memory document doesn't match
    /// what's on disk: either dirty (document differs from
    /// `original_text`) OR the target file doesn't exist yet
    /// (the documented "first-run / create-a-config" flow on a
    /// `--config new.toml` invocation). The latter case lets
    /// Ctrl+S create an empty file from a fresh editor session
    /// without requiring a sentinel mutation first; it also
    /// rescues a config that was deleted externally between boot
    /// and save.
    ///
    /// On `Saved`, `original_text` is updated to the just-written
    /// stringified document so [`is_dirty`] returns `false` until
    /// the next edit. On `Error`, the dirty flag stays set so the
    /// user can retry.
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

/// Pure state transition. Save (Ctrl+S) and unconditional-quit
/// keys (`q`, Ctrl+C) are handled at the top level regardless of
/// which screen is active; everything else routes to the screen's
/// own `update`, whose [`ScreenOutcome`] the caller applies to
/// `Model`.
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
    // Ctrl+S is global: any screen, including the ConfirmQuit
    // modal. Save-from-modal lets the user dismiss the prompt
    // afterward via 'n' since the dirty flag is now false.
    if is_save_key(&key) {
        apply_save(&mut model);
        return model;
    }
    // Ctrl+C always quits, even from ConfirmQuit — the user
    // explicitly asked for the universal escape hatch and we
    // shouldn't second-guess.
    if is_force_quit(&key) {
        model.quit = true;
        return model;
    }
    if is_quit_attempt(&key) {
        return apply_quit(model);
    }
    let outcome = match &mut model.screen {
        AppScreen::MainMenu(state) => main_menu::update(state, &model.config, key),
        AppScreen::Placeholder(state) => placeholder::update(state, key),
        AppScreen::ConfirmQuit(state) => confirm_quit_update(state, key),
        AppScreen::ItemsEditor(state) => {
            items_editor::update(state, &mut model.document, &mut model.config, key)
        }
    };
    match outcome {
        ScreenOutcome::Stay => {}
        ScreenOutcome::NavigateTo(screen) => model.screen = screen,
        ScreenOutcome::Quit => return apply_quit(model),
    }
    model
}

/// Ctrl+S — save and emit diagnostics through the macro channel
/// so they surface in the warnings panel via the captured sink.
/// Per-arm visibility:
///   - `Saved` → `lsm_debug!` (silent at default `LINESMITH_LOG=warn`)
///   - `Clean` → silent (Ctrl+S spam shouldn't litter the panel)
///   - `NoTarget` → `lsm_warn!` (visible by default)
///   - `Error` → `lsm_error!` (always visible, even at `off`)
fn apply_save(model: &mut Model) {
    match model.save() {
        SaveOutcome::Saved(path) => {
            linesmith_core::lsm_debug!("saved {}", path.display());
        }
        SaveOutcome::Clean => {
            // No edits — silent. Surfacing "nothing to save"
            // every Ctrl+S would litter the warnings panel.
        }
        SaveOutcome::NoTarget => {
            linesmith_core::lsm_warn!(
                "save not available: no config path provided or file failed to parse on load",
            );
        }
        SaveOutcome::Error { path, error } => {
            linesmith_core::lsm_error!("save failed for {}: {error}", path.display());
        }
    }
}

/// Apply a quit signal. Quits immediately when the document is
/// clean OR the user is already on the ConfirmQuit modal (whose
/// `y`/Enter arm returns `ScreenOutcome::Quit` and re-enters here
/// — we trust the modal's confirmation and skip the dirty
/// re-check). Otherwise navigates to the modal, stashing the
/// prior screen for the cancel path.
#[must_use]
fn apply_quit(mut model: Model) -> Model {
    if matches!(model.screen, AppScreen::ConfirmQuit(_)) || !model.is_dirty() {
        model.quit = true;
        return model;
    }
    let prior = std::mem::replace(
        &mut model.screen,
        AppScreen::MainMenu(MainMenuState::default()),
    );
    model.screen = AppScreen::ConfirmQuit(ConfirmQuitState {
        prior: Box::new(prior),
    });
    model
}

/// `q` only. Esc isn't here because Esc has screen-specific
/// semantics — Placeholder's Esc backs out to MainMenu, ListScreen's
/// Esc exits move-mode. MainMenu's Esc means "quit" and reaches
/// `apply_quit` via its `ScreenOutcome::Quit` return, not via this
/// matcher. Ctrl+C is split out to [`is_force_quit`] because it
/// must quit even from ConfirmQuit.
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

/// Ctrl+S, the canonical save shortcut.
fn is_save_key(key: &KeyEvent) -> bool {
    matches!(
        (key.code, key.modifiers),
        (KeyCode::Char('s' | 'S'), m) if m.contains(KeyModifiers::CONTROL),
    )
}

/// Modal update. `y`/`Y`/Enter confirms the quit; `n`/`N`/Esc
/// cancels and returns to the prior screen. Everything else
/// stays on the modal so a stray keypress can't silently dismiss
/// the prompt. `q` is caught upstream by `is_quit_attempt` and
/// re-routed through `apply_quit`, whose `ConfirmQuit` short-
/// circuit treats it as "quit anyway".
fn confirm_quit_update(state: &mut ConfirmQuitState, key: KeyEvent) -> ScreenOutcome {
    if key.modifiers != KeyModifiers::NONE {
        return ScreenOutcome::Stay;
    }
    match key.code {
        KeyCode::Char('y' | 'Y') | KeyCode::Enter => ScreenOutcome::Quit,
        KeyCode::Char('n' | 'N') | KeyCode::Esc => ScreenOutcome::NavigateTo(std::mem::replace(
            state.prior.as_mut(),
            AppScreen::MainMenu(MainMenuState::default()),
        )),
        _ => ScreenOutcome::Stay,
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
    let (preview_lines, warnings) = preview::render_lines(
        &model.config,
        &model.theme,
        model.capability,
        inner_width,
        model.sink.as_deref(),
    );

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
        AppScreen::ConfirmQuit(_) => render_confirm_quit(frame, chunks[1]),
        AppScreen::ItemsEditor(state) => {
            items_editor::view(state, &model.document, frame, chunks[1])
        }
    }
}

/// Render the unsaved-changes modal. Centered prompt with the
/// title, a one-line body, and a key-hint footer. No state needed
/// in the body itself (the prior screen lives on the state struct
/// only for the `n`/Esc cancel path).
fn render_confirm_quit(frame: &mut Frame, area: ratatui::layout::Rect) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " unsaved changes ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    let body = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::raw("  You have unsaved changes. Quit anyway?")),
        Line::from(""),
        Line::from(Span::styled(
            "  [y]/[q] discard and quit    [n]/Esc cancel    [Ctrl+S] save",
            Style::default().add_modifier(Modifier::DIM),
        )),
    ]);
    frame.render_widget(body, inner);
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
            DocumentMut::new(),
            String::new(),
            None,
            crate::theme::default_theme().clone(),
            Capability::None,
            None,
        )
    }

    /// Build a model loaded from `original` with `save_target` set
    /// to a tempfile path. Mutating `model.document` afterward
    /// flips `is_dirty()` true and lets `Model::save` actually
    /// write something.
    fn model_with_loaded_text(original: &str, save_target: PathBuf) -> Model {
        let document: DocumentMut = original.parse().expect("test text must parse");
        Model::new(
            config::Config::default(),
            document,
            original.to_string(),
            Some(save_target),
            crate::theme::default_theme().clone(),
            Capability::None,
            None,
        )
    }

    #[test]
    fn is_dirty_false_when_document_matches_original_text() {
        // Without this, the confirm-on-quit modal would fire on
        // every quit attempt regardless of whether the user edited
        // anything.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let m = model_with_loaded_text(raw, PathBuf::from("/tmp/ignored"));
        assert!(!m.is_dirty(), "untouched doc must report clean");
    }

    #[test]
    fn is_dirty_true_after_document_mutation() {
        // Inserting a key the original text doesn't have
        // guarantees toml_edit's stringification differs from
        // `original_text` — using a table-internal mutation
        // (e.g. `document["line"]["segments"]`) could produce
        // byte-identical output for an idempotent assignment and
        // wouldn't pin the contract.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let mut m = model_with_loaded_text(raw, PathBuf::from("/tmp/ignored"));
        m.document["theme"] = toml_edit::value("dracula");
        assert!(m.is_dirty(), "mutation must flip dirty true");
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
            Capability::None,
            None,
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
        assert!(!m.is_dirty(), "loaded model must start clean");
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
        // ordering bug would leave is_dirty() false while losing
        // the edit on disk; only the byte-equality assertion
        // catches that.
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
        assert!(!m.is_dirty(), "post-save dirty flag must be false");
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
        assert!(m.is_dirty(), "error path must leave dirty flag set");
        assert_eq!(
            m.original_text, raw,
            "error path must not advance original_text — that would silently mark the failed write as the new baseline",
        );
    }

    #[test]
    fn apply_save_emits_warn_when_save_target_unset() {
        // Captured-sink pin: the NoTarget arm fires `lsm_warn!`
        // so the user sees "save not available" in the warnings
        // panel. A regression that swapped the macro for
        // `lsm_debug!` would suppress the warning at default
        // level and leave the user wondering why Ctrl+S did
        // nothing.
        use crate::logging::{self, Level};

        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let mut m = model();
        // Force a "would-write" state so save() doesn't no-op via
        // the Clean arm.
        m.original_text = "old".to_string();
        apply_save(&mut m);

        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[warn]") && e.contains("save not available")),
            "expected NoTarget warn in {entries:?}",
        );
    }

    #[test]
    fn apply_save_emits_error_when_atomic_write_fails() {
        // Pin the error path: I/O failure during save fires
        // `lsm_error!`, not a silent drop. Without this, a save
        // that hits a permission-denied or disk-full would look
        // identical to a successful one (silent at default level)
        // and the user would lose data without warning.
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
        apply_save(&mut m);

        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[error]") && e.contains("save failed")),
            "expected save-failed error in {entries:?}",
        );
    }

    #[test]
    fn apply_save_emits_debug_on_success() {
        // Pin the success-feedback contract: `Saved` fires at
        // `lsm_debug!` so a `LINESMITH_LOG=debug` user sees the
        // confirmation. At default level the entry is filtered
        // before reaching the sink, which is the documented
        // "silent on success" UX.
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
        apply_save(&mut m);

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
    fn ctrl_s_from_confirm_quit_modal_saves_without_dismissing_modal() {
        // The diff's `update` doc and the in-code comment promise
        // "Ctrl+S is global: any screen, including the ConfirmQuit
        // modal." Pin that contract so a future refactor that
        // moves Ctrl+S handling into per-screen update arms breaks
        // this test before the user notices their save key stopped
        // working from the prompt.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path.clone());
        m.document["theme"] = toml_edit::value("dracula");
        // Open the modal.
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::ConfirmQuit(_)));
        // Ctrl+S should save AND keep us on the modal so the user
        // can still cancel out via 'n'/Esc.
        let m = update(m, key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(!m.is_dirty(), "Ctrl+S from modal must save");
        assert!(
            matches!(m.screen, AppScreen::ConfirmQuit(_)),
            "modal must persist after save",
        );
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.contains("theme"));
    }

    #[test]
    fn ctrl_s_on_main_menu_routes_through_save() {
        // Wire-up pin: pressing Ctrl+S as a global key triggers
        // Model::save (visible here as the file write) regardless
        // of which screen is active. A future refactor that scopes
        // Ctrl+S to a specific screen would break this test before
        // the user notices their data isn't persisted.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path.clone());
        m.document["theme"] = toml_edit::value("dracula");
        m = update(m, key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(!m.is_dirty(), "post-Ctrl+S dirty flag must be false");
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(written.contains("theme"));
    }

    #[test]
    fn quit_attempt_when_clean_quits_immediately() {
        // Pin: quit attempts on a clean document don't trigger
        // the modal. Without this, every quit would prompt and
        // the user would have to dismiss every time.
        let m = model();
        assert!(!m.is_dirty());
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(m.quit, "clean quit must commit immediately");
    }

    #[test]
    fn quit_attempt_when_dirty_navigates_to_confirm_modal() {
        // Pin the dirty-quit gate: q with edits pending opens
        // the ConfirmQuit modal instead of quitting outright.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path);
        m.document["theme"] = toml_edit::value("dracula");
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(!m.quit, "dirty quit must defer through modal");
        assert!(matches!(m.screen, AppScreen::ConfirmQuit(_)));
    }

    #[test]
    fn confirm_quit_y_quits_and_n_returns_to_prior_screen_with_state_preserved() {
        // Pin both modal exits AND that cancel restores the
        // EXACT prior screen state — not a fresh
        // `MainMenuState::default()`. A regression that built the
        // restore as `model.screen = AppScreen::MainMenu(...)`
        // (default) would silently reset cursor position to row 0.
        //
        // The cursor is private to the `main_menu` module, so the
        // assertion goes through observable behavior: move the
        // cursor to row 2 (Powerline Setup), open the modal,
        // cancel, then activate. If the restore preserved cursor
        // position the next Enter opens the Powerline placeholder;
        // if it reset to row 0 we'd get the Edit Lines placeholder
        // instead.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path);
        m.document["theme"] = toml_edit::value("dracula");
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        // Open the modal.
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::ConfirmQuit(_)));
        // 'n' returns to MainMenu — cursor must round-trip.
        let m = update(m, key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::MainMenu(_)));
        assert!(!m.quit);
        // Activate the (hopefully preserved) cursor row. Row 2 is
        // Powerline Setup; row 0 would be Edit Lines.
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        match &m.screen {
            AppScreen::Placeholder(p) => assert_eq!(
                p.name, "Powerline Setup",
                "cancel must restore cursor at row 2; got placeholder for {:?} instead",
                p.name,
            ),
            other => panic!("expected Placeholder after activate, got {other:?}"),
        }
        // Re-open the modal from the placeholder via 'q' and
        // confirm with 'y' — the modal-from-non-MainMenu path
        // exercises the prior-screen capture for a different
        // screen type.
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::ConfirmQuit(_)));
        let m = update(m, key(KeyCode::Char('y'), KeyModifiers::NONE));
        assert!(m.quit, "y must commit the quit");
    }

    #[test]
    fn confirm_quit_esc_cancels_back_to_prior_screen() {
        // Pin the standard modal-cancel idiom: Esc returns to the
        // prior screen without quitting. The render_confirm_quit
        // hint promises "[n]/Esc cancel"; without this assertion,
        // a future contributor refactoring the keymap could
        // silently strip Esc and leave the user stuck on the modal.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path);
        m.document["theme"] = toml_edit::value("dracula");
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::ConfirmQuit(_)));
        let m = update(m, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::MainMenu(_)));
        assert!(!m.quit);
    }

    #[test]
    fn ctrl_c_force_quits_from_confirm_modal_bypassing_dirty_check() {
        // Pin the universal escape hatch: Ctrl+C quits even from
        // ConfirmQuit, even when dirty. The user explicitly chose
        // the escape hatch; we don't second-guess.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path);
        m.document["theme"] = toml_edit::value("dracula");
        let m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::ConfirmQuit(_)));
        let m = update(m, key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(m.quit, "Ctrl+C from modal must force quit");
    }

    #[test]
    fn confirm_quit_ignores_unrelated_keys() {
        // Modal must absorb stray keypresses so a typo can't
        // silently dismiss the prompt. Pin Down, F12, and Tab
        // all stay on the modal without quitting.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let mut m = model_with_loaded_text(raw, path);
        m.document["theme"] = toml_edit::value("dracula");
        let mut m = update(m, key(KeyCode::Char('q'), KeyModifiers::NONE));
        for stray in [
            key(KeyCode::Down, KeyModifiers::NONE),
            key(KeyCode::F(12), KeyModifiers::NONE),
            key(KeyCode::Tab, KeyModifiers::NONE),
        ] {
            m = update(m, stray);
            assert!(matches!(m.screen, AppScreen::ConfirmQuit(_)));
            assert!(!m.quit);
        }
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
        // update → NavigateTo application. Walks past EditLines
        // (now routed to ItemsEditor) to a row that still uses
        // Placeholder, so this test stays focused on the dispatch
        // chain rather than which screen variant a specific row
        // happens to open.
        let m = update(model(), key(KeyCode::Down, KeyModifiers::NONE));
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
        let m = update(model(), key(KeyCode::Down, KeyModifiers::NONE));
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
        assert_eq!(line.segments, vec!["b".to_string(), "a".to_string()]);
        let written = m.document.to_string();
        assert!(
            written.contains("\"b\"") && written.contains("\"a\""),
            "document should retain both segments: {written}",
        );
    }

    #[test]
    fn ctrl_s_from_items_editor_persists_swap_to_disk() {
        // The global Ctrl+S handler in `update` runs before screen
        // dispatch, so it should work from any screen. ItemsEditor
        // is the first screen that actually mutates `document`, so
        // the edit→save→clear pipeline is load-bearing here in a
        // way it isn't from MainMenu.
        let raw = "[line]\nsegments = [\"a\", \"b\"]\n";
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        let m = model_with_loaded_text(raw, path.clone());
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Down, KeyModifiers::NONE));
        assert!(m.is_dirty(), "swap should flip dirty true");
        let m = update(m, key(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(!m.is_dirty(), "Ctrl+S from items editor should clear dirty");
        let written = std::fs::read_to_string(&path).expect("read");
        assert!(
            written.contains("\"b\", \"a\"") || written.contains("\"b\",\"a\""),
            "saved file should reflect swap: {written:?}",
        );
    }

    #[test]
    fn esc_on_placeholder_returns_to_main_menu() {
        // Activate from MainMenu to land on Placeholder, then Esc
        // navigates back. Pins both the screen restoration and the
        // top-level Esc handling (Esc must reach the screen's
        // update — `is_unconditional_quit` rejects it).
        let m = update(model(), key(KeyCode::Down, KeyModifiers::NONE));
        let m = update(m, key(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(m.screen, AppScreen::Placeholder(_)));
        let m = update(m, key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!m.quit);
        assert!(matches!(m.screen, AppScreen::MainMenu(_)));
    }
}
