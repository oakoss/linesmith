# Input Schema

- Status: draft
- Version: 0.2
- Last updated: 2026-04-17
- Driving ADRs: [ADR-0001](../adrs/0001-use-rust-for-runtime.md), [ADR-0003](../adrs/0003-segment-widget-system.md), [ADR-0008](../adrs/0008-canonical-type-refinements.md) (supersedes [ADR-0006](../adrs/0006-tool-agnostic-json-schema.md))

## Overview

linesmith reads a JSON payload on stdin from an AI coding CLI (Claude Code, Qwen Code, eventually Codex/Copilot) and parses it into a canonical internal model called `StatusContext`. This spec defines:

1. The `StatusContext` struct and all its sub-types, expressed in Rust
2. Tool detection: how we decide which incoming schema we're parsing
3. Per-tool normalizers: how we map vendor JSON to the canonical model
4. Nullability: which fields can legitimately be absent and why
5. Error handling: what happens when input is malformed, unknown, or partial

Everything downstream (segments, themes, config, plugins) consumes `StatusContext`, not raw JSON. Getting this contract right isolates tool-specific quirks to normalizers and keeps the rendering pipeline vendor-agnostic.

## Requirements

### Functional

- Parse the full Claude Code statusline JSON schema (see `research/claude-code-statusline-api.md`) into `StatusContext`
- Parse Qwen Code JSON (near-identical shape per `research/cross-tool-statusline-support.md`) into the same `StatusContext`
- Leave extension points for OpenAI Codex CLI and GitHub Copilot CLI without rewriting the core model
- Preserve tool-specific fields not represented in the canonical model via `raw: Arc<serde_json::Value>`, so plugins can access them
- Never silently drop data; if a field is unknown, it survives in `raw`
- Model all legitimately-absent fields as `Option<T>` (documented in Edge cases)
- Make illegal states unrepresentable where feasible (invariant-preserving newtypes, sum-type collapse)
- Provide clear, actionable parse errors with field path and typed expected/actual JSON kinds

### Non-functional

- Parse time: <1ms for a 2KB payload (serde_json is fast enough; not the bottleneck)
- `StatusContext::clone` is O(1) (raw JSON lives behind `Arc`)
- Zero unsafe code; this is a trust boundary, safety matters more than the 5% allocator win
- Work on macOS, Linux, and Windows (cwd / path fields must be `PathBuf`, not `String`)
- Binary-size cost: schema types should not pull in heavy dependencies (jiff is the only justified extra for timestamps; avoid uuid, url, etc. unless required)

## Interface / Contract

### Top-level type

```rust
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use jiff::Timestamp;

/// The canonical, tool-agnostic input to linesmith's rendering pipeline.
/// Every segment receives a reference to this. Cloning is O(1) because
/// `raw` lives behind an `Arc`.
#[derive(Debug, Clone)]
pub struct StatusContext {
    /// Which upstream tool produced the payload. Only field guaranteed
    /// populated by `normalize`; everything else may be `None` per
    /// ADR-0014's per-leaf warn-and-degrade contract.
    pub tool: Tool,

    /// Per ADR-0014: `None` when the wrapper is missing or malformed.
    /// Segments hide; unrelated segments still render.
    pub model: Option<ModelInfo>,
    pub workspace: Option<WorkspaceInfo>,

    /// Potentially absent. See Edge cases section.
    pub context_window: Option<ContextWindow>,
    pub cost: Option<CostMetrics>,
    // Rate-limit data lives on `ctx.usage()` (OAuth endpoint + JSONL
    // fallback cascade) per rate-limit-segments.md; the stdin
    // `rate_limits` field is deliberately NOT parsed.
    pub effort: Option<EffortLevel>,
    pub vim: Option<VimMode>,
    pub output_style: Option<OutputStyle>,
    pub agent_name: Option<String>,
    /// Tool CLI version string from the top-level `version` field
    /// (e.g. Claude Code emits `"2.1.90"`). Folds null/missing/empty
    /// to `None`.
    pub version: Option<String>,

    /// Full original JSON. Plugins consult this for tool-specific fields
    /// that aren't in the canonical model.
    pub raw: Arc<serde_json::Value>,
}
```

### Tool identification

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    ClaudeCode,
    QwenCode,
    CodexCli,
    CopilotCli,
    /// Unknown tool; structure is parsed best-effort and tool-specific
    /// fields remain accessible via `raw`. Carries a forensic identifier
    /// (heuristic guess or runtime-supplied name) for debugging.
    Other(Cow<'static, str>),
}
```

`Tool::Other` carries a `Cow<'static, str>` so fallback names like `"unknown".into()` stay zero-alloc while runtime-detected identifiers allocate once. Pattern matching stays exhaustive; see [ADR-0008](../adrs/0008-canonical-type-refinements.md) for the rationale.

