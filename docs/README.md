# linesmith docs

This directory holds the design knowledge that drives linesmith: research findings, brainstormed ideas, architecture decisions, and implementation specs. Beads handles the execution layer (epics, tasks, status).

## Pipeline

```text
research  →  ideas  →  ADRs  →  specs  →  beads
 (what      (what      (what     (how we     (who does
  exists)    if?)       we'll     build)      what,
                        do)                    when)
```

Each stage feeds the next. Work flows left to right, but it's not strictly linear — a spec might reveal an open question that kicks off new research, a failed experiment might retire an ADR.

## Folders

| Folder            | Purpose                                                                                                                                                  | Who reads it                                |
| ----------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------- |
| `research/`       | Surveys, deep dives, competitive analysis, user demand data. Captures what we learned and when.                                                          | Anyone catching up on context               |
| `ideas/`          | Exploratory notes: "what if we did X?" Rough, optional, may not lead anywhere                                                                            | The author while thinking                   |
| `ideas/archived/` | Ideas that got promoted to ADRs (or were explicitly dropped). Kept for provenance                                                                        | Anyone wondering where a decision came from |
| `adrs/`           | **Architecture Decision Records.** MADR format. One decision per file, numbered chronologically. Immutable once accepted — supersede rather than rewrite | Anyone implementing or reviewing            |
| `specs/`          | Implementation contracts. One per feature area. Change as the contract evolves                                                                           | Anyone building the feature                 |

## File naming

All files are lowercase-kebab-case markdown. ADRs, ideas, and specs use a numeric prefix for chronological ordering:

- ADRs: `0001-use-rust.md`, `0002-name-linesmith.md`, ...
- Ideas: `0001-heat-map-segment.md`, `0002-background-daemon.md`, ...
- Specs: per feature area, no numeric prefix needed initially (e.g. `segment-system.md`, `plugin-api.md`)
- Research: descriptive name (e.g. `competitor-landscape-2026-04.md`)

Each folder has a `0000-template.md` seed file. Copy it to start a new document.

## Promotion flow

### Idea → ADR

1. Draft an idea in `docs/ideas/NNNN-idea-name.md`
2. When the idea hardens into a decision, create `docs/adrs/NNNN-decision-name.md` (MADR format)
3. Move the original idea to `docs/ideas/archived/` with a link in the idea's front matter to the ADR that superseded it

### ADR → Spec

1. Once an ADR is accepted, any feature area it governs gets a spec in `docs/specs/`
2. Specs reference their driving ADRs by number
3. Specs can be revised as the contract evolves; ADRs cannot — supersede instead

### Spec → Beads

1. A spec is the contract; beads issues are the execution
2. Create an epic per spec, tasks per deliverable in the spec
3. Close beads issues as work completes

## ADR lifecycle

ADR statuses (MADR):

- `proposed` — drafted, under discussion
- `accepted` — decided, in force
- `rejected` — drafted but not adopted (kept for record)
- `deprecated` — no longer in force, not superseded by a specific ADR
- `superseded by ADR-NNNN` — replaced by a newer decision

**Accepted ADRs are immutable.** If we change our mind, write a new ADR that supersedes it. This preserves the reasoning trail.
