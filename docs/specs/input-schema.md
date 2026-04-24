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
- Binary-size cost: schema types should not pull in heavy dependencies (chrono is the only justified extra for timestamps; avoid uuid, url, etc. unless required)

## Interface / Contract

### Top-level type

```rust
use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;
use chrono::{DateTime, Utc};

/// The canonical, tool-agnostic input to linesmith's rendering pipeline.
/// Every segment receives a reference to this. Cloning is O(1) because
/// `raw` lives behind an `Arc`.
#[derive(Debug, Clone)]
pub struct StatusContext {
    /// Which upstream tool produced the payload.
    pub tool: Tool,

    /// Always present across every tool.
    pub model: ModelInfo,
    pub session: SessionInfo,
    pub workspace: WorkspaceInfo,

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

`Percent` prevents `used + remaining != 100` and negative percentages. Normalizers construct via `Percent::new` and propagate the `Option` or map to `ParseError::TypeMismatch`. `Percent::new` rejects `NaN` (NaN fails the range check because `(0.0..=100.0).contains(&NaN)` returns `false`); normalizers therefore never produce a `Some(Percent(NaN))`. `Percent` does not derive `Eq`/`Hash` because `f32` lacks total ordering in general; if a segment's cache invalidator needs to compare percentages, compare the underlying `f32` directly.

### Sub-types

```rust
#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub id: String,             // e.g. "claude-sonnet-4-6"
    pub display_name: String,   // e.g. "Claude Sonnet 4.6"
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub name: Option<String>,   // not all tools emit one
}

#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub cwd: PathBuf,           // absolute path from invocation
    pub current_dir: PathBuf,   // relative to project_dir in Claude
    pub project_dir: PathBuf,   // project root
    pub added_dirs: Vec<PathBuf>,
    pub git_worktree: Option<GitWorktree>,
}

