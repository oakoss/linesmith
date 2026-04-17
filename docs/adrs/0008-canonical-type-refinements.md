# Refine canonical StatusContext types with newtypes, sum-type collapse, and runtime strings

- Status: accepted
- Date: 2026-04-17
- Deciders: Jace
- Supersedes: [ADR-0006](0006-tool-agnostic-json-schema.md)

## Context and Problem Statement

[ADR-0006](0006-tool-agnostic-json-schema.md) committed to a union canonical model (`StatusContext`) with per-tool normalizers. A type-design review of the draft `specs/input-schema.md` and `specs/segment-system.md` flagged several design decisions that would fail downstream: fields that allow illegal states (`used + remaining > 100`), redundant nullable states (`Some(RateLimits { None, None })` vs `None`), and compile-time-only strings locking out user config. The underlying architecture from ADR-0006 stands; this ADR refines the type-level contract before the types become code.

## Decision Drivers

- Make illegal states unrepresentable where feasible without runtime cost
- Preserve forensic info for debugging misdetection (argued against unit `Tool::Other` in review)
- Accept user-provided runtime strings where the architecture requires them (`Separator::Literal`)
- Respect the `Send`-only reality of `rhai::AST` in plugin segments
- Avoid expensive deep clones when the canonical model crosses cache boundaries
- Keep the refactor small and local: changes must not force ADR-0003, 0004, or 0005 to supersede

## Considered Options

- **Ignore the review** (keep the original sketch from ADR-0006)
- **Supersede ADR-0006 with an amended full re-statement** (rewrite the full model in a new ADR)
- **Supersede ADR-0006 with a delta ADR** (this ADR) that names the specific refinements and points to specs for the full shape

## Decision Outcome

Chosen option: **delta ADR (this one) superseding ADR-0006**, because the architectural decision (union schema + per-tool normalizers) is unchanged; only the type-level encoding refines. A full restatement would duplicate unchanged reasoning. The refinements are captured as explicit deltas and the full final shape lives in `specs/input-schema.md` and `specs/segment-system.md`, per our docs pipeline.

### Refinements

**1. `Tool::Other` carries a runtime identifier.**

```rust
pub enum Tool {
    ClaudeCode,
    QwenCode,
    CodexCli,
    CopilotCli,
    Other(Cow<'static, str>),
}
```

Originally specified as `Other(String)` in ADR-0006 without discussion of alternatives. The `Cow<'static, str>` form costs 24 bytes only when used, enables zero-alloc fallback names from heuristic detection (`Tool::Other("unknown".into())`), and allocates only when a runtime-derived identifier is needed. Matches the oakterm pattern of typed-data enum variants.

**2. `Percent` newtype; derive `remaining` from `used`.**

```rust
pub struct Percent(f32); // 0.0..=100.0 enforced at construction

pub struct ContextWindow {
    used: Percent,
    // remaining removed — derivable as Percent(100.0 - used.0)
    size: u32,
    /* ... */
}
```

`ContextWindow` no longer allows `used + remaining != 100`. Applied to `RateLimitWindow.used_percentage` identically.

**3. Collapse `RateLimits` so `(None, None)` is unrepresentable.**

```rust
pub enum RateLimits {
    FiveHourOnly(RateLimitWindow),
    SevenDayOnly(RateLimitWindow),
    Both { five_hour: RateLimitWindow, seven_day: RateLimitWindow },
}
```

On `StatusContext`, `rate_limits: Option<RateLimits>` means "absent"; every `Some` carries at least one window. Previously `Some(RateLimits { five_hour: None, seven_day: None })` was legal and semantically identical to `None`. That state is now illegal.

**4. Typed `ParseError` positions.**

```rust
pub enum JsonType { Object, Array, String, Number, Bool, Null }

pub struct SourcePos { line: usize, column: usize }

pub enum ParseError {
    InvalidJson { message: String, location: Option<SourcePos> },
    MissingField { tool: Tool, path: String },
    TypeMismatch { tool: Tool, path: String, expected: JsonType, got: JsonType },
    NormalizerError { tool: Tool, message: String },
}
```

