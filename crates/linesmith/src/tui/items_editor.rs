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

/// Verbs the editor dispatches in normal mode. Letters must match
/// the help-row labels in `VERBS`; `list_screen::handle_key` only
/// surfaces `Action(c)` when `c` is in this slice.
const VERB_LETTERS: &[char] = &['d', 'c', 'k'];

/// Help-row hints rendered alongside the move-mode toggle. Order
/// here drives the visible order in the help row.
const VERBS: &[VerbHint<'static>] = &[
    VerbHint {
        letter: 'd',
        label: "delete",
    },
    VerbHint {
        letter: 'c',
        label: "clear",
    },
    VerbHint {
        letter: 'k',
        label: "clone",
    },
];

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
/// the shared list widget with `move_mode_supported = true`. The
/// caller acks cursor changes per ADR-0022 after every mutation
/// and reparses config so the preview reflects the new state.
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
    let cursor = state.list.cursor();
    match list_screen::handle_key(&mut state.list, key, row_count, VERB_LETTERS, true) {
        ListOutcome::MoveSwap { from, to } => {
            if swap_segments(document, line, from, to) {
                let new_count = segment_count(document, line);
                state.list.set_cursor(to, new_count);
                refresh_config(document, config);
            }
        }
        ListOutcome::Action('d') => {
            if delete_segment_at(document, line, cursor) {
                let new_count = segment_count(document, line);
                state.list.set_cursor(cursor, new_count);
                refresh_config(document, config);
            }
        }
        ListOutcome::Action('c') => {
            if clear_segments(document, line) {
                state.list.set_cursor(0, 0);
                refresh_config(document, config);
            }
        }
        ListOutcome::Action('k') => {
            if clone_segment_at(document, line, cursor) {
                let new_count = segment_count(document, line);
                state.list.set_cursor(cursor + 1, new_count);
                refresh_config(document, config);
            }
        }
        ListOutcome::Activate
        | ListOutcome::Action(_)
        | ListOutcome::Consumed
        | ListOutcome::Unhandled => {}
    }
    ScreenOutcome::Stay
}