### Invariant-preserving primitives

```rust
/// A percentage in `0.0..=100.0`. Cannot be constructed out of range.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct Percent(f32);

impl Percent {
    pub fn new(v: f32) -> Option<Self> {
        (0.0..=100.0).contains(&v).then_some(Self(v))
    }
    pub fn value(self) -> f32 { self.0 }
    pub fn complement(self) -> Self { Self(100.0 - self.0) } // always in range
}
```

`Percent` prevents `used + remaining != 100` and negative percentages. Normalizers construct via `Percent::from_f64`; per ADR-0014, missing/null/wrong-typed `used_percentage` warn-and-degrade to `Option::None`, while a negative value raises `ParseError::InvalidValue` (carve-out — undocumented CC state surfaces loud). `Percent::from_f64` also rejects `NaN` defensively (NaN fails the range check because `(0.0..=100.0).contains(&NaN)` returns `false`); the NaN branch is unreachable through `parse()` because `serde_json` rejects NaN literals as `InvalidJson` upstream. `Percent` does not derive `Eq`/`Hash` because `f32` lacks total ordering in general; if a segment's cache invalidator needs to compare percentages, compare the underlying `f32` directly.

### Sub-types

```rust
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub display_name: String,   // e.g. "Claude Sonnet 4.6"
}

#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub project_dir: PathBuf,   // project root
    pub git_worktree: Option<GitWorktree>,
}

#[derive(Debug, Clone)]
pub struct GitWorktree {
    pub name: String,           // worktree name (e.g. "main" or a feature branch name)
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ContextWindow {
    /// Per ADR-0014, leaves degrade independently. `None` arises when
    /// CC emits the leaf as null (the documented pre-first-API-call
    /// shape for `used`) or the leaf is otherwise malformed.
    pub used: Option<Percent>,
    pub size: Option<u32>,            // e.g. 200_000 for Sonnet, 1_000_000 for 1M contexts
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    /// None before the first API call in a session. TurnUsage's
    /// inner leaves are non-Option — when `current_usage` is `Some`,
    /// every field is populated.
    pub current_usage: Option<TurnUsage>,
}

impl ContextWindow {
    /// Remaining percentage; `None` iff `used` is `None`.
    pub fn remaining(&self) -> Option<Percent> { self.used.map(Percent::complement) }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct CostMetrics {
    /// Per ADR-0014, leaves degrade independently. `None` arises on
    /// missing/null/wrong-typed leaves.
    pub total_cost_usd: Option<f64>,
    pub total_duration_ms: Option<u64>,
    pub total_api_duration_ms: Option<u64>,
    pub total_lines_added: Option<u64>,
    pub total_lines_removed: Option<u64>,
}

// Rate-limit data is not modeled on StatusContext — it lives on
// `DataContext::usage()` (OAuth endpoint + JSONL fallback cascade) per
// [rate-limit-segments.md](rate-limit-segments.md).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
    XHigh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    Command,
    Replace,
}

#[derive(Debug, Clone)]
pub struct OutputStyle {
    pub name: String,  // e.g. "default", "concise", "explanatory"
}
```

Flattened wrappers (removed in ADR-0008): `VimState` collapses to `VimMode` directly on `StatusContext`; `AgentInfo` collapses to `agent_name: Option<String>`. `OutputStyle` retains its struct shell because `name` may evolve into an enum with `Custom(String)`.

### Parser entry point

```rust
/// Parse stdin-style JSON bytes into a StatusContext.
///
/// Tool detection order (first match wins):
///   1. Explicit override via `opts.tool` (from --tool flag)
///   2. `LINESMITH_TOOL` env var (set by caller)
///   3. Heuristic detection from JSON shape
///   4. Default to ClaudeCode
pub fn parse(input: &[u8], opts: &ParseOptions) -> Result<StatusContext, ParseError>;

pub struct ParseOptions {
    pub tool: Option<Tool>,
}

/// Typed JSON kinds used in `ParseError::TypeMismatch`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonType {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

/// Source position within the input. Optional because some errors
/// (empty input, non-UTF-8 bytes) aren't positional. `line` and
/// `column` are 1-indexed to match serde_json's `Error::line`/`Error::column`
/// and most editor conventions.
#[derive(Debug, Clone, Copy)]
pub struct SourcePos {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug)]
pub enum ParseError {
    /// Not valid JSON at all.
    InvalidJson {
        message: String,
        location: Option<SourcePos>,
    },
    /// JSON is valid but required field missing.
    MissingField {
        tool: Tool,
        path: String,   // e.g. "model.display_name"
    },
    /// Field present but wrong JSON type.
    TypeMismatch {
        tool: Tool,
        path: String,
        expected: JsonType,
        got: JsonType,
    },
    /// Tool-specific normalizer failed (with tool context).
    NormalizerError {
        tool: Tool,
        message: String,
    },
}
```

