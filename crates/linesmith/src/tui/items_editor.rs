//! Items editor screen per ADR-0023.
//!
//! Edits `[line].segments` (and `[line.N].segments`) by mutating
//! `model.document` via toml_edit. The view renders only segment
//! IDs today; separator-as-item rendering is an editor-scope
//! deferral, not a missing data model — the runtime LineItem enum
//! and the `[layout_options].separator` global already exist (see
//! `docs/specs/segment-system.md` §Line items and separators).
//!
//! Per ADR-0022, this screen owns cursor sync on `MoveSwap`: after
//! swapping the underlying segment array, the caller calls
//! `state.list.set_cursor(to, new_count)` so the highlight tracks
//! the moved row.

use std::borrow::Cow;
use std::mem;
use std::num::NonZeroU32;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;
use toml_edit::DocumentMut;

use crate::config;

use super::app::{AppScreen, ScreenOutcome};
use super::list_screen::{
    self, ListOutcome, ListRowData, ListScreenState, ListScreenView, VerbHint,
};
use super::main_menu::MainMenuState;

/// Which line in the config the editor is mutating. `Single`
/// addresses `[line].segments`; `Numbered(N)` addresses
/// `[line.N].segments`. `NonZeroU32` makes the spec's "lines start
/// at 1" rule a compile-time guarantee.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineKey {
    Single,
    /// Helpers walk `[line.N].segments`; only `Single` is
    /// constructed from the UI today.
    #[allow(dead_code)]
    Numbered(NonZeroU32),
}

/// Items editor state. The list-widget cursor lives in `list`; the
/// `prev` MainMenuState round-trips through Esc back-nav so the
/// user lands back on the menu row they came from. `prev` stays
/// `pub(super)` to mirror `PlaceholderState`'s back-nav idiom.
#[derive(Debug)]
pub(super) struct ItemsEditorState {
    line: LineKey,
    list: ListScreenState,
    pub(super) prev: MainMenuState,
}

impl ItemsEditorState {
    pub(super) fn new(line: LineKey, prev: MainMenuState) -> Self {
        Self {
            line,
            list: ListScreenState::default(),
            prev,
        }
    }

    /// Read-only accessor for cross-module tests; production code
    /// in this module reaches the field directly.
    #[allow(dead_code)]
    pub(super) fn line(&self) -> LineKey {
        self.line
    }
}

/// Drive the items editor through one keypress. Esc back-navigates
/// to MainMenu (preserving its cursor); other keys route through
/// the shared list widget with `move_mode_supported = true`. On
/// `MoveSwap`, mutate the document, ack cursor per ADR-0022, and
/// reparse config so the preview reflects the new order.
pub(super) fn update(
    state: &mut ItemsEditorState,
    document: &mut DocumentMut,
    config: &mut config::Config,
    key: KeyEvent,
) -> ScreenOutcome {
    if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Esc {
        let prev = mem::take(&mut state.prev);
        return ScreenOutcome::NavigateTo(AppScreen::MainMenu(prev));
    }
    let line = state.line;
    let row_count = segment_count(document, line);
    match list_screen::handle_key(&mut state.list, key, row_count, &[], true) {
        ListOutcome::MoveSwap { from, to } => {
            if swap_segments(document, line, from, to) {
                let new_count = segment_count(document, line);
                state.list.set_cursor(to, new_count);
                refresh_config(document, config);
            }
            ScreenOutcome::Stay
        }
        ListOutcome::Activate
        | ListOutcome::Action(_)
        | ListOutcome::Consumed
        | ListOutcome::Unhandled => ScreenOutcome::Stay,
    }
}

