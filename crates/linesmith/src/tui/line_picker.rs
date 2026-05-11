//! Line Picker screen per ADR-0023 §Architecture.
//!
//! Sits between Main Menu and Items Editor for multi-line configs:
//! lists every `[line.N]` table the document declares, lets the
//! user add or delete lines, and routes Enter to the items editor
//! with the right [`LineKey::Numbered(N)`]. Single-line configs
//! bypass this screen entirely (Main Menu → Items Editor with
//! `LineKey::Single`).
//!
//! Keybindings:
//! - ↑/↓ to move between lines
//! - Enter to open the Items Editor for the highlighted line
//! - `a` to add a new `[line.N]` (N = max-existing + 1, or 1 when empty)
//! - `d` to delete the highlighted `[line.N]` table
//! - Esc to back-nav to Main Menu

use std::borrow::Cow;
use std::mem;
use std::num::NonZeroU32;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::Frame;
use toml_edit::{Array, DocumentMut, Item, Table, Value};

use crate::config;

use super::app::{AppScreen, ScreenOutcome};
use super::items_editor::{ItemsEditorPrev, ItemsEditorState, LineKey};
use super::list_screen::{
    self, ListOutcome, ListRowData, ListScreenState, ListScreenView, VerbHint,
};
use super::main_menu::MainMenuState;

/// Verbs the picker dispatches through `list_screen::handle_key`.
/// `a` (add) is handled at the screen level so it remains reachable
/// when the line list is empty (ListScreen gates `Action(c)` on
/// `num_rows > 0`); `d` (delete) requires a row to act on, so the
/// gate is correct for it.
const VERB_LETTERS: &[char] = &['d'];

const VERBS: &[VerbHint<'static>] = &[
    VerbHint {
        letter: 'a',
        label: "add",
    },
    VerbHint {
        letter: 'd',
        label: "delete",
    },
];

/// Picker state. The list-widget cursor lives in `list`; `prev`
/// round-trips the Main Menu state through Esc back-nav so the
/// user lands on the row they came from.
#[derive(Debug, Default)]
pub(super) struct LinePickerState {
    list: ListScreenState,
    pub(super) prev: MainMenuState,
}

impl LinePickerState {
    pub(super) fn new(prev: MainMenuState) -> Self {
        Self {
            list: ListScreenState::default(),
            prev,
        }
    }
}

pub(super) fn update(
    state: &mut LinePickerState,
    document: &mut DocumentMut,
    config: &mut config::Config,
    key: KeyEvent,
) -> ScreenOutcome {
    if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Esc {
        let prev = mem::take(&mut state.prev);
        return ScreenOutcome::NavigateTo(AppScreen::MainMenu(prev));
    }

    // Screen-level 'a' (add new line) so it stays reachable when the
    // list is empty. Mirrors the items-editor pattern for `a`/`i`.
    if key.modifiers == KeyModifiers::NONE && matches!(key.code, KeyCode::Char('a')) {
        let lines = numbered_lines(document);
        let next = next_line_index(&lines);
        match add_empty_line_outcome(document, next) {
            AddLineOutcome::Added => {
                refresh_config(document, config);
                // Land cursor on the freshly-added line so Enter opens
                // its (empty) Items Editor immediately.
                let new_lines = numbered_lines(document);
                if let Some(idx) = new_lines.iter().position(|n| *n == next) {
                    state.list.set_cursor(idx, new_lines.len());
                }
                return ScreenOutcome::Committed;
            }
            AddLineOutcome::DuplicateIndex => {
                // The numeric line index is taken — possibly by a
                // zero-padded shadow that the picker collapsed in
                // its dedup view. Distinct from the genuine "doc
                // not editable" failure so users get an actionable
                // hint rather than chasing a permission ghost.
                linesmith_core::lsm_warn!(
                    "line picker: line index {} is already in use (possibly via a zero-padded duplicate)",
                    next.get(),
                );
            }
            AddLineOutcome::DocumentNotEditable => {
                linesmith_core::lsm_warn!(
                    "line picker: could not add `[line.{}]`; `[line]` is not a table",
                    next.get(),
                );
            }
        }
        return ScreenOutcome::Stay;
    }

    let lines = numbered_lines(document);
    let row_count = lines.len();
    let cursor = state.list.cursor();
    let mut committed = false;

    match list_screen::handle_key(&mut state.list, key, row_count, VERB_LETTERS, false) {
        ListOutcome::Activate => {
            if let Some(&n) = lines.get(cursor) {
                // Pass the picker state itself as the items editor's
                // back-nav target so Esc returns to this picker with
                // its cursor intact, instead of skipping back to
                // MainMenu and forcing the user to re-pick the line
                // they were just editing. `mem::take` leaves a
                // defaulted picker behind that the screen transition
                // overwrites before any render observes it.
                let prev_picker = mem::take(state);
                let editor = ItemsEditorState::new(
                    LineKey::Numbered(n),
                    ItemsEditorPrev::LinePicker(prev_picker),
                );
                return ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(editor));
            }
        }
        ListOutcome::Action('d') => {
            if let Some(&n) = lines.get(cursor) {
                if delete_line(document, n) {
                    refresh_config(document, config);
                    let new_count = numbered_lines(document).len();
                    state.list.set_cursor(cursor, new_count);
                    committed = true;
                } else {
                    linesmith_core::lsm_warn!(
                        "line picker: could not remove `[line.{}]`; document not editable",
                        n.get(),
                    );
                }
            }
        }
        ListOutcome::Action(_)
        | ListOutcome::MoveSwap { .. }
        | ListOutcome::Consumed
        | ListOutcome::Unhandled => {}
    }
    if committed {
        ScreenOutcome::Committed
    } else {
        ScreenOutcome::Stay
    }
}

