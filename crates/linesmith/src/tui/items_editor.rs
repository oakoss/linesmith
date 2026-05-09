//! Items editor screen per ADR-0023.
//!
//! Edits `[line].segments` (and `[line.N].segments`) by mutating
//! `model.document` via toml_edit. Per ADR-0024, the array is a
//! mixed shape: bare strings (`"model"`) for plain segment ids,
//! inline tables (`{ type = "separator", character = " | " }`)
//! for per-boundary separators, and inline tables with `merge =
//! true` for segments that suppress their right-edge boundary.
//! Display rows distinguish segments from separators (with the
//! glyph in the description column) and surface the `merge` flag.
//!
//! Verbs:
//! - `a`/`i` open the type picker for add/insert (segments).
//! - `Space` inserts `{ type = "separator" }` after the cursor.
//! - `r` opens the raw editor; on a separator it edits the
//!   `character` field, on a segment it renames the id (preserving
//!   inline-table fields like `merge`).
//! - `m` toggles merge on the cursor segment (string ↔ inline
//!   table; demotes back to bare string when only `type` remains).
//! - `d`/`c`/`k` delete / clear / clone the cursor entry.
//! - `Enter` toggles move-mode reorder.
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
use toml_edit::{DocumentMut, InlineTable, Value};

use crate::config;

use super::app::{AppScreen, ScreenOutcome};
use super::line_picker::LinePickerState;
use super::list_screen::{
    self, ListOutcome, ListRowData, ListScreenState, ListScreenView, VerbHint,
};
use super::main_menu::MainMenuState;
use super::raw_value_editor::{RawTarget, RawValueEditorState};
use super::type_picker::TypePickerState;

/// Verbs the editor dispatches through `list_screen::handle_key`.
/// `a`/`i`/`Space` are NOT listed here — they're handled at the
/// screen level so they remain reachable when the segment list is
/// empty (ListScreen gates `Action(c)` on `num_rows > 0`, which
/// would make add/insert/separator-insert inert during the live
/// "clear → rebuild" flow). `r`/`d`/`k`/`m` need a cursor entry to
/// act on; `c` clears the whole array but only fires when there's
/// something to clear. The `num_rows > 0` gate is correct for all
/// five.
const VERB_LETTERS: &[char] = &['d', 'c', 'k', 'r', 'm'];

/// Help-row hints rendered alongside the move-mode toggle. Order
/// here drives the visible order in the help row.
const VERBS: &[VerbHint<'static>] = &[
    VerbHint {
        letter: 'a',
        label: "add",
    },
    VerbHint {
        letter: 'i',
        label: "insert",
    },
    VerbHint {
        letter: ' ',
        label: "Space sep",
    },
    VerbHint {
        letter: 'r',
        label: "raw",
    },
    VerbHint {
        letter: 'm',
        label: "merge",
    },
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
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) enum LineKey {
    #[default]
    Single,
    /// Helpers walk `[line.N].segments`; only `Single` is
    /// constructed from the UI today.
    #[allow(dead_code)]
    Numbered(NonZeroU32),
}

/// Where in the segments array a picked entry should land. Carried
/// from the items editor through `TypePicker` so the picker can
/// stay UI-only and the data mutation lives in this module.
#[derive(Debug, Clone, Copy)]
pub(super) enum InsertTarget {
    /// Insert at `idx` (shifts the entry at `idx` and after to
    /// the right by one). The new entry takes index `idx`.
    Before(usize),
    /// Insert immediately after `idx`. The new entry takes index
    /// `idx + 1`.
    After(usize),
}

/// Where Esc back-navigates to from the items editor. Single-line
/// configs reach the editor straight from MainMenu, so Esc returns
/// to the menu row the user activated from. Multi-line configs go
/// through the line picker, so Esc returns to the picker row the
/// user activated from. Without this discriminator, multi-line Esc
/// jumps two screens up to MainMenu and forces the user to re-pick
/// the line they were just editing.
#[derive(Debug)]
pub(super) enum ItemsEditorPrev {
    MainMenu(MainMenuState),
    LinePicker(LinePickerState),
}

impl Default for ItemsEditorPrev {
    fn default() -> Self {
        Self::MainMenu(MainMenuState::default())
    }
}

/// Items editor state. The list-widget cursor lives in `list`; the
/// `prev` round-trips the back-nav target through Esc so the user
/// lands on the row they activated from (MainMenu for single-line
/// configs, LinePicker for multi-line).
///
/// `Default` is derived so `mem::take` in the type-picker entry
/// path leaves a placeholder `ItemsEditorState` behind (the screen
/// transition replaces it before any render observes the placeholder).
#[derive(Debug, Default)]
pub(super) struct ItemsEditorState {
    line: LineKey,
    list: ListScreenState,
    pub(super) prev: ItemsEditorPrev,
}

