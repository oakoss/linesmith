# Color segment groups by their lead, and grow the role vocabulary minimally

- Status: accepted
- Date: 2026-06-08
- Deciders: Jace
- Surfacing beads: lsm-hvbs (this decision), lsm-ywk9 (role coherence), lsm-bak3 (threshold colors, closed)

## Context and Problem Statement

[ADR-0005](0005-role-based-themes.md) chose role-based theming with per-segment overrides as an escape hatch, and named its own revisit trigger: _"Per-segment overrides become the norm rather than the exception (signals the role vocabulary is wrong)."_ A live dogfooding session mirroring a real, telemetry-heavy status line (version, model, context_bar, git_branch, workspace, rate_limit_5h, 5h-reset, rate_limit_7d, 7d-reset, session_duration) hit exactly that: to get a visually parseable line under `catppuccin-mocha`, **9 of 10 segments needed an explicit `fg:` override**. Strip the overrides and the line collapses onto ~3 hues — `Primary` (mauve) for version+model, `Success` (green) for both rate-limit %s, and `Info` (teal) for workspace **and** both reset timers.

Two structural faults underlie this, plus one insight from the dogfood session:

- **Role assignments are incoherent** (lsm-ywk9): the three duration-like segments split across `Muted` (session_duration) and `Info` (the reset timers); `workspace` — a location segment like `git_branch` — is colored `Info` (telemetry) instead of joining `git_branch`'s location color.
- **The vocabulary is too small for telemetry density**: ~6 colored base roles must cover ~10 segments, and `Success`/`Warning`/`Error` are semantically loaded (good/caution/bad) so they can't be spent decoratively without breaking the contract.
- **Insight**: several "colliding" segments are _satellites_ of a lead — `↻ 34m` (5h-reset) is a continuation of `5h: 35%`, fused into one visual unit by a non-dividing `" "` separator. If a fused unit took **one** color, satellites would stop consuming distinct hues at all.

How should linesmith make the default line visually parseable without forcing a per-segment override on nearly every segment, while keeping ADR-0005's one-file-per-theme promise intact?

## Decision Drivers

- **ADR-0005's promise holds**: adding a theme stays a single TOML file; segments keep declaring roles, not hex.
- **Default line is parseable with zero per-segment overrides** — the override is an escape hatch, not the norm.
- **Semantic roles stay semantic**: `Success`/`Warning`/`Error` keep their good/caution/bad meaning; they are not repurposed as decorative palette slots.
- **Theme-authoring cost stays low**: every new role is a slot every shipped theme must fill, so the vocabulary grows by the minimum that buys distinctness.
- **Coherence**: a segment's color reflects its _kind_ (identity / location / telemetry / timer / gauge), so the line reads in groups.
- **Reuse existing machinery**: [ADR-0024](0024-per-boundary-separator-toml.md) already models per-boundary entries with a `merge` flag and a forward-compat `extra` bag; prefer extending it over inventing a parallel grouping concept.

## Considered Options

The decision has two coupled axes — a **coloring mechanism** and a **vocabulary policy**.

Coloring mechanism:

- **M1 — Group-lead coloring**: segments fused into a group all render in the group lead's resolved color; dividing boundaries start a new group.
- **M2 — Per-segment role reassignment only**: keep one-color-per-segment; just fix which role each segment targets.
- **M3 — Threshold-driven telemetry color**: telemetry segments color by magnitude, not identity (partly shipped via lsm-bak3).

Vocabulary policy (applies under any mechanism):

- **V1 — No new base roles**: rely on mechanism + reassignment; accept that same-_kind_ segments may share a color, disambiguated by their own text/icon.
- **V2 — Minimal additions**: add the smallest set of neutral/tertiary roles needed so the surviving _group leads_ are distinct.
- **V3 — Per-family vocabulary**: a role per segment family. Rejected on sight — it is ADR-0005's "too many roles, themes become tedious" failure mode.

## Decision Outcome

Chosen: **M1 (group-lead coloring) + role reassignment by kind (M2 applied to leads) + V2 (one neutral tertiary `Timer` role)**, extending — not superseding — [ADR-0005](0005-role-based-themes.md). Distinctness alone needs zero new roles; the single `Timer` role is spent to lift the duration family off `Muted` grey (see §Role assignment by kind). The role-based-with-overrides model is correct and stays unchanged; this decision adds a coloring layer above it and tightens role assignment. ADR-0005 keeps its `accepted` status and is not edited (its decision is intact, not reversed).