pub(super) fn view(state: &LinePickerState, document: &DocumentMut, frame: &mut Frame, area: Rect) {
    let lines = numbered_lines(document);
    let row_data: Vec<ListRowData<'_>> = lines
        .iter()
        .map(|n| {
            let counts = line_entry_counts(document, *n);
            let label = format!("Line {}", n.get());
            let description = describe_line_counts(&counts);
            ListRowData {
                label: Cow::Owned(label),
                description: Cow::Owned(description),
            }
        })
        .collect();
    let view = ListScreenView {
        title: " pick line to edit ",
        rows: &row_data,
        verbs: VERBS,
        move_mode_supported: false,
    };
    list_screen::render(&state.list, &view, area, frame);
}

/// Discover existing `[line.N]` numeric keys from the document,
/// sorted ascending and deduped by parsed numeric value. Non-
/// numeric keys (e.g. `[line.foo]`) and non-table values are
/// filtered out — the runtime builder warns on those at render
/// time; the picker only surfaces lines it can actually edit.
///
/// Dedup is load-bearing: a hand-edited config can carry both
/// `[line.1]` and `[line.01]`, both parse to `NonZeroU32(1)`, and
/// the lookup helpers (`segments_array`, `delete_line`, etc.)
/// resolve numbered lines by the FIRST parsed-numeric match. Without
/// dedup the picker would render two indistinguishable "Line 1"
/// rows and Enter / Delete on either would silently target the
/// same first table, leaving the shadow inaccessible from the UI.
fn numbered_lines(document: &DocumentMut) -> Vec<NonZeroU32> {
    let Some(line) = document.get("line").and_then(Item::as_table) else {
        return Vec::new();
    };
    let mut keys: Vec<NonZeroU32> = line
        .iter()
        .filter_map(|(k, v)| {
            if !v.is_table() {
                return None;
            }
            k.parse::<u32>().ok().and_then(NonZeroU32::new)
        })
        .collect();
    keys.sort();
    keys.dedup();
    keys
}

/// Pick the next available line index. Empty list yields `1`;
/// otherwise yields max-existing + 1, saturating at u32::MAX so
/// a pathological config doesn't panic the picker.
fn next_line_index(existing: &[NonZeroU32]) -> NonZeroU32 {
    match existing.last() {
        None => NonZeroU32::new(1).expect("1 is non-zero"),
        Some(last) => NonZeroU32::new(last.get().saturating_add(1)).unwrap_or(*last),
    }
}

/// Per ADR-0024, `[line.N].segments` is a mixed array of segment
/// ids and inline-table separators. The picker description splits
/// the count so an array like `["model", { type = "separator" }]`
/// reads as "1 segment + 1 separator" rather than the misleading
/// "2 segments" — separators don't render alone, so users want to
/// see at a glance which line actually has content.
#[derive(Debug, Default, PartialEq, Eq)]
struct LineEntryCounts {
    segments: usize,
    separators: usize,
    other: usize,
}