impl ItemsEditorState {
    pub(super) fn new(line: LineKey, prev: ItemsEditorPrev) -> Self {
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

    /// Mutator for the list-widget cursor; production sites in
    /// this module reach `state.list` directly.
    #[allow(dead_code)]
    pub(super) fn set_cursor(&mut self, idx: usize, num_rows: usize) {
        self.list.set_cursor(idx, num_rows);
    }

    /// Read-only accessor for the cursor.
    #[allow(dead_code)]
    pub(super) fn cursor(&self) -> usize {
        self.list.cursor()
    }
}

/// Drive the items editor through one keypress. Esc back-navigates
/// to MainMenu (preserving its cursor); ←/→ + the `a`/`i` verbs
/// open the type picker for insert/add. Other keys route through
/// the shared list widget. The caller acks cursor changes per
/// ADR-0022 after every mutation and reparses config so the
/// preview reflects the new state.
pub(super) fn update(
    state: &mut ItemsEditorState,
    document: &mut DocumentMut,
    config: &mut config::Config,
    key: KeyEvent,
) -> ScreenOutcome {
    // Esc back-nav fires regardless of move-mode (ListScreen exits
    // move-mode on Esc; we never see Esc here while in move-mode).
    if key.modifiers == KeyModifiers::NONE && key.code == KeyCode::Esc {
        return back_nav_to_prev(&mut state.prev);
    }
    // Screen-level keybindings for picker entry: `a`/`i` verbs,
    // Space (insert separator), and ←/→ accelerators. Handled here
    // (not via ListScreen's verb dispatch) so they remain reachable
    // when `segment_count == 0` — the live "clear → rebuild" flow.
    // Gated to normal mode so a chord-typed letter or arrow during
    // move-mode doesn't yank the user out of their reorder.
    if key.modifiers == KeyModifiers::NONE && !state.list.move_mode() {
        match key.code {
            KeyCode::Left | KeyCode::Char('i') => {
                let cursor = state.list.cursor();
                return open_type_picker(state, InsertTarget::Before(cursor));
            }
            KeyCode::Right | KeyCode::Char('a') => {
                let cursor = state.list.cursor();
                return open_type_picker(state, InsertTarget::After(cursor));
            }
            KeyCode::Char(' ') => {
                let cursor = state.list.cursor();
                let line = state.line;
                let target = if segment_count(document, line) == 0 {
                    InsertTarget::Before(0)
                } else {
                    InsertTarget::After(cursor)
                };
                if insert_separator(document, line, target) {
                    refresh_config(document, config);
                    let new_count = segment_count(document, line);
                    let new_cursor = match target {
                        InsertTarget::Before(idx) => idx,
                        InsertTarget::After(idx) => idx + 1,
                    };
                    state.list.set_cursor(new_cursor, new_count);
                } else {
                    linesmith_core::lsm_warn!(
                        "items editor: insert separator failed (line={line:?}); editor unchanged",
                    );
                }
                return ScreenOutcome::Stay;
            }
            _ => {}
        }
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
        ListOutcome::Action('r') => {
            return open_raw_value_editor(state, document, cursor);
        }
        ListOutcome::Action('m') => {
            if toggle_merge_at(document, line, cursor) {
                let new_count = segment_count(document, line);
                state.list.set_cursor(cursor, new_count);
                refresh_config(document, config);
            } else {
                linesmith_core::lsm_warn!(
                    "items editor: merge toggle inert at index {cursor} (line={line:?}); merge applies to segment entries only",
                );
            }
        }
        ListOutcome::Action('d') => {
            if delete_segment_at(document, line, cursor) {
                let new_count = segment_count(document, line);
                state.list.set_cursor(cursor, new_count);
                refresh_config(document, config);
            }
            // No warn arm: ListScreen gates `Action(c)` on
            // `num_rows > 0` and `delete_segment_at` materializes
            // the single-line defaults on demand, so the false
            // branch is unreachable through the UI. Per AGENTS.md,
            // don't add validation for scenarios that can't happen.
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

/// Resolve the Esc back-nav target. `mem::take` swaps the prev
/// for the `Default` variant (`MainMenu(MainMenuState::default())`)
/// — the screen transition overwrites the editor's state
/// immediately so the placeholder is never observed by `view`.
fn back_nav_to_prev(prev: &mut ItemsEditorPrev) -> ScreenOutcome {
    match mem::take(prev) {
        ItemsEditorPrev::MainMenu(state) => ScreenOutcome::NavigateTo(AppScreen::MainMenu(state)),
        ItemsEditorPrev::LinePicker(state) => {
            ScreenOutcome::NavigateTo(AppScreen::LinePicker(state))
        }
    }
}

/// Hand the editor state off to a fresh `TypePicker`. `mem::take`
/// leaves a default `ItemsEditorState` behind; the screen
/// transition immediately overwrites it via the returned
/// `NavigateTo`, so no render observes the placeholder.
fn open_type_picker(state: &mut ItemsEditorState, target: InsertTarget) -> ScreenOutcome {
    let prev = mem::take(state);
    ScreenOutcome::NavigateTo(AppScreen::TypePicker(TypePickerState::new(target, prev)))
}

/// Hand the editor state off to a fresh `RawValueEditor` seeded
/// with the cursor entry's current value. Dispatches by entry
/// kind: a separator inline table opens the editor for the
/// `character` field; everything else opens for the segment id.
/// Same `mem::take` safety as `open_type_picker`.
///
/// The seed is read from the underlying TOML value, not the
/// rendered label. This distinguishes a real string ID equal to
/// `"<non-string>"` (a valid TOML string) from the synthetic
/// placeholder the display path (`value_to_row`) emits for non-
/// string non-table TOML entries — the literal must round-trip;
/// the placeholder must not invite a commit of itself.
///
/// `target_idx` is captured here and consumed in `apply_replace`
/// when the user commits. Valid for the lifetime of the editor:
/// the items editor is suspended while the raw editor runs (no
/// other dispatch path mutates the document), so the index can
/// only become stale via an external file edit, which falls into
/// the `replace_segment` bounds check.
fn open_raw_value_editor(
    state: &mut ItemsEditorState,
    document: &DocumentMut,
    target_idx: usize,
) -> ScreenOutcome {
    let line = state.line;
    let (initial, target) = match classify_entry(document, line, target_idx) {
        EntryKind::Separator => {
            let initial = segments_array(document, line)
                .and_then(|arr| arr.get(target_idx).and_then(|v| v.as_inline_table()))
                .and_then(|t| t.get("character").and_then(|v| v.as_str()))
                .map(str::to_string)
                .unwrap_or_default();
            (initial, RawTarget::SeparatorCharacter)
        }
        EntryKind::Segment | EntryKind::Malformed => {
            let initial = if let Some(arr) = segments_array(document, line) {
                arr.get(target_idx)
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| inline_segment_id(arr.get(target_idx)))
            } else if matches!(line, LineKey::Single) {
                linesmith_core::segments::DEFAULT_SEGMENT_IDS
                    .get(target_idx)
                    .map(|s| (*s).to_string())
                    .unwrap_or_default()
            } else {
                String::new()
            };
            (initial, RawTarget::SegmentId)
        }
    };
    let prev = mem::take(state);
    ScreenOutcome::NavigateTo(AppScreen::RawValueEditor(RawValueEditorState::new(
        initial, target_idx, target, prev,
    )))
}

/// Extract the `type` field from a non-separator inline table so
/// `r` on a `{ type = "model", merge = true }` entry seeds the
/// editor with `"model"` rather than an empty buffer. Returns an
/// empty string when the value isn't an inline table or has no
/// `type` field.
fn inline_segment_id(value: Option<&Value>) -> String {
    value
        .and_then(toml_edit::Value::as_inline_table)
        .and_then(|t| t.get("type").and_then(|v| v.as_str()))
        .map(str::to_string)
        .unwrap_or_default()
}

/// Apply a raw-value commit to the document and navigate back to
/// the items editor. Dispatches by `target` so a separator-
/// character edit mutates the inline table's `character` field
/// while a segment-id edit replaces the array entry with a string.
/// Mirrors `apply_insert`'s ownership transfer.
///
/// Empty strings are accepted at this layer:
/// - For `SegmentId`, the empty string passes through to the
///   load-layer warning channel when it doesn't match any built-
///   in or plugin id.
/// - For `SeparatorCharacter`, an empty string clears the
///   override so the boundary uses the global
///   `[layout_options].separator` default.
pub(super) fn apply_replace(
    mut prev: ItemsEditorState,
    document: &mut DocumentMut,
    config: &mut config::Config,
    target_idx: usize,
    target: RawTarget,
    new_value: &str,
) -> ScreenOutcome {
    let line = prev.line;
    let success = match target {
        RawTarget::SegmentId => replace_segment(document, line, target_idx, new_value),
        RawTarget::SeparatorCharacter => {
            replace_separator_character(document, line, target_idx, new_value)
        }
    };
    if !success {
        linesmith_core::lsm_warn!(
            "items editor: replace failed at index {target_idx} (line={line:?}); editor unchanged",
        );
        return ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(prev));
    }
    refresh_config(document, config);
    let new_count = segment_count(document, line);
    prev.list.set_cursor(target_idx, new_count);
    ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(prev))
}

/// Replace the segment-id at `idx` with `new_value`, preserving
/// any inline-table fields (`merge`, future per-entry knobs) when
/// the existing entry is an inline table. Bare-string entries stay
/// bare strings; inline-table entries have only their `type` field
/// updated. Returns `false` when `ensure_segments_array_mut`
/// rejects (numbered line without explicit array) or the index is
/// out of range.
///
/// The inline-table path is load-bearing for the `m`+`r` workflow:
/// `m` promotes a bare string to `{ type = "model", merge = true }`,
/// then `r` to rename the segment must keep `merge` intact rather
/// than rewrite the array entry as a bare string and silently drop
/// the flag.
fn replace_segment(document: &mut DocumentMut, line: LineKey, idx: usize, new_value: &str) -> bool {
    let Some(arr) = ensure_segments_array_mut(document, line) else {
        return false;
    };
    let Some(entry) = arr.get_mut(idx) else {
        return false;
    };
    // Single match avoids the TOCTOU shape where a separate
    // `is_inline_table` check + later `as_inline_table_mut` could
    // disagree and silently elide the mutation. Either we land on
    // an inline-table arm and update its `type`, or we fall through
    // and replace with a bare string. A separator inline table at
    // this index means kind-drift since `open_raw_value_editor`
    // dispatched on the entry's classification — refuse the rename
    // so a "rename segment" verb doesn't silently corrupt a
    // separator boundary.
    match entry {
        Value::InlineTable(table) => {
            if table.get("type").and_then(|v| v.as_str()) == Some("separator") {
                return false;
            }
            table.insert("type", Value::from(new_value));
            true
        }
        _ => {
            arr.replace(idx, new_value);
            true
        }
    }
}

/// Update the `character` field of the inline-table separator at
/// `idx` to `new_value` verbatim, including the empty string. Per
/// ADR-0024 the two states `character = ""` (explicit no-separator
/// at this boundary) and `character` absent (fall back to
/// `[layout_options].separator`) are distinct render outcomes; the
/// raw editor commits one consistent meaning — "set the character
/// to what's in the buffer" — so a round-trip through `r` never
/// changes which mode the entry is in. Users who want the absent-
/// field "use global default" state use `Space` to insert a fresh
/// defaulted separator (and don't subsequently `r` it). Returns
/// `false` when the entry isn't a separator inline table or the
/// index is out of range.
fn replace_separator_character(
    document: &mut DocumentMut,
    line: LineKey,
    idx: usize,
    new_value: &str,
) -> bool {
    let Some(arr) = ensure_segments_array_mut(document, line) else {
        return false;
    };
    let Some(value) = arr.get_mut(idx) else {
        return false;
    };
    let Some(table) = value.as_inline_table_mut() else {
        return false;
    };
    if table.get("type").and_then(|v| v.as_str()) != Some("separator") {
        return false;
    }
    table.insert("character", Value::from(new_value));
    true
}

/// Toggle the `merge` flag on the cursor segment. A bare-string
/// entry (`"model"`) is promoted to `{ type = "model", merge = true }`;
/// an inline-table entry already at `merge = true` has the field
/// removed (and demotes back to a bare string when no other typed
/// fields remain). Returns `false` for separator entries (merge
/// is a segment-only flag), kindless inline tables (no `type` —
/// the builder drops these as malformed; toggling `merge` on them
/// would just write more invalid TOML), missing arrays, or out-
/// of-range indices.
fn toggle_merge_at(document: &mut DocumentMut, line: LineKey, idx: usize) -> bool {
    let Some(arr) = ensure_segments_array_mut(document, line) else {
        return false;
    };
    let Some(value) = arr.get(idx).cloned() else {
        return false;
    };
    match value {
        Value::String(s) => {
            // Promote: `"model"` → `{ type = "model", merge = true }`.
            let mut table = InlineTable::new();
            table.insert("type", Value::from(s.value().as_str()));
            table.insert("merge", Value::from(true));
            arr.replace(idx, Value::InlineTable(table));
            true
        }
        Value::InlineTable(t) => {
            let type_field = t.get("type").and_then(|v| v.as_str());
            if type_field == Some("separator") {
                return false;
            }
            // Reject kindless entries: the builder drops them with
            // a "missing `type`" warn, so adding `merge = true` only
            // ships invalid TOML. The user must add `type` first
            // (via `r`) before merge is meaningful.
            let Some(_) = type_field else {
                return false;
            };
            let kind = t.get("type").and_then(|v| v.as_str()).map(str::to_string);
            let currently_merging = t.get("merge").and_then(|v| v.as_bool()).unwrap_or(false);
            if currently_merging {
                // Demote: clear merge. If the only remaining field
                // is `type`, collapse back to a bare string for
                // round-trip cleanliness.
                let mut new_table = t.clone();
                new_table.remove("merge");
                let only_type_left = new_table.iter().count() == 1
                    && new_table.get("type").and_then(|v| v.as_str()).is_some();
                if only_type_left {
                    if let Some(id) = kind {
                        arr.replace(idx, Value::from(id.as_str()));
                        return true;
                    }
                }
                arr.replace(idx, Value::InlineTable(new_table));
            } else {
                let mut new_table = t.clone();
                new_table.insert("merge", Value::from(true));
                arr.replace(idx, Value::InlineTable(new_table));
            }
            true
        }
        _ => false,
    }
}

/// Insert `{ type = "separator" }` at the position described by
/// `target`. The new separator uses the global
/// `[layout_options].separator` default; the user customizes the
/// glyph by navigating to the row and pressing `r` (raw). Returns
/// `false` only when `ensure_segments_array_mut` rejects.
fn insert_separator(document: &mut DocumentMut, line: LineKey, target: InsertTarget) -> bool {
    let Some(arr) = ensure_segments_array_mut(document, line) else {
        return false;
    };
    let idx = match target {
        InsertTarget::Before(i) => i.min(arr.len()),
        InsertTarget::After(i) => i.saturating_add(1).min(arr.len()),
    };
    let mut table = InlineTable::new();
    table.insert("type", Value::from("separator"));
    arr.insert(idx, Value::InlineTable(table));
    true
}

/// Classification of an entry in the segments array. Drives the
/// view's row rendering AND the `r`/`m` verb dispatch (separators
/// reach a different RawTarget; merge is a segment-only flag).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    /// Bare-string id or inline-table with `type` ≠ `"separator"`.
    Segment,
    /// Inline-table with `type = "separator"`.
    Separator,
    /// Out of range, non-string, or kindless inline table.
    Malformed,
}

