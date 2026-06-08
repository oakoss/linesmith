# What if user overrides could name theme palette colors, beyond roles and hex?

- Status: draft
- Date: 2026-06-08
- Author: Jace
- Promoted to:

## The idea

linesmith gives a user two ways to color a segment override today: a **role** (`style = "role:accent"`, semantic and portable across every theme) or **raw hex** (`style = "fg:#fab387"`, absolute and theme-blind). There is a missing middle: naming a color from the **active theme's palette** — `style = "palette:peach"` — which resolves to that theme's peach and re-resolves when the user switches flavors.

A role-based palette is deliberately small (~9 roles), which is correct for the _default_ line and for _plugins_, because the vocabulary is shared across all 11 themes and the leanest one (`minimal`, no color) sets the ceiling. But a curated theme like Catppuccin Mocha ships **14 vivid accents**, of which the role vocabulary taps only ~6 (mauve, blue, green, yellow, red, teal). A power user on Mocha who wants every segment a distinct color has to drop to raw hex — losing theme portability — because there is no way to say "the peach my theme already defines."

## Why it might matter

- **Unlocks a theme's full palette for users without growing the role vocabulary.** Adding a role is a tax on every theme ([ADR-0005](../adrs/0005-role-based-themes.md)'s "too many roles, themes become tedious"); palette-names spend a theme's own colors only where the user opts in, with zero burden on themes that lack them.
- **Keeps overrides portable within a theme family.** `palette:peach` survives a flip between Catppuccin Latte/Frappé/Macchiato/Mocha (all four define `peach`); raw hex does not. Portability across _families_ (Nord has no peach) is intentionally not promised — that's what roles are for.
- **Directly answers the dogfooding pain.** [ADR-0028](../adrs/0028-group-lead-coloring-and-role-vocabulary.md) keeps the default line minimal (ancillary segments stay `Muted`); palette-names is the sanctioned path for a user who wants `cost = peach`, `tokens = sky`, `effort = lavender` on their rich theme — turning the ~7 unused Mocha accents into reachable color.

## Sketch

Style-string syntax gains a `palette:<name>` token alongside `role:` and `fg:`:

```toml
[segments.cost]
style = "palette:peach"          # the active theme's peach
[segments.tokens]
style = "palette:sky bold"       # composes with decorations like role:/fg:
```

Resolution sits in theming.md's precedence as a peer of `role:` and `fg:`, under the user-override layer (it _is_ a user override):

- `palette:<name>` looks the name up in the active theme's palette map.
- **Miss handling** is the load-bearing design question (see below) — a name the current theme doesn't define needs a defined fallback, not a crash or silent black.

Themes already carry their palette via the source they're built from (Catppuccin via the `catppuccin` crate's `FlavorColors`, which exposes all 26 names). The work is exposing a `name → Color` lookup per theme, distinct from the `Role → Color` map. Built-in curated themes can publish their named palette; `default`/`minimal` expose little or nothing and rely on fallback.

## Open questions

- **Miss/fallback semantics.** What does `palette:peach` render under Nord (no peach)? Options: fall back to `Foreground`, warn-and-fall-back, or reject at config-load with a diagnostic. Leaning warn-and-fall-back-to-`Foreground` so a config stays usable across themes, but this needs deciding.
- **Which names are canonical?** Catppuccin's 26 names are well-defined; do other themes (Nord, Gruvbox, Tokyo Night, Rose Pine) expose their upstream palette names, or only a common subset? A shared "blessed" name set vs. per-theme arbitrary names changes the portability story.
- **Plugins: allow but discourage.** A plugin run already accepts an absolute `fg` hex (`docs/specs/plugin-api.md`, `plugins/output.rs`), so palette-names would naturally extend the same per-run style vocabulary rather than being walled off. The open question is whether to expose `palette:` in plugin output at all: allowing it is consistent with the existing `fg`, but — like `fg` — it pins the plugin to colors that break under a theme lacking that name, so it should be documented as non-portable and discouraged for distributed plugins (roles stay the recommended path). Excluding it entirely would be inconsistent with the `fg` the API already permits.
- **Discoverability.** Would need `linesmith themes show <name>` (or similar) to list a theme's available palette names, or users are guessing.
- **Interaction with group-lead coloring (ADR-0028).** A `palette:` override is a per-segment user override, so it wins over group-lead color per the precedence — consistent, but worth a test.

## Related work

- [ADR-0005](../adrs/0005-role-based-themes.md) — role-based theming; this idea is the user-facing escape hatch that complements it without diluting the role abstraction.
- [ADR-0028](../adrs/0028-group-lead-coloring-and-role-vocabulary.md) — keeps the default vocabulary minimal and names this idea as the path to richer per-segment color; the ~7 unused Catppuccin Mocha accents are the motivating evidence.
- `docs/specs/theming.md` §Style syntax / §Resolution precedence — where `palette:` would slot in.
- Prior art: Starship's `palette` + `[palettes.<name>]` tables; Helix theme palettes; Catppuccin's per-port named-color convention.

## If this gets promoted

When this idea matures into an accepted decision, create an ADR under `docs/adrs/` and move this file to `docs/ideas/archived/`, updating the `Promoted to` field with the ADR link.
