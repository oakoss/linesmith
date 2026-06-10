# Mark color groups with a `group` flag, and decouple it from `merge` spacing

- Status: accepted
- Date: 2026-06-09
- Deciders: Jace
- Surfacing bead: lsm-l0ok (this decision)

## Context and Problem Statement

[ADR-0028](0028-group-lead-coloring-and-role-vocabulary.md) decided that segments fused into a **color group** all render in the group lead's resolved color, but left one sub-decision open: **how a boundary declares fuse-vs-divide.** It deferred the marker to "the theming/segment-system spec update" and named the tangle to resolve — reconciling a first-class fuse marker with the existing `merge` flag, whose shipped behavior contradicts what [ADR-0024](0024-per-boundary-separator-toml.md) reserved for it.

The contradiction, concretely:

- **Shipped `merge = true` abuts.** The builder (`crates/linesmith-core/src/segments/builder/dispatch.rs`, `build_one_line`) drops _every_ separator at a merged segment's right boundary — both the implicit interleave and any explicit `{ type = "separator" }` entry. Two merged segments render with **zero** cells between them (`5h: 35%↻ 34m`). Pinned by `merge_flag_suppresses_implicit_interleave_at_boundary` and `merge_flag_suppresses_explicit_separator_entry_at_boundary`.
- **ADR-0024 reserved the opposite.** Its §Consequences note reserved a future `Merge { Bool(bool), NoPadding }` enum on the assumption that `merge = true` keeps padding (a space) and only a separate `NoPadding` abuts — mirroring ccstatusline's `merge: boolean | 'no-padding'`. The shipped boolean behaves like ccstatusline's `'no-padding'`, so the reserved enum's `Bool(true)` arm describes a behavior that **does not exist in the code**, and the padded/abut distinction is unspecified.
- **ADR-0028's motivating line needs spaced fusion.** The dogfood line `5h: 35% ↻ 34m` fuses `rate_limit_5h` with its reset for color **while keeping the space between them**. `merge` cannot express this: `merge = true` would abut the two into `5h: 35%↻ 34m`. So color-grouping cannot ride on `merge` without either breaking the space the line wants or breaking the shipped abut contract.

How should a boundary mark its members as one color group, given that `merge` already owns spacing and its shipped semantics conflict with the only enum ADR-0024 reserved?

## Decision Drivers

- **Preserve the shipped `merge = true` abut contract.** Two passing builder tests pin it; changing it is a config migration for every user who merges segments. ADR-0028's own revisit trigger names "can't express spaced fusion without breaking the shipped `merge = true` (abut) contract" as the outcome to avoid.
- **Express the dogfood line's spaced fusion.** `rate_limit_5h` + reset must share a color group while keeping the separator between them.
- **Don't key color off the separator glyph.** ADR-0028 calls glyph-sniffing (`" "` fuses, `" | "` divides) brittle by name — a user who changes the divider character would silently re-group.
- **Reuse existing machinery.** The builder already threads a per-segment right-boundary intent (`merge_pending`); a parallel intent is a one-field, one-flag addition, not a new subsystem.
- **Keep the common case zero-config.** Abutting two segments (`merge = true`) almost always means "one visual unit" — it should imply one color without a second flag.
- **Orthogonality.** Spacing (does a separator render?) and color-grouping (do these share a hue?) are independent questions; conflating them into one enum is what created this tangle.

## Considered Options

- **Option A — `group` flag on the segment entry, orthogonal to `merge`.** Add `group: Option<bool>` to `LineEntryItem`. `group = true` on a segment fuses the boundary to its right into one color group (the leftmost member is the lead, per ADR-0028). It says nothing about spacing — the separator (implicit or explicit) renders as it would without the flag. `merge = true` implies `group = true` unless `group = false` is explicit.
- **Option B — `fuses` flag on the separator entry.** Put the marker on the explicit `{ type = "separator", ... }` entry: `{ type = "separator", character = " ", fuses = true }`.
- **Option C — Correct `merge` into a tri-state spacing+color enum.** Make `merge` carry ccstatusline semantics: `merge = true` keeps the space _and_ fuses color, `merge = "no-padding"` abuts and fuses; absent divides. Realize ADR-0024's reserved `Merge { Bool(bool), NoPadding }`, keyed to color.

