# Per-boundary separator TOML uses a mixed string-or-table segment array

- Status: accepted
- Date: 2026-05-08
- Deciders: Jace
- Surfacing bead: lsm-herx.7

## Context and Problem Statement

[ADR-0008](0008-canonical-type-refinements.md) and the v0.7 segment-system spec moved separators out of `SegmentDefaults` and into positional `LineItem::Separator` entries the builder produces. The runtime walks a `Vec<LineItem>` and the layout engine handles per-boundary survival/drop. But the **TOML schema** still only carries segment IDs: `LineConfig.segments: Vec<String>`. The boundary glyph is a single global key (`[layout_options].separator`) interleaved at build time. The items editor's `Space` (edit separator at boundary) and `m` (merge with next segment, suppress separator) verbs need a per-boundary representation in the file.

How should per-boundary separator settings be expressed in TOML, given that the runtime can already consume them but the schema can't yet write them?

## Decision Drivers

- **Backward compatibility with the existing string array.** `segments = ["model", "git_branch"]` is the documented shape; the test suite, the example configs, and `with_schema_directive` output all use it. Breaking it forces a v0.x → v0.x+1 migration on every user.
- **Forward-compat "unknown keys warn, never error."** The spec contract (`docs/specs/config.md` §Validation) says scalar typos and future fields must parse cleanly. The `toml::Value`-typed flatten pattern (memory: `toml-serde-flatten-with-a-sibling-typed-field`) is the established way to preserve this.
- **ccstatusline parity for users porting their config.** ccstatusline's `WidgetItem` is a flat shape with `id`, `type`, `color`, `character`, `merge`, `hide`, etc. — both segments and separators are entries with the same field set, distinguished by `type`. Users migrating from ccstatusline shouldn't have to mentally restructure.
- **Reorder safety.** A user reordering segments via the items editor (`Enter` move-mode) shouldn't accidentally rebind separators to wrong boundaries. Boundary-index lookup tables (e.g., `[line.separators] 0 = " | "`) silently corrupt under reorder.
- **Single source of truth for each boundary.** The renderer reads one place per boundary; the editor writes one place per boundary; saved files don't drift between two tables describing the same boundary.

## Considered Options

- **Option 1 — Mixed array, untagged: strings or inline tables.** `segments = ["model", { type = "separator", character = " | " }, "git_branch"]`. Strings stay strings; only boundaries the user has touched promote to inline tables.
- **Option 2 — All inline tables.** `segments = [{ type = "model" }, { type = "separator" }, { type = "git_branch" }]`. Drop the string shorthand entirely.
- **Option 3 — Sibling table, boundary-indexed.** `segments = ["model", "git_branch"]` plus `[line.separators] 0 = " | "` indexed by boundary number.
- **Option 4 — Per-segment trailing separator on `[segments.<id>]`.** Each segment's override block grows a `right_separator` key.

## Decision Outcome

Chosen option: **Option 1 — mixed array of strings or inline tables**, because (a) every existing config keeps parsing untouched (string shorthand is preserved as `LineEntry::Id(String)`), (b) inline tables extend forward-compatibly via `BTreeMap<String, toml::Value>` so future ccstatusline-parity fields (`color`, `bold`, `merge`, `hide`, `character`) land without schema bumps, (c) the array order _is_ the boundary order — reorder via the items editor moves the separator with its neighbors instead of leaving stale indexed overrides behind, (d) ccstatusline's mental model ports over directly, and (e) the renderer's existing `LineItem::Segment | Separator` enum is the natural consumer; the builder just decides whether each entry materializes as `Segment(...)` or `Separator(...)`.

### TOML shape

```toml
[line]
segments = [
  "model",                                      # string → LineItem::Segment("model")
  { type = "separator", character = " | " },    # explicit per-boundary separator
  "git_branch",
  { type = "separator" },                       # explicit boundary using global default
  { type = "cost", merge = true },              # segment with merge flag (suppresses next sep)
]

[layout_options]
separator = " | "  # global default; applied between adjacent segments when no explicit
                   # Separator entry sits between them, AND consulted by inline-table
                   # `{ type = "separator" }` entries with no `character` of their own.
```

### Rust shape

