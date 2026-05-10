//! Theme picker screen per ADR-0016.
//!
//! Lists every theme registered at boot (built-ins + user-discovered)
//! so the user can switch the active theme without hand-editing
//! `theme = "..."` in TOML. Live preview override: while the picker
//! is active, `app::view` renders the preview header with the
//! cursor's theme rather than the committed `model.theme`, so moving
//! up/down shows what each theme actually looks like at the user's
//! current segments.
//!
//! Verbs:
//! - `Enter` commits the cursor's theme to `[theme]` in the document
//!   AND swaps `model.theme` so the next render after Esc-back uses
//!   the new selection.
//! - `Esc` back-navigates to MainMenu without committing — the
//!   document and `model.theme` stay at their pre-picker values, so
//!   the preview reverts.
//!
//! The picker carries a cloned `Vec<Theme>` snapshot from the
//! registry rather than a borrow because borrowing would make
//! `ThemePickerState` self-referential — the picker lives inside
//! `Model` next to the registry it'd borrow from. `Theme` is
//! `Clone`, the registry is bounded by built-ins plus user-
//! discovered files, and `Theme::name()` returns `&'static str`
//! so the snapshot doesn't need to duplicate the name field.

use std::borrow::Cow;
use std::mem;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;
use toml_edit::{value, DocumentMut};

use linesmith_core::theme::{Theme, ThemeRegistry};

use crate::config;

use super::app::{AppScreen, ScreenOutcome};
use super::list_screen::{
    self, ListOutcome, ListRowData, ListScreenState, ListScreenView, VerbHint,
};
use super::main_menu::MainMenuState;

/// Picker state. The list-widget cursor lives in `list`; `prev`
/// round-trips MainMenu state through Esc back-nav. `themes` is
/// a snapshot of the registry at construction time — see
/// `ThemeRegistry::iter` for the ordering contract (built-ins
/// followed by user themes, with user-overrides retaining the
/// built-in's slot rather than appending). `unknown_current`
/// captures the configured theme name when it doesn't match any
/// registered theme so the view can warn the user before they
/// overwrite their typo with the cursor's fallback.
///
/// No `Default` derive: every `ThemePickerState` must come from
/// `new()` so the cursor lands on a valid theme and `themes` is
/// non-empty (built-ins guarantee at least `default` and `minimal`).
#[derive(Debug)]
pub(super) struct ThemePickerState {
    list: ListScreenState,
    prev: MainMenuState,
    themes: Vec<Theme>,
    unknown_current: Option<String>,
}

impl ThemePickerState {
    pub(super) fn new(prev: MainMenuState, registry: &ThemeRegistry, current_name: &str) -> Self {
        let themes: Vec<Theme> = registry.iter().map(|rt| rt.theme.clone()).collect();
        debug_assert!(
            !themes.is_empty(),
            "theme registry must contain at least the built-in default theme; \
             constructed via `ThemeRegistry::default()` instead of `with_built_ins()`?",
        );
        let position = themes.iter().position(|theme| theme.name() == current_name);
        let unknown_current = if position.is_none() {
            Some(current_name.to_string())
        } else {
            None
        };
        let cursor = position.unwrap_or(0);
        let mut list = ListScreenState::default();
        list.set_cursor(cursor, themes.len());
        Self {
            list,
            prev,
            themes,
            unknown_current,
        }
    }

    /// Theme at the cursor — used by `app::view` to render the
    /// preview header with the cursor's selection while the picker
    /// is active. `ListScreenState::set_cursor` clamps the cursor
    /// to `[0, num_rows)` and `themes` is non-empty by construction,
    /// so direct indexing is safe.
    #[must_use]
    pub(super) fn cursor_theme(&self) -> &Theme {
        &self.themes[self.list.cursor()]
    }

    /// Take the stashed `MainMenuState` for Esc / Enter back-nav.
    /// Wrapping `mem::take` in a method keeps the field private so
    /// only `update`'s navigation arms can consume it; an accidental
    /// second consume mid-flight would leave a defaulted MainMenu
    /// behind and silently reset the cursor.
    fn take_prev(&mut self) -> MainMenuState {
        mem::take(&mut self.prev)
    }
}

