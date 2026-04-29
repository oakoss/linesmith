# Parse-failure isolation across CC statusline tools (and starship)

- Date: 2026-04-25
- Author: Jace Babin (w/ Claude Code)
- Scope: Survey how comparable tools handle CC stdin parse failures and partial-data states. Drives [ADR-0014](../adrs/0014-best-effort-parse-with-segment-isolation.md). Surfacing case is `lsm-6z9e` (parse_context_window TypeMismatch on null sub-fields tanks the entire statusline render).

## Question

linesmith's `input::parse(stdin)` is atomic: any TypeMismatch in any nested field collapses the whole `StatusContext` and the driver renders `?` for everything. A captured CC 2.1.120 pre-first-API-call payload triggers this:

```json
"context_window": {
  "total_input_tokens": 0,
  "total_output_tokens": 0,
  "context_window_size": 200000,
  "current_usage": null,
  "used_percentage": null,
  "remaining_percentage": null
}
```

`parse_context_window` calls `require_f64("context_window.used_percentage")` → null fails the whole parse → every segment renders `?` for the ~15-second pre-first-call window at every fresh CC session. Architecturally: should parse be atomic, or should each field/segment fail independently?

How do other tools answer this?

## Sources

- ccstatusline @ `cef29e12` — `src/types/StatusJSON.ts`, `src/utils/context-window.ts`, `src/widgets/ContextPercentage.ts`, `src/types/Widget.ts`, `src/ccstatusline.ts`. Stack: TS + zod v4. 8.2k⭐.
- CCometixLine @ `a73b1665` — `src/config/types.rs`, `src/core/segments/mod.rs`, `src/core/segments/context_window.rs`, `src/core/statusline.rs`, `src/main.rs`. Stack: Rust + serde. 2.8k⭐.
- claude-powerline @ `a3abd71c` — `src/utils/claude.ts`, `src/index.ts`, `src/powerline.ts`, `src/segments/context.ts`. Stack: TS, no validator. 1.0k⭐.
- claudia-statusline @ `ebb79b8c` — `src/models.rs`, `src/main.rs`, `src/utils.rs`, `src/display.rs`. Stack: Rust + serde + SQLite. 26⭐.
- starship @ `c97455d9` — `src/print.rs`, `src/utils/statusline.rs`, `src/modules/mod.rs`, `src/modules/claude_context.rs`. Stack: Rust + serde + rayon. 56.8k⭐. Not CC-specific but the canonical "many-segment, segment-isolation" prompt tool.
- claude-hud, felipeelias' tool: excluded. claude-hud is a plugin package, not a stdin-fed statusline; felipeelias' tool wasn't locatable via `gh search`.

## Findings

### ccstatusline

**Parse strategy.** Single zod schema `StatusJSONSchema` (`src/types/StatusJSON.ts:22`) declared `z.looseObject(...)` with **every nested field marked `.nullable().optional()`**. Numeric leaves wrapped in a custom `CoercedNumberSchema` (defined separately at `src/types/StatusJSON.ts:3`) that string-coerces but tolerates whatever it gets. The whole `context_window` object (`src/types/StatusJSON.ts:47-67`) is itself `.nullable().optional()`, including `used_percentage: CoercedNumberSchema.nullable().optional()` at line 60. Validation uses `safeParse` at the entry point (`src/ccstatusline.ts:307-315`); on schema fail it `console.error`s and `process.exit(1)`. The schema is permissive enough that almost nothing fails.

**Segment isolation.** Each widget is `Widget.render(item, context, settings): string | null` (`src/types/Widget.ts:39`). Returning `null` omits the widget. No shared `Result` plumbing — widgets are independent.

**`used_percentage: null` outcome.** `getContextWindowMetrics` (`src/utils/context-window.ts:17-23`) short-circuits if `context_window` is missing, then runs every numeric field through `toFiniteNonNegativeNumber` which returns `null` for any non-number. `ContextPercentageWidget.render` (`src/widgets/ContextPercentage.ts:42-63`) returns `null` if `usedPercentage` is `null` and no transcript fallback exists. The widget hides; every other widget renders normally.

### CCometixLine

**Parse strategy.** `InputData` (`src/config/types.rs:84-92`) is a serde struct with **`context_window` not present at all**. They sidestep the field by reading token usage directly from the transcript JSONL (`src/core/segments/context_window.rs:99-152`, `parse_transcript_usage`). Stdin parse is atomic (`serde_json::from_reader(stdin.lock())?` in `src/main.rs:60`) but the schema's required surface is small.

**Segment isolation.** `Segment` trait (`src/core/segments/mod.rs:16-24`) returns `Option<SegmentData>`. The orchestrator in `core/statusline.rs:46-58` filters out empty renders: `if !rendered.is_empty() { output.push(rendered); }`. Each segment owns its failure boundary.