```rust
/// One entry in `[line].segments`. The string shorthand is the default
/// for boundaries the user hasn't customized; the table form carries
/// per-entry settings (separator glyph, merge flag, future fields).
///
/// Named `LineEntry` rather than `SegmentEntry` because
/// `linesmith_core::layout` already uses the latter for the engine's
/// internal post-render entry shape.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum LineEntry {
    /// `"model"` — equivalent to `{ type = "model" }`.
    Id(String),
    /// `{ type = "...", ... }` — keys other than the typed fields land
    /// in `extra` per the forward-compat contract.
    Item(LineEntryItem),
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize, JsonSchema)]
#[serde(default)]
pub struct LineEntryItem {
    /// `"model"`, `"separator"`, `"git_branch"`, ... When absent or
    /// empty, the builder warns and drops the entry.
    #[serde(rename = "type")]
    pub kind: Option<String>,

    /// Separator glyph for `type = "separator"` entries. Ignored on
    /// non-separator entries (warned at build time).
    pub character: Option<String>,

    /// When true on a segment entry, the boundary to its right
    /// renders without a separator. Ignored on separator entries.
    pub merge: Option<bool>,

    /// Forward-compat: any other key (`color`, `bold`, `hide`, ...)
    /// parses into `extra` per the `toml::Value` flatten pattern.
    /// Today's builder warn-and-drops; future ADRs may consume.
    #[serde(flatten)]
    #[schemars(with = "serde_json::Value")]
    pub extra: BTreeMap<String, toml::Value>,
}
```

`LineConfig.segments` becomes `Vec<LineEntry>`. The builder's existing `Vec<String>`-walking sites translate by matching `LineEntry::Id(s) | LineEntry::Item(LineEntryItem { kind: Some(s), .. })` to extract the type tag, then routing `"separator"` → `LineItem::Separator(...)` and everything else → `LineItem::Segment(build_segment(s, ...))`.

### Backward compat

- Existing `segments = ["model", "git_branch"]` configs deserialize as `vec![Id("model"), Id("git_branch")]` and continue to interleave the global `[layout_options].separator` between them at build time. No behavioral change.
- The items editor adds inline-table entries only when the user invokes `Space` (insert/edit a separator) or `m` (set `merge = true`). A config that never sees those verbs stays a pure string array.
- Builder: unknown keys inside an inline table land in `LineEntryItem.extra` (per `docs/specs/config.md` §Validation — "unknown keys warn, never fail"). `type` missing warns and drops the entry. `character` on a non-separator entry warns and is ignored. `merge` on a separator entry warns and is ignored. Per-key typo diagnostics (e.g. `tpye` instead of `type`) are not yet surfaced at config-load time; the existing `validate_keys` pass walks only top-level / `[layout_options]` / `[segments.<id>]` shapes.

### Builder semantics

`build_lines` walks `Vec<LineEntry>` and produces `Vec<LineItem>`:

1. Each entry's effective `(kind, separator_char, merge_flag)` is extracted.
2. `Id(s)` → `(s, None, false)`. `Item { kind, character, merge, .. }` → `(kind?, character, merge.unwrap_or(false))`.
3. `kind == "separator"` → `LineItem::Separator(Separator::from_string(character.unwrap_or_else(|| layout_options.separator.clone())))`.
4. Otherwise → `LineItem::Segment(build_segment(kind, ...))`.
5. Adjacency pass: between two `LineItem::Segment` entries with **no** `LineItem::Separator` between them in the source, the global `[layout_options].separator` is interleaved (preserves today's behavior). Between two segments **with** an explicit `LineItem::Separator`, no extra interleave happens. A `merge = true` flag on the left segment suppresses any interleave at its right boundary (whether explicit or implicit).

The interleave-only-when-absent rule means existing `segments = ["model", "git_branch"]` configs stay byte-identical at the renderer; only configs that opt in to explicit boundary entries get explicit boundaries.

### Consequences

- Good, because every existing config keeps working — the mixed array is a strict superset of the string shorthand.
- Good, because the items editor's `Space` and `m` verbs map to first-class entries instead of side tables; reorder is just `Vec::swap` on `Vec<LineEntry>` and the separators move with their boundaries.
- Good, because ccstatusline-style flat widget shape ports directly: a future ADR can extend `LineEntryItem` with `color`, `bold`, `hide`, etc. without revisiting this decision.
- Good, because the builder change is bounded — every existing string-walking site becomes a kind-extracting site, and the `LineItem` enum that downstream consumes is unchanged.
- Bad, because `untagged` enums in serde produce less precise parse errors: a typo'd inline table (`{ tpye = "separator" }` for `type`) is reported as "data did not match any variant" rather than "unknown field `tpye`". Typo'd keys instead land in the entry's `extra` bag and surface at builder time as a "kindless inline table" warning rather than a per-key diagnostic; tightening this requires extending `validate_keys` to walk `[line].segments` array entries (follow-up). The forward-compat contract forces the loose shape: `extra: BTreeMap<String, toml::Value>` for unknown-key capture requires `Option<...>` typed fields plus `flatten`, and serde's tagged enum would reject unknown keys at parse time.
- Bad, because the JSON Schema for `untagged` is a `oneOf` of two shapes; editor autocomplete is slightly less helpful inside inline-table entries than inside a single-shape array.
- Neutral, because the `merge` semantics ("suppress separator at right boundary") is a simpler subset of ccstatusline's `merge: boolean | 'no-padding'`. v0.1 ships boolean only; if `'no-padding'` (suppress separator AND padding) becomes load-bearing, extending to a `Merge { Bool(bool), NoPadding }` enum is additive.

### Confirmation

Revisit if:

- ccstatusline diverges from "flat widget shape" in a way that breaks parity expectations for porting users.
- A future segment field needs to land on the entry rather than the `[segments.<id>]` override block, and the `extra` flatten doesn't accommodate it cleanly.
- Editor diagnostics from `untagged` parse errors prove user-hostile in practice; the fallback is to add a typed-error pass that re-parses each entry as `toml::Value` and emits per-key warnings when the strict deserialize fails.

## Pros and Cons of the Options

### Option 1 — Mixed array, untagged

- Good: zero migration; existing string arrays stay strings.
- Good: forward-compat via `extra: BTreeMap<String, toml::Value>`.
- Good: array order = boundary order — reorder is correct by construction.
- Good: ccstatusline mental model ports over.
- Bad: `untagged` parse errors are less precise (mitigated only at builder time today; per-key validator-pass coverage is a follow-up).

### Option 2 — All inline tables

- Good: one shape, predictable JSON Schema.
- Bad: every existing config breaks; users must rewrite `segments = ["model"]` as `segments = [{ type = "model" }]`.
- Bad: visual noise for the common case (configs with no per-boundary customization).

### Option 3 — Sibling table, boundary-indexed

- Good: keeps the segments array as pure strings.
- Bad: reorder via the items editor desyncs `[line.separators]` indices. Either every reorder rewrites the sibling table (two writers per move) or the user gets silently-wrong separators.
- Bad: two sources of truth per boundary; render path consults both.
- Bad: doesn't represent ccstatusline's mental model — separators are first-class items there, not side data.

### Option 4 — Per-segment trailing separator on `[segments.<id>]`

- Good: no change to `[line].segments`.
- Bad: `[segments.<id>]` is shared across every line that uses the segment — a single `[segments.git_branch]` block can't hold one separator for line 1 and a different one for line 2.
- Bad: returns to the pre-v0.7 "segment owns separator" mental model the runtime just left.
- Bad: doesn't represent merge (`m`) — that's a per-position flag, not a per-segment-id one.

## More Information

- Bead: lsm-herx.7 (items editor — first consumer)
- Companion: [ADR-0008](0008-canonical-type-refinements.md) — `Separator::Literal` Cow semantics
- Companion: [ADR-0023](0023-tui-items-editor-data-model.md) — items editor operates on `DocumentMut` directly
- Spec: `docs/specs/segment-system.md` v0.7 (separator-as-item refactor)
- Spec: `docs/specs/config.md` §Multi-line layouts (the `BTreeMap<String, toml::Value>` flatten pattern this ADR reuses)
- Reference: ccstatusline `src/types/Settings.ts` + `src/types/Widget.ts` — `WidgetItem` flat shape with `type: "separator"` first-class
- Out of scope: per-boundary background-color and powerline-cap configuration (ccstatusline's `PowerlineConfig.separators[]` / `startCaps[]` / `endCaps[]`). v0.1 ships character + merge only; a future ADR extends `LineEntryItem` with the visual fields if/when powerline parity becomes a goal.