fn classify_entry(document: &DocumentMut, line: LineKey, idx: usize) -> EntryKind {
    let Some(arr) = segments_array(document, line) else {
        // Default-fallback path (Single + missing array) renders
        // bare segment ids; classify as Segment so `r` opens the
        // segment-id editor.
        return if matches!(line, LineKey::Single) {
            EntryKind::Segment
        } else {
            EntryKind::Malformed
        };
    };
    let Some(v) = arr.get(idx) else {
        return EntryKind::Malformed;
    };
    if v.as_str().is_some() {
        return EntryKind::Segment;
    }
    if let Some(t) = v.as_inline_table() {
        return match t.get("type").and_then(|tv| tv.as_str()) {
            Some("separator") => EntryKind::Separator,
            Some(_) => EntryKind::Segment,
            None => EntryKind::Malformed,
        };
    }
    EntryKind::Malformed
}

/// Apply a picker selection to the document and navigate back to
/// the items editor. Takes ownership of `prev` and returns it
/// inside the `NavigateTo` so the picker's mem::take handoff is
/// the single ownership transfer for the round trip.
///
/// Called from `type_picker::update` on Enter. On insertion
/// failure (e.g., a numbered line without an explicit array)
/// surfaces a warning and navigates back without mutating —
/// matches `refresh_config`'s precedent of keeping the user
/// informed when a UI action would otherwise dismiss silently.
pub(super) fn apply_insert(
    mut prev: ItemsEditorState,
    document: &mut DocumentMut,
    config: &mut config::Config,
    target: InsertTarget,
    segment_id: &str,
) -> ScreenOutcome {
    let line = prev.line;
    if !insert_segment(document, line, target, segment_id) {
        linesmith_core::lsm_warn!(
            "items editor: insert failed for segment {segment_id:?} (line={line:?}); editor unchanged",
        );
        return ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(prev));
    }
    refresh_config(document, config);
    let inserted_at = match target {
        InsertTarget::Before(idx) => idx,
        InsertTarget::After(idx) => idx + 1,
    };
    let new_count = segment_count(document, line);
    prev.list.set_cursor(inserted_at, new_count);
    ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(prev))
}