## Decision Outcome

Chosen: **Option A — a first-class `group` boolean on the segment entry, orthogonal to `merge`**, and **retire ADR-0024's reserved `Merge { Bool(bool), NoPadding }` enum**. `merge` stays a boolean spacing concern (its shipped abut behavior is now the documented contract); `group` is the color-grouping concern ADR-0028 needs. The two compose:

| `merge` | `group`          | renders       | color group                |
| ------- | ---------------- | ------------- | -------------------------- |
| absent  | absent           | `A` `\|` `B`  | two (today's default)      |
| absent  | `true`           | `A` `sep` `B` | **one** — the dogfood case |
| `true`  | absent (implied) | `AB` (abut)   | one                        |
| `true`  | `false`          | `AB` (abut)   | two                        |

`group` mirrors `merge`'s existing right-boundary mechanic: the builder gains a `group_pending` flag set alongside `merge_pending`, and a maximal run of `group`-fused boundaries forms one color group led by its leftmost member (chains exactly as `merge_pending` does across an explicit separator). It works at **both** implicit boundaries (two bare-adjacent segments) and explicit ones (a `{ type = "separator" }` between them), because it lives on the segment, not the separator. A satellite written as a bare string still fuses when its lead declares `group = true`.

The `merge ⟹ group` default keeps the common case zero-config: abutting two segments yields one visual unit and one color with no second flag, and the rare "abut but keep two colors" case is the explicit `group = false` override.

The shipped abut contract is untouched (the two pinning tests stay green, no migration); the dogfood line is `{ type = "rate_limit_5h", group = true }, { type = "separator", character = " " }, "rate_limit_5h_reset"` — space kept, color shared; nothing sniffs the glyph; the builder change is one field plus one parallel flag; and spacing and color stay orthogonal.

### Reconciling ADR-0024

ADR-0024's decision (a mixed string-or-table `[line].segments` array; `merge: Option<bool>` on `LineEntryItem`; `extra` forward-compat bag) **stands** — this ADR neither supersedes it nor touches the array shape. The single reconciliation is to **retire the reserved `Merge { Bool(bool), NoPadding }` enum** from its §Consequences note:

- `merge` remains `Option<bool>` with its **shipped abut semantics**: `merge = true` suppresses the boundary at the segment's right edge (implicit interleave _and_ any adjacent explicit separator). This is now the documented contract, not a v0.1 placeholder.
- The "keep padding vs no-padding" distinction the reserved enum was meant to carry is **not entangled with color** anymore — color-grouping moved to `group`. If a future need arises to fuse-_with_-a-forced-space purely as a spacing affordance (distinct from `group`, which keeps whatever separator already renders), it lands as an additive `merge` value (e.g. a `"keep-space"` string variant) without revisiting color. Until such a need is demonstrated, `merge` is boolean-only.

ADR-0024 keeps its `accepted` status; its status block gains an "Amended by: ADR-0029" pointer (the sanctioned status-field annotation, not a body rewrite).

### Resolution-precedence interaction

ADR-0028 inserted group-lead color as a new step 2 in theming.md's resolution precedence. `group` is the marker that _populates_ that step: which segments are non-lead members of a group is exactly the set fused by `group`-flagged boundaries. A `group = false` (or absent) boundary starts a new group, so its right segment is a fresh lead and resolves independently. The flag changes grouping only — it never overrides a segment's own user `style` (precedence step 1 still wins over the group color).

### Consequences

- Good, because the shipped `merge = true` abut contract is preserved verbatim — the two pinning tests stay green and no config migrates.
- Good, because the dogfood line's spaced fusion is now expressible, clearing the ADR-0028 revisit trigger that flagged this exact case.
- Good, because spacing and color-grouping are orthogonal flags, so any of the four `(merge, group)` combinations is reachable.
- Good, because the builder change is minimal — one `Option<bool>` field and a `group_pending` flag parallel to the existing `merge_pending`.
- Good, because `merge ⟹ group` keeps the abut case zero-config while leaving an explicit escape (`group = false`).
- Bad, because `group` is a second per-boundary flag users must learn alongside `merge`; the orthogonality that makes it correct also makes the model wider. Mitigated by the implication default (most users only ever touch `merge`).
- Bad, because the `merge ⟹ group` implication is an implicit coupling between two nominally-orthogonal flags — documented, but a reader must know abut auto-fuses.
- Neutral, because ADR-0024's reserved enum was never implemented; retiring it deletes a future reservation, not shipped code.

### Confirmation

Revisit if:

- A user demonstrably needs fuse-with-forced-space as distinct from `group` over an existing separator (would motivate the additive `merge = "keep-space"` value this ADR defers).
- The `merge ⟹ group` default surprises users in practice (abutted segments they wanted in two colors) often enough that the implicit coupling costs more than it saves.

Confirmed when: the canonical dogfood line renders `5h: 35% ↻ 34m` with the reset sharing the 5h lead's color **and** the space between them intact, under a snapshot test, with no per-segment `style` override (this is also ADR-0028's confirmation gate, which `group` is the marker for).