M1 is the lever that breaks the collision count. A telemetry line has few independent _things_ (identity, where-am-I, usage-this-window, time) but many _segments_, because each thing is rendered as a lead plus satellites. Coloring by group collapses the satellites into their lead, so the vocabulary only has to separate **leads**, not every segment. With leads-only to separate and roles assigned by kind, the existing palette very nearly suffices — keeping ADR-0005's low theme-authoring cost — and any residual lead collision needs at most one neutral role, never a per-family explosion. M3 stays as a complementary refinement for the usage %s (already partly shipped) but does not address identity/location/time, so it is not the primary lever.

### What a "group" is, and how a boundary fuses vs divides

A **color group** is a maximal run of adjacent segments the user has joined into one visual unit; a **dividing** boundary ends the group. The motivating case is the dogfood line's `5h: 35% ↻ 34m` — `rate_limit_5h` and its reset, written today with a light separator between them and the line's `" | "` dividing the windows:

```toml
[line]
segments = [
  "rate_limit_5h",
  { type = "separator", character = " " },     # the 5h window's two segments, visually one unit
  "rate_limit_5h_reset",                       # we want this satellite to take the 5h lead's color
  { type = "separator", character = " | " },   # the line divider — ends the window group
  "rate_limit_7d",
  { type = "separator", character = " " },
  "rate_limit_7d_reset",
]
```

This ADR decides only the **coloring rule**: the members of a group all take the **lead's** (leftmost member's) resolved color; a satellite's own role still drives any non-color style (bold/italic) it declares. How a boundary is _marked_ fused-vs-dividing is the open sub-decision below — the example above is illustrative, not a claim that a `" "` separator already fuses (today nothing does, for color).

**Open sub-decision — how a boundary declares fuse-vs-divide.** Keying purely off the separator glyph (`" "` vs `" | "`) is brittle. The clean encoding is a first-class boolean on the boundary, but reconciling it with `merge` is tangled: ADR-0024 reserved `Merge { Bool(bool), NoPadding }` on the assumption that `merge = true` keeps padding, whereas the shipped `merge = true` abuts — so the padded/no-padding distinction is unspecified in code today. Settling the exact marker (a `fuses`/`group` flag on the entry, or a corrected `merge` spacing enum) and reconciling ADR-0024's inconsistency is deferred to the theming/segment-system spec update; this ADR does not freeze it, because the coloring rule holds regardless of which marker wins.

### Resolution precedence (amends theming.md §Resolution precedence)

Group-lead coloring inserts one new step into theming.md's existing 5-step order, between the user override (step 1) and the segment's own resolution, so the escape hatch still wins and an ungrouped segment is unchanged:

1. User per-segment `style` override (theming.md step 1 — takes precedence, including over the group color).
2. **Group-lead color** (new) — a non-lead group member with no override of its own takes the lead's resolved color; the lead itself resolves via the steps below.
3. Segment's declared style from its `render()` output (theming.md step 2).
4. Theme role mapping, if the segment requested a role (theming.md step 3).
5. Terminal-capability downgrade (theming.md step 4).
6. `NO_COLOR` / `FORCE_COLOR` final override (theming.md step 5).

Group-lead coloring replaces only the **color** of a satellite; its own declared decorations (bold/italic/underline) from step 3 still apply. A group **lead**, or a segment in no group, resolves exactly as theming.md specifies today.

### Role assignment by kind (drives lsm-ywk9)

Assign each _lead's_ role by what kind of thing it is, so kinds are distinguishable and members of a kind share. After group-lead coloring collapses the reset satellites into their usage windows, the canonical line's leads map to existing roles with **one** addition:

| Kind          | Segments                                          | Role                                                          | mocha hue |
| ------------- | ------------------------------------------------- | ------------------------------------------------------------- | --------- |
| Identity      | version, model                                    | `Primary`                                                     | mauve     |
| Location      | git_branch, workspace                             | `Accent`                                                      | blue      |
| Gauge         | context_bar                                       | `Info`                                                        | teal      |
| Telemetry     | rate_limit_5h, rate_limit_7d (+ reset satellites) | threshold `Success`/`Warning`/`Error` by magnitude (lsm-bak3) | green→red |
| Time/duration | session_duration (+ ungrouped resets)             | **`Timer` (new neutral role)**                                | pink      |