`location: Option<SourcePos>` handles non-positional errors (empty input). `expected` / `got` become typed rather than `&'static str`.

**5. Flatten one-field wrappers.**

`VimState { mode: VimMode }` → just `vim: Option<VimMode>` on `StatusContext`. `AgentInfo { name: Option<String> }` → `agent_name: Option<String>`. `OutputStyle { name: String }` stays for now (the `name` may grow into an enum for known styles).

**6. `Separator::Literal(Cow<'static, str>)`.**

User config provides runtime strings; `&'static str` locked these out. `Cow` preserves zero-alloc for built-in defaults and one allocation for user config.

**7. `Segment: Send` (drop `Sync`).**

`rhai::AST` is `Send` but its `Sync` story depends on feature flags. Dropping `Sync` reflects reality. If shared-reference parallelism is ever needed, adding `Sync` later is a non-breaking extension; removing it would be breaking.

**8. `raw: Arc<serde_json::Value>` on `StatusContext`.**

`StatusContext` is cloned across cache boundaries. `Value::clone` is deep-recursive on a 2KB payload. `Arc` makes clones `O(1)`.

**9. `CachePolicy::Invalidated` with explicit `any_of` semantics.**

```rust
pub enum CachePolicy {
    AlwaysFresh,
    Ttl(Duration),
    Invalidated { any_of: Vec<CacheInvalidator>, ttl: Option<Duration> },
}
```

Previously `Until(Vec<CacheInvalidator>)` with implicit OR. The new shape names `any_of` explicitly and allows an optional TTL ceiling.

### Consequences

- Good, because illegal states are eliminated (bad percentage arithmetic, redundant `RateLimits` states, `SegmentDefaults` with `min_width > max_width` caught by newtype)
- Good, because `Tool::Other` keeps forensic value
- Good, because user-provided strings can land in `Separator::Literal` without forcing `String` across the board
- Good, because `Arc` avoids expensive `Value::clone` on cache paths
- Bad, because the type surface is marginally richer (more newtypes, more `.into()` calls in tests)
- Bad, because `Percent::new` returning `Option<Percent>` forces every construction path to handle the out-of-range case; we treat this as a feature, not a bug, since normalizers were already checking ranges implicitly
- Neutral, because the `Cow<'static, str>` pattern is unfamiliar to some Rust readers; we accept the learning tax in exchange for the ergonomics

### Confirmation

Revisit if:

- The newtype machinery generates friction (tests feel verbose, segments fight the types)
- Forensic identifiers in `Tool::Other` prove unnecessary in practice (delete the `Cow` in v0.2+)
- A segment trait consumer genuinely needs `Sync` (re-add it then, non-breaking)

## Pros and Cons of the Options

### Delta ADR (chosen)

- Good: captures the specific refinements; preserves ADR-0006's reasoning
- Good: docs pipeline still works (ADR → spec → code)
- Bad: readers must cross-reference two ADRs to reconstruct the full model

### Full restatement

- Good: one ADR is self-contained
- Bad: duplicates unchanged reasoning; makes ADR-0006 feel wholesale wrong rather than sketch-stage

### Ignore the review

- Good: no ADR churn
- Bad: known-illegal states survive into code; review effort wasted; later refactors cost more

## More Information

- Driven by: type-design review of the `specs/input-schema.md` and `specs/segment-system.md` v0.1 drafts (2026-04-17)
- Full type definitions: [`specs/input-schema.md`](../specs/input-schema.md), [`specs/segment-system.md`](../specs/segment-system.md)
- Related ADRs: [ADR-0003](0003-segment-widget-system.md) (segment trait), [ADR-0006](0006-tool-agnostic-json-schema.md) (superseded)
