# TUI items editor operates on DocumentMut directly

- Status: accepted
- Date: 2026-05-07
- Deciders: Jace
- Surfacing bead: lsm-q2t3

## Context and Problem Statement

The items editor (lsm-herx.7) lets the user reorder, insert, duplicate, and delete entries in `[line].segments` via move-mode and verb-letters. The natural-looking type to operate on is `Vec<LineItem>` — the rendered form the layout engine consumes. But `LineItem::Segment(Box<dyn Segment>)` is `!Clone`: the trait object can't be naively duplicated for the editor's "k)lone" verb, and a future plugin segment with cached state (compiled rhai AST, memoized regex) makes "what does cloning mean" non-obvious.

Type-design-analyzer flagged this during lsm-herx.2 review and recommended a sibling `LineItemConfig` enum that round-trips TOML cheaply (`Clone + Serialize`), with `Vec<LineItem>` materialized only at render time. The alternative was `Segment::clone_box(&self) -> Box<dyn Segment>` cascading to every built-in segment impl plus `RhaiSegment` — the cascade hits a couple dozen sites today and grows with each new segment type.

What's the items editor's source of truth — a parallel config-shaped type, a clonable trait object, or something else?

## Decision Drivers

- **ADR-0016 commits the model's saveable state to `toml_edit::DocumentMut`.** The shipped `Model` carries `document: DocumentMut` plus a parsed `config: config::Config` snapshot. Save and dirty-check are already wired against the document. Adding a third source-of-truth (a typed mid-layer) would force per-mutation sync between three representations.
- **Plugin clone semantics are ambiguous.** `RhaiSegment` holds a compiled AST + per-instance state. A `clone_box` impl has to choose between `Arc`-share (cheap, but plugin authors can't reason about state isolation), deep-recompile (expensive, surprising on duplicate), or refuse (`unimplemented!`, surfaces as a runtime error in the editor). None is a clean default for a feature that fires from a verb-letter.
- **The "clone" verb's user-facing meaning isn't deep-copy.** A user duplicating `model` in their segments list expects "another model entry that I can configure separately", not "an exact byte-clone of the in-memory rendered segment". The TOML-level operation (insert another `"model"` string) matches user intent better than the trait-level clone.
- **Cascade cost is real.** `Segment::clone_box` would touch every built-in segment impl plus `RhaiSegment`, and would have to be re-implemented for every future segment. The trait-fattening tax is paid forever.
- **Round-trip already exists.** `build_lines` accepts a parsed `Config` and returns `Vec<Vec<LineItem>>` (one inner vec per `[line.N]`). The items editor reuses this path; it doesn't need a new conversion layer.
- **Multi-line layout exists today.** Per `docs/specs/config.md`, `layout = "multi-line"` configs declare `[line.1]`, `[line.2]`, … sub-tables and the spec already round-trips them. The items editor must edit a _specific_ line's segments — the screen is entered from the Main Menu (single-line case) OR the Line Picker (multi-line case) with a target line key. Hard-coding the editor to `[line].segments` would silently break multi-line edits.

## Considered Options

- **Option 1 — Items editor operates on `model.document: DocumentMut` directly.** Reorder = `toml_edit` array swap on the segments array under the user's chosen `[line]` or `[line.N]` table. "k)lone" = duplicate string in the array. Type picker = string replacement. Display rows are computed on demand from the document. No `LineItem` clone surface needed.
- **Option 2 — Sibling `LineItemConfig` enum.** Define a parallel `Clone + Serialize` shape for items. Editor operates on `Vec<LineItemConfig>`; render path materializes `Vec<LineItem>` from it. Two parallel types to keep in sync.
- **Option 3 — `Segment::clone_box(&self) -> Box<dyn Segment>` (DynClone).** Add a clone method to the trait; cascade to every impl. `Vec<LineItem>: Clone` falls out for free.

## Decision Outcome

Chosen option: **Option 1 — operate on `model.document: DocumentMut` directly**, because (a) ADR-0016 already commits to `DocumentMut` as the saveable state and a parallel typed mid-layer would split it, (b) the user's "k)lone" intent maps cleanly to a string-array duplication at the TOML level rather than to a trait-object deep-copy, (c) `build_lines` accepts a parsed `Config` and is already the conversion path the live preview uses on each frame, and (d) `LineItem` and `Segment` stay free of editor-only concerns — cloneability becomes a non-question instead of a per-impl tax.

### Document → Config sync (load-bearing detail)

The shipped `Model` carries both `document: DocumentMut` (mutated by the editor + Ctrl+S writes) and `config: config::Config` (parsed once at boot, read by `preview::render_lines`). Today nothing refreshes `config` after `document` mutations — the items editor lands the first writer of the document and forces a decision. Two options:

- **Sync `config` after each document mutation.** Items editor calls `model.config = Config::from_str_validated(&model.document.to_string(), warn)` after each move/insert/delete/clone. Two representations to keep aligned; mutations cost a parse.
- **Drop `config` and re-parse for the preview each frame.** `preview::render_lines` reads `model.document`, parses on demand, renders. One source of truth; per-frame parse cost.

This ADR commits to the first option — sync `config` after each mutation — because the parse runs at user-event rate (≤1 per keystroke in move-mode, less in normal mode), not at frame rate, so the cost stays bounded; and the existing `preview::render_lines(&model.config, ...)` signature stays untouched. A small editor-side helper (e.g., `model.refresh_config_from_document(warn)`) absorbs the call.

### Display rows

The items editor's display row data (label, description, type tag, verb hints) is computed on demand from the document by walking the `LineKey`-resolved segments array (`[line].segments` for `Single`, `[line.N].segments` for `Numbered`). Each segment ID is mapped to display strings through a new segment-id → metadata table the items editor introduces (today's `Segment::defaults()` only carries layout hints).

