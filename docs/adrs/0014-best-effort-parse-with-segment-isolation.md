# Best-effort parse with segment-level isolation for stdin payloads

- Status: accepted
- Date: 2026-04-25
- Deciders: Jace
- Driving research: [parse-failure-isolation-survey](../research/parse-failure-isolation-survey.md) (commit `e39184a`), [context-window-correctness](../research/context-window-correctness.md) (commit `ab22dca`)
- Surfacing bug: lsm-6z9e

## Context and Problem Statement

linesmith's `input::parse(stdin) -> Result<StatusContext, ParseError>` is atomic: any `TypeMismatch` in any nested field collapses the whole parse, and the driver renders `?` for every segment. A captured CC 2.1.120 pre-first-API-call payload reproduces this every fresh session — `context_window` is present but with `used_percentage`, `remaining_percentage`, and `current_usage` all `null`. `parse_context_window` calls `require_f64("context_window.used_percentage")` which throws on null → entire statusline is `?` for the ~15-second pre-first-call window. Model, git_branch, workspace, cost — none of them depend on `context_window`, but all disappear because the shared parse failed.

How should linesmith handle partial / malformed CC stdin so that one field's failure doesn't tank unrelated segments?

## Decision Drivers

- **User-visible correctness.** The fresh-session `?` window is a real bug every user hits every session; even ignoring that, the pattern of "any new CC field shape we don't expect breaks everything" is a contract-fragility liability for v0.1.x and beyond.
- **Architectural alignment with the working competitor field.** The parse-failure survey shows every working CC statusline tool (ccstatusline, CCometixLine, claude-powerline, claudia-statusline) returns `Option`-shaped per-segment values; none has a shared `Result` that blanks the whole prompt on `Err`. linesmith is the outlier.
- **starship as cautionary reference.** starship has excellent per-module isolation but its CC modules collectively share one fragile `ClaudeCodeData` struct that fails atomically — module isolation at the render layer doesn't help when the deserializer collapsed the input upstream. This rules out "fix segments only, leave parser atomic."
- **Existing partial Option shape.** `StatusContext` already has `context_window: Option<ContextWindow>`, `cost: Option<CostMetrics>`, `effort: Option<EffortLevel>`. The Option declaration is half-implemented — the parsers return `Result<Option<T>>` but propagate inner `TypeMismatch` as a hard error rather than degrading to `Ok(None)`.
- **Diagnostic preservation.** Whatever we ship has to keep upstream-contract-drift signal visible. Silently swallowing CC schema changes is worse than the current loud failure for project maintainers, even if it's better for users. The `lsm_warn!` macro writes unconditionally to stderr (no rate-limiting at the macro layer); since linesmith is a single-shot per-render process, the practical bound is "at most one warn per failed field per render" without any further machinery.
- **Minimal blast radius for plugin authors.** Plugins read `ctx.status` via `ctx_mirror.rs`; whatever shape we settle on has to round-trip cleanly to rhai without forcing every plugin to re-validate fields.

## Considered Options

- **Option 1 — Per-field nullable, per-segment elision (ccstatusline pattern).** Make every nested field `Option<T>` in the type itself; sub-parsers tolerate null/malformed sub-fields and downgrade to `Ok(None)` for the field, emitting `lsm_warn!` for diagnostics. Segments check `is_none()` and hide. The whole parse returns `Result<StatusContext, ParseError>` only for catastrophic failures (invalid JSON, not-an-object root); field-level failures are absorbed.
- **Option 2 — Atomic parse with `Default::default()` fallback (claudia-statusline pattern).** On any `serde_json::from_str` failure, log via `lsm_warn!` and substitute a `StatusContext::default()` where every top-level field is `None`. Simpler than Option 1 but loses field-level granularity — one bad field zeros all fields.
- **Option 3 — Schema bypass for `context_window` (CCometixLine / claudia pattern).** Stop trusting CC's `context_window` field; derive token counts from the transcript JSONL. Sidesteps the bug class entirely for that one field but doesn't generalize to other fields (model, workspace, cost, effort) that don't have a transcript equivalent. Adds a per-render JSONL parse cost.
- **Option 4 — Status quo + targeted null tolerance.** Keep atomic parse; add narrow null-handling to `parse_context_window` for the specific `used_percentage: null` case (lsm-6z9e quick fix). Fixes the one observed instance; leaves the architectural fragility in place; the next CC field shape change tanks the whole parse again.

## Decision Outcome

Chosen option: **Option 1 — per-field nullable, per-segment elision**, because (a) it's the convergent pattern across every working tool in the survey, (b) `StatusContext` already declares the relevant fields `Option<T>`, so the type-shape change is small relative to its impact, (c) it preserves CCometixLine-style segment isolation (`Option<SegmentData>` orchestrator pattern is already aligned with linesmith's existing render layer — we just need parse to feed it correctly), (d) it keeps diagnostics visible via `lsm_warn!` per failed sub-field rather than collapsing them into a single "parse failed" line, and (e) it avoids both Option 3's transcript dependency and Option 2's all-or-nothing collapse.