#[derive(Debug, Clone)]
pub struct GitWorktree {
    pub name: String,           // worktree name (e.g. "main" or a feature branch name)
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ContextWindow {
    /// Used percentage. `remaining()` returns `used.complement()`.
    pub used: Percent,
    pub size: u32,              // e.g. 200_000 for Sonnet, 1_000_000 for 1M contexts
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    /// None before the first API call in a session.
    pub current_usage: Option<TokenUsage>,
}

impl ContextWindow {
    /// Remaining percentage; always consistent with `used`.
    pub fn remaining(&self) -> Percent { self.used.complement() }
}

#[derive(Debug, Clone)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct CostMetrics {
    pub total_cost_usd: f64,
    pub total_duration_ms: u64,
    pub total_api_duration_ms: u64,
    pub total_lines_added: u32,
    pub total_lines_removed: u32,
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

Concrete normalizers live in `crates/linesmith/src/input/normalizers/`:

- `claude.rs`: primary, most complete
- `qwen.rs`: leans on claude.rs; maps the ~5% differences
- `codex.rs`: stub (activates when Codex ships statusLine API)
- `copilot.rs`: stub
- `other.rs`: best-effort fallback; populates what it can, leaves rest in `raw`

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

1. **QwenCode**: `version` field present at top level (Qwen emits a version; Claude does not)
2. **ClaudeCode**: `cost` object present with `total_api_duration_ms` field (Claude-specific)
3. **CodexCli**: _stub; detection to be defined when Codex ships the statusLine API_
4. **CopilotCli**: _stub; detection TBD_
5. **Fallback**: `ClaudeCode` (most common, most conservative; Qwen fields are a near-superset so this degrades gracefully)

If heuristic matching is ambiguous, emit a warning-level log line once (to stderr; stdout is reserved for rendering) and proceed with the guessed tool.

### Normalizer behavior

- Each normalizer parses only fields in the canonical model; everything else stays in `raw`
- Unknown enum values (e.g. a new `EffortLevel` string) fall back to the closest known value and log a warning once per variant per run
- Paths (`cwd`, `current_dir`, etc.) use `PathBuf::from`, preserving platform-native separators
- Timestamps parse with `chrono::DateTime::parse_from_rfc3339` and convert to UTC
- Nullable fields stay `None` when absent or JSON-null
- `Percent::new` failure (out-of-range value in input JSON) becomes `ParseError::TypeMismatch` with `path` pointing at the offending field

## Edge cases

### Field-level

| Case                                         | Handling                                                                                         |
| -------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `current_usage` is `null`                    | `ContextWindow.current_usage = None` (pre-first-call)                                            |
| `rate_limits` field present on stdin         | Ignored — see [rate-limit-segments.md](rate-limit-segments.md); `ctx.usage()` is the data source |
| `workspace.git_worktree` is `null` or absent | `WorkspaceInfo.git_worktree = None` (not in a worktree)                                          |
| `added_dirs` is absent                       | `WorkspaceInfo.added_dirs = Vec::new()`                                                          |
| `vim` is absent                              | `StatusContext.vim = None`                                                                       |
| `cost.total_cost_usd` present but `0.0`      | Emit as-is (zero is valid, not missing)                                                          |
| `context_window.size` is `0`                 | Parse as-is; segments decide whether to render                                                   |
| `used_percentage` out of `0.0..=100.0`       | `ParseError::TypeMismatch { path: "context_window.used_percentage", ... }`                       |
| `effort` field absent or `null`              | `StatusContext.effort = None`                                                                    |
| `effort` is object `{"level": "xhigh"}`      | Canonical shape as of Claude Code 2.1.x; parse `effort.level` (bare-string form also accepted)   |
| `effort.level` absent in object form         | `ParseError::MissingField { path: "effort.level" }`                                              |
| `effort.level` is explicit `null`            | `StatusContext.effort = None` (same as absent outer `effort`)                                    |

### Input-level

| Case                                              | Handling                                                             |
| ------------------------------------------------- | -------------------------------------------------------------------- |
| Empty stdin                                       | `ParseError::InvalidJson { message: "empty input", location: None }` |
| Non-UTF-8 bytes                                   | `ParseError::InvalidJson { location: None }`                         |
| Malformed JSON (truncated, invalid escape)        | `ParseError::InvalidJson { location: Some(SourcePos { .. }) }`       |
| Valid JSON but not an object at top level         | `ParseError::TypeMismatch { path: "", expected: Object, got: ? }`    |
| Tool detected, normalizer fails on required field | `ParseError::MissingField { tool, path }`                            |
| Unknown tool, heuristic fails                     | Fall through to `ClaudeCode` default; most fields will still parse   |

### Tool-level

| Case                                               | Handling                                                                                                         |
| -------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `opts.tool = Some(CodexCli)` but payload is Claude | Use Codex normalizer anyway; most fields fail → `NormalizerError`. User asked for it; we don't second-guess.     |
| `opts.tool = None`, env unset, heuristic works     | Detected tool used                                                                                               |
| `opts.tool = None`, env set to invalid name        | Log warning, fall back to heuristic                                                                              |
| `Tool::Other("gemini")` passed via `--tool`        | Dispatch to `other.rs`; identifier stored on the `Tool::Other` variant so logs show which name the user supplied |

### Cross-tool normalization

- Claude and Qwen share ~95% of schema; Qwen normalizer delegates to Claude normalizer for common fields and handles the deltas
- When Codex and Copilot ship, their normalizers are new files, not modifications to Claude's
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

- **Chrono vs. a lighter time crate?** Chrono is ~350KB; `time` is lighter but has had API churn. Decision: chrono for now (widely used, stable); revisit if binary size becomes a problem.
- **Should `parse` consume the `&[u8]` or take ownership?** Current design: borrow. If we need the raw bytes for plugin access, we clone into `raw` anyway (then `Arc`-wrap).
- **How do we version the schema itself?** No explicit version field in the canonical model; we version via breaking type changes + migration guides. A v2 schema would mean a new `StatusContextV2` type and a migration path.
- **Should `OutputStyle.name` become an enum with `Custom(String)`?** Deferred until we enumerate Claude's actual output-style values.

## Change log

- 2026-04-17: initial draft (v0.1)
- 2026-04-17: v0.2 incorporating [ADR-0008](../adrs/0008-canonical-type-refinements.md) refinements (Percent newtype, RateLimits enum collapse, Tool::Other(Cow), JsonType + optional SourcePos, Arc<Value> for raw, flattened VimState/AgentInfo)
- 2026-04-21: removed `rate_limits` field, `RateLimits` enum, and `RateLimitWindow` struct. Rate-limit data is sourced via `ctx.usage()` (OAuth endpoint + JSONL fallback cascade); the stdin field is no longer parsed. Driven by lsm-7po; see [rate-limit-segments.md](rate-limit-segments.md).