### Architecture

The items editor is entered with a target `LineKey` so the same screen handles both layout modes:

- **single-line config** (default, or `layout = "single-line"`): Main Menu → Items Editor with `LineKey::Single`. The editor mutates `[line].segments`.
- **multi-line config** (`layout = "multi-line"`): Main Menu → Line Picker → Items Editor with `LineKey::Numbered(N)`. The editor mutates `[line.N].segments` for the chosen line.

```rust
// In tui::items_editor (lsm-herx.7):

/// Which `[line]` / `[line.N]` table the editor is operating on.
/// `Numbered` carries `NonZeroU32` because the spec defines line
/// keys as positive integers; zero / non-numeric keys are dropped
/// at parse time per `docs/specs/config.md` §Edge cases.
pub enum LineKey {
    Single,
    Numbered(std::num::NonZeroU32),
}

pub struct ItemsEditorState {
    list: ListScreenState,
    line: LineKey,
    // No Vec<LineItem>, no Vec<LineItemConfig>.
    // Display rows materialized on demand from model.document.
}

fn rows_for_view(document: &DocumentMut, line: &LineKey) -> Vec<ListRowData<'_>> {
    let segments = segments_array(document, line)
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    segments.iter().map(|id| {
        // `lookup_segment_meta` is one of the few things herx.7
        // does need to build: today's `Segment::defaults()`
        // returns layout hints (priority, width), not human-
        // readable labels / descriptions. The items editor
        // introduces a static segment-id → display-metadata
        // table mapping (e.g.) "model" → ("Model", "Shows the AI
        // model name").
        let meta = lookup_segment_meta(id);
        ListRowData { label: meta.label.into(), description: meta.description.into() }
    }).collect()
}

fn handle_move_swap(
    document: &mut DocumentMut,
    line: &LineKey,
    from: usize,
    to: usize,
) -> Result<(), ItemsEditorError> {
    let segments = segments_array_mut(document, line)
        .ok_or(ItemsEditorError::MissingSegments)?;
    if from < segments.len() && to < segments.len() {
        // toml_edit doesn't expose Array::swap; the round-trip is
        // remove-from + insert-at-to (preserves trailing comments
        // on the moved entry).
        let item = segments.remove(from);
        segments.insert(to, item);
    }
    Ok(())
}

/// Resolve the segments array for `line`. The two paths walk
/// different parents (`[line]` vs `[line.N]`) but both terminate
/// at a `segments` array of strings. Mirror impl exists for `_mut`.
fn segments_array<'a>(document: &'a DocumentMut, line: &LineKey) -> Option<&'a Array> {
    match line {
        LineKey::Single => document
            .get("line")
            .and_then(|l| l.get("segments"))
            .and_then(|s| s.as_array()),
        LineKey::Numbered(n) => document
            .get("line")
            .and_then(|l| l.get(n.get().to_string()))
            .and_then(|t| t.get("segments"))
            .and_then(|s| s.as_array()),
    }
}
```

