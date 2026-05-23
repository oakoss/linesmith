//! `StatusContext` is the canonical, tool-agnostic model parsed from a
//! statusline JSON payload (Claude Code today; per-tool normalizers are
//! added as other tools wire in). Rate-limit windows live on
//! `DataContext::usage()` and are not parsed from stdin; see
//! `docs/specs/input-schema.md` for the full contract.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

/// The canonical, tool-agnostic input to the rendering pipeline. `Arc`
/// around `raw` keeps `StatusContext::clone` at O(1) when segments cache.
///
/// The stdin-payload `rate_limits` field is deliberately NOT parsed:
/// `ctx.usage()` (OAuth endpoint + JSONL fallback) is strictly richer,
/// per `docs/specs/rate-limit-segments.md`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StatusContext {
    pub tool: Tool,
    /// Per ADR-0014: `None` when the `model` wrapper is missing or
    /// malformed. Segments that depend on it hide.
    pub model: Option<ModelInfo>,
    /// Per ADR-0014: `None` when the `workspace` wrapper is missing or
    /// malformed (including a missing/null `project_dir`). Segments
    /// that depend on it hide.
    pub workspace: Option<WorkspaceInfo>,
    pub context_window: Option<ContextWindow>,
    pub cost: Option<CostMetrics>,
    pub effort: Option<EffortLevel>,
    pub vim: Option<VimMode>,
    pub output_style: Option<OutputStyle>,
    /// Active sub-agent name (collapsed from `agent.name` per ADR-0008).
    /// **Invariant:** `Some(s)` always carries a non-empty `s`; the
    /// parser folds null/missing/empty to `None`. See `lsm-srvz` for the
    /// follow-up to lift this into the type via a `NonEmptyString`.
    pub agent_name: Option<String>,
    /// Tool CLI version string from the top-level `version` field
    /// (e.g. Claude Code emits `"2.1.90"`). Trimmed; folds
    /// null/missing/empty/whitespace-only to `None`. Per
    /// `docs/specs/input-schema.md`, both Claude Code 2.x and Qwen
    /// Code emit this; it is no longer a tool-detection discriminator.
    pub version: Option<String>,
    pub raw: Arc<serde_json::Value>,
}

/// `Tool::Other(s)` is intentionally NOT canonicalized: it compares
/// unequal to a known variant even when `s.eq_ignore_ascii_case("claude")`.
/// Supply runtime-detected tool names through the public entry points
/// ([`parse_with_opts`] with [`ParseOpts::with_tool`], or the
/// `LINESMITH_TOOL` env var) — the internal alias table folds known
/// names into canonical variants before reaching `Other`, so direct
/// `Tool::Other("claude")`-style construction is a contract violation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tool {
    ClaudeCode,
    QwenCode,
    CodexCli,
    CopilotCli,
    /// Unknown tool; structure is parsed best-effort and tool-specific
    /// fields remain accessible via `StatusContext::raw`.
    Other(Cow<'static, str>),
}

impl std::fmt::Display for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ClaudeCode => f.write_str("claude"),
            Self::QwenCode => f.write_str("qwen"),
            Self::CodexCli => f.write_str("codex"),
            Self::CopilotCli => f.write_str("copilot"),
            Self::Other(name) => f.write_str(name),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub display_name: String,
}

#[derive(Debug, Clone)]
pub struct WorkspaceInfo {
    pub project_dir: PathBuf,
    pub git_worktree: Option<GitWorktree>,
}

#[derive(Debug, Clone)]
pub struct GitWorktree {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct ContextWindow {
    /// Used percentage. `remaining()` derives from this. Per ADR-0014,
    /// `None` when CC emits `used_percentage: null` (the pre-first-API-
    /// call window, see `docs/research/context-window-correctness.md`)
    /// or the leaf is otherwise malformed.
    pub used: Option<Percent>,
    /// Context-window size in tokens. `u32` matches ADR-0014's Shape
    /// section; values outside the u32 range degrade to `None`.
    pub size: Option<u32>,
    pub total_input_tokens: Option<u64>,
    pub total_output_tokens: Option<u64>,
    /// Tokens consumed by the most recent API call; `None` before the
    /// first call in a session. Distinct from `total_*_tokens` above,
    /// which are cumulative across the whole session.
    pub current_usage: Option<TurnUsage>,
}

impl ContextWindow {
    /// Percentage remaining; always consistent with `used`. Returns
    /// `None` when `used` is `None` (per-leaf Option per ADR-0014).
    #[must_use]
    pub fn remaining(&self) -> Option<Percent> {
        self.used.map(Percent::complement)
    }
}

/// Per-turn token breakdown from `context_window.current_usage`. All
/// counts are for the most recent API call only — use `ContextWindow`'s
/// `total_*_tokens` for cumulative session values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TurnUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct CostMetrics {
    /// Per ADR-0014, leaves degrade independently. `total_cost_usd:
    /// None` means the leaf was missing, null, or wrong-typed;
    /// segments hide the affected metric and unrelated cost leaves
    /// still render.
    pub total_cost_usd: Option<f64>,
    pub total_duration_ms: Option<u64>,
    pub total_api_duration_ms: Option<u64>,
    /// Session lines added; `u64` to match the JSON wire width and avoid
    /// silent truncation on sessions with very large aggregated counts.
    pub total_lines_added: Option<u64>,
    pub total_lines_removed: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
    XHigh,
}