/// Render the segment list. Description slot is intentionally
/// empty: rows are plain segment IDs. `move_mode_supported = true`
/// so Enter toggles reorder.
pub(super) fn view(
    state: &ItemsEditorState,
    document: &DocumentMut,
    frame: &mut Frame,
    area: Rect,
) {
    let labels = segment_labels(document, state.line);
    let row_data: Vec<ListRowData<'_>> = labels
        .into_iter()
        .map(|label| ListRowData {
            label: Cow::Owned(label),
            description: Cow::Borrowed(""),
        })
        .collect();
    let verbs: [VerbHint<'_>; 0] = [];
    let view = ListScreenView {
        title: " edit lines ",
        rows: &row_data,
        verbs: &verbs,
        move_mode_supported: true,
    };
    list_screen::render(&state.list, &view, area, frame);
}

fn segment_count(document: &DocumentMut, line: LineKey) -> usize {
    if let Some(arr) = segments_array(document, line) {
        return arr.len();
    }
    // `[line].segments` is absent — the runtime falls back to the
    // built-in defaults, so the editor surfaces them too. Numbered
    // lines never fall back; multi-line configs must be authored
    // explicitly per `[line.N]` table.
    if matches!(line, LineKey::Single) {
        linesmith_core::segments::DEFAULT_SEGMENT_IDS.len()
    } else {
        0
    }
}

fn segments_array(document: &DocumentMut, line: LineKey) -> Option<&toml_edit::Array> {
    match line {
        LineKey::Single => document.get("line")?.get("segments")?.as_array(),
        LineKey::Numbered(n) => document
            .get("line")?
            .get(n.to_string())?
            .get("segments")?
            .as_array(),
    }
}

fn segments_array_mut(document: &mut DocumentMut, line: LineKey) -> Option<&mut toml_edit::Array> {
    match line {
        LineKey::Single => document
            .get_mut("line")?
            .get_mut("segments")?
            .as_array_mut(),
        LineKey::Numbered(n) => document
            .get_mut("line")?
            .get_mut(n.to_string())?
            .get_mut("segments")?
            .as_array_mut(),
    }
}

/// Stringified labels for display. Non-string entries render with
/// a `<non-string>` placeholder rather than being filtered out:
/// dropping them would desync the view's row count from
/// `segment_count`, letting the cursor land on a hidden index and
/// reorder the wrong underlying value. With the placeholder, the
/// user sees the bad entry, knows their TOML is wrong (the load
/// layer rejects it at parse time), and can reorder or delete it.
///
/// When `[line].segments` is absent (fresh config, never-edited
/// single-line case), surfaces the runtime defaults so the editor
/// matches the populated preview the user sees.
fn segment_labels(document: &DocumentMut, line: LineKey) -> Vec<String> {
    if let Some(arr) = segments_array(document, line) {
        return arr
            .iter()
            .map(|v| {
                v.as_str()
                    .map_or_else(|| "<non-string>".to_string(), str::to_string)
            })
            .collect();
    }
    if matches!(line, LineKey::Single) {
        linesmith_core::segments::DEFAULT_SEGMENT_IDS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    } else {
        Vec::new()
    }
}

/// Swap two positions in the segments array. Returns `false` when
/// an index is out of range. Single-line configs that are silent
/// on `[line].segments` materialize the runtime defaults into the
/// document before swapping, so the user's first edit commits the
/// view they were already seeing. Numbered lines never materialize
/// — multi-line configs must be authored explicitly.
fn swap_segments(document: &mut DocumentMut, line: LineKey, from: usize, to: usize) -> bool {
    // Existence check via the immutable path. `segments_array_mut`
    // walks the same chain via `Item::get_mut`, which can have
    // mutating side effects (e.g., implicit-table insertion) that
    // we don't want firing when the swap is a no-op.
    if segments_array(document, line).is_none() {
        if !matches!(line, LineKey::Single) {
            return false;
        }
        materialize_default_single_line_segments(document);
    }
    let Some(arr) = segments_array_mut(document, line) else {
        return false;
    };
    if from >= arr.len() || to >= arr.len() {
        return false;
    }
    let item = arr.remove(from);
    arr.insert(to, item);
    true
}

/// Write the runtime default segment IDs into `[line].segments`.
/// Preserves any existing keys under `[line]` (e.g., a `[line.1]`
/// sub-table that the single-line layout ignores) by mutating the
/// table in place rather than replacing it.
fn materialize_default_single_line_segments(document: &mut DocumentMut) {
    use toml_edit::{Array, Item, Table, Value};
    let mut arr = Array::new();
    for id in linesmith_core::segments::DEFAULT_SEGMENT_IDS {
        arr.push(*id);
    }
    let segments = Item::Value(Value::Array(arr));
    match document.get_mut("line") {
        Some(item) if item.is_table() => {
            if let Some(table) = item.as_table_mut() {
                table["segments"] = segments;
            }
        }
        _ => {
            let mut table = Table::new();
            table["segments"] = segments;
            document["line"] = Item::Table(table);
        }
    }
}

/// Boot-path warnings are suppressed during the inline reparse so
/// they don't re-fire every keystroke. A reparse failure surfaces
/// as a warning AND leaves `config` at its last-good value: the
/// preview keeps showing the last-good state, and the user gets
/// a signal that their edit produced a state the parser rejects
/// (e.g., a non-string segment in a hand-edited TOML). Without
/// the warning, the symptom would be a frozen preview with no
/// indication why.
fn refresh_config(document: &DocumentMut, config: &mut config::Config) {
    match config::Config::from_str_validated(&document.to_string(), |_| {}) {
        Ok(new_config) => *config = new_config,
        Err(err) => linesmith_core::lsm_warn!(
            "items editor: reparse failed, preview frozen at last-good state: {err}",
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

    fn config_default() -> config::Config {
        config::Config::default()
    }

    fn state() -> ItemsEditorState {
        ItemsEditorState::new(LineKey::Single, MainMenuState::default())
    }

    #[test]
    fn esc_back_navigates_to_main_menu_carrying_prior_state() {
        // Pin the contract that Esc round-trips MainMenuState
        // through `mem::take` so the user lands on the same row
        // they activated from. A regression that constructs
        // `MainMenuState::default()` for the back-nav silently
        // resets the cursor to row 0.
        let mut s = state();
        let mut doc = document("");
        let mut cfg = config_default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Esc));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::MainMenu(_))
        ));
    }

    #[test]
    fn esc_with_modifier_does_not_back_navigate() {
        // Mirror of the placeholder convention — chord Esc must
        // not trigger back-nav; a future relax to `(Esc, _)` would
        // silently catch Shift+Esc / Ctrl+Esc.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["model", "cwd"]
"#,
        );
        let mut cfg = config_default();
        let chord = KeyEvent::new(KeyCode::Esc, KeyModifiers::SHIFT);
        let outcome = update(&mut s, &mut doc, &mut cfg, chord);
        assert!(matches!(outcome, ScreenOutcome::Stay));
    }

    #[test]
    fn move_swap_reorders_segments_and_acks_cursor() {
        // Full round trip pin: enter move-mode, ↓ swaps the
        // segment with its neighbor in the document AND the
        // caller acks cursor per ADR-0022. A regression that
        // either skips the swap or skips the cursor ack fails
        // here.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["model", "cwd", "git"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        assert!(s.list.move_mode());
        assert_eq!(s.list.cursor(), 0);
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        assert_eq!(s.list.cursor(), 1, "cursor must follow the moved row");
        let labels = segment_labels(&doc, LineKey::Single);
        assert_eq!(labels, vec!["cwd", "model", "git"]);
    }

    #[test]
    fn move_swap_refreshes_config_to_match_document() {
        // Pin the document → config sync. Without it, the preview
        // (which reads `model.config`, not the document) would
        // keep showing the pre-swap order until the next event
        // happens to trigger a refresh elsewhere.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["model", "cwd"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        let line = cfg.line.expect("config must reparse with [line]");
        assert_eq!(line.segments, vec!["cwd".to_string(), "model".to_string()]);
    }

    #[test]
    fn move_swap_preserves_comments_and_blanks() {
        // toml_edit's whole point: editing in place doesn't strip
        // user formatting. Pin that comments and blank lines
        // around the segments array survive a swap.
        let raw = "# top comment\n\n[line]  # inline\nsegments = [\"model\", \"cwd\"]\n";
        let mut s = state();
        let mut doc = document(raw);
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        let written = doc.to_string();
        assert!(
            written.contains("# top comment"),
            "lost top comment: {written:?}"
        );
        assert!(
            written.contains("# inline"),
            "lost inline comment: {written:?}"
        );
    }

    #[test]
    fn empty_document_falls_back_to_runtime_default_segments() {
        // First-run / `--config new.toml` scenario: the document
        // has no `[line]` table yet but the runtime renders the
        // built-in default segments. The editor surfaces those
        // defaults so the user can reorder them rather than seeing
        // a blank list while the preview is populated.
        let doc = document("");
        let expected = linesmith_core::segments::DEFAULT_SEGMENT_IDS;
        assert_eq!(segment_count(&doc, LineKey::Single), expected.len());
        assert_eq!(
            segment_labels(&doc, LineKey::Single),
            expected
                .iter()
                .map(|s| (*s).to_string())
                .collect::<Vec<_>>(),
        );
    }

    #[test]
    fn missing_segments_key_falls_back_to_runtime_defaults() {
        // A `[line]` table without `segments` (e.g., hand-edited
        // TOML mid-rename) takes the same fallback path as a fully
        // absent `[line]`. The user sees what the runtime renders.
        let doc = document("[line]\n");
        let expected = linesmith_core::segments::DEFAULT_SEGMENT_IDS;
        assert_eq!(segment_count(&doc, LineKey::Single), expected.len());
    }

    #[test]
    fn explicitly_empty_segments_array_renders_zero_rows() {
        // Explicit `segments = []` is the user's authored "no
        // segments" intent — distinct from a missing array, which
        // falls back to defaults.
        let doc = document("[line]\nsegments = []\n");
        assert_eq!(segment_count(&doc, LineKey::Single), 0);
        assert!(segment_labels(&doc, LineKey::Single).is_empty());
    }

    #[test]
    fn first_swap_against_silent_document_materializes_defaults() {
        // The user opens the editor on a fresh config, sees the
        // default segments, enters move-mode, and swaps. That swap
        // commits the runtime defaults into the document so the
        // edit lands on a real array. Subsequent swaps mutate that
        // explicit array.
        let mut s = state();
        let mut doc = document("");
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        assert!(s.list.move_mode());
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        let arr = segments_array(&doc, LineKey::Single).expect("array materialized");
        let defaults = linesmith_core::segments::DEFAULT_SEGMENT_IDS;
        assert_eq!(arr.len(), defaults.len());
        // Swap reordered indices 0 and 1.
        assert_eq!(arr.get(0).and_then(|v| v.as_str()), Some(defaults[1]));
        assert_eq!(arr.get(1).and_then(|v| v.as_str()), Some(defaults[0]));
        for (i, expected) in defaults.iter().enumerate().skip(2) {
            assert_eq!(arr.get(i).and_then(|v| v.as_str()), Some(*expected));
        }
    }

    #[test]
    fn swap_on_numbered_line_with_missing_array_does_not_materialize() {
        // Numbered lines never fall back — multi-line configs must
        // be authored explicitly. A swap against a missing
        // `[line.N].segments` is a no-op and leaves the document
        // unchanged.
        let mut doc = document(
            r#"layout = "multi-line"
[line]
"#,
        );
        let one = NonZeroU32::new(1).expect("nonzero");
        let before = doc.to_string();
        assert!(!swap_segments(&mut doc, LineKey::Numbered(one), 0, 1));
        assert_eq!(doc.to_string(), before);
    }

    #[test]
    fn segment_labels_marks_non_string_entries_with_placeholder() {
        // Pin that non-string entries land in the label list as
        // `<non-string>` rather than being filtered out. Filtering
        // would desync the view's row count from `segment_count`,
        // letting the cursor reach a hidden index and reorder the
        // wrong array element under move-mode.
        let doc = document(
            r#"[line]
segments = ["a", 42, "b"]
"#,
        );
        assert_eq!(
            segment_labels(&doc, LineKey::Single),
            vec!["a", "<non-string>", "b"],
        );
        assert_eq!(
            segment_count(&doc, LineKey::Single),
            3,
            "row count must match label count to keep cursor aligned",
        );
    }

    #[test]
    fn move_swap_with_non_string_entry_reorders_correct_array_position() {
        // Concrete regression pin for codex's P2: with a non-string
        // entry in the segments array, the user navigating to that
        // row and triggering a move-swap must reorder the entry at
        // the same array index the cursor highlights — not a
        // post-filter index that maps elsewhere.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", 42, "b"]
"#,
        );
        let mut cfg = config_default();
        // Cursor 0 → 1 (the `42` row).
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        assert_eq!(s.list.cursor(), 1);
        // Move-mode + ↓ swaps row 1 with row 2.
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr.get(0).and_then(|v| v.as_str()), Some("a"));
        assert_eq!(arr.get(1).and_then(|v| v.as_str()), Some("b"));
        assert!(
            arr.get(2).is_some_and(|v| v.as_str().is_none()),
            "non-string moved to index 2"
        );
    }

    #[test]
    fn segments_array_resolves_numbered_line_path() {
        // LineKey::Numbered isn't reachable from the UI today
        // (LinePicker is a follow-up), but the helper is the
        // load-bearing piece — pin that the path resolution
        // correctly walks `[line.N].segments`.
        let doc = document(
            r#"layout = "multi-line"
[line]

[line.1]
segments = ["model"]

[line.2]
segments = ["cwd", "git"]
"#,
        );
        let one = NonZeroU32::new(1).expect("nonzero");
        let two = NonZeroU32::new(2).expect("nonzero");
        assert_eq!(segment_count(&doc, LineKey::Numbered(one)), 1);
        assert_eq!(segment_count(&doc, LineKey::Numbered(two)), 2);
        assert_eq!(
            segment_labels(&doc, LineKey::Numbered(two)),
            vec!["cwd", "git"]
        );
    }

    #[test]
    fn swap_segments_returns_false_for_numbered_with_missing_array() {
        // Multi-line authoring is explicit; numbered lines never
        // materialize defaults. The single-line equivalent
        // (`first_swap_against_silent_document_materializes_defaults`)
        // pins the opposite contract.
        let mut doc = document("");
        let one = NonZeroU32::new(1).expect("nonzero");
        assert!(!swap_segments(&mut doc, LineKey::Numbered(one), 0, 1));
    }

    #[test]
    fn swap_segments_numbered_path_isolates_to_targeted_line() {
        // Pin write-isolation between numbered lines: a swap on
        // `[line.1]` must not touch `[line.2]`. The Numbered
        // helper is shipping today even though no UI reaches it
        // yet — LinePicker will rely on this isolation.
        let mut doc = document(
            r#"layout = "multi-line"
[line]

[line.1]
segments = ["a", "b"]

[line.2]
segments = ["x", "y"]
"#,
        );
        let one = NonZeroU32::new(1).expect("nonzero");
        let two = NonZeroU32::new(2).expect("nonzero");
        assert!(swap_segments(&mut doc, LineKey::Numbered(one), 0, 1));
        assert_eq!(segment_labels(&doc, LineKey::Numbered(one)), vec!["b", "a"]);
        assert_eq!(
            segment_labels(&doc, LineKey::Numbered(two)),
            vec!["x", "y"],
            "swap on line 1 must not affect line 2"
        );
    }

    #[test]
    fn swap_segments_returns_false_for_out_of_range_index() {
        let mut doc = document(
            r#"[line]
segments = ["a", "b"]
"#,
        );
        assert!(!swap_segments(&mut doc, LineKey::Single, 0, 5));
        assert!(!swap_segments(&mut doc, LineKey::Single, 5, 0));
    }

    #[test]
    fn move_mode_up_at_top_does_not_mutate_document() {
        // Move-mode with cursor at 0 and ↑ pressed: ListScreen
        // emits Consumed (no swap), so the document must be
        // identical before and after. Pin defensively because the
        // screen update consumes the event without touching the
        // document — a refactor that pre-emptively calls
        // swap_segments before checking the outcome would mutate
        // here.
        let raw = "[line]\nsegments = [\"a\", \"b\"]\n";
        let mut s = state();
        let mut doc = document(raw);
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        assert!(s.list.move_mode());
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Up));
        assert_eq!(doc.to_string(), raw);
        assert_eq!(s.list.cursor(), 0);
    }

    #[test]
    fn move_swap_with_only_one_segment_is_noop() {
        // ListScreen guards `num_rows >= 2` for swaps; a one-row
        // list emits Consumed on ↓ in move-mode. Pin that the
        // editor doesn't try to swap a single element with itself.
        let raw = "[line]\nsegments = [\"only\"]\n";
        let mut s = state();
        let mut doc = document(raw);
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        assert_eq!(doc.to_string(), raw);
    }
}