/// Empty — `Enter` is implicit (handled by ListScreen).
const VERBS: &[VerbHint<'static>] = &[];

pub(super) fn update(
    state: &mut ThemePickerState,
    document: &mut DocumentMut,
    config: &mut config::Config,
    model_theme: &mut Theme,
    key: KeyEvent,
) -> ScreenOutcome {
    if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Esc {
        return ScreenOutcome::NavigateTo(AppScreen::MainMenu(state.take_prev()));
    }
    let row_count = state.themes.len();
    match list_screen::handle_key(&mut state.list, key, row_count, &[], false) {
        ListOutcome::Activate => {
            let cursor = state.list.cursor();
            if let Some(theme) = state.themes.get(cursor) {
                set_theme_in_document(document, theme.name());
                refresh_config(document, config);
                *model_theme = theme.clone();
                return ScreenOutcome::NavigateTo(AppScreen::MainMenu(state.take_prev()));
            }
            // Defensive: `list_screen::handle_key` currently returns
            // `Unhandled` (not `Activate`) when `row_count == 0`, so
            // this arm is unreachable today. If that guard ever
            // regresses, the construction `debug_assert!` traps the
            // empty-themes precondition in dev; the warning here is
            // the release-build counterpart so the Enter-ignored
            // path surfaces in the warnings panel.
            linesmith_core::lsm_warn!(
                "theme picker: cursor {cursor} out of range (themes len {row_count}); Enter ignored",
            );
        }
        ListOutcome::Action(_)
        | ListOutcome::MoveSwap { .. }
        | ListOutcome::Consumed
        | ListOutcome::Unhandled => {}
    }
    ScreenOutcome::Stay
}

pub(super) fn view(state: &ThemePickerState, frame: &mut Frame, area: Rect) {
    let cursor_idx = state.list.cursor();
    let row_data: Vec<ListRowData<'_>> = state
        .themes
        .iter()
        .enumerate()
        .map(|(idx, theme)| {
            let description = if idx == cursor_idx {
                "• press Enter to apply"
            } else {
                ""
            };
            ListRowData {
                label: Cow::Borrowed(theme.name()),
                description: Cow::Borrowed(description),
            }
        })
        .collect();
    // Bake the unknown-theme warning into the title so the user
    // sees "config references unknown theme `drakula`" before they
    // press Enter and silently overwrite the typo with the cursor's
    // fallback. Without this banner, the picker boots with cursor
    // on the first registered theme and Enter writes that name —
    // the user's typo is gone with no surface signal.
    let title: Cow<'_, str> = match &state.unknown_current {
        Some(name) => Cow::Owned(format!(
            " pick theme — config references unknown theme `{name}` "
        )),
        None => Cow::Borrowed(" pick theme "),
    };
    let view = ListScreenView {
        title: title.as_ref(),
        rows: &row_data,
        verbs: VERBS,
        move_mode_supported: false,
    };
    list_screen::render(&state.list, &view, area, frame);
}

/// Write `theme = "<name>"` to the document.
///
/// Three branches by current state of the `theme` key:
/// - **Absent:** insert as a fresh root scalar. Suppressed when
///   `name == "default"` so re-confirming the implicit default
///   doesn't dirty the buffer (see `theme::default_theme`).
/// - **Present and a scalar:** rewrite the inner value in place,
///   cloning decor across the swap so both channels survive — the
///   Item-level leading-line decor (comments ABOVE `theme = ...`)
///   AND the Value's suffix decor (trailing inline `# ...` on the
///   same line). A naive `document["theme"] = value(name)` keeps
///   the Item decor but rebuilds the Value with default decor,
///   stripping the inline comment. The rewrite is unconditional
///   even when `name` matches the existing value's text, so byte
///   representation can shift (e.g., `'default'` → `"default"`)
///   when the surrounding decor is preserved but the inner string
///   is reformatted by `Value::from`.
/// - **Present and non-scalar** (a `[theme]` table from a
///   forward-compatible newer config, an array, etc.): refuse and
///   warn. The picker has no business flattening structured config
///   into a scalar; preserving unknown shapes is core to the
///   round-trip contract.
fn set_theme_in_document(document: &mut DocumentMut, name: &str) {
    match document.get_mut("theme") {
        None => {
            if name == "default" {
                return;
            }
            document["theme"] = value(name);
        }
        Some(item) => {
            if let Some(existing) = item.as_value_mut() {
                let decor = existing.decor().clone();
                *existing = toml_edit::Value::from(name);
                *existing.decor_mut() = decor;
                return;
            }
            linesmith_core::lsm_warn!(
                "theme picker: existing `theme` entry is not a scalar value; \
                 refusing to overwrite the structured form. Edit the config \
                 manually or remove the `theme` entry to use the picker.",
            );
        }
    }
}