## Pros and Cons of the Options

### Option A — `group` flag on the segment entry (chosen)

- Good: orthogonal to `merge`, so spacing and color are independently controllable.
- Good: preserves the shipped abut contract; zero migration.
- Good: works at implicit and explicit boundaries; mirrors `merge_pending`.
- Good: `merge ⟹ group` keeps the common case zero-config.
- Bad: a second per-boundary flag; implicit `merge ⟹ group` coupling to document.

### Option B — `fuses` flag on the separator entry

- Good: reads naturally — "this separator joins" — and co-locates the marker with the glyph it sits next to.
- Bad: only explicit-separator boundaries can carry it; implicit (bare-adjacent) boundaries need a separate default, splitting the rule across two places.
- Bad: doesn't mirror the existing `merge_pending` segment-side machinery, so the builder grows a second, differently-shaped grouping path.
- Bad: a `merge = true` boundary has _no_ separator entry to hang `fuses` on, so the abut-implies-fuse default has nowhere to live.

### Option C — Correct `merge` into a tri-state spacing+color enum

- Good: maximal ccstatusline parity; realizes ADR-0024's reserved enum and folds color + spacing into one knob.
- Bad: **breaks the shipped `merge = true` abut contract** — existing merged configs would gain a space; a config migration ADR-0028 explicitly warns against.
- Bad: re-entangles spacing and color in one enum, the exact conflation that produced this tangle; a user wanting abut-with-two-colors has no expressible state.

## More Information

- Settles: [ADR-0028](0028-group-lead-coloring-and-role-vocabulary.md) §Open sub-decision (the fuse-vs-divide marker) and clears its third revisit trigger.
- Amends: [ADR-0024](0024-per-boundary-separator-toml.md) — retires the reserved `Merge { Bool(bool), NoPadding }` enum; `merge` stays `Option<bool>` with shipped abut semantics. ADR-0024's array-shape decision is unchanged.
- Builds on: [ADR-0003](0003-segment-widget-system.md) (segments declare roles) and [ADR-0005](0005-role-based-themes.md) (role-based theming) via ADR-0028.
- Will drive: `docs/specs/segment-system.md` and `docs/specs/config.md` (the `group` field on `LineEntryItem`, builder semantics, the four `(merge, group)` combinations) and `docs/specs/theming.md` (which boundaries populate the group-lead color step — folded into lsm-s55y).
- Reference: ccstatusline `merge: boolean | 'no-padding'` (`src/types/Widget.ts`) — the parity target the retired enum chased; linesmith keeps `merge` boolean and carries color separately.
- Implementation beads: lsm-v6f0 (the `group` field + `group_pending` builder flag + the four-combination tests — this marker); lsm-p0p2 (the group-color render layer that consumes the grouping `group` produces); lsm-s55y (the theming-spec amendment).