The chosen direction also resolves three open questions from the parse-failure survey:

- **Field-level Option vs. whole-field Option** for `ContextWindow`. Settled on **field-level Option** (e.g., `pub used: Option<Percent>`, `pub size: Option<u64>`). Strictly more information than whole-field collapse: when `context_window_size = 200000` is present but `used_percentage = null`, segments can still report "200k context window, no usage yet" rather than hiding the entire row.
- **`Default::default()` fallback (claudia) vs. per-field nulls (ccstatusline).** Per-field nulls. claudia's all-or-nothing zeroing matches Option 2 above and was rejected for the same reason — losing per-field granularity.
- **`lsm_warn!` placement.** **At the parser**, with full JSON path in the warning. Reasoning: parser-side warnings give loud signal of upstream contract drift the moment it happens; segment-side warnings only fire if the degraded field actually mattered to the rendered output, which conceals real schema regressions until they bite a user. linesmith spawns once per CC prompt with ~300ms debounce, so the upper bound on warns is one per failed field per spawn — small enough to flow to stderr without further rate-limiting machinery.

### Shape

This ADR's delta against the canonical shape in [docs/specs/input-schema.md](../specs/input-schema.md): widen `model` and `workspace` from required to `Option`, and convert `ContextWindow`'s leaf fields from required to `Option`. Other top-level fields (`session`, `vim`, `output_style`, `agent_name`) keep their declared shapes; this ADR doesn't touch them. The intended shape:

```rust
pub struct StatusContext {
    pub tool: Tool,
    pub model: Option<ModelInfo>,           // this ADR: was required
    pub session: SessionInfo,               // unchanged from spec
    pub workspace: Option<WorkspaceInfo>,   // this ADR: was required
    pub context_window: Option<ContextWindow>,  // unchanged at top level
    pub cost: Option<CostMetrics>,              // unchanged at top level
    pub effort: Option<EffortLevel>,            // unchanged at top level
    pub vim: Option<VimMode>,                   // unchanged from spec
    pub output_style: Option<OutputStyle>,      // unchanged from spec
    pub agent_name: Option<String>,             // unchanged from spec
    pub raw: Arc<serde_json::Value>,
}

pub struct ContextWindow {
    pub used: Option<Percent>,                  // this ADR: was Percent
    pub size: Option<u32>,                      // this ADR: was u32 (matches spec)
    pub total_input_tokens: Option<u64>,        // this ADR: was u64
    pub total_output_tokens: Option<u64>,       // this ADR: was u64
    pub current_usage: Option<TurnUsage>,       // unchanged
}
```

Note: implementation today has `pub size: u64` (input.rs:60), which already drifts from the spec's `u32`. The drift predates this ADR and isn't introduced by it; resolving it is a follow-up — the implementation epic bead should narrow `size` back to `u32` while applying the Option wrapper.