/// Reparse the document into `model.config` so the live preview
/// reads the new theme name. Mirrors the items_editor / line_picker
/// pattern. Validation warnings are suppressed (the segment builder
/// re-fires them per render) but parse failures warn and freeze
/// the config at last-good — same contract.
fn refresh_config(document: &DocumentMut, config: &mut config::Config) {
    match config::Config::from_str_validated(&document.to_string(), |_| {}) {
        Ok(new_config) => *config = new_config,
        Err(err) => linesmith_core::lsm_warn!(
            "theme picker: reparse failed, preview frozen at last-good state: {err}",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn document(toml: &str) -> DocumentMut {
        toml.parse().expect("test toml must parse")
    }

    fn registry() -> ThemeRegistry {
        ThemeRegistry::with_built_ins()
    }

    fn state_for(current: &str) -> ThemePickerState {
        ThemePickerState::new(MainMenuState::default(), &registry(), current)
    }

    #[test]
    fn esc_back_navigates_to_main_menu_without_committing() {
        // Pin that Esc reverts cleanly: document untouched, no
        // theme written. A regression that always commits on screen
        // close would silently change the user's saved theme on
        // accidental Esc.
        let mut s = state_for("default");
        let raw = "theme = \"default\"\n";
        let mut doc = document(raw);
        let mut cfg = config::Config::default();
        let mut theme = registry()
            .lookup("default")
            .expect("default theme exists")
            .clone();
        // Move cursor to the next theme.
        update(&mut s, &mut doc, &mut cfg, &mut theme, key(KeyCode::Down));
        let outcome = update(&mut s, &mut doc, &mut cfg, &mut theme, key(KeyCode::Esc));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::MainMenu(_))
        ));
        assert_eq!(doc.to_string(), raw, "Esc must not mutate the document");
    }

    #[test]
    fn enter_commits_cursor_theme_to_document_and_model() {
        // Pin both halves of the commit contract: the document gets
        // `theme = "<name>"` AND `model.theme` swaps to the chosen
        // theme. A regression that updates only one would surface
        // as either a saved-but-not-rendered selection or a
        // rendered-but-not-saved one.
        let mut s = state_for("default");
        let mut doc = document("");
        let mut cfg = config::Config::default();
        let mut theme = registry().lookup("default").expect("present").clone();
        let initial_theme_name = theme.name().to_string();

        // Move cursor to a non-current theme. Pick the second
        // registered theme to avoid accidentally landing on the
        // initial selection (which would make the test assertion
        // pass trivially).
        update(&mut s, &mut doc, &mut cfg, &mut theme, key(KeyCode::Down));
        let chosen_name = s.cursor_theme().name().to_string();
        assert_ne!(
            chosen_name, initial_theme_name,
            "test invariant: picker has at least 2 themes",
        );

        let outcome = update(&mut s, &mut doc, &mut cfg, &mut theme, key(KeyCode::Enter));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::MainMenu(_))
        ));
        let serialized = doc.to_string();
        assert!(
            serialized.contains(&format!("theme = \"{chosen_name}\"")),
            "document must record `theme = \"{chosen_name}\"`: {serialized}",
        );
        assert_eq!(theme.name(), chosen_name);
    }

    #[test]
    fn cursor_starts_on_currently_configured_theme() {
        // The picker should land its cursor on the user's current
        // theme so the live preview reflects "what's saved" rather
        // than always defaulting to the registry's first entry. Pin
        // the construction contract.
        let names: Vec<String> = registry()
            .iter()
            .map(|rt| rt.theme.name().to_string())
            .collect();
        // Pick the LAST registered theme so the cursor seek is
        // doing real work (not trivially landing on index 0).
        let target = names
            .last()
            .expect("registry has at least one theme")
            .clone();
        let s = state_for(&target);
        assert_eq!(
            s.cursor_theme().name(),
            target,
            "picker cursor must land on the configured theme",
        );
    }

    #[test]
    fn cursor_falls_back_to_first_theme_when_current_unknown() {
        // A config naming an unknown theme (typo, removed user
        // theme) shouldn't crash the picker. Pin that the cursor
        // falls back to index 0 AND the typo is captured in
        // `unknown_current` so the view can surface a banner before
        // the user overwrites their authored name with the cursor's
        // fallback theme. Without that capture, Enter would silently
        // replace `theme = "drakula"` with `theme = "default"`.
        let s = state_for("drakula");
        let first_registered = registry()
            .iter()
            .next()
            .expect("at least one theme registered")
            .theme
            .name()
            .to_string();
        assert_eq!(s.cursor_theme().name(), first_registered);
        assert_eq!(
            s.unknown_current.as_deref(),
            Some("drakula"),
            "unknown current theme name must round-trip into state for the banner",
        );
    }

    #[test]
    fn unknown_current_is_none_when_configured_theme_resolves() {
        // Counter-pin to `cursor_falls_back_to_first_…`: a known
        // theme name must NOT trip the banner path. A regression
        // that always populates `unknown_current` would turn a
        // healthy config into a perpetual "config references
        // unknown theme" warning.
        let s = state_for("default");
        assert_eq!(s.unknown_current, None);
    }

    #[test]
    fn picker_iterates_full_registry_so_user_themes_surface_through_with_user_themes() {
        // Pin that user themes registered via
        // `ThemeRegistry::with_user_themes` flow through to the
        // picker without an extra wiring step. The picker
        // constructor calls `registry.iter()`, which yields built-
        // ins followed by every user theme. A regression that
        // filters to built-ins only (e.g., switching to
        // `BUILTIN_THEMES.iter()`) would silently hide user themes
        // from the UI.
        use linesmith_core::theme::ThemeSource;
        use tempfile::TempDir;

        let tmp = TempDir::new().expect("tempdir");
        let theme_toml = r##"name = "custom"

[roles]
foreground = "#ffffff"
background = "#000000"
muted = "#666666"
primary = "#ff8800"
accent = "#00ffaa"
success = "#00ff00"
warning = "#ffff00"
error = "#ff0000"
info = "#0088ff"
"##;
        std::fs::write(tmp.path().join("custom.toml"), theme_toml).expect("write fixture");

        let mut warns: Vec<String> = Vec::new();
        let registry = ThemeRegistry::with_built_ins().with_user_themes(tmp.path(), |m| {
            warns.push(m.to_string());
        });
        assert!(
            warns.is_empty(),
            "fixture must parse without warnings: {warns:?}",
        );

        let picker = ThemePickerState::new(MainMenuState::default(), &registry, "default");
        let names: Vec<&str> = picker.themes.iter().map(Theme::name).collect();
        assert!(
            names.contains(&"custom"),
            "user theme must appear in the picker: {names:?}",
        );
        // Sanity: the user theme is registered as UserFile (not
        // BuiltIn). Catches a wiring regression that registers user
        // themes with the wrong source.
        let custom = registry
            .iter()
            .find(|rt| rt.theme.name() == "custom")
            .expect("custom registered");
        assert!(
            matches!(custom.source, ThemeSource::UserFile(_)),
            "user theme must carry UserFile source",
        );
    }

    #[test]
    fn set_theme_replaces_existing_root_value_in_place_preserving_decor() {
        // Pin the round-trip contract `set_theme_in_document`
        // documents: when `theme = "old"` already exists with
        // surrounding comments + blank lines, switching to a
        // different theme keeps the decor intact. A future refactor
        // that does `remove + insert` instead of indexed assignment
        // would silently strip user comments.
        let raw = "# my linesmith config\ntheme = \"default\"\n\n[line]\nsegments = [\"model\"]\n";
        let mut doc = document(raw);
        set_theme_in_document(&mut doc, "dracula");
        let serialized = doc.to_string();
        assert!(
            serialized.contains("# my linesmith config"),
            "leading comment must survive: {serialized}",
        );
        assert!(
            serialized.contains("theme = \"dracula\""),
            "value must update in place: {serialized}",
        );
        assert!(
            !serialized.contains("theme = \"default\""),
            "old value must be replaced, not duplicated: {serialized}",
        );
        // Reparse: theme is at root, single key.
        let reparsed: DocumentMut = serialized.parse().expect("reparses");
        assert_eq!(
            reparsed
                .get("theme")
                .and_then(|i| i.as_value())
                .and_then(|v| v.as_str()),
            Some("dracula"),
        );
    }

    #[test]
    fn set_theme_preserves_trailing_inline_comment_on_existing_value() {
        // `toml_edit` tracks suffix decor (the comment after a
        // value) on a separate channel from the leading-line comment
        // already pinned by `set_theme_replaces_existing_root_value...`.
        // A regression that uses `remove + insert` instead of indexed
        // assignment would strip trailing inline comments without
        // tripping the leading-comment test. Pin both decor channels.
        let raw = "theme = \"default\" # before tables\n\n[line]\nsegments = [\"model\"]\n";
        let mut doc = document(raw);
        set_theme_in_document(&mut doc, "dracula");
        let serialized = doc.to_string();
        assert!(
            serialized.contains("# before tables"),
            "trailing inline comment must survive: {serialized}",
        );
        assert!(
            serialized.contains("theme = \"dracula\""),
            "value must update in place: {serialized}",
        );
    }

    #[test]
    fn set_theme_default_is_noop_when_document_omits_theme_key() {
        // The user's config has no `theme = ...` line, so default is
        // implicit. Opening the picker lands the cursor on "default"
        // and pressing Enter should NOT insert `theme = "default"`.
        // Otherwise the common "open picker, eyeball, press Enter
        // to dismiss" workflow dirties the buffer and triggers an
        // unsaved-changes prompt on quit.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let mut doc = document(raw);
        set_theme_in_document(&mut doc, "default");
        assert_eq!(
            doc.to_string(),
            raw,
            "implicit-default re-selection must leave the document unchanged",
        );
    }

    #[test]
    fn set_theme_default_writes_explicit_value_when_document_already_has_theme_key() {
        // Counter-pin to the no-op above: when the document already
        // declares `theme = "dracula"`, picking default writes the
        // explicit `theme = "default"` rather than removing the key
        // silently. The user authored a theme line; we don't strip
        // bytes they wrote, even when the result would be implicit.
        let raw = "theme = \"dracula\"\n";
        let mut doc = document(raw);
        set_theme_in_document(&mut doc, "default");
        let serialized = doc.to_string();
        assert!(
            serialized.contains("theme = \"default\""),
            "must write explicit default when prior key existed: {serialized}",
        );
    }

    #[test]
    fn set_theme_refuses_to_clobber_non_scalar_theme_entry() {
        // Forward-compat guard: if a future linesmith version uses
        // a `[theme]` table for richer per-role overrides, the
        // current picker shouldn't silently flatten it to a scalar.
        // The round-trip contract preserves unknown shapes; the
        // picker honours that by warning and leaving the structured
        // entry in place. Today `Config::from_str_validated` rejects
        // a non-string `theme`, but that's an upstream guarantee —
        // this function defends its own contract.
        let raw = "[theme]\npalette = \"dracula\"\n";
        let mut doc = document(raw);
        set_theme_in_document(&mut doc, "default");
        let serialized = doc.to_string();
        assert_eq!(
            serialized, raw,
            "non-scalar `theme` entry must survive untouched: {serialized}",
        );
    }

    #[test]
    fn set_theme_same_name_on_existing_value_is_idempotent() {
        // When `theme = "default"` already exists and the user
        // re-confirms "default", the rewrite path runs (decor is
        // preserved across the swap, but `Value::from` rebuilds the
        // inner string). Pin that the resulting bytes match the
        // original — a regression in `Decor::clone` would shift
        // surrounding whitespace and dirty the buffer for what the
        // user perceives as a no-op.
        let raw = "theme = \"default\" # explicit\n";
        let mut doc = document(raw);
        set_theme_in_document(&mut doc, "default");
        assert_eq!(doc.to_string(), raw);
    }

    #[test]
    fn enter_on_default_with_absent_theme_key_does_not_dirty_document() {
        // End-to-end version of `set_theme_default_is_noop_…`:
        // exercise the full Enter dispatch through `update` so the
        // refresh_config + model_theme writes are also pinned to
        // not dirty the buffer for the implicit-default common case.
        let mut s = state_for("default");
        let raw = "[line]\nsegments = [\"model\"]\n";
        let mut doc = document(raw);
        let mut cfg = config::Config::default();
        let mut theme = registry().lookup("default").expect("present").clone();
        let outcome = update(&mut s, &mut doc, &mut cfg, &mut theme, key(KeyCode::Enter));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::MainMenu(_))
        ));
        assert_eq!(
            doc.to_string(),
            raw,
            "Enter on implicit default must not dirty the document",
        );
    }

    #[test]
    fn set_theme_lands_at_root_when_inserted_into_doc_with_existing_tables() {
        // Pin that `document["theme"] = ...` lands the new root-
        // level scalar BEFORE any table headers when the key is
        // absent. toml_edit positions the scalar above the first
        // `[table]`, which is the only place a root-level key
        // can live legally; without that, reparse would treat the
        // key as a child of the last table (or fail outright).
        // Both halves matter: serialized form has theme above
        // [line], AND reparse sees theme at root.
        let raw = "[line]\nsegments = [\"model\"]\n";
        let mut doc = document(raw);
        doc["theme"] = value("dracula");
        let serialized = doc.to_string();
        let theme_pos = serialized.find("theme = ").expect("theme written");
        let line_pos = serialized.find("[line]").expect("line preserved");
        assert!(
            theme_pos < line_pos,
            "theme key must land before [line] header in serialized form: {serialized}",
        );
        let reparsed: DocumentMut = serialized.parse().expect("reparses");
        assert!(
            reparsed.get("theme").and_then(|i| i.as_value()).is_some(),
            "reparse must see theme at root, not as a child of [line]: {serialized}",
        );
    }
}
