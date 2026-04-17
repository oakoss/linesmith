# Design the input schema as a union of Claude + Qwen fields with per-tool normalizers

- Status: accepted
- Date: 2026-04-17
- Deciders: Jace

## Context and Problem Statement

Research (`research/cross-tool-statusline-support.md`) shows that the `statusLine` contract (command invoked with JSON on stdin, text on stdout) is becoming a de-facto cross-tool standard. Two tools ship it today (Claude Code, Qwen Code) with ~95% schema overlap. Two more (OpenAI Codex, GitHub Copilot CLI) have active feature requests explicitly adopting Claude's shape. Should linesmith's internal data model be Claude-specific or tool-agnostic?

## Decision Drivers

- Future-proofing: tool-agnostic positioning is core to our name decision ([ADR-0002](0002-name-linesmith.md))
- Code reuse: 95%+ of segments should work on every supported tool without special-casing
- Minimal per-tool code: vendor-specific quirks handled at the edge, not throughout the codebase
- No overfitting to future tools: design for Claude + Qwen now; leave space for Codex/Copilot without premature abstraction
- Clear error surfaces: when a required field is missing, the failure mode should be explicit (e.g., "no rate limits on API-key tier")

## Considered Options

- **Claude-only schema** — model exactly Claude's fields; add support for other tools later as forks or branches
- **Qwen-compatible only** — model Qwen's shape (which is Claude-compatible); works for both
- **Union schema + thin normalizer** — define a canonical internal model; per-tool normalizers map incoming JSON to it
- **Plugin-based adapters** — each tool's input handled by a separate adapter plugin

## Decision Outcome

Chosen option: **Union schema + thin per-tool normalizer**, because it's the only option that lets segments be written once and work across Claude Code and Qwen Code today (nearly free given schema similarity) while leaving an obvious extension point for Codex and Copilot when they ship. Plugin-based adapters would be over-abstracted for two known tools; Claude-only would require a rewrite when Qwen support becomes compelling.

Tool detection strategy (resolved at startup):

1. Explicit `--tool` CLI flag (overrides everything)
2. `LINESMITH_TOOL` env var
3. Heuristic from input JSON shape (presence of tool-specific fields)
4. Default: Claude Code

Internal canonical model:

```rust
pub struct StatusContext {
    pub tool: Tool,                      // enum Claude | Qwen | Codex | Copilot | Other(String)
    pub model: ModelInfo,
    pub session: SessionInfo,
    pub workspace: WorkspaceInfo,        // cwd, git_worktree, project_dir
    pub context_window: Option<ContextWindow>,
    pub cost: Option<CostMetrics>,
    pub rate_limits: Option<RateLimits>, // only Claude Pro/Max today
    pub effort: Option<EffortLevel>,     // only Claude (partial support per user demand)
    pub vim: Option<VimState>,
    pub raw: serde_json::Value,          // kept for plugin access to tool-specific fields
}
```

Normalizer responsibility: parse tool-specific JSON into this model. Absent fields become `None`; tool-specific extras live in `raw`.

### Consequences

- Good, because ~95% of segments are tool-agnostic by default — they render `StatusContext`, not vendor JSON
- Good, because shipping a Qwen preset alongside Claude is nearly free given the schema overlap
- Good, because adding Codex/Copilot support when they ship is one normalizer file + a preset
- Good, because plugins can access tool-specific fields via `raw` when they need to, without forcing the core to handle vendor weirdness
- Good, because explicit nullability (`Option<T>`) means segments must handle missing fields — no silent rendering of bogus data
- Bad, because the canonical model must evolve as new tools appear — we'll accumulate optional fields
- Bad, because tool detection heuristics may misidentify in edge cases (mitigated by the `--tool` flag and env var)
- Neutral, because internally the schema is slightly richer than any single vendor's — no tool sees its full payload, just its subset

### Confirmation

Revisit if:

- Canonical model accumulates more than ~5 tool-specific optional fields (signals abstraction is leaking)
- A new tool's input diverges enough that the union approach becomes painful
- Segments begin routinely switching on `ctx.tool` to render — signals that tool-specific normalization is leaking into rendering

## Pros and Cons of the Options

### Claude-only schema

- Good: simplest today, no abstraction to build
- Bad: every other tool requires a fork or schema migration
- Bad: contradicts the linesmith name decision ([ADR-0002](0002-name-linesmith.md)) which is premised on tool-agnostic positioning
- Bad: wastes the nearly-free Qwen support that the research identified

### Qwen-compatible only

- Good: works for Claude too (schemas overlap)
- Bad: Qwen's shape is slightly narrower than Claude's — we'd lose access to Claude-specific fields like `rate_limits` detail
- Bad: positions Qwen as primary when Claude has dominant market share

### Union schema + thin normalizer (chosen)

- Good: segments work everywhere without special-casing
- Good: tool-specific quirks isolated to normalizers
- Good: clear extension path for Codex/Copilot
- Bad: requires upfront design of the canonical model
- Bad: more code than a single-tool schema

### Plugin-based adapters

- Good: maximum flexibility for future tools
- Bad: over-abstracted for two known tools today
- Bad: would require a plugin API that's rich enough to express arbitrary schemas — big investment, little payoff

## More Information

- Driven by: `research/cross-tool-statusline-support.md` (de-facto standard emerging), `research/claude-code-statusline-api.md` (Claude schema shape)
- Related ADRs: [ADR-0002](0002-name-linesmith.md) (tool-agnostic naming), [ADR-0003](0003-segment-widget-system.md) (segments render `StatusContext`)
- Will drive: `specs/input-schema.md` (full canonical model), `specs/tool-normalizers.md` (per-tool detection and mapping)