impl EffortLevel {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Max => "max",
            Self::XHigh => "xhigh",
        }
    }
}

impl std::str::FromStr for EffortLevel {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "max" => Ok(Self::Max),
            "xhigh" => Ok(Self::XHigh),
            _ => Err(()),
        }
    }
}

/// Vim editing mode reflected from Claude Code's `vim.mode` field.
/// `Command` is Vim's `:`-prefix command-line buffer, not "a command was
/// run".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VimMode {
    Normal,
    Insert,
    Visual,
    Command,
    Replace,
}

impl VimMode {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Insert => "insert",
            Self::Visual => "visual",
            Self::Command => "command",
            Self::Replace => "replace",
        }
    }
}

impl std::str::FromStr for VimMode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "normal" => Ok(Self::Normal),
            "insert" => Ok(Self::Insert),
            "visual" => Ok(Self::Visual),
            "command" => Ok(Self::Command),
            "replace" => Ok(Self::Replace),
            _ => Err(()),
        }
    }
}

/// Active output style. Kept as a struct (rather than collapsing to
/// `Option<String>`) so `name` can later evolve to an enum with a
/// `Custom(String)` variant without breaking downstream type signatures.
/// See ADR-0008.
///
/// **Invariant:** `name` is never empty. The Claude normalizer collapses
/// empty/null/missing names to `Option::None` at the parser boundary, so
/// every `Some(OutputStyle)` reaching a segment carries a non-empty name.
/// In-crate constructors should preserve this contract; lsm-srvz tracks
/// lifting it into the type system via a constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct OutputStyle {
    pub name: String,
}

/// Percentage in `0.0..=100.0`. Construction outside that range returns
/// `None` so normalizers can translate to `ParseError::InvalidValue`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, serde::Serialize)]
pub struct Percent(f32);