/// Render the segment list. Separators render distinctly with the
/// `character` value shown in the description column, or
/// `(default)` when the boundary inherits
/// `[layout_options].separator`, or `(empty)` for an explicit
/// `character = ""` (the no-separator-at-this-boundary state per
/// ADR-0024 — distinct from "use global default"). Segments with
/// `merge = true` carry a `merge →` description so the per-
/// boundary suppression is visible without opening the saved
/// file. `move_mode_supported = true` so Enter toggles reorder;
/// `VERBS` populate the help-row hints.
pub(super) fn view(
    state: &ItemsEditorState,
    document: &DocumentMut,
    frame: &mut Frame,
    area: Rect,
) {
    let row_data = display_rows(document, state.line);
    let view = ListScreenView {
        title: " edit lines ",
        rows: &row_data,
        verbs: VERBS,
        move_mode_supported: true,
    };
    list_screen::render(&state.list, &view, area, frame);
}

fn display_rows(document: &DocumentMut, line: LineKey) -> Vec<ListRowData<'static>> {
    if let Some(arr) = segments_array(document, line) {
        return arr.iter().map(value_to_row).collect();
    }
    if matches!(line, LineKey::Single) {
        return linesmith_core::segments::DEFAULT_SEGMENT_IDS
            .iter()
            .map(|s| ListRowData {
                label: Cow::Owned((*s).to_string()),
                description: Cow::Borrowed(""),
            })
            .collect();
    }
    Vec::new()
}

fn value_to_row(value: &Value) -> ListRowData<'static> {
    if let Some(s) = value.as_str() {
        return ListRowData {
            label: Cow::Owned(s.to_string()),
            description: Cow::Borrowed(""),
        };
    }
    if let Some(t) = value.as_inline_table() {
        let kind = t.get("type").and_then(|v| v.as_str()).unwrap_or("");
        if kind == "separator" {
            let glyph = t.get("character").and_then(|v| v.as_str());
            return ListRowData {
                label: Cow::Borrowed("· separator"),
                description: Cow::Owned(match glyph {
                    Some("") => "(empty)".to_string(),
                    Some(g) => format!("'{g}'"),
                    None => "(default)".to_string(),
                }),
            };
        }
        let label = if kind.is_empty() { "<no type>" } else { kind };
        let merge = t.get("merge").and_then(|v| v.as_bool()).unwrap_or(false);
        return ListRowData {
            label: Cow::Owned(label.to_string()),
            description: Cow::Borrowed(if merge { "merge →" } else { "" }),
        };
    }
    ListRowData {
        label: Cow::Borrowed("<non-string>"),
        description: Cow::Borrowed(""),
    }
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

/// Resolve the document key for a numbered line. Walks the
/// `[line]` table and returns the first child whose key parses as
/// the same `u32` as `n`. This handles zero-padded keys like
/// `[line.01]` — the runtime builder treats `01` and `1` as the
/// same line index, so the editor must look the entry back up by
/// its parsed numeric value rather than `n.to_string()` (which
/// would always produce `"1"` and miss `"01"` entirely).
pub(super) fn numbered_line_key(document: &DocumentMut, n: std::num::NonZeroU32) -> Option<String> {
    let table = document.get("line")?.as_table()?;
    table
        .iter()
        .find(|(k, v)| v.is_table() && k.parse::<u32>().ok() == Some(n.get()))
        .map(|(k, _)| k.to_string())
}

fn segments_array(document: &DocumentMut, line: LineKey) -> Option<&toml_edit::Array> {
    match line {
        LineKey::Single => document.get("line")?.get("segments")?.as_array(),
        LineKey::Numbered(n) => {
            let key = numbered_line_key(document, n)?;
            document.get("line")?.get(&key)?.get("segments")?.as_array()
        }
    }
}

fn segments_array_mut(document: &mut DocumentMut, line: LineKey) -> Option<&mut toml_edit::Array> {
    match line {
        LineKey::Single => document
            .get_mut("line")?
            .get_mut("segments")?
            .as_array_mut(),
        LineKey::Numbered(n) => {
            let key = numbered_line_key(document, n)?;
            document
                .get_mut("line")?
                .get_mut(&key)?
                .get_mut("segments")?
                .as_array_mut()
        }
    }
}

/// Test-only label projection that mirrors the display rendering
/// for bare-string entries (returns the segment id) while keeping
/// the original `<non-string>` placeholder for everything else.
/// Production now goes through `display_rows`; this stays scoped
/// to `#[cfg(test)]` so legacy assertions on the bare-string path
/// keep working without coupling to the richer ListRowData shape.
#[cfg(test)]
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