### Normalizer trait (internal)

```rust
trait Normalizer {
    fn tool(&self) -> Tool;
    fn detect(raw: &serde_json::Value) -> bool;
    fn normalize(raw: Arc<serde_json::Value>) -> Result<StatusContext, ParseError>;
}
```

Concrete normalizers live in `crates/linesmith-core/src/input/normalizers/`:

- `claude.rs`: primary, most complete. Also the **Fallback** for tools whose detection rule is a stub today; the dispatcher passes the detected `Tool` through so the resulting `StatusContext` and any `ParseError` carry the right discriminator.
- `qwen.rs`: doc-only stub. Qwen routes through the Fallback until a discriminator that survives CC 2.x's `version` emission materializes.
- `other.rs`: doc-only stub covering `CodexCli`, `CopilotCli`, and `Tool::Other(_)`. Each gets its own file when detection lands; consolidating stubs avoids one empty file per tool today.

### Error path naming convention

`ParseError::MissingField.path` and `ParseError::TypeMismatch.path` use the **incoming JSON field path** (dot-separated), not the canonical Rust field name. For example, if Claude's `context_window.used_percentage` field contains an out-of-range value, the error carries `path: "context_window.used_percentage"` — matching what a user or debugger sees in the raw payload — even though the normalized Rust type stores it as `ContextWindow.used: Percent`. Keep the reporter aligned with the wire; translation to the Rust shape is an internal concern.

## Behavior

### Parse flow

```text
stdin bytes
    │
    ▼
serde_json::from_slice → Value  ───► InvalidJson on failure
    │
    ▼
Arc-wrap Value
    │
    ▼
tool detection
    ├─ opts.tool (if Some) ──────────► chosen tool
    ├─ env LINESMITH_TOOL ──────────► chosen tool
    ├─ heuristic (shape match) ────► chosen tool
    └─ default ClaudeCode ───────────► chosen tool
    │
    ▼
normalizer dispatch
    │
    ▼
StatusContext (or ParseError)
```

### Heuristic detection

Priority order, first true wins:

1. **ClaudeCode**: `cost` object present with `total_api_duration_ms` field (Claude-specific). The top-level `version` string is no longer a distinguishing field — Claude Code 2.x emits it too, alongside Qwen.
2. **QwenCode**: _stub; the prior `version`-presence rule is invalid since CC 2.x. A new discriminator needs a current Qwen payload to compare against current CC. Until then, Qwen routes through the Fallback at rule 5._
3. **CodexCli**: _stub; detection to be defined when Codex ships the statusLine API_
4. **CopilotCli**: _stub; detection TBD_
5. **Fallback**: `ClaudeCode` (most common, most conservative; Qwen fields are a near-superset so this degrades gracefully)

If heuristic matching is ambiguous, emit a warning-level log line once (to stderr; stdout is reserved for rendering) and proceed with the guessed tool.

### Normalizer behavior