Three notes on this mapping:

- **Within-kind members share.** Identity (version + model → mauve) and Location (git_branch + workspace → blue) each render one color; the segments are told apart by their own text and icon, not hue. This is the cost of a small vocabulary — it resolves lsm-ywk9's "workspace colored as telemetry" by moving workspace onto the location color, at the price of `git_branch`/`workspace` sharing blue. Finer per-segment splits are the user's escape hatch (`[segments.<id>] style`) or the future palette-names enhancement, not a default.
- **Ancillary segments stay `Muted` by design.** Segments outside this canonical line that read as secondary — `cost`, `effort`, `tokens`, `vim`, `agent`, `output_style` — keep `Muted` (grey) as their default; the catppuccin theme file comments call this out ("overlay grey … text like cost/effort"). They are not minted their own roles: a rich palette like mocha has ~7 unused accents, but a role is a slot _every_ theme must fill, and lean themes (`minimal`, `default`, Nord) can't. Coloring those segments distinctly is a per-theme **user** choice, served by the palette-names enhancement (More Information), not by growing the vocabulary. The default stays parseable-not-rainbow.
- **The one new role.** Distinctness alone needs **zero** new roles — Identity/Location/Gauge/Time fit `Primary`/`Accent`/`Info`/`Muted`. The single added `Timer` role is spent not on a collision but to lift the duration family off anonymous `Muted` grey onto its own quiet hue (pink in mocha), giving `session_duration` and the reset timers a shared identity. Under M1 (not yet implemented), grouped resets will inherit their usage-window lead; `Timer` is their fallback when ungrouped. This is the "at most one neutral tertiary" the decision budgets.

These values and behaviors are **prescribed, not existing**: the `Role` enum and `Role::fallback()` (`crates/linesmith-core/src/theme/mod.rs`) carry no `Timer` today, and a new role does not auto-degrade — `fallback()`'s catch-all returns the role itself (→ `NoColor`), so making "themes that omit `Timer` render `Muted`" work requires adding an explicit `Timer → Muted` arm. The exact pink per theme, the `Timer` variant, and its `→ Muted` fallback arm are pinned in the theming.md / `mod.rs` updates the implementation beads below cover.

### Plugins and themes

Nothing here changes the plugin or theme contract; both ride the existing role system ([ADR-0005](0005-role-based-themes.md)):

- **Plugins should prefer roles; the new `Timer` role just joins what they can target.** A plugin run that declares a role (`role:accent`, and once added, `role:timer`) inherits whatever the active theme maps it to, so the same plugin reads correctly under any theme — that's the portable path, and the only plugin-API change is one more role name. The plugin API _does_ already permit an absolute `fg` hex on a run (`docs/specs/plugin-api.md`, `plugins/output.rs`), and palette-names would extend the same per-run vocabulary; but both pin a plugin to specific colors and break under themes that differ, so they're discouraged for distributed plugins for the same reason ADR-0005 steers plugins toward roles. This ADR doesn't change that contract — it neither removes plugin `fg` nor grants plugins anything beyond the new role name.
- **Group-lead coloring applies to plugin segments transparently.** Under M1, a plugin segment fused into a group will take the lead's color (built-in or plugin), and a plugin segment that is a lead will color its satellites — no plugin opt-out, because the user controls which segments fuse. The precedence (user override > group color > declared role) means a plugin's role is its color when standalone or lead, and yields to the group when fused.
- **Themes absorb `Timer` through fallback.** Once the `Timer → Muted` arm lands, a theme (built-in or user-authored) that does not define `timer` renders the family as `Muted` rather than breaking; only themes that want the distinct hue add the one mapping. That bounds the per-theme cost of the role to "opt in if you care," preserving ADR-0005's one-file-per-theme promise.

### Consequences