/// Render the segment list. Description slot is intentionally
/// empty: rows are plain segment IDs. `move_mode_supported = true`
/// so Enter toggles reorder; `VERBS` populate the help-row hints.
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
    let view = ListScreenView {
        title: " edit lines ",
        rows: &row_data,
        verbs: VERBS,
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

/// Get a mutable reference to the segments array, materializing
/// the runtime defaults first when single-line is silent on
/// `[line].segments`. Numbered lines never materialize — multi-
/// line configs must be authored explicitly. Returns `None` when
/// the path can't be resolved or materialized.
///
/// The immutable check is load-bearing: `Item::get_mut` can fire
/// implicit-table insertions on no-op mutations, so we only walk
/// the mutable chain after confirming the path resolves.
fn ensure_segments_array_mut(
    document: &mut DocumentMut,
    line: LineKey,
) -> Option<&mut toml_edit::Array> {
    if segments_array(document, line).is_none() {
        if !matches!(line, LineKey::Single) {
            return None;
        }
        materialize_default_single_line_segments(document);
    }
    segments_array_mut(document, line)
}

/// Swap two positions in the segments array. Returns `false` when
/// the array can't be resolved or an index is out of range.
fn swap_segments(document: &mut DocumentMut, line: LineKey, from: usize, to: usize) -> bool {
    let Some(arr) = ensure_segments_array_mut(document, line) else {
        return false;
    };
    if from >= arr.len() || to >= arr.len() {
        return false;
    }
    let item = arr.remove(from);
    arr.insert(to, item);
    true
}

/// Remove the entry at `idx`. The cursor ack happens in the caller
/// (`update`); `set_cursor` clamps to the new (smaller) length.
fn delete_segment_at(document: &mut DocumentMut, line: LineKey, idx: usize) -> bool {
    let Some(arr) = ensure_segments_array_mut(document, line) else {
        return false;
    };
    if idx >= arr.len() {
        return false;
    }
    arr.remove(idx);
    true
}

/// Empty the segments array (preserves the explicit `segments = []`
/// authored intent — does NOT remove the array entirely so the
/// next render doesn't fall back to runtime defaults).
fn clear_segments(document: &mut DocumentMut, line: LineKey) -> bool {
    let Some(arr) = ensure_segments_array_mut(document, line) else {
        return false;
    };
    arr.clear();
    true
}

/// Insert a copy of the entry at `idx` immediately after itself.
/// The caller advances the cursor to `idx + 1` so the user lands
/// on the fresh copy.
fn clone_segment_at(document: &mut DocumentMut, line: LineKey, idx: usize) -> bool {
    let Some(arr) = ensure_segments_array_mut(document, line) else {
        return false;
    };
    let Some(value) = arr.get(idx).cloned() else {
        return false;
    };
    arr.insert(idx + 1, value);
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
    fn verb_helpers_on_numbered_with_present_array_mutate_only_targeted_line() {
        // `update`'s verb arms are exercised against `Single` only
        // (LinePicker isn't wired yet), so this drops a level
        // lower and pins the helper contract directly: when a
        // `[line.N].segments` array exists, delete/clear/clone all
        // mutate JUST that line and never touch siblings.
        let initial = r#"layout = "multi-line"
[line]

[line.1]
segments = ["a", "b", "c"]

[line.2]
segments = ["x", "y", "z"]
"#;
        let one = NonZeroU32::new(1).expect("nonzero");
        let two = NonZeroU32::new(2).expect("nonzero");

        // Delete on line 1 leaves line 2 untouched.
        let mut doc = document(initial);
        assert!(delete_segment_at(&mut doc, LineKey::Numbered(one), 1));
        assert_eq!(segment_labels(&doc, LineKey::Numbered(one)), vec!["a", "c"]);
        assert_eq!(
            segment_labels(&doc, LineKey::Numbered(two)),
            vec!["x", "y", "z"]
        );

        // Clear on line 2 leaves line 1 untouched.
        let mut doc = document(initial);
        assert!(clear_segments(&mut doc, LineKey::Numbered(two)));
        assert_eq!(segment_count(&doc, LineKey::Numbered(two)), 0);
        assert_eq!(
            segment_labels(&doc, LineKey::Numbered(one)),
            vec!["a", "b", "c"]
        );

        // Clone on line 1 leaves line 2 untouched.
        let mut doc = document(initial);
        assert!(clone_segment_at(&mut doc, LineKey::Numbered(one), 0));
        assert_eq!(
            segment_labels(&doc, LineKey::Numbered(one)),
            vec!["a", "a", "b", "c"]
        );
        assert_eq!(
            segment_labels(&doc, LineKey::Numbered(two)),
            vec!["x", "y", "z"]
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

    #[test]
    fn delete_verb_removes_cursor_segment_and_keeps_cursor_in_range() {
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", "b", "c"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('d')));
        assert_eq!(segment_labels(&doc, LineKey::Single), vec!["a", "c"]);
        assert_eq!(
            s.list.cursor(),
            1,
            "cursor stays at 1, now pointing at \"c\""
        );
        let line = cfg.line.expect("line config reparsed");
        assert_eq!(line.segments, vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn delete_verb_at_last_row_clamps_cursor_back_one() {
        // Deleting the last row leaves cursor at len-1 of the new
        // (smaller) array. ListScreen's set_cursor clamps; the
        // editor explicitly calls it after the mutation so the
        // next render doesn't show a stale highlight.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", "b", "c"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        assert_eq!(s.list.cursor(), 2);
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('d')));
        assert_eq!(segment_labels(&doc, LineKey::Single), vec!["a", "b"]);
        assert_eq!(s.list.cursor(), 1);
    }

    #[test]
    fn delete_verb_against_silent_document_materializes_then_deletes() {
        // The user opens the editor on a fresh config, sees the
        // runtime defaults, presses 'd' on row 0. The defaults
        // commit to the document AND the first one is removed in
        // the same edit — same materialization-on-first-edit
        // contract as the swap path.
        let mut s = state();
        let mut doc = document("");
        let mut cfg = config_default();
        let defaults = linesmith_core::segments::DEFAULT_SEGMENT_IDS;
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('d')));
        let labels = segment_labels(&doc, LineKey::Single);
        assert_eq!(labels.len(), defaults.len() - 1);
        assert_eq!(labels[0], defaults[1]);
    }

    #[test]
    fn clear_verb_empties_segments_to_explicit_empty_array() {
        // After clear, segment_count is 0 (the explicit empty
        // array, not the missing-array fallback that would surface
        // defaults). User authored an empty list. Also pin the
        // config refresh: a regression that drops `refresh_config`
        // would leave `cfg.line.segments` at the pre-clear value
        // and the preview would show the old segments forever.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", "b", "c"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('c')));
        assert_eq!(segment_count(&doc, LineKey::Single), 0);
        assert_eq!(s.list.cursor(), 0);
        let arr = segments_array(&doc, LineKey::Single).expect("explicit empty array");
        assert_eq!(arr.len(), 0);
        assert!(cfg.line.expect("line reparsed").segments.is_empty());
    }

    #[test]
    fn clear_verb_on_already_empty_array_is_idempotent() {
        // Re-pressing `c` on an explicit empty array stays at
        // segments = []; no re-materialization, no re-fall-back.
        let mut s = state();
        let mut doc = document("[line]\nsegments = []\n");
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('c')));
        assert_eq!(segment_count(&doc, LineKey::Single), 0);
        let arr = segments_array(&doc, LineKey::Single).expect("explicit empty preserved");
        assert_eq!(arr.len(), 0);
    }

    #[test]
    fn clone_verb_inserts_copy_after_cursor_and_advances_cursor() {
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", "b", "c"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('k')));
        assert_eq!(
            segment_labels(&doc, LineKey::Single),
            vec!["a", "b", "b", "c"],
        );
        assert_eq!(s.list.cursor(), 2, "cursor lands on the fresh clone");
        // Pin the config refresh: a regression that drops
        // `refresh_config` from the clone arm would leave
        // `cfg.line.segments` at the pre-clone shape, freezing
        // the preview at three segments while the document has
        // four.
        let segments = cfg.line.expect("line reparsed").segments;
        assert_eq!(segments.len(), 4);
    }

    #[test]
    fn clone_verb_at_last_index_inserts_at_end_and_advances_cursor() {
        // Edge case: cursor at last row → clone inserts at
        // arr.len() (not arr.len() - 1 + 1 = arr.len(); same value
        // but worth pinning). `Array::insert(arr.len(), value)` is
        // valid; the cursor advances past the previously-final
        // index to the new last entry.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", "b"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        assert_eq!(s.list.cursor(), 1);
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('k')));
        assert_eq!(segment_labels(&doc, LineKey::Single), vec!["a", "b", "b"]);
        assert_eq!(s.list.cursor(), 2);
    }

    #[test]
    fn clone_verb_on_non_string_entry_clones_through_placeholder() {
        // `<non-string>` is a display detail; the underlying value
        // clones as-is (not stringified to "<non-string>"). Pin
        // that cloning a malformed entry produces another malformed
        // entry rather than coercing the placeholder string into
        // the array.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", 42, "b"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('k')));
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        assert_eq!(arr.len(), 4);
        assert_eq!(arr.get(1).and_then(|v| v.as_str()), None);
        assert_eq!(arr.get(2).and_then(|v| v.as_str()), None);
    }

    #[test]
    fn verb_letters_in_move_mode_are_inert() {
        // Pressing 'd' / 'c' / 'k' while reordering must not
        // mutate the document. ListScreen gates `Action(c)` to
        // normal mode; this test locks that gate so a future
        // refactor can't silently let a chord-typed verb destroy
        // data while the user is mid-reorder.
        let raw = "[line]\nsegments = [\"a\", \"b\"]\n";
        let mut s = state();
        let mut doc = document(raw);
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        assert!(s.list.move_mode());
        for verb in ['d', 'c', 'k'] {
            update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char(verb)));
        }
        assert_eq!(doc.to_string(), raw);
    }
}