- Each normalizer parses only fields in the canonical model; everything else stays in `raw`
- Unknown enum values (e.g. a new `EffortLevel` string) warn-and-degrade to `None` per ADR-0014, with the unknown raw value logged at the JSON path
- Paths (`project_dir`, `git_worktree.path`, etc.) use `PathBuf::from`, preserving platform-native separators
- Timestamps parse via `s.parse::<jiff::Timestamp>()` (RFC 3339, always UTC)
- Nullable fields stay `None` when absent or JSON-null
- `Percent::new` failure on a negative or NaN `used_percentage` becomes `ParseError::InvalidValue` (ADR-0014 carve-out: undocumented CC state surfaces loud rather than degrading silently). An above-100 value clamps to 100 with a warn (claude-code#37163).

## Edge cases

Per [ADR-0014](../adrs/0014-best-effort-parse-with-segment-isolation.md), sub-field failures degrade per-leaf to `Option::None` with `lsm_warn!` for diagnostics; only catastrophic root failures (invalid JSON, non-object root) surface as `ParseError`. Segments check `is_none()` and elide.

### Field-level

| Case                                                                                             | Handling                                                                                                                      |
| ------------------------------------------------------------------------------------------------ | ----------------------------------------------------------------------------------------------------------------------------- |
| `model` absent or malformed                                                                      | `StatusContext.model = None`; segments hide. Wrapper-non-object/non-string `display_name` warns.                              |
| `workspace` absent or malformed                                                                  | `StatusContext.workspace = None`; segments hide. Wrapper-non-object/non-string `project_dir` warns.                           |
| `workspace.git_worktree` is `null` or absent                                                     | `WorkspaceInfo.git_worktree = None` (not in a worktree). Non-object wrapper warns; the `project_dir` peer survives.           |
| `current_usage` is `null` or absent                                                              | `ContextWindow.current_usage = None` (pre-first-call). Non-object warns.                                                      |
| `current_usage` inner field null/missing                                                         | `ContextWindow.current_usage = None` (TurnUsage is all-or-nothing; partial token counts mislead).                             |
| `context_window` leaf `null` (any of `used_percentage`, `context_window_size`, `total_*_tokens`) | Per-leaf `None`; peers survive. Type drift on a leaf warns.                                                                   |
| `context_window.context_window_size` exceeds `u32::MAX`                                          | `ContextWindow.size = None` + warn (no real CC payload comes anywhere close).                                                 |
| `used_percentage` > 100                                                                          | Clamp to 100 + warn (claude-code#37163 known upstream bug).                                                                   |
| `used_percentage` < 0 or NaN                                                                     | `ParseError::InvalidValue` (carve-out from warn-and-degrade — undocumented CC state, surfacing loud catches real corruption). |
| `cost` absent or non-object wrapper                                                              | `StatusContext.cost = None`; segments hide.                                                                                   |
| `cost` leaf null/missing/non-finite                                                              | Per-leaf `None`; peers survive. Type drift warns.                                                                             |
| `cost.total_cost_usd` present but `0.0`                                                          | Emit as-is (zero is valid, not missing).                                                                                      |
| `effort` absent, null, malformed, or unknown level                                               | `StatusContext.effort = None`; warn for unknown variants and type drift.                                                      |
| `vim` absent, null, malformed, or unknown mode                                                   | `StatusContext.vim = None`; warn for unknown variants and type drift.                                                         |
| `output_style` absent, null, or malformed                                                        | `StatusContext.output_style = None`; warn for type drift.                                                                     |
| `agent` absent, null, or malformed                                                               | `StatusContext.agent_name = None`; warn for type drift.                                                                       |
| `version` absent, null, empty, whitespace-only, or non-string                                    | `StatusContext.version = None`; warn for type drift.                                                                          |
| `rate_limits` field present on stdin                                                             | Ignored — see [rate-limit-segments.md](rate-limit-segments.md); `ctx.usage()` is the data source.                             |

### Input-level

| Case                                       | Handling                                                                                          |
| ------------------------------------------ | ------------------------------------------------------------------------------------------------- |
| Empty stdin                                | `ParseError::InvalidJson { message: "empty input", location: None }`                              |
| Non-UTF-8 bytes                            | `ParseError::InvalidJson { location: None }`                                                      |
| Malformed JSON (truncated, invalid escape) | `ParseError::InvalidJson { location: Some(SourcePos { .. }) }`                                    |
| Valid JSON but not an object at top level  | `ParseError::TypeMismatch { path: "", expected: Object, got: ? }`                                 |
| Empty object `{}`                          | `Ok(StatusContext)` with every top-level Option field `None`; `tool` and `raw` populate normally. |
| Unknown tool, heuristic fails              | Fall through to `ClaudeCode` default; most fields will still parse.                               |

### Tool-level

| Case                                               | Handling                                                                                                                                                                                                                                                                                                 |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `opts.tool = Some(CodexCli)` but payload is Claude | Route through the Fallback (Claude normalizer) while preserving `Tool::CodexCli` on `StatusContext.tool` and any `ParseError`.                                                                                                                                                                           |
| `opts.tool = None`, env unset, heuristic works     | Detected tool used                                                                                                                                                                                                                                                                                       |
| `opts.tool = None`, env set to invalid name        | Route to `Tool::Other(Cow::Owned(trimmed))` so the operator-supplied name reaches downstream diagnostics; a warn-level log echoes the unknown alias so a typo'd `LINESMITH_TOOL` is visible at default verbosity.                                                                                        |
| `Tool::Other("gemini")` passed via `--tool` or env | Route through the Fallback (Claude normalizer); `other.rs` is a doc-only stub today and will gain its own normalizer once a real discriminator lands. The `Tool::Other` variant preserves the operator-supplied name on `StatusContext.tool` and any `ParseError` so diagnostics show the input as typed |

### Cross-tool normalization

- Claude and Qwen share ~95% of schema; the Qwen normalizer, once it exists, will delegate to the Claude normalizer for common fields and handle the deltas
- When Codex and Copilot ship, their normalizers split out of `other.rs` into `codex.rs` and `copilot.rs` rather than modifying the Claude normalizer
- The canonical model evolves via new optional fields; existing fields never break

## Testing strategy

Follows the testing approach in `AGENTS.md`: inline `#[cfg(test)] mod tests` for unit tests (colocated with the code), `tests/` directory for integration, `insta` for snapshots, `proptest` for property tests, `criterion` for benchmarks. No coverage-number target; this module is a trust boundary, so every branch needs coverage.

### Unit tests (inline `mod tests` per file)

- `Percent::new`: in-range, out-of-range, edge values (0.0, 100.0, -0.0, NaN)
- `Tool` variant parsing and display, including `Tool::Other(...)` roundtrip
- `EffortLevel` string parsing (low/medium/high/max/xhigh, unknown variant fallback)
- `VimMode` string parsing
- `JsonType` display and round-trip
- `PathBuf` handling on Windows (forward vs backward slashes)

### Integration tests (in `crates/linesmith/tests/`)

Each uses a JSON fixture in `tests/fixtures/`:

- `claude_pro_full.json`: Pro tier with all fields
- `claude_api_minimal.json`: API tier (current_usage null)
- `claude_first_call.json`: after first API call (current_usage populated)
- `claude_1m_context.json`: 1M context window
- `claude_post_compact.json`: post-`/compact` state (context % behavior)
- `claude_worktree.json`: inside a git worktree
- `qwen_full.json`: Qwen Code payload
- `malformed_truncated.json`: truncated JSON
- `empty.json`: zero-byte
- `non_object_root.json`: root is a JSON array

For each fixture, assert the parsed `StatusContext` matches an `insta` snapshot, or that the expected `ParseError` variant is returned.

### Snapshot tests

`insta` for `StatusContext` → JSON snapshots. Snapshots live under `crates/linesmith/tests/snapshots/` and are reviewed on PRs. Re-accepting snapshots requires explicit `cargo insta review`.

### Property tests

`proptest`:

- Round-trip: random `StatusContext` → serialize → parse → equal?
- `Option<T>` preservation: `None` stays `None`, `Some(x)` stays `Some(x)`
- Unknown fields in input JSON: adding an unrecognized field never changes the typed fields
- `Percent::new`: for any `f32 x`, `Percent::new(x).is_some() == (0.0..=100.0).contains(&x)`

### Benchmarks

`criterion` in `crates/linesmith/benches/`:

- `parse_claude_pro_full` (cold): <1ms target
- `parse_claude_pro_full` (warm): <200µs target
- `parse_qwen_full`: sanity

## Open questions

- **Datetime library choice.** Resolved 2026-05-04 (lsm-3912): jiff replaces chrono workspace-wide. jiff reads the system tzdb on macOS/Linux at runtime; only Windows builds embed the IANA database via `jiff-tzdb`.
- **Should `parse` consume the `&[u8]` or take ownership?** Current design: borrow. If we need the raw bytes for plugin access, we clone into `raw` anyway (then `Arc`-wrap).
- **How do we version the schema itself?** No explicit version field in the canonical model; we version via breaking type changes + migration guides. A v2 schema would mean a new `StatusContextV2` type and a migration path.
- **Should `OutputStyle.name` become an enum with `Custom(String)`?** Deferred until we enumerate Claude's actual output-style values.

## Change log

- 2026-04-17: initial draft (v0.1)
- 2026-04-17: v0.2 incorporating [ADR-0008](../adrs/0008-canonical-type-refinements.md) refinements (Percent newtype, RateLimits enum collapse, Tool::Other(Cow), JsonType + optional SourcePos, Arc<Value> for raw, flattened VimState/AgentInfo)
- 2026-04-21: removed `rate_limits` field, `RateLimits` enum, and `RateLimitWindow` struct. Rate-limit data is sourced via `ctx.usage()` (OAuth endpoint + JSONL fallback cascade); the stdin field is no longer parsed. Driven by lsm-7po; see [rate-limit-segments.md](rate-limit-segments.md).