impl Percent {
    #[must_use]
    pub fn new(value: f32) -> Option<Self> {
        if (0.0..=100.0).contains(&value) {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Construct from an `f64` (JSON's native number width). Range check
    /// runs before narrowing, so values like `100.0000001` that would
    /// round down to `100.0` in the cast are rejected rather than silently
    /// accepted.
    #[must_use]
    pub fn from_f64(value: f64) -> Option<Self> {
        if (0.0..=100.0).contains(&value) {
            Some(Self(value as f32))
        } else {
            None
        }
    }

    /// Construct from an `f64`, clamping finite out-of-range values into
    /// `0.0..=100.0`. Returns `None` only for NaN. Use this when a field's
    /// upstream producer is known to emit values slightly past 100 (e.g.
    /// Claude Code's `context_window.used_percentage` post-`/compact`,
    /// see claude-code#37163). Callers that want visibility into the
    /// clamp should compare the raw value against the range before
    /// invoking and emit a diagnostic — this helper is silent.
    #[must_use]
    pub fn from_f64_clamped(value: f64) -> Option<Self> {
        if value.is_nan() {
            return None;
        }
        Some(Self(value.clamp(0.0, 100.0) as f32))
    }

    #[must_use]
    pub fn value(self) -> f32 {
        self.0
    }

    /// `100.0 - self`, always in-range.
    #[must_use]
    pub fn complement(self) -> Self {
        Self(100.0 - self.0)
    }
}

// --- Parse entry + error taxonomy -------------------------------------

/// Caller-side hooks for [`parse_with_opts`].
///
/// Only `tool` is wired (overrides heuristic detection). Marked
/// `#[non_exhaustive]` so adding more knobs later (per-tool feature
/// toggles, sample-rate caps, etc.) is non-breaking.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ParseOpts {
    /// Force the detected tool. Skips both the `LINESMITH_TOOL` env
    /// override and the shape-based heuristic. `None` runs the full
    /// detection precedence per `docs/specs/input-schema.md`.
    pub tool: Option<Tool>,
}

impl ParseOpts {
    /// Set the explicit tool override.
    #[must_use]
    pub fn with_tool(mut self, tool: Tool) -> Self {
        self.tool = Some(tool);
        self
    }
}

/// Parse a statusline JSON payload into a [`StatusContext`].
///
/// Equivalent to [`parse_with_opts`] with [`ParseOpts::default`]. Use
/// this for the common case; pass opts when you need to force a tool
/// (tests, plugin harnesses, sample fixtures).
///
/// # Errors
///
/// See [`parse_with_opts`].
pub fn parse(input: &[u8]) -> Result<StatusContext, ParseError> {
    parse_with_opts(input, &ParseOpts::default())
}

/// Parse a statusline JSON payload into a [`StatusContext`] with caller
/// hooks. Tool detection follows the precedence in
/// `docs/specs/input-schema.md` §"Heuristic detection": opts override
/// → `LINESMITH_TOOL` env → shape heuristic → Fallback (ClaudeCode).
///
/// # Errors
///
/// Per ADR-0014, sub-field failures degrade to [`Option::None`] with
/// `lsm_warn!` rather than propagating through `Result`. Returns `Err`
/// only for catastrophic failures:
/// [`ParseError::InvalidJson`] on malformed JSON,
/// [`ParseError::TypeMismatch`] when the root is not a JSON object,
/// and [`ParseError::InvalidValue`] for a `used_percentage` < 0
/// (carve-out for undocumented CC corruption signals; NaN is rejected
/// upstream by `serde_json` as `InvalidJson`).
pub fn parse_with_opts(input: &[u8], opts: &ParseOpts) -> Result<StatusContext, ParseError> {
    let raw_value: serde_json::Value =
        serde_json::from_slice(input).map_err(|err| ParseError::InvalidJson {
            message: err.to_string(),
            // serde_json returns 0/0 for non-positional errors (e.g. EOF
            // before any content); only carry a position when it's real.
            location: (err.line() > 0).then(|| SourcePos {
                line: err.line(),
                column: err.column(),
            }),
        })?;

    let raw = Arc::new(raw_value);
    normalizers::dispatch(raw, opts)
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ParseError {
    InvalidJson {
        message: String,
        location: Option<SourcePos>,
    },
    /// **Reserved variant — not currently constructed by any parser
    /// path.** Per ADR-0014, missing leaves degrade to `Option::None`
    /// with `lsm_warn!`, never `Err`. The variant stays declared so
    /// re-introducing a strict required-field policy in a future ADR
    /// is non-breaking; today it cannot fire and pattern-matching for
    /// it as a distinct case is dead code.
    MissingField {
        tool: Tool,
        path: String,
    },
    /// The JSON kind at `path` didn't match what the normalizer expected.
    /// Used strictly for JSON-shape mismatches; value-domain failures
    /// (e.g. out-of-range percentage) use `InvalidValue`.
    TypeMismatch {
        tool: Tool,
        path: String,
        expected: JsonType,
        got: JsonType,
    },
    /// JSON kind matched but the value violates a canonical-model
    /// invariant (e.g. a percentage field was NaN or below 0, or an
    /// enum-like string carried an unknown variant).
    InvalidValue {
        tool: Tool,
        path: String,
        reason: &'static str,
    },
    NormalizerError {
        tool: Tool,
        message: String,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct SourcePos {
    /// 1-indexed line (matches serde_json and editor conventions).
    pub line: usize,
    /// 1-indexed column (matches serde_json).
    pub column: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonType {
    Object,
    Array,
    String,
    Number,
    Bool,
    Null,
}

impl JsonType {
    #[must_use]
    pub fn of(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Object(_) => Self::Object,
            serde_json::Value::Array(_) => Self::Array,
            serde_json::Value::String(_) => Self::String,
            serde_json::Value::Number(_) => Self::Number,
            serde_json::Value::Bool(_) => Self::Bool,
            serde_json::Value::Null => Self::Null,
        }
    }
}

impl std::fmt::Display for JsonType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Object => "object",
            Self::Array => "array",
            Self::String => "string",
            Self::Number => "number",
            Self::Bool => "bool",
            Self::Null => "null",
        };
        f.write_str(name)
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson { message, location } => match location {
                Some(pos) => write!(f, "invalid JSON at {}:{}: {message}", pos.line, pos.column),
                None => write!(f, "invalid JSON: {message}"),
            },
            Self::MissingField { tool, path } => {
                write!(f, "missing field {} for {tool}", display_path(path))
            }
            Self::TypeMismatch {
                tool,
                path,
                expected,
                got,
            } => {
                write!(
                    f,
                    "type mismatch at {} for {tool}: expected {expected}, got {got}",
                    display_path(path)
                )
            }
            Self::InvalidValue { tool, path, reason } => {
                write!(
                    f,
                    "invalid value at {} for {tool}: {reason}",
                    display_path(path)
                )
            }
            Self::NormalizerError { tool, message } => {
                write!(f, "normalizer error for {tool}: {message}")
            }
        }
    }
}

fn display_path(path: &str) -> String {
    if path.is_empty() {
        "<root>".to_string()
    } else {
        format!("{path:?}")
    }
}

impl std::error::Error for ParseError {}

mod normalizers;

#[cfg(test)]
mod tests;
