# Adopt a rich segment/widget system with priority, width, visibility, caching, async, and composition

- Status: accepted
- Date: 2026-04-17
- Deciders: Jace

## Context and Problem Statement

The status line is composed of units (git branch, model, context %, cost, etc.). Existing tools hardcode these units with fixed rendering logic; the richer tools (ccstatusline) have many units but treat each as a simple string producer. How should linesmith structure its composable units such that we can ship rich layout behavior, plugin extensibility, and correctness across edge cases?

## Decision Drivers

- Composability: users mix and match units into their line
- Conditional visibility: only show worktree indicator in worktrees, cache countdown only near expiry, etc.
- Layout behavior: priority-based truncation when terminal width is tight
- Caching: expensive units (git status, HTTP lookups) shouldn't run every invocation
- Async-capable: some data must be fetched out-of-band
- Sub-composition: a "git group" might internally combine branch + status + ahead/behind
- Plugin compatibility: user-written units (in rhai) should use the same interface as built-ins

## Considered Options

- **Static concatenation**: hardcoded print-outs, user toggles which show
- **Simple segments**: each segment renders a string, placed in order (ccstatusline-style)
- **Rich widget system**: segments with priority, width hints, visibility predicates, cache policy, async capability, composition

## Decision Outcome

Chosen option: **Rich widget system**, because the decision drivers are not separable. A simple segment model forces users to live with "context % shows even when 100%" or "cost segment blocks on a slow disk read." Cheap to design once, painful to retrofit. We keep the terminology "segment" (ecosystem standard from powerline/starship/tmux) but give each segment the richer capabilities internally.

Segments are defined by a trait with these capabilities:

- `render(ctx) -> Option<String>`: returns `None` to hide
- `priority: u8`: lower priority drops first under width pressure
- `min_width` / `max_width`: layout hints
- `cache_policy`: TTL, invalidation triggers, or "always fresh"
- `kind`: sync | async-prefetched | sub-composed

### Consequences

- Good, because we can implement priority-based truncation (biggest layout UX improvement over existing tools)
- Good, because expensive segments (git status, rate-limit API scrape) can cache and not block the <20ms budget
- Good, because conditional visibility eliminates "dead" indicators (segment returns `None` and disappears cleanly)
- Good, because the same trait shape applies to built-ins and rhai plugins; no dual API
- Good, because sub-composition lets us build "git group" widgets without hardcoding
- Bad, because more surface area than a simple segment (more to design, document, and test)
- Bad, because priority-based truncation has subtle UX (which segment gets dropped? how is it indicated?) that we'll need to tune empirically
- Neutral, because the caching layer adds complexity we'd need anyway once we add HTTP calls

### Confirmation

Revisit if:

- The trait becomes over-abstracted and v0.1 segments look awkward using it
- Users can't author reasonable rhai plugins against the trait surface
- Width/priority layout produces confusing truncation behavior users can't predict

## Pros and Cons of the Options

### Static concatenation

- Good: trivial to implement
- Bad: no layout intelligence, no conditional visibility, no caching, no async
- Bad: kills plugin story before it starts

### Simple segments (string producers)

- Good: easiest API to teach
- Bad: every feature driver above requires retrofitting; priority, caching, async become per-segment hacks
- Bad: sub-composition is impossible without introducing a richer type

### Rich widget system

- Good: all drivers met in one design
- Good: matches mature adjacent tools (p10k segments have priority, Starship has conditional modules)
- Bad: requires upfront design effort
- Bad: easier to over-engineer if we're not disciplined about "ship v0.1 first"

## More Information

- Driven by: `research/user-demand.md` (worktree-conditional, rate-limit async, cache-hit countdown all require visibility/caching/async), `research/competitor-landscape.md` (no tool has this combination)
- Related ADRs: [ADR-0004](0004-rhai-for-plugins.md) (plugins use the same trait), [ADR-0005](0005-role-based-themes.md) (segments request colors by role, not hex)
- Will drive: `specs/segment-system.md` (to be written)