fn line_entry_counts(document: &DocumentMut, n: NonZeroU32) -> LineEntryCounts {
    let Some(line) = document.get("line").and_then(Item::as_table) else {
        return LineEntryCounts::default();
    };
    // Match the resolution rule the items editor uses: parsed-numeric
    // equality, so `[line.01]` and `[line.1]` both resolve to n=1.
    let Some(arr) = line
        .iter()
        .find(|(k, v)| v.is_table() && k.parse::<u32>().ok() == Some(n.get()))
        .and_then(|(_, v)| v.get("segments"))
        .and_then(Item::as_array)
    else {
        return LineEntryCounts::default();
    };
    let mut counts = LineEntryCounts::default();
    for entry in arr.iter() {
        if entry.as_str().is_some() {
            counts.segments += 1;
            continue;
        }
        if let Some(table) = entry.as_inline_table() {
            match table.get("type").and_then(|v| v.as_str()) {
                Some("separator") => counts.separators += 1,
                Some(_) => counts.segments += 1,
                None => counts.other += 1,
            }
            continue;
        }
        counts.other += 1;
    }
    counts
}

/// Format the counts for the picker's description column. Empty
/// arrays read "(empty)"; lines with only separators read
/// "(no segments)" so the user sees the line is non-empty but
/// won't render anything.
fn describe_line_counts(counts: &LineEntryCounts) -> String {
    let total = counts.segments + counts.separators + counts.other;
    if total == 0 {
        return "(empty)".to_string();
    }
    if counts.segments == 0 {
        return "(no segments)".to_string();
    }
    let segments = match counts.segments {
        1 => "1 segment".to_string(),
        n => format!("{n} segments"),
    };
    if counts.separators == 0 {
        segments
    } else {
        format!("{segments} + {} sep", counts.separators)
    }
}

/// Per-failure-mode classification for `add_empty_line_outcome`.
/// The picker dispatches to distinct warn messages on each variant
/// — collapsing into a single bool would force one wording to
/// cover both "document shape is broken" and "line index is taken",
/// which mislead users in opposite directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AddLineOutcome {
    Added,
    /// A sibling sub-table already declares the same numeric line
    /// index (matched by parsed `u32`, so `[line.01]` counts as a
    /// duplicate of `[line.1]`).
    DuplicateIndex,
    /// `[line]` exists as a non-table value, so we can't insert
    /// any child sub-table under it.
    DocumentNotEditable,
}

fn add_empty_line_outcome(document: &mut DocumentMut, n: NonZeroU32) -> AddLineOutcome {
    let line_item = document
        .entry("line")
        .or_insert_with(|| Item::Table(Table::new()));
    let Some(table) = line_item.as_table_mut() else {
        return AddLineOutcome::DocumentNotEditable;
    };
    if table
        .iter()
        .any(|(k, v)| v.is_table() && k.parse::<u32>().ok() == Some(n.get()))
    {
        return AddLineOutcome::DuplicateIndex;
    }
    let key = n.to_string();
    let mut sub = Table::new();
    sub["segments"] = Item::Value(Value::Array(Array::new()));
    table[&key] = Item::Table(sub);
    AddLineOutcome::Added
}

/// Bool wrapper around [`add_empty_line_outcome`] for tests that
/// only need the success/failure axis.
#[cfg(test)]
fn add_empty_line(document: &mut DocumentMut, n: NonZeroU32) -> bool {
    matches!(add_empty_line_outcome(document, n), AddLineOutcome::Added,)
}

/// Remove `[line.N]` from the document. Walks `[line]` to find
/// the matching numeric child by parsed `u32` so zero-padded keys
/// (`[line.01]`) get deleted correctly — `n.to_string()` alone
/// would always look up `"1"` and miss `"01"`. Returns `false`
/// when the path can't be resolved or no matching child exists.
fn delete_line(document: &mut DocumentMut, n: NonZeroU32) -> bool {
    let Some(table) = document.get_mut("line").and_then(Item::as_table_mut) else {
        return false;
    };
    let Some(matched_key) = table
        .iter()
        .find(|(k, v)| v.is_table() && k.parse::<u32>().ok() == Some(n.get()))
        .map(|(k, _)| k.to_string())
    else {
        return false;
    };
    table.remove(&matched_key).is_some()
}