Per-segment config tables (`[model]`, `[git_branch]`, etc.) stay in their original positions in the document. Reordering a line's segments array doesn't move the config tables, and doesn't affect any other line.

### Consequences

- Good, because `Segment` and `LineItem` stay editor-agnostic; future plugin authors implementing `Segment` don't have to think about clone semantics.
- Good, because `model.document` is the single saveable source of truth — no sync logic between the editor's view and the saved file.
- Good, because the "k)lone" verb's semantics are honest about the user-facing operation (duplicate the segment ID in the array) rather than dressing up a trait-level deep-copy.
- Bad, because `model.config` and `model.document` aren't automatically in sync — the items editor pays a `Config::from_str_validated` parse per mutation to refresh `config` from the document. At user-event rate on a few KB of TOML this is sub-millisecond, but it's a real call the boot path doesn't make.
- Bad, because `toml_edit`'s array API (`Array::remove` + `Array::insert`) is more verbose than `Vec::swap`; the helper above absorbs that, and herx.7 will own one or two such helpers regardless.
- Bad, because per-segment config that should NOT be shared between duplicates (e.g., user clones `model` and wants the second instance to use a different format) needs separate-config handling. v0.1 punts on this — the user gets two `model` entries that share the same `[model]` config table. Per-instance configuration is a v0.2 design problem; not filed yet.
- Neutral, because we still don't have a `Clone` impl for `LineItem`. Tests that need a duplicated rendered segment construct it via `build_segments(...)` from a synthetic Config, same as the production path.

### Confirmation

Revisit if:

- The items editor finds itself doing enough document-walking that a typed `Vec<LineItemConfig>` cache materially simplifies the screen logic. Today the walk is one helper at most.
- A future v0.2 feature (e.g., per-instance segment config) needs cheap clone-with-distinct-state. At that point, the per-segment config keying problem is the harder design call; revisit whether `clone_box` is part of any answer rather than assuming this ADR settles it.
- ccstatusline diverges from "config-as-state" toward a typed mid-layer in a way that affects parity expectations.

## Pros and Cons of the Options

### Option 1 — Operate on `DocumentMut` directly

- Good: single saveable source of truth (matches ADR-0016).
- Good: zero changes to `Segment` / `LineItem` traits.
- Good: live preview reuses the existing `render_lines(&Config, ...)` path with a small `refresh_config_from_document` call after each mutation.
- Bad: per-frame display row materialization walks the array; trivially cheap at TOML scale, but not free.

### Option 2 — `LineItemConfig` sibling enum

- Good: typed mid-layer; editor doesn't pay per-frame parse cost.
- Bad: two sources of truth to sync (`DocumentMut` + `Vec<LineItemConfig>`).
- Bad: every segment type needs a `Config` variant matching the trait impl; cascade across the codebase.
- Bad: doesn't solve the per-instance config problem either.

### Option 3 — `Segment::clone_box`

- Good: `Vec<LineItem>: Clone` falls out.
- Bad: cascades to 22+ built-in impls + `RhaiSegment`.
- Bad: plugin clone semantics ambiguous; `RhaiSegment` has to choose between Arc-share (state aliasing) and deep-recompile (surprise expense).
- Bad: trait-fattening tax paid forever; future segment authors carry it.
- Bad: still doesn't represent user intent — "k)lone" means "another entry of the same type", not "byte-identical clone of this instance".

## More Information

- Bead: lsm-q2t3 (this design decision)
- Bead: lsm-herx.7 (Items Editor — first consumer)
- Companion: [ADR-0016](0016-tui-screen-state-machine.md) — already commits to `DocumentMut` as the model's source of truth
- Companion: [ADR-0021](0021-module-organization-conventions.md) — module organization conventions (folder-as-façade, lib.rs discipline). The TUI's existing per-screen module precedent (`main_menu.rs`, `placeholder.rs`) is in-codebase practice rather than a rule prescribed by ADR-0021; the items editor follows the same shape.
- Reference: type-design-analyzer's lsm-herx.2 round-1 recommendation surfaced this question
- Out of scope: per-instance segment configuration (e.g., two `model` entries with different formats) — needs a separate design pass when it becomes load-bearing