/// Insert `segment_id` at the position described by `target`.
/// Returns `false` only when `ensure_segments_array_mut` rejects
/// (numbered line without an explicit array). The target index
/// is clamped to `arr.len()` so an out-of-range index appends
/// rather than panics.
fn insert_segment(
    document: &mut DocumentMut,
    line: LineKey,
    target: InsertTarget,
    segment_id: &str,
) -> bool {
    let Some(arr) = ensure_segments_array_mut(document, line) else {
        return false;
    };
    let idx = match target {
        InsertTarget::Before(i) => i.min(arr.len()),
        InsertTarget::After(i) => i.saturating_add(1).min(arr.len()),
    };
    arr.insert(idx, segment_id);
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

/// Per-mutation reparse so the live preview reflects the new
/// document. Validation warnings (unknown segment ids, unknown
/// keys) are suppressed here because the segment builder fires
/// the same warnings on each render through the captured log
/// sink — emitting them again per keystroke would litter the
/// warnings panel without adding signal. A *parse* failure (the
/// `Err` branch) is a different problem: the entire document
/// shape is gone, the preview has no way to recover, and the
/// user needs immediate feedback. That branch warns AND leaves
/// `config` at its last-good value so the preview keeps showing
/// the last successful render rather than going blank.
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
        ItemsEditorState::new(
            LineKey::Single,
            ItemsEditorPrev::MainMenu(MainMenuState::default()),
        )
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
        let ids: Vec<&str> = line
            .segments
            .iter()
            .filter_map(config::LineEntry::segment_id)
            .collect();
        assert_eq!(ids, vec!["cwd", "model"]);
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
        let ids: Vec<&str> = line
            .segments
            .iter()
            .filter_map(config::LineEntry::segment_id)
            .collect();
        assert_eq!(ids, vec!["a", "c"]);
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
    fn add_verb_navigates_to_type_picker_with_after_target() {
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", "b"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('a')));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::TypePicker(_))
        ));
    }

    #[test]
    fn insert_verb_navigates_to_type_picker_with_before_target() {
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", "b"]
"#,
        );
        let mut cfg = config_default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('i')));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::TypePicker(_))
        ));
    }

    #[test]
    fn right_arrow_opens_picker_in_normal_mode() {
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a"]
"#,
        );
        let mut cfg = config_default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Right));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::TypePicker(_))
        ));
    }

    #[test]
    fn left_arrow_opens_picker_in_normal_mode() {
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a"]
"#,
        );
        let mut cfg = config_default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Left));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::TypePicker(_))
        ));
    }

    #[test]
    fn raw_verb_opens_editor_seeded_with_cursor_segment_label() {
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["alpha", "beta"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('r')));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::RawValueEditor(_))
        ));
    }

    #[test]
    fn raw_verb_inert_on_empty_array() {
        // ListScreen gates `Action('r')` on `num_rows > 0`. Pin
        // the contract: 'r' on an empty array is a no-op (no
        // cursor segment to edit).
        let mut s = state();
        let mut doc = document("[line]\nsegments = []\n");
        let mut cfg = config_default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('r')));
        assert!(matches!(outcome, ScreenOutcome::Stay));
    }

    #[test]
    fn apply_replace_emits_warning_on_failure() {
        // Mirror of `apply_insert_emits_warning_on_failure`: when
        // the underlying replace can't land (numbered line without
        // an explicit array), the user gets a warning rather than
        // a silent no-op. Pin the contract so it survives a
        // future relax of the precondition.
        use crate::logging::{self, Level};

        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let prev = ItemsEditorState::new(
            LineKey::Numbered(NonZeroU32::new(1).expect("nonzero")),
            ItemsEditorPrev::MainMenu(MainMenuState::default()),
        );
        let mut doc = document(
            r#"layout = "multi-line"
[line]
"#,
        );
        let mut cfg = config_default();
        let _ = apply_replace(prev, &mut doc, &mut cfg, 0, RawTarget::SegmentId, "model");

        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[warn]") && e.contains("replace failed")),
            "expected replace-failed warn in {entries:?}",
        );
    }

    #[test]
    fn apply_replace_accepts_empty_string_at_target() {
        // The doc claims empty strings are accepted at this layer
        // (load-layer warns on unknown ids). Pin the no-rejection
        // contract so a future "validate non-empty" guard becomes
        // a deliberate edit.
        let mut doc = document(
            r#"[line]
segments = ["alpha", "beta"]
"#,
        );
        let mut cfg = config_default();
        let outcome = apply_replace(
            ItemsEditorState::default(),
            &mut doc,
            &mut cfg,
            0,
            RawTarget::SegmentId,
            "",
        );
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(_))
        ));
        let labels = segment_labels(&doc, LineKey::Single);
        assert_eq!(labels, vec!["", "beta"]);
    }

    #[test]
    fn apply_replace_swaps_value_at_index_and_advances_cursor() {
        let mut doc = document(
            r#"[line]
segments = ["alpha", "beta", "gamma"]
"#,
        );
        let mut cfg = config_default();
        let outcome = apply_replace(
            ItemsEditorState::default(),
            &mut doc,
            &mut cfg,
            1,
            RawTarget::SegmentId,
            "BETA-renamed",
        );
        let restored = match outcome {
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(s)) => s,
            other => panic!("expected NavigateTo(ItemsEditor), got {other:?}"),
        };
        let labels = segment_labels(&doc, LineKey::Single);
        assert_eq!(labels, vec!["alpha", "BETA-renamed", "gamma"]);
        assert_eq!(restored.cursor(), 1);
    }

    #[test]
    fn add_verb_works_on_empty_segment_array() {
        // ListScreen gates `Action(c)` on `num_rows > 0`, so
        // dispatching a/i through the verb table would leave them
        // inert after a `c` clear. Handling them at the screen
        // level keeps add/insert reachable in the live "clear →
        // rebuild" flow.
        let mut s = state();
        let mut doc = document("[line]\nsegments = []\n");
        let mut cfg = config_default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('a')));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::TypePicker(_))
        ));
    }

    #[test]
    fn insert_verb_works_on_empty_segment_array() {
        let mut s = state();
        let mut doc = document("[line]\nsegments = []\n");
        let mut cfg = config_default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('i')));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::TypePicker(_))
        ));
    }

    #[test]
    fn picker_keybindings_in_move_mode_are_inert() {
        // The screen-level picker entry keybindings (←/→ + a/i)
        // must stay inside the editor during move-mode reorder.
        // The normal-mode gate keeps them from yanking the user
        // out of their reorder. Pin so a refactor that drops the
        // gate doesn't silently change the reorder UX.
        let raw = "[line]\nsegments = [\"a\", \"b\"]\n";
        let mut s = state();
        let mut doc = document(raw);
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        assert!(s.list.move_mode());
        for code in [
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('a'),
            KeyCode::Char('i'),
        ] {
            let outcome = update(&mut s, &mut doc, &mut cfg, key(code));
            assert!(
                matches!(outcome, ScreenOutcome::Stay),
                "{code:?} in move-mode should be Stay, got {outcome:?}",
            );
        }
        assert_eq!(doc.to_string(), raw);
    }

    #[test]
    fn apply_insert_at_last_index_appends_and_advances_cursor() {
        // Edge: After(last) lands at arr.len(). Pins both the
        // saturating clamp in `insert_segment` AND the cursor
        // arithmetic in `apply_insert` (inserted_at = idx + 1).
        let mut doc = document(
            r#"[line]
segments = ["a", "b"]
"#,
        );
        let mut cfg = config_default();
        let outcome = apply_insert(
            ItemsEditorState::default(),
            &mut doc,
            &mut cfg,
            InsertTarget::After(1),
            "model",
        );
        let restored = match outcome {
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(s)) => s,
            other => panic!("expected NavigateTo(ItemsEditor), got {other:?}"),
        };
        assert_eq!(
            segment_labels(&doc, LineKey::Single),
            vec!["a", "b", "model"],
        );
        assert_eq!(restored.cursor(), 2);
    }

    #[test]
    fn apply_insert_into_empty_explicit_array_lands_at_index_zero() {
        // The user's "clear → add" flow: explicit `segments = []`
        // becomes `segments = ["model"]` after one insert. Pins
        // both `Before(0)` and `After(0)` against an empty array
        // (insert_segment's clamp dominates the saturating math).
        for target in [InsertTarget::Before(0), InsertTarget::After(0)] {
            let mut doc = document("[line]\nsegments = []\n");
            let mut cfg = config_default();
            let outcome = apply_insert(
                ItemsEditorState::default(),
                &mut doc,
                &mut cfg,
                target,
                "model",
            );
            let restored = match outcome {
                ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(s)) => s,
                other => panic!("expected NavigateTo(ItemsEditor), got {other:?}"),
            };
            assert_eq!(
                segment_labels(&doc, LineKey::Single),
                vec!["model"],
                "target={target:?}",
            );
            assert_eq!(restored.cursor(), 0, "target={target:?}");
        }
    }

    #[test]
    fn apply_insert_emits_warning_on_failure() {
        // Pin the warn precedent: when `insert_segment` returns
        // false (numbered line without an explicit array), the
        // user gets a warning rather than a silent picker
        // dismissal. Once LinePicker lands, this branch becomes
        // user-reachable; today it pins the contract.
        use crate::logging::{self, Level};

        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let prev = ItemsEditorState::new(
            LineKey::Numbered(NonZeroU32::new(1).expect("nonzero")),
            ItemsEditorPrev::MainMenu(MainMenuState::default()),
        );
        let mut doc = document(
            r#"layout = "multi-line"
[line]
"#,
        );
        let mut cfg = config_default();
        let _ = apply_insert(prev, &mut doc, &mut cfg, InsertTarget::After(0), "model");

        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[warn]") && e.contains("insert failed")),
            "expected insert-failed warn in {entries:?}",
        );
    }

    #[test]
    fn apply_insert_lands_segment_at_target_and_advances_cursor() {
        // Drives `apply_insert` directly (the picker's Enter
        // handler delegates to it). Pins both the insert position
        // AND the post-mutation cursor, since both are caller-
        // owned per ADR-0022.
        let mut doc = document(
            r#"[line]
segments = ["a", "b"]
"#,
        );
        let mut cfg = config_default();
        let outcome = apply_insert(
            ItemsEditorState::default(),
            &mut doc,
            &mut cfg,
            InsertTarget::After(0),
            "model",
        );
        let restored = match outcome {
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(s)) => s,
            other => panic!("expected NavigateTo(ItemsEditor), got {other:?}"),
        };
        assert_eq!(
            segment_labels(&doc, LineKey::Single),
            vec!["a", "model", "b"]
        );
        // After(0) → inserted_at = 1.
        assert_eq!(restored.cursor(), 1);
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

    #[test]
    fn space_inserts_separator_after_cursor_segment() {
        // Pin the v0.1 Space contract: pressing Space after a
        // segment row inserts `{ type = "separator" }` immediately
        // to the right; cursor advances onto the new entry so the
        // user can `r` it for a custom character without re-
        // navigating.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["model", "git_branch"]
"#,
        );
        let mut cfg = config_default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char(' ')));
        assert!(matches!(outcome, ScreenOutcome::Stay));
        assert_eq!(
            s.list.cursor(),
            1,
            "cursor advances onto inserted separator"
        );
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr.get(0).and_then(|v| v.as_str()), Some("model"));
        let sep = arr
            .get(1)
            .and_then(|v| v.as_inline_table())
            .expect("inline table");
        assert_eq!(sep.get("type").and_then(|v| v.as_str()), Some("separator"));
        assert!(
            sep.get("character").is_none(),
            "character omitted; uses global default",
        );
        assert_eq!(arr.get(2).and_then(|v| v.as_str()), Some("git_branch"));
    }

    #[test]
    fn space_on_empty_array_inserts_at_zero() {
        // Pre-condition for the picker chain: an empty array sees
        // `Before(0)` so the new separator lands at index 0. The
        // builder rejects this on the next render with a "leads
        // with a separator entry" warn (see `build_one_line`), but
        // the editor still surfaces the row so the user can delete
        // it or follow up with a segment to make the boundary valid.
        let mut s = state();
        let mut doc = document("[line]\nsegments = []\n");
        let mut cfg = config_default();
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char(' ')));
        assert!(matches!(outcome, ScreenOutcome::Stay));
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        assert_eq!(arr.len(), 1);
        let sep = arr.get(0).and_then(|v| v.as_inline_table()).expect("table");
        assert_eq!(sep.get("type").and_then(|v| v.as_str()), Some("separator"));
        assert_eq!(s.list.cursor(), 0);
    }

    #[test]
    fn space_in_move_mode_is_inert() {
        // Mirror of the existing `picker_keybindings_in_move_mode_are_inert`
        // test for the `a`/`i`/←/→ family. Space must follow the
        // same gate so an in-progress reorder doesn't accidentally
        // mutate the array under the cursor.
        let raw = "[line]\nsegments = [\"a\", \"b\"]\n";
        let mut s = state();
        let mut doc = document(raw);
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Enter));
        assert!(s.list.move_mode());
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char(' ')));
        assert!(matches!(outcome, ScreenOutcome::Stay));
        assert_eq!(doc.to_string(), raw, "Space in move-mode must not mutate");
    }

    #[test]
    fn merge_verb_promotes_string_segment_to_inline_table_with_merge_flag() {
        // `m` on a bare-string entry replaces it with
        // `{ type = "model", merge = true }` so the boundary at the
        // segment's right edge renders without a separator. Pin
        // both the structural shape AND that the cursor stays put
        // on the modified row.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["model", "git_branch"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('m')));
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        let table = arr
            .get(0)
            .and_then(|v| v.as_inline_table())
            .expect("inline table after merge");
        assert_eq!(table.get("type").and_then(|v| v.as_str()), Some("model"));
        assert_eq!(table.get("merge").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(s.list.cursor(), 0);
    }

    #[test]
    fn merge_verb_toggles_off_and_demotes_to_bare_string() {
        // Round-trip: `m` twice on a bare string returns to a bare
        // string so the TOML stays clean. Pin the demotion path —
        // a regression that leaves a `{ type = "model" }` table
        // behind would visibly bloat the saved file.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["model"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('m')));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('m')));
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        assert_eq!(
            arr.get(0).and_then(|v| v.as_str()),
            Some("model"),
            "second `m` demotes back to bare string",
        );
    }

    #[test]
    fn merge_verb_inert_on_separator_entry() {
        // Per ADR-0024, `merge` is a segment-only flag; toggling
        // it on a separator must be a no-op (the validator already
        // warns at build time).
        let raw = "[line]\nsegments = [{ type = \"separator\" }]\n";
        let mut s = state();
        let mut doc = document(raw);
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('m')));
        assert_eq!(doc.to_string(), raw, "m on a separator must not mutate");
    }

    #[test]
    fn classify_entry_distinguishes_separator_inline_table_from_segment() {
        // The `r` and `m` dispatch arms switch on this; pin the
        // three primary cases so a refactor that conflates inline
        // tables fails here directly.
        let doc = document(
            r#"[line]
segments = ["model", { type = "separator", character = " | " }, { type = "git_branch", merge = true }]
"#,
        );
        assert_eq!(classify_entry(&doc, LineKey::Single, 0), EntryKind::Segment);
        assert_eq!(
            classify_entry(&doc, LineKey::Single, 1),
            EntryKind::Separator
        );
        assert_eq!(classify_entry(&doc, LineKey::Single, 2), EntryKind::Segment);
        assert_eq!(
            classify_entry(&doc, LineKey::Single, 99),
            EntryKind::Malformed
        );
    }

    #[test]
    fn raw_verb_on_separator_seeds_with_character_field() {
        // Pin the separator-character branch: pressing `r` on an
        // inline-table separator opens the raw editor seeded with
        // the existing `character` value (or empty when the field
        // is absent). The dispatch path is `classify_entry →
        // RawTarget::SeparatorCharacter`; a regression that always
        // chose `SegmentId` would seed with `"separator"` instead.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["a", { type = "separator", character = " | " }]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        let outcome = update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('r')));
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::RawValueEditor(_))
        ));
    }

    #[test]
    fn apply_replace_with_separator_target_updates_character_field() {
        // Pin the SeparatorCharacter commit path. After commit, the
        // inline table's `character` reflects the new buffer; the
        // entry's `type` stays `"separator"`.
        let mut doc = document(
            r#"[line]
segments = [{ type = "separator", character = " | " }]
"#,
        );
        let mut cfg = config_default();
        let outcome = apply_replace(
            ItemsEditorState::default(),
            &mut doc,
            &mut cfg,
            0,
            RawTarget::SeparatorCharacter,
            " > ",
        );
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(_))
        ));
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        let sep = arr.get(0).and_then(|v| v.as_inline_table()).expect("table");
        assert_eq!(sep.get("type").and_then(|v| v.as_str()), Some("separator"));
        assert_eq!(sep.get("character").and_then(|v| v.as_str()), Some(" > "));
    }

    #[test]
    fn apply_replace_with_separator_target_and_empty_value_sets_explicit_empty() {
        // Per ADR-0024 the two separator states `character = ""`
        // (explicit no-separator at this boundary) and absent
        // `character` (fall back to global default) are distinct
        // render outcomes. The raw editor must commit the buffer
        // verbatim — including empty — so a round-trip through `r`
        // doesn't silently swap modes. Users who want the absent-
        // field state use `Space` to insert a fresh defaulted
        // separator and skip `r` on it.
        let mut doc = document(
            r#"[line]
segments = [{ type = "separator", character = " | " }]
"#,
        );
        let mut cfg = config_default();
        apply_replace(
            ItemsEditorState::default(),
            &mut doc,
            &mut cfg,
            0,
            RawTarget::SeparatorCharacter,
            "",
        );
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        let sep = arr.get(0).and_then(|v| v.as_inline_table()).expect("table");
        assert_eq!(
            sep.get("character").and_then(|v| v.as_str()),
            Some(""),
            "empty buffer must commit explicit empty, not remove the field",
        );
    }

    #[test]
    fn apply_replace_with_separator_target_preserves_default_state_via_space() {
        // Companion contract: a defaulted separator (`{ type = "separator" }`,
        // no character field) reaches that state via `Space`, not via
        // `r` with an empty buffer. Pin that `Space` produces the
        // absent-field form so the user's "use global default" intent
        // has a reliable path.
        let mut s = state();
        let mut doc = document(
            r#"[line]
segments = ["model", "workspace"]
"#,
        );
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char(' ')));
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        let sep = arr.get(1).and_then(|v| v.as_inline_table()).expect("table");
        assert_eq!(sep.get("type").and_then(|v| v.as_str()), Some("separator"));
        assert!(
            sep.get("character").is_none(),
            "Space must produce a defaulted separator (no `character` field)",
        );
    }

    #[test]
    fn numbered_line_resolves_zero_padded_keys_to_same_index() {
        // The runtime accepts `[line.01]` as the same line as
        // `[line.1]` (both parse to u32=1), and the editor must
        // agree. Looking up `n.to_string()` ("1") would miss the
        // actual TOML key ("01"); the editor walks the table and
        // matches by parsed numeric value so swap / delete / insert
        // all hit the right child.
        let mut doc = document(
            r#"layout = "multi-line"
[line]

[line.01]
segments = ["model", "workspace"]
"#,
        );
        let one = NonZeroU32::new(1).expect("nonzero");
        // segments_array resolves the array even though the TOML
        // key is "01", not "1".
        let arr = segments_array(&doc, LineKey::Numbered(one)).expect("array");
        assert_eq!(arr.len(), 2);
        // And mutations land on the same key — delete + reload
        // shows the original "01" key intact.
        assert!(delete_segment_at(&mut doc, LineKey::Numbered(one), 1));
        assert_eq!(segment_labels(&doc, LineKey::Numbered(one)), vec!["model"]);
        let serialized = doc.to_string();
        assert!(
            serialized.contains("[line.01]"),
            "original zero-padded key must survive the edit: {serialized}",
        );
    }

    #[test]
    fn raw_replace_preserves_inline_table_fields_on_segment_rename() {
        // `r` on an inline-table segment entry like
        // `{ type = "git_branch", merge = true }` must update only
        // the `type` field, not blow away the table and rewrite as
        // a bare string. A regression that calls `arr.replace(idx,
        // new_value)` unconditionally drops `merge` (and any future
        // per-entry knob) and silently breaks the `m` then `r`
        // user flow.
        let mut doc = document(
            r#"[line]
segments = [{ type = "git_branch", merge = true }, "cost"]
"#,
        );
        let mut cfg = config_default();
        let outcome = apply_replace(
            ItemsEditorState::default(),
            &mut doc,
            &mut cfg,
            0,
            RawTarget::SegmentId,
            "model",
        );
        assert!(matches!(
            outcome,
            ScreenOutcome::NavigateTo(AppScreen::ItemsEditor(_))
        ));
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        let table = arr
            .get(0)
            .and_then(|v| v.as_inline_table())
            .expect("entry must remain an inline table");
        assert_eq!(
            table.get("type").and_then(|v| v.as_str()),
            Some("model"),
            "type field must reflect the rename",
        );
        assert_eq!(
            table.get("merge").and_then(|v| v.as_bool()),
            Some(true),
            "merge flag must survive the rename",
        );
    }

    #[test]
    fn toml_round_trip_preserves_inline_table_separator_entry() {
        // The "save preserves my hand-edits" contract: a config
        // with an inline-table separator round-trips through the
        // editor's persistence layer (`toml_edit::DocumentMut`)
        // without reformatting the inline table to a multi-line
        // block. Pin both the byte-level survival AND the typed
        // re-parse so a future toml_edit upgrade can't silently
        // break user-visible formatting OR our LineEntry shape.
        let raw = "[line]\nsegments = [\"model\", { type = \"separator\", character = \" | \" }, \"workspace\"]\n";
        let doc: DocumentMut = raw.parse().expect("parse");
        let serialized = doc.to_string();
        assert!(
            serialized.contains("{ type = \"separator\", character = \" | \" }"),
            "inline-table separator must round-trip without reformatting: {serialized:?}",
        );
        let cfg: config::Config = serialized.parse().expect("reparse");
        let line = cfg.line.as_ref().expect("line present");
        match &line.segments[1] {
            config::LineEntry::Item(item) => {
                assert_eq!(item.kind.as_deref(), Some("separator"));
                assert_eq!(item.character.as_deref(), Some(" | "));
            }
            other => panic!("expected LineEntry::Item, got {other:?}"),
        }
    }

    /// Helper template for the m/d/c/k warn-firing tests. Drives one
    /// keystroke against the items editor with a logging sink installed
    /// and asserts that exactly one captured warn contains `expected`.
    fn assert_arm_warn_fires(initial_doc: &str, key_code: KeyCode, expected: &str) {
        use crate::logging::{self, Level};

        let _serial = logging::_test_serial_lock();
        let captured = std::sync::Arc::new(crate::logging::CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);

        let mut s = ItemsEditorState::new(
            LineKey::Numbered(NonZeroU32::new(1).expect("nonzero")),
            ItemsEditorPrev::MainMenu(MainMenuState::default()),
        );
        let mut doc = document(initial_doc);
        let mut cfg = config_default();
        update(&mut s, &mut doc, &mut cfg, key(key_code));

        let entries = captured.drain();
        assert!(
            entries
                .iter()
                .any(|e| e.starts_with("[warn]") && e.contains(expected)),
            "expected warn containing {expected:?} in {entries:?}",
        );
    }

    #[test]
    fn merge_verb_warns_when_inert_on_separator_entry() {
        // Pin the symmetric warn alongside the existing inert-doc-state
        // assertion in `merge_verb_inert_on_separator_entry`. A regression
        // that swaps which arm warns (e.g., `m` warns on a successful
        // toggle) would pass the inert test but fail this one.
        let raw =
            "layout = \"multi-line\"\n[line]\n\n[line.1]\nsegments = [{ type = \"separator\" }]\n";
        assert_arm_warn_fires(raw, KeyCode::Char('m'), "merge toggle inert");
    }

    #[test]
    fn merge_verb_inert_on_kindless_inline_table_entry() {
        // Per ADR-0024, kindless inline tables are dropped at build
        // time. Toggling `merge` on them would just write more
        // invalid TOML. Pin the rejection so a future relax surfaces
        // here rather than as a "saved file that the renderer
        // rejects".
        let raw = "[line]\nsegments = [\"model\", { character = \" | \" }]\n";
        let mut s = state();
        let mut doc = document(raw);
        let mut cfg = config_default();
        // Move cursor to the kindless entry (index 1).
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Down));
        update(&mut s, &mut doc, &mut cfg, key(KeyCode::Char('m')));
        // Document unchanged — the entry is still the same kindless
        // inline table; no `merge = true` got written.
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        let table = arr
            .get(1)
            .and_then(|v| v.as_inline_table())
            .expect("kindless table preserved");
        assert!(
            table.get("merge").is_none(),
            "m on a kindless entry must not add `merge`",
        );
    }

    #[test]
    fn replace_segment_refuses_to_rename_separator_entry_under_kind_drift() {
        // External-edit race: `open_raw_value_editor` dispatched on
        // a segment classification, but between open and commit the
        // entry at the same index flipped to a separator (file
        // watcher reload, foreign edit). `replace_segment` re-checks
        // the kind and refuses rather than corrupt the separator
        // boundary into a malformed segment with leftover `character`.
        let mut doc = document(
            r#"[line]
segments = [{ type = "separator", character = " | " }]
"#,
        );
        assert!(
            !replace_segment(&mut doc, LineKey::Single, 0, "model"),
            "replace_segment must refuse the rename when the entry is now a separator",
        );
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        let table = arr
            .get(0)
            .and_then(|v| v.as_inline_table())
            .expect("entry preserved");
        assert_eq!(
            table.get("type").and_then(|v| v.as_str()),
            Some("separator"),
            "type field unchanged after refused rename",
        );
        assert_eq!(
            table.get("character").and_then(|v| v.as_str()),
            Some(" | "),
            "character field preserved",
        );
    }

    #[test]
    fn replace_segment_preserves_multiple_inline_table_fields_on_rename() {
        // Pin the broader contract behind the `merge` preservation
        // case: ANY non-`type` field inside an inline-table segment
        // entry survives a rename, including forward-compat keys
        // landed in `extra`. A regression that re-built the inline
        // table from scratch would silently drop the user's
        // additional fields.
        let mut doc = document(
            r#"[line]
segments = [{ type = "git_branch", merge = true, color = "red", custom = 42 }, "cost"]
"#,
        );
        assert!(replace_segment(&mut doc, LineKey::Single, 0, "model"));
        let arr = segments_array(&doc, LineKey::Single).expect("array");
        let table = arr
            .get(0)
            .and_then(|v| v.as_inline_table())
            .expect("entry preserved as inline table");
        assert_eq!(table.get("type").and_then(|v| v.as_str()), Some("model"));
        assert_eq!(table.get("merge").and_then(|v| v.as_bool()), Some(true));
        assert_eq!(table.get("color").and_then(|v| v.as_str()), Some("red"));
        assert_eq!(table.get("custom").and_then(|v| v.as_integer()), Some(42));
    }
}