**`used_percentage: null` outcome.** Irrelevant — they don't consume the field. `ContextWindowSegment` reads transcript JSONL; if no transcript usage is found, it emits `"-"` placeholders (`src/core/segments/context_window.rs:55-56`) but still returns `Some(SegmentData)` so the segment renders with literal dashes.

### claude-powerline

**Parse strategy.** No schema validator. `ClaudeHookData` is a hand-written `interface` (`src/utils/claude.ts:6-58`) with `used_percentage?: number | null` and `current_usage?` both optional. Stdin parse is `await json(process.stdin)` cast straight to `ClaudeHookData` (`src/index.ts:69`) — pure structural typing. A string where a number is expected slips through and breaks only the consumer that touches it. On stdin error, `console.error` + `process.exit(1)`.

**Segment isolation.** Renderer (`src/powerline.ts`) calls each provider only when at least one configured line uses it (`needsSegmentInfo`, line 134). Each provider returns `T | null` and renders independently.

**`used_percentage: null` outcome.** `calculateContextFromHookData` (`src/segments/context.ts:88-130`) only consults `used_percentage` after computing tokens from `current_usage`; explicit `if (nativePct != null)` null-guards. With both null, returns `null` and falls back to transcript parsing.

### claudia-statusline

**Parse strategy.** The most aggressive recovery in the survey. `StatuslineInput` (`src/models.rs:13-24`) is `#[derive(Default, Deserialize)]` with **every top-level field `Option<T>`**. There's no `context_window` field — like CCometixLine, they pull from transcript. The recovery pattern is at `src/main.rs:255-263`:

```rust
let input: StatuslineInput = match serde_json::from_str(&buffer) {
    Ok(input) => input,
    Err(e) => {
        warn!("Failed to parse JSON input: {}. Using defaults.", e);
        StatuslineInput::default()
    }
};
```

**They fall back to `Default::default()` on any parse failure** and log a warning. Combined with everything being `Option<T>`, any malformed payload still produces a mostly-empty but renderable statusline.

**Segment isolation.** No formal segment trait; `format_output` in `src/display.rs` builds output by procedurally inspecting the parsed input with `if let Some(...)` checks. No per-segment Result, but no shared atomic state either.

**`used_percentage: null` outcome.** Not consumed. Context comes from `calculate_context_usage` in `src/utils.rs:519` reading the transcript file by path.

### starship

**Parse strategy.** Recently added CC stdin support. `prompt_with_claude_code` (`src/print.rs:80-90`):

```rust
let claude_data = serde_json::from_reader(io::stdin())
    .inspect_err(|e| log::error!("Failed to read Claude Code JSON from stdin: {e}"))
    .unwrap_or_default();
```

`ClaudeCodeData` (`src/utils/statusline.rs:5-58`) uses `#[derive(Default, Deserialize)] #[serde(default)]` on every struct, but **fields are non-Optional concrete types** (e.g. `pub used_percentage: f32`, `pub context_window_size: u64`). `#[serde(default)]` rescues _missing_ fields but **not `null`** — null on a non-Option field produces a serde TypeMismatch.

**Module isolation.** starship's general module system (independent of CC) treats every module as `pub fn handle(module, context) -> Option<Module>` (`modules/mod.rs:121`), invoked from `handle_module` (`print.rs:343-373`). Modules are processed via `rayon::par_iter()`; each returns `Option<Module>` and `None` simply omits it. The contract is uniform across the 128+ modules in the codebase.

**`used_percentage: null` outcome (inferred, not runtime-tested).** stdin fails to deserialize → `unwrap_or_default()` → `claude_context::module` reads `claude_data.context_window.used_percentage = 0.0` and renders "0%" with the gauge empty. **Worse than ccstatusline** for this specific case: starship would show false-correct zero-usage data rather than hiding the gauge. Other (non-CC) modules are entirely unaffected.

The "0%" inference is mechanical (serde rejects null on f32 → `unwrap_or_default` chain → all CC fields zero). Local empirical validation was attempted but the test machine's starship config doesn't enable CC modules, so the rendered output didn't include them. Validation at runtime against a CC-module-enabled starship config would confirm the prediction; deferred as low-value since the failure mode is type-system-determined.

## Cross-cutting comparison

| Tool               | Parse strategy                                                                                | Segment isolation                                                    | `used_percentage: null` outcome                                               |
| ------------------ | --------------------------------------------------------------------------------------------- | -------------------------------------------------------------------- | ----------------------------------------------------------------------------- |
| ccstatusline       | zod schema, every field `.nullable().optional()`, `safeParse` + `process.exit(1)` on fail     | `Widget.render → string \| null`                                     | Context % widget hides; all other widgets render                              |
| CCometixLine       | serde, narrow schema; `context_window` field omitted entirely (transcript-based)              | `Segment → Option<SegmentData>`; orchestrator filters empties        | Field not consumed; transcript path gives `"-"` placeholder                   |
| claude-powerline   | No validator; `await json(stdin) as ClaudeHookData`                                           | Per-provider `T \| null`                                             | Returns null, falls back to transcript                                        |
| claudia-statusline | serde with all fields `Option<T>`, `Default::default()` on parse fail + `warn!` log           | Procedural `if let Some(...)`                                        | Field not consumed (transcript-based)                                         |
| starship           | serde `#[serde(default)]` but **non-Optional** concrete fields; `unwrap_or_default()` on fail | Per-module `fn module(context) → Option<Module>`, parallel via rayon | Whole `ClaudeCodeData` zeroed → renders "0%" empty gauge for _all_ CC modules |

