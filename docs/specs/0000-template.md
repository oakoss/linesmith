# {feature area name — e.g. "Segment System", "Plugin API", "Theming"}

- Status: {draft | approved | deprecated}
- Version: {0.1, 0.2, ...}
- Last updated: YYYY-MM-DD
- Driving ADRs: {comma-separated list of ADR numbers, e.g. "ADR-0003, ADR-0004"}

## Overview

{Two or three paragraphs describing what this feature area does, why it exists, and its boundaries with other feature areas.}

## Requirements

### Functional

- {what the feature must do}
- {...}

### Non-functional

- {performance targets, binary size, startup budget}
- {security, sandboxing, reliability}
- {...}

## Interface / Contract

{The public surface. Function signatures, config schema, data types, CLI arguments, file formats. Whatever forms the boundary between this feature and the rest of the system.}

```rust
// example — replace with actual shape
pub trait Segment {
    fn id(&self) -> &str;
    fn render(&self, ctx: &StatusContext) -> Option<String>;
}
```

## Behavior

{How the feature behaves at runtime. Rendering flow, ordering, error handling, state transitions. Focus on observable behavior, not implementation details — those belong in code.}

## Edge cases

- {cases we explicitly handle}
- {cases we explicitly do NOT handle and why}

## Testing strategy

- Unit tests: {what's covered by unit tests}
- Integration tests: {what's covered by integration tests}
- Snapshot / golden tests: {if applicable}

## Open questions

- {things not yet decided — may need a new ADR}
- {...}

## Change log

- YYYY-MM-DD: initial draft (vN.N)
- YYYY-MM-DD: {what changed and why}