/// Reparse `model.document` into `model.config` so the preview path
/// (which reads the typed `Config`) reflects line additions and
/// deletions immediately. Mirrors `items_editor::refresh_config`.
fn refresh_config(document: &DocumentMut, config: &mut config::Config) {
    match config::Config::from_str_validated(&document.to_string(), |_| {}) {
        Ok(new_config) => *config = new_config,
        Err(err) => linesmith_core::lsm_warn!(
            "line picker: reparse failed, preview frozen at last-good state: {err}",
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

    fn state() -> LinePickerState {
        LinePickerState::new(MainMenuState::default())
    }

    #[test]
    fn esc_back_navigates_to_main_menu() {
        let mut s = state();
        let mut doc = document("");
        let mut cfg = config::Config::default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Esc));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::MainMenu(_))
        ));
    }

    #[test]
    fn numbered_lines_sorts_ascending_and_skips_non_numeric() {
        let doc = document(
            r#"layout = "multi-line"
[line.10]
segments = []

[line.foo]
segments = []

[line.2]
segments = []
"#,
        );
        let lines = numbered_lines(&doc);
        let nums: Vec<u32> = lines.iter().map(|n| n.get()).collect();
        assert_eq!(nums, vec![2, 10]);
    }

    #[test]
    fn add_empty_line_creates_line_one_when_document_is_empty() {
        let mut s = state();
        let mut doc = document("");
        let mut cfg = config::Config::default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('a')));
        assert!(
            matches!(outcome, ScreenOutcome::Committed),
            "successful `a` add must signal Committed so the dispatcher auto-saves: {outcome:?}",
        );
        let lines = numbered_lines(&doc);
        let nums: Vec<u32> = lines.iter().map(|n| n.get()).collect();
        assert_eq!(nums, vec![1]);
        assert_eq!(
            line_entry_counts(&doc, NonZeroU32::new(1).unwrap()),
            LineEntryCounts::default(),
        );
    }

    #[test]
    fn add_empty_line_picks_next_index_above_existing_max() {
        let mut s = state();
        let mut doc = document(
            r#"layout = "multi-line"
[line.1]
segments = ["a"]

[line.3]
segments = ["b"]
"#,
        );
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('a')));
        let lines = numbered_lines(&doc);
        let nums: Vec<u32> = lines.iter().map(|n| n.get()).collect();
        assert_eq!(
            nums,
            vec![1, 3, 4],
            "next index appended above existing max"
        );
    }

    #[test]
    fn delete_verb_removes_highlighted_line() {
        let mut s = state();
        let mut doc = document(
            r#"layout = "multi-line"
[line.1]
segments = ["a"]

[line.2]
segments = ["b"]
"#,
        );
        let mut cfg = config::Config::default();
        // Cursor at row 0 (line 1).
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('d')));
        let lines = numbered_lines(&doc);
        let nums: Vec<u32> = lines.iter().map(|n| n.get()).collect();
        assert_eq!(nums, vec![2], "line 1 removed");
    }

    #[test]
    fn enter_navigates_to_items_editor_with_correct_line_key() {
        let mut s = state();
        let mut doc = document(
            r#"layout = "multi-line"
[line.1]
segments = ["a"]

[line.2]
segments = ["b"]
"#,
        );
        let mut cfg = config::Config::default();
        // Move cursor to line 2.
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        match outcome {
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(editor)) => {
                assert_eq!(
                    editor.line(),
                    LineKey::Numbered(NonZeroU32::new(2).expect("nonzero")),
                );
            }
            other => panic!("expected NavigateTo(ItemsEditor), got {other:?}"),
        }
    }

    #[test]
    fn enter_on_empty_picker_is_inert() {
        let mut s = state();
        let mut doc = document("");
        let mut cfg = config::Config::default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        assert!(matches!(outcome, ScreenOutcome::Stay));
    }

    #[test]
    fn add_then_enter_round_trips_through_items_editor_for_new_line() {
        // Pin the user flow: open multi-line picker, press `a` to
        // add a line, press Enter to edit it. The new line opens
        // with an empty segments array (not the runtime defaults
        // fallback that single-line uses) so the user can add
        // segments deliberately.
        let mut s = state();
        let mut doc = document(r#"layout = "multi-line""#);
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('a')));
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        match outcome {
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(editor)) => {
                assert_eq!(
                    editor.line(),
                    LineKey::Numbered(NonZeroU32::new(1).expect("nonzero")),
                );
            }
            other => panic!("expected NavigateTo(ItemsEditor), got {other:?}"),
        }
    }

    #[test]
    fn delete_on_empty_picker_is_inert() {
        let mut s = state();
        let mut doc = document("");
        let mut cfg = config::Config::default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('d')));
        assert!(matches!(outcome, ScreenOutcome::Stay));
    }

    #[test]
    fn next_line_index_handles_saturation() {
        // Pathological config with a line at u32::MAX shouldn't
        // panic the add path; we saturate to the existing max
        // (which `add_empty_line` then rejects as duplicate).
        let max = NonZeroU32::new(u32::MAX).expect("nonzero");
        assert_eq!(next_line_index(&[max]), max);
    }

    #[test]
    fn numbered_lines_dedups_keys_that_parse_to_same_index() {
        // A hand-edited config can carry both `[line.1]` and
        // `[line.01]` — both parse to `NonZeroU32(1)`. Without dedup
        // the picker renders two identical "Line 1" rows and the
        // lookup helpers silently target the first match, making the
        // shadow row inaccessible. Pin the dedup so a refactor that
        // drops it surfaces here.
        let doc = document(
            r#"layout = "multi-line"
[line.1]
segments = ["a"]

[line.01]
segments = ["b"]
"#,
        );
        let lines = numbered_lines(&doc);
        let nums: Vec<u32> = lines.iter().map(|n| n.get()).collect();
        assert_eq!(
            nums,
            vec![1],
            "dedup must collapse `[line.1]` and `[line.01]` to one picker row",
        );
    }

    #[test]
    fn dedup_edit_round_trip_leaves_shadow_table_byte_identical() {
        // Pin that mutating the surviving `[line.1]` entry doesn't
        // accidentally clobber the shadow `[line.01]` on save. The
        // dedup hides the shadow from the picker view, but the
        // mutation helpers all key on parsed-numeric *first match*,
        // so a refactor that keys mutations on the parsed index
        // rather than the raw key would silently overwrite the
        // shadow's contents. Walking the document text after
        // delete catches that regression directly.
        let mut doc = document(
            r#"layout = "multi-line"
[line.1]
segments = ["a"]

[line.01]
segments = ["shadow_canary"]
"#,
        );
        // delete_line targets the first parsed-numeric match — the
        // visible `[line.1]`. The shadow `[line.01]` survives.
        assert!(delete_line(&mut doc, NonZeroU32::new(1).unwrap()));
        let serialized = doc.to_string();
        assert!(
            !serialized.contains("[line.1]\nsegments = [\"a\"]"),
            "first match removed: {serialized}",
        );
        assert!(
            serialized.contains("[line.01]") && serialized.contains("\"shadow_canary\""),
            "shadow table byte-identical after delete: {serialized}",
        );
    }

    #[test]
    fn delete_line_removes_zero_padded_key() {
        // Parsing `[line.01]` to NonZeroU32 and stringifying back
        // gives "1", which would never match the original "01" key.
        // Pin that the picker walks the table and matches by parsed
        // numeric value so the line gets removed correctly.
        let mut doc = document(
            r#"layout = "multi-line"
[line]

[line.01]
segments = ["model"]
"#,
        );
        assert!(delete_line(&mut doc, NonZeroU32::new(1).unwrap()));
        let line = doc
            .get("line")
            .and_then(Item::as_table)
            .expect("line table present");
        let serialized = doc.to_string();
        assert!(
            !line.contains_key("01"),
            "zero-padded key must be removed: {serialized}",
        );
    }

    #[test]
    fn add_empty_line_rejects_zero_padded_duplicate() {
        // Companion to delete: adding line 1 to a doc that already
        // declares `[line.01]` must fail (rather than silently
        // creating a second `[line.1]` that shadows the first at
        // render time).
        let mut doc = document(
            r#"layout = "multi-line"
[line.01]
segments = ["model"]
"#,
        );
        assert!(!add_empty_line(&mut doc, NonZeroU32::new(1).unwrap()));
    }

    #[test]
    fn line_entry_counts_splits_segments_separators_and_other() {
        // A line containing
        // `["model", { type = "separator" }, "workspace", { character = " | " }]`
        // must report 2 segments, 1 separator, 1 other — not the
        // misleading "4 segments" that `arr.len()` would yield. The
        // picker description gates user attention; a wrong count
        // hides the fact that some entries don't render or are
        // malformed.
        let doc = document(
            r#"layout = "multi-line"
[line.1]
segments = ["model", { type = "separator" }, "workspace", { character = " | " }]
"#,
        );
        let counts = line_entry_counts(&doc, NonZeroU32::new(1).unwrap());
        assert_eq!(counts.segments, 2);
        assert_eq!(counts.separators, 1);
        assert_eq!(counts.other, 1);
    }

    #[test]
    fn describe_line_counts_distinguishes_no_segments_from_empty() {
        // A line with only separators isn't (empty) — it has entries
        // — but it renders nothing. "(no segments)" surfaces that
        // distinction so the user can spot lines that need a segment
        // before saving.
        let only_seps = LineEntryCounts {
            segments: 0,
            separators: 2,
            other: 0,
        };
        assert_eq!(describe_line_counts(&only_seps), "(no segments)");

        let empty = LineEntryCounts::default();
        assert_eq!(describe_line_counts(&empty), "(empty)");

        let mixed = LineEntryCounts {
            segments: 2,
            separators: 1,
            other: 0,
        };
        assert_eq!(describe_line_counts(&mixed), "2 segments + 1 sep");

        let segments_only = LineEntryCounts {
            segments: 1,
            separators: 0,
            other: 0,
        };
        assert_eq!(describe_line_counts(&segments_only), "1 segment");
    }

    #[test]
    fn add_then_delete_returns_to_empty() {
        let mut s = state();
        let mut doc = document("");
        let mut cfg = config::Config::default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('a')));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('d')));
        assert!(numbered_lines(&doc).is_empty());
    }

    #[test]
    fn enter_carries_picker_state_so_esc_back_nav_returns_to_picker() {
        // Opening a numbered line from the picker must pass the
        // *picker's* state into ItemsEditor's `prev` slot, not
        // unwrap the picker's own `prev` MainMenuState. A regression
        // here makes Esc from the items editor jump two screens up
        // to MainMenu, forcing the user to re-pick the line they
        // were just editing.
        use super::super::items_editor::ItemsEditorPrev;
        let mut s = state();
        let mut doc = document(
            r#"layout = "multi-line"
[line.1]
segments = ["a"]

[line.2]
segments = ["b"]
"#,
        );
        let mut cfg = config::Config::default();
        // Move cursor to line 2 so the round-trip checks cursor
        // preservation, not just default-state survival.
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        let editor = match outcome {
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(e)) => e,
            other => panic!("expected NavigateTo(ItemsEditor), got {other:?}"),
        };
        match &editor.prev {
            ItemsEditorPrev::LinePicker(picker) => {
                assert_eq!(
                    picker.list.cursor(),
                    1,
                    "picker cursor must round-trip so Esc lands on line 2 again",
                );
            }
            ItemsEditorPrev::MainMenu(_) => panic!(
                "items editor's prev must be LinePicker, not MainMenu — Esc would skip the picker",
            ),
        }
    }

    fn render_to_string(
        state: &LinePickerState,
        doc: &DocumentMut,
        width: u16,
        height: u16,
    ) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("backend");
        terminal
            .draw(|frame| view(state, doc, frame, frame.area()))
            .expect("draw");
        crate::tui::buffer_to_string(terminal.backend().buffer())
    }

    #[test]
    fn snapshot_line_picker_multiple_lines() {
        // `[line.status]` is in the fixture intentionally — non-numeric
        // keys must be dropped from the picker list.
        let s = state();
        let doc = document(
            r#"layout = "multi-line"
[line.1]
segments = []

[line.2]
segments = []

[line.status]
segments = []
"#,
        );
        insta::assert_snapshot!(
            "line_picker_multiple_lines",
            render_to_string(&s, &doc, 60, 16)
        );
    }
}