## Synthesis

Three patterns dominate, none used in pure form by linesmith today:

1. **Permissive schema, per-field nulls (ccstatusline).** Every nested field `Option`/nullable in the type itself, with an aggressive normalizer that converts everything to `null` on bad data. Per-widget render returns `null` to hide. Best partial-rendering coverage; cost is verbose option chasing in every consumer.

2. **Schema bypass via transcript (CCometixLine, claudia-statusline).** Don't trust CC's `context_window`; derive token counts from the transcript JSONL. Whole class of bug disappears. Cost is a per-render JSONL parse and reliance on transcript stability.

3. **Atomic parse with default fallback (claudia-statusline, starship).** On `serde_json::from_str` failure, log and use `Default::default()`. Combined with `Option<T>` everywhere (claudia) it's graceful; with concrete-typed fields (starship) it silently produces zero-valued data, which is worse than the failure case for a field like `used_percentage` because you can't distinguish "0% used" from "we don't know."

**Universal element across all four working tools:** every one returns an `Option`-shaped per-segment value (`string | null`, `Option<SegmentData>`, `T | null`, `Option<Module>`) so segments are independently elideable. **No tool has a shared `Result` that, when `Err`, blanks the whole prompt** — which is exactly what linesmith's atomic `parse(stdin) → Result<StatusContext, _>` produces.

**What this implies for linesmith** (debated formally in [ADR-0014](../adrs/0014-best-effort-parse-with-segment-isolation.md)):

- **At parse:** make `parse_context_window` (and the other sub-parsers) tolerate null sub-fields and downgrade to `Ok(None)` for the whole field rather than propagate `TypeMismatch` up. Concretely: wrap each `require_*` call in a `try_*` variant that yields `None` on TypeMismatch and emits `lsm_warn!` for diagnostics, OR make every leaf field `Option<T>` and let downstream consumers null-check. ccstatusline's `getContextWindowMetrics` is the reference implementation here.

- **At render:** segments must already individually decide they have nothing to render and elide. The "directory and git branch shouldn't wait for context_window" complaint is solved structurally if directory and git never depended on `StatusContext` parse success — they only need `cwd`, which is robust.

The combination — best-effort parse + per-segment elision — is what every working tool in the field converges on. starship's case is instructive: per-module isolation at the render layer doesn't help when the deserializer has already collapsed the shared input struct upstream.

## Implications / actions

- **lsm-6z9e** — quick fix for the immediate `parse_context_window` null-sub-field bug. Shipped in v0.1.2 ahead of the ADR.
- **ADR-0014** — formalized the architectural shift to best-effort parse + per-segment Option-shaped contract; chose Option 1 (permissive parse + per-segment Option) per the synthesis.
- **Implementation: lsm-9zvh** — the implementation epic. Refactor landed across `input.rs` (all sub-parsers warn-and-degrade per-leaf), every segment that consumes `model`/`workspace`/`context_window`/`cost`/`effort`/`vim`/`output_style`/`agent_name`/`version`, plugin `ctx_mirror.rs`, and tests pinning the partial-data contract.

## Open questions

- **starship's `used_percentage: null` outcome is inferred, not empirically tested.** Worth confirming if a future contributor hits the case in the wild. Not blocking the ADR; the inference is mechanical from the type signature.
- **Should the `ContextWindow` struct itself become field-level `Option`** (e.g. `pub used: Option<Percent>`) so we can partially populate it (e.g. `context_window_size` is known even when `used_percentage` is null), rather than collapsing the whole field to `None`? ccstatusline's pattern is per-field nulls; we'd be matching it more closely. Tradeoff is more option chasing in segments. Worth comparing in ADR-0014's Considered Options.
- **claudia's `Default::default()` fallback vs. ccstatusline's per-field nulls.** Both produce a renderable line under partial data, but they have different failure-mode signatures: claudia fails-loud on first parse error and degrades the whole struct to defaults (one big diagnostic, everything zeroed); ccstatusline degrades per-field silently (no diagnostic unless a widget logs it). ADR-0014 needs to pick one (or hybridize) — synthesis recommends the per-field path, but the choice deserves explicit framing in the ADR's Considered Options.
- **Where do `lsm_warn!` diagnostics for downgraded fields belong** — at the parser (one warn per failed sub-field, with full path) or at the segment (one warn per "I tried to render but had no data")? Parser-side gives loud signal of upstream contract drift; segment-side keeps the parse silent and only warns when a degraded field actually mattered to the rendered statusline. ADR-0014 needs to pick.