`input::parse(stdin) -> Result<StatusContext, ParseError>` keeps both the signature and the full `ParseError` enum (`#[non_exhaustive]`, removing variants would be a breaking change). Variants `MissingField`, `TypeMismatch`, and `InvalidValue` stay declared but in practice are constructed only when the failure is at the JSON root (e.g., input isn't an object). Every nested failure — missing fields, null sub-fields, type mismatches inside sub-objects — produces an `lsm_warn!` with full JSON path and a `None` for the affected field.

Sub-parsers (`parse_context_window`, `parse_cost`, etc.) keep their `fn(_) -> Result<Option<T>, ParseError>` signature: the `Result::Err` branch fires only when a sub-tree is structurally invalid in a way the warn-and-degrade contract can't sensibly absorb (e.g., `cost` is a string instead of an object). Even those cases can be downgraded to warn + `None` if the implementation finds no value in propagating; the ADR leaves that as a per-sub-parser judgment call.

Each segment that reads `ctx.status.X` already checks for `Option`; the change is mostly type-system enforcement of "you must check before you read" plus updating segments that previously assumed `model.display_name` and `workspace.project_dir` were always present.

### Consequences

- Good, because the fresh-session `?` window goes away — model/git/workspace render immediately on first stdin payload, even with `context_window` partially null.
- Good, because the next CC field shape change (effort object form, future fields) doesn't tank the whole render — only the affected segment.
- Good, because `lsm_warn!` per failed sub-field keeps upstream-contract-drift signal loud without collapsing the user-visible output.
- Good, because the type system enforces "check before read" for fields previously required (model, workspace), catching bugs at compile time during the migration.
- Bad, because every consumer of `ctx.status.model` and `ctx.status.workspace` has to handle `None`. Most segments that need these fields are already small enough to hide gracefully, but the refactor touches every segment.
- Bad, because plugin `ctx_mirror.rs` exposure changes — plugins that previously assumed `ctx.status.model.display_name` was always present now see `None` until first stdin populates it. Plugin authors writing against the old surface will need updates; mitigated by linesmith having no external plugin authors today (the rhai surface from ADR-0004 hasn't been documented for external use).
- Neutral, because the `Result` signature on `input::parse` is preserved — callers don't change their error-handling shape, and the `ParseError` variants stay declared (no breaking enum change), they're just constructed only for root-level failures in practice.
- Neutral, because every render is a fresh process (per ADR-0012); the per-spawn warn count is bounded by the number of failed fields per render, capping noise without dedicated rate-limiting machinery.

### Confirmation

Confirmed when:

- The lsm-6z9e regression test (replaying the captured pre-first-API-call payload) renders model/git_branch/workspace successfully with `context_window` segment hidden.
- A synthetic test fixture with malformed `cost.total_cost_usd` (e.g., `"cost": {"total_cost_usd": "not_a_number"}`) renders all other segments with the cost segment hidden and a single `lsm_warn!` emitted citing `cost.total_cost_usd`.
- `parse(b"{}").is_ok()` — completely empty payload returns a `StatusContext` with every top-level field `None`, not an error.
- `cargo test -p linesmith` green; new tests added for partial-data paths in each top-level segment.

Revisit if:

- Real CC stdin payloads start producing warnings in steady state (i.e., outside the documented pre-first-API-call window) — would suggest CC's contract has shifted enough that warn-on-every-failed-field is too noisy and the parser needs a different strategy (e.g., known-CC-quirks suppression list).
- Plugin authors emerge and complain about the breaking surface change — at that point the ADR may need superseding with a versioning strategy.
- A future field has semantics where "absent" and "present-but-null" mean different things and the all-Option flattening loses meaningful signal.

## Pros and Cons of the Options

### Option 1 — Per-field nullable, per-segment elision

- Good, because every working tool in the survey converges on this pattern.
- Good, because `StatusContext`'s existing partial-Option shape means the type-system delta is small (mostly extending Option to `model`/`workspace` and to ContextWindow's leaves).
- Good, because per-field warnings preserve diagnostic signal at the granularity the upstream contract drift actually occurs.
- Bad, because every consumer of newly-`Option` fields needs `if let Some(...)` updates. Migration touches segments + ctx_mirror + plugin docs.

### Option 2 — Atomic parse with `Default::default()` fallback

- Good, because it's the simplest possible implementation — one `match` arm in the parser entry point.
- Good, because it produces a guaranteed-renderable line under any input (even bytes that fail JSON parse).
- Bad, because one bad field zeros all fields; the user can't tell whether `cost: $0.00` means "no API calls yet" or "cost.total_cost_usd was malformed and we defaulted." Loses signal in the user-visible output.
- Bad, because it doesn't help the surfacing case — the lsm-6z9e payload isn't malformed JSON; it's valid JSON with null sub-fields, which Default::default() can't catch without per-field tolerance anyway.

### Option 3 — Schema bypass for `context_window`

- Good, because it sidesteps CC's flaky `context_window` field entirely.
- Good, because it gives linesmith the same "transcript-derived" robustness CCometixLine and claudia have.
- Bad, because it doesn't generalize. The bug pattern (CC sends a new shape, parser tanks) recurs for any other field — the next time `effort` or `rate_limits` shape shifts, we're back where we started.
- Bad, because it adds a per-render JSONL parse cost (~5-15ms per typical transcript, more for long sessions).
- Bad, because it depends on transcript layout stability, which is its own contract-fragility risk.

### Option 4 — Status quo + targeted null tolerance

- Good, because it's the smallest possible change — one `match` in `parse_context_window`, one test fixture.
- Good, because it's the right shape for the lsm-6z9e quick fix (ships in v0.1.2 regardless of this ADR's outcome).
- Bad, because it leaves the architectural fragility in place. The next CC field shape change tanks the whole parse and we file lsm-Nxxx for the same root cause.
- Bad, because it sets a precedent where parser fragility is patched per-bug rather than designed away.

## More Information

- [parse-failure-isolation-survey](../research/parse-failure-isolation-survey.md) — driving research, surveys ccstatusline / CCometixLine / claude-powerline / claudia-statusline / starship.
- [context-window-correctness](../research/context-window-correctness.md) — sister research that surfaced the lsm-6z9e payload via the lsm-mdd7 capture wrapper.
- [ADR-0006](0006-tool-agnostic-json-schema.md) — defines the `StatusContext` canonical shape this ADR amends.
- [ADR-0003](0003-segment-widget-system.md) — the segment/widget contract this ADR aligns the parser layer with.
- [ADR-0004](0004-rhai-for-plugins.md) — plugin contract; plugin `ctx_mirror.rs` exposure changes per the Consequences section.
- lsm-6z9e — P1 quick-fix bug (lands in v0.1.2 independent of this ADR).
- lsm-gyda — follow-up research note on starship's broader architectural patterns; out of scope here.
- Future: implementation epic bead depends on this ADR being accepted.