- Good, because the default telemetry line becomes parseable with **zero** per-segment overrides — the ADR-0005 revisit trigger is cleared at the root, not patched per user.
- Good, because the vocabulary grows by the minimum (target zero, cap one), so themes stay cheap to author and the Catppuccin contract is untouched.
- Good, because color reuses the grouping the user already expresses for layout (merge / non-dividing separators) — no separate "color group" config concept.
- Good, because semantic roles keep their meaning; nothing decorative is stolen from Success/Warning/Error.
- Bad, because color now depends on layout grouping — a reorder that moves a segment across a divider changes its color (color _follows_ grouping, intentionally); this coupling needs documentation.
- Bad, because the group-boundary marker is left as an open sub-decision (see above): the shipped `merge = true` abuts while the dogfood line fuses with a space via an explicit separator, and ADR-0024's reserved `Merge { Bool(bool), NoPadding }` is inconsistent with that shipped behavior. The spec must settle one fuse-vs-divide marker and reconcile ADR-0024 before implementation — this ADR fixes only the coloring rule, not the marker.
- Bad, because same-kind segments can now share a color (git_branch and workspace both "location"); distinctness for those leans on their text/icon, not hue. Accepted as the cost of a small vocabulary.
- Neutral, because group-lead resolution adds one lookup per fused member on the render path — negligible against the <20ms budget.

### Confirmation

Revisit if:

- After implementation, the default line under any shipped theme still needs a per-segment override to be parseable (the trigger would have re-fired).
- The "minimal additions" cap creeps past the one neutral tertiary role the decision allows — that would signal group-lead coloring isn't doing the work and the model needs rethinking.
- The group-boundary marker chosen in the spec can't express the dogfood line's spaced fusion without breaking the shipped `merge = true` (abut) contract — that would force a config migration the coloring rule was meant to avoid.

Confirmed when: a snapshot test renders the canonical 10-segment line under `catppuccin-mocha` with no `[segments.*] style` overrides and every adjacent _kind_ is visually distinct.

## Pros and Cons of the Options

### M1 — Group-lead coloring (chosen mechanism)

- Good: collapses satellites into leads, so the vocabulary only separates leads.
- Good: matches the user's mental model — a fused unit is one thing, one color.
- Good: reuses ADR-0024's `merge`/entry model; no parallel grouping concept.
- Bad: couples color to layout order; introduces a precedence layer to document.

### M2 — Per-segment role reassignment only

- Good: no new concepts; pure assignment fix (this is lsm-ywk9 on its own).
- Bad: doesn't change the collision _count_ — 10 segments still need ~10 separable colors, which the small vocabulary can't supply without V3.

### M3 — Threshold-driven telemetry color

- Good: color conveys magnitude; partly shipped (lsm-bak3) and complementary.
- Bad: only addresses the usage %s; identity, location, and time still collide.

### V1 — No new base roles

- Good: zero theme-authoring cost.
- Bad: may leave one lead collision (e.g. identity vs location) with no hue to separate it.

### V2 — Minimal additions (chosen vocabulary policy)

- Good: buys exactly the distinctness M1 can't, and no more.
- Bad: every added role is a slot all shipped themes must fill (bounded to ≤1 here).

### V3 — Per-family vocabulary

- Rejected: re-creates ADR-0005's "too many roles, themes become tedious" failure.

## More Information

- Extends: [ADR-0005](0005-role-based-themes.md) (role-based theming — its decision stands unchanged and the ADR is not edited; 0005 keeps `accepted` status).
- Builds on: [ADR-0024](0024-per-boundary-separator-toml.md) (`LineEntryItem`, `merge`, `extra` forward-compat) and [ADR-0003](0003-segment-widget-system.md) (segments declare roles).
- Will drive: `docs/specs/theming.md` (new §Group-lead coloring + §Resolution precedence amendment + role assignment table) and `docs/specs/segment-system.md` / `config.md` (the group-boundary marker that settles the open sub-decision and reconciles ADR-0024's `merge` spacing).
- Implementation beads to file on acceptance: settle + implement the group-boundary marker; the group-color render layer; add the `Role::Timer` variant + its `Timer → Muted` `fallback()` arm + the per-theme `timer` color (pink in the Catppuccin flavors); role reassignment by kind (lsm-ywk9); the default-line snapshot test.
- Open / future direction: expose theme **named-palette** colors to user overrides (e.g. `style = "palette:peach"`) so power users get fine-grained, family-portable per-segment color without raw hex or new roles — a complement to this decision, not a substitute (defaults and plugins still use roles). Portable within a theme family (Catppuccin's 4 flavors share names) but not across families; ADR-0005 kept defaults semantic for exactly that reason. Tracked as [idea 0002](../ideas/0002-palette-name-overrides.md); the ~7 unused mocha accents are its motivating evidence.
- Driven by: live dogfood session 2026-06-05 / 2026-06-08 mirroring the work status line.
