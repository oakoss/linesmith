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
    pub model: ModelInfo,
    pub workspace: WorkspaceInfo,
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
    pub raw: Arc<serde_json::Value>,
}

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
    /// Used percentage. `remaining()` derives from this.
    pub used: Percent,
    pub size: u64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    /// Tokens consumed by the most recent API call; `None` before the
    /// first call in a session. Distinct from `total_*_tokens` above,
    /// which are cumulative across the whole session.
    pub current_usage: Option<TurnUsage>,
}

impl ContextWindow {
    /// Percentage remaining; always consistent with `used`.
    #[must_use]
    pub fn remaining(&self) -> Percent {
        self.used.complement()
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
    pub total_cost_usd: f64,
    pub total_duration_ms: u64,
    pub total_api_duration_ms: u64,
    /// Session lines added; `u64` to match the JSON wire width and avoid
    /// silent truncation on sessions with very large aggregated counts.
    pub total_lines_added: u64,
    pub total_lines_removed: u64,
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

/// Parse a Claude Code statusline JSON payload into a `StatusContext`.
///
/// Currently dispatches to the Claude normalizer unconditionally;
/// tool-detection heuristics are added when a second tool is wired in.
///
/// # Errors
///
/// Returns `ParseError::InvalidJson` on malformed JSON, `MissingField`
/// when a required key is absent, `TypeMismatch` when a value has the
/// wrong JSON kind, `InvalidValue` when a value violates a canonical-model
/// invariant (e.g. out-of-range percentage), and `NormalizerError` for
/// tool-specific mapping failures.
pub fn parse(input: &[u8]) -> Result<StatusContext, ParseError> {
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
    claude::normalize(raw)
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ParseError {
    InvalidJson {
        message: String,
        location: Option<SourcePos>,
    },
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
                write!(f, "missing field {} for {tool:?}", display_path(path))
            }
            Self::TypeMismatch {
                tool,
                path,
                expected,
                got,
            } => {
                write!(
                    f,
                    "type mismatch at {} for {tool:?}: expected {expected}, got {got}",
                    display_path(path)
                )
            }
            Self::InvalidValue { tool, path, reason } => {
                write!(
                    f,
                    "invalid value at {} for {tool:?}: {reason}",
                    display_path(path)
                )
            }
            Self::NormalizerError { tool, message } => {
                write!(f, "normalizer error for {tool:?}: {message}")
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

// --- Claude normalizer -----------------------------------------------
//
// One normalizer today, inline. This module moves to
// `input/normalizers/claude.rs` alongside siblings when a second tool
// lands.

mod claude {
    use super::{
        ContextWindow, CostMetrics, EffortLevel, GitWorktree, JsonType, ModelInfo, OutputStyle,
        ParseError, Percent, StatusContext, Tool, TurnUsage, VimMode, WorkspaceInfo,
    };
    use std::path::PathBuf;
    use std::sync::Arc;

    const TOOL: Tool = Tool::ClaudeCode;

    pub fn normalize(raw: Arc<serde_json::Value>) -> Result<StatusContext, ParseError> {
        let root = expect_object(&raw, "")?;

        let model = parse_model(root)?;
        let workspace = parse_workspace(root)?;
        let context_window = parse_context_window(root)?;
        let cost = parse_cost(root)?;
        let effort = parse_effort(root)?;
        let vim = parse_vim(root)?;
        let output_style = parse_output_style(root)?;
        let agent_name = parse_agent_name(root)?;

        Ok(StatusContext {
            tool: TOOL,
            model,
            workspace,
            context_window,
            cost,
            effort,
            vim,
            output_style,
            agent_name,
            raw,
        })
    }

    fn parse_model(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<ModelInfo, ParseError> {
        let model = root
            .get("model")
            .ok_or_else(|| missing("model"))
            .and_then(|v| expect_object(v, "model"))?;
        let display_name = require_string(model, "model.display_name")?.to_owned();
        Ok(ModelInfo { display_name })
    }

    fn parse_workspace(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<WorkspaceInfo, ParseError> {
        let workspace = root
            .get("workspace")
            .ok_or_else(|| missing("workspace"))
            .and_then(|v| expect_object(v, "workspace"))?;
        let project_dir = PathBuf::from(require_string(workspace, "workspace.project_dir")?);

        let git_worktree = match workspace.get("git_worktree") {
            Some(serde_json::Value::Null) | None => None,
            Some(serde_json::Value::Object(obj)) => {
                let name = require_string(obj, "workspace.git_worktree.name")?.to_owned();
                let path = PathBuf::from(require_string(obj, "workspace.git_worktree.path")?);
                Some(GitWorktree { name, path })
            }
            Some(other) => {
                return Err(type_mismatch(
                    "workspace.git_worktree",
                    JsonType::Object,
                    JsonType::of(other),
                ));
            }
        };

        Ok(WorkspaceInfo {
            project_dir,
            git_worktree,
        })
    }

    fn parse_context_window(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<ContextWindow>, ParseError> {
        let Some(value) = root.get("context_window") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let cw = expect_object(value, "context_window")?;

        // Pre-first-API-call payloads (CC 2.1.x) carry a context_window
        // object with `used_percentage` null — there's no usage data
        // yet. (`current_usage: null` is the same shape, handled by
        // parse_current_usage's own guard below.) Treat any required-
        // leaf null the same as `context_window: null` and hide the
        // segment for the ~15s pre-first-call window. Defensive sweep
        // on the other required leaves keeps the next CC schema wobble
        // (a single `total_*_tokens: null`) from tanking the segment.
        for key in [
            "used_percentage",
            "context_window_size",
            "total_input_tokens",
            "total_output_tokens",
        ] {
            if cw.get(key).is_some_and(serde_json::Value::is_null) {
                let version = root
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown");
                crate::lsm_debug!(
                    "context_window.{key} is null (CC {version}); hiding context segment \
                     — expected during pre-first-API-call window; persistence past the \
                     first assistant response indicates real schema drift"
                );
                return Ok(None);
            }
        }

        let used_raw = require_f64(cw, "context_window.used_percentage")?;
        // Asymmetric handling. Claude Code has been observed emitting
        // values slightly past 100 post-/compact (claude-code#37163)
        // so an above-100 value is a known upstream bug: clamp to 100
        // and warn, rather than degrade the whole statusline to `?`.
        // A below-zero value is NOT a documented Claude Code state —
        // it points at a corrupted payload or a misrouted upstream —
        // so let it surface as InvalidValue so the failure is loud.
        let used = if used_raw > 100.0 {
            crate::lsm_warn!("context_window.used_percentage = {used_raw} > 100; clamping to 100",);
            Percent::from_f64_clamped(used_raw).expect("non-NaN value > 100 clamps successfully")
        } else {
            Percent::from_f64(used_raw).ok_or_else(|| {
                invalid_value(
                    "context_window.used_percentage",
                    "percentage must be a number in [0, 100]",
                )
            })?
        };

        let size = require_u64(cw, "context_window.context_window_size")?;
        let total_input_tokens = require_u64(cw, "context_window.total_input_tokens")?;
        let total_output_tokens = require_u64(cw, "context_window.total_output_tokens")?;
        let current_usage = parse_current_usage(cw)?;

        Ok(Some(ContextWindow {
            used,
            size,
            total_input_tokens,
            total_output_tokens,
            current_usage,
        }))
    }

    fn parse_current_usage(
        cw: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<TurnUsage>, ParseError> {
        // Claude Code emits `current_usage: null` before the first API
        // call in a session (see docs/research/claude-code-statusline-api.md).
        // The key's presence isn't guaranteed by the schema either, so
        // tolerate outright omission as defense-in-depth; both map to
        // Option::None.
        let Some(value) = cw.get("current_usage") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let obj = expect_object(value, "context_window.current_usage")?;
        Ok(Some(TurnUsage {
            input_tokens: require_u64(obj, "context_window.current_usage.input_tokens")?,
            output_tokens: require_u64(obj, "context_window.current_usage.output_tokens")?,
            cache_creation_input_tokens: require_u64(
                obj,
                "context_window.current_usage.cache_creation_input_tokens",
            )?,
            cache_read_input_tokens: require_u64(
                obj,
                "context_window.current_usage.cache_read_input_tokens",
            )?,
        }))
    }

    fn parse_cost(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<CostMetrics>, ParseError> {
        let Some(value) = root.get("cost") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let cost = expect_object(value, "cost")?;

        let total_cost_usd = require_f64(cost, "cost.total_cost_usd")?;
        let total_duration_ms = require_u64(cost, "cost.total_duration_ms")?;
        let total_api_duration_ms = require_u64(cost, "cost.total_api_duration_ms")?;
        let total_lines_added = require_u64(cost, "cost.total_lines_added")?;
        let total_lines_removed = require_u64(cost, "cost.total_lines_removed")?;

        Ok(Some(CostMetrics {
            total_cost_usd,
            total_duration_ms,
            total_api_duration_ms,
            total_lines_added,
            total_lines_removed,
        }))
    }

    fn parse_effort(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<EffortLevel>, ParseError> {
        let Some(value) = root.get("effort") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        // Claude Code 2.1.x emits `effort: { level: "xhigh" }`; the object
        // form is canonical. A bare string is tolerated for forward/backward
        // compat with other tools or earlier contracts.
        let (raw, path): (&str, &'static str) = match value {
            serde_json::Value::Object(obj) => {
                let level = obj.get("level").ok_or_else(|| missing("effort.level"))?;
                if level.is_null() {
                    return Ok(None);
                }
                let s = level.as_str().ok_or_else(|| {
                    type_mismatch("effort.level", JsonType::String, JsonType::of(level))
                })?;
                (s, "effort.level")
            }
            serde_json::Value::String(s) => (s.as_str(), "effort"),
            other => {
                return Err(type_mismatch(
                    "effort",
                    JsonType::Object,
                    JsonType::of(other),
                ));
            }
        };
        raw.parse::<EffortLevel>()
            .map(Some)
            .map_err(|()| invalid_value(path, "expected one of: low, medium, high, max, xhigh"))
    }

    fn parse_vim(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<VimMode>, ParseError> {
        let Some(value) = root.get("vim") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        // Canonical CC shape is `vim: { mode: "<name>" }` per
        // research/claude-code-statusline-api.md. A bare string is
        // tolerated for forward/backward compat (mirrors the effort
        // normalizer's two-shape acceptance).
        let (raw, path): (&str, &'static str) = match value {
            serde_json::Value::Object(obj) => {
                let mode = obj.get("mode").ok_or_else(|| missing("vim.mode"))?;
                if mode.is_null() {
                    return Ok(None);
                }
                let s = mode.as_str().ok_or_else(|| {
                    type_mismatch("vim.mode", JsonType::String, JsonType::of(mode))
                })?;
                (s, "vim.mode")
            }
            serde_json::Value::String(s) => {
                // Bare-string is a tolerated forward/backward-compat
                // shape; canonical CC emits `vim: { mode: "..." }`.
                // Log when the fallback fires so it leaves a trail
                // whether CC drifts to bare-string or a non-canonical
                // producer slips in.
                crate::lsm_debug!(
                    "vim: accepted bare-string compat shape {:?}; canonical is {{ mode }}",
                    s
                );
                (s.as_str(), "vim")
            }
            other => {
                return Err(type_mismatch("vim", JsonType::Object, JsonType::of(other)));
            }
        };
        // Unknown vim modes degrade the segment, not the whole render:
        // `vim` is opt-in and informational, so a future CC mode (e.g.
        // `select`, `terminal`) shouldn't blank the statusline. Warn so
        // schema drift surfaces at the default log level. The strict
        // `MissingField` / `TypeMismatch` paths above stay as-is because
        // those signal a malformed wrapper, not an unrecognized variant.
        // ADR-0014 / lsm-9zvh extends this discipline to other enum
        // parsers (`parse_effort`).
        match raw.parse::<VimMode>() {
            Ok(mode) => Ok(Some(mode)),
            Err(()) => {
                crate::lsm_warn!(
                    "vim: unknown mode {raw:?} at {path}; treating as None (possible CC schema drift — known: normal, insert, visual, command, replace)"
                );
                Ok(None)
            }
        }
    }

    fn parse_output_style(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<OutputStyle>, ParseError> {
        let Some(value) = root.get("output_style") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let obj = expect_object(value, "output_style")?;
        // Tolerate a missing `name` as None so the schema can grow,
        // but warn: the wrapper is present, so a future CC rename of
        // `name` would otherwise dark-ship this segment without a
        // diagnostic trail.
        //
        // This deliberately diverges from `parse_effort` / `parse_vim`,
        // which raise `MissingField` on absent inner keys. Effort and
        // vim mirror closed enums whose inner key is part of the
        // canonical CC contract; output_style and agent are open-ended
        // wrapper structs whose shape may grow (per ADR-0008), so the
        // parser soft-tolerates and surfaces drift through logging
        // instead of through the parse error path.
        let Some(name_value) = obj.get("name") else {
            crate::lsm_warn!(
                "output_style: wrapper present but `name` field missing; treating as None (possible CC schema drift)"
            );
            return Ok(None);
        };
        if name_value.is_null() {
            return Ok(None);
        }
        let name = name_value
            .as_str()
            .ok_or_else(|| {
                type_mismatch(
                    "output_style.name",
                    JsonType::String,
                    JsonType::of(name_value),
                )
            })?
            .to_owned();
        if name.is_empty() {
            return Ok(None);
        }
        Ok(Some(OutputStyle { name }))
    }

    fn parse_agent_name(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<String>, ParseError> {
        let Some(value) = root.get("agent") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let obj = expect_object(value, "agent")?;
        // Same drift-detection rationale as `parse_output_style`: warn
        // when the wrapper is present but `name` is absent.
        let Some(name_value) = obj.get("name") else {
            crate::lsm_warn!(
                "agent: wrapper present but `name` field missing; treating as None (possible CC schema drift)"
            );
            return Ok(None);
        };
        if name_value.is_null() {
            return Ok(None);
        }
        let name = name_value
            .as_str()
            .ok_or_else(|| type_mismatch("agent.name", JsonType::String, JsonType::of(name_value)))?
            .to_owned();
        if name.is_empty() {
            return Ok(None);
        }
        Ok(Some(name))
    }

    // --- helpers ------------------------------------------------------

    fn expect_object<'a>(
        value: &'a serde_json::Value,
        path: &str,
    ) -> Result<&'a serde_json::Map<String, serde_json::Value>, ParseError> {
        value
            .as_object()
            .ok_or_else(|| type_mismatch(path, JsonType::Object, JsonType::of(value)))
    }

    fn require_string<'a>(
        obj: &'a serde_json::Map<String, serde_json::Value>,
        path: &'static str,
    ) -> Result<&'a str, ParseError> {
        let value = obj.get(path_tail(path)).ok_or_else(|| missing(path))?;
        value
            .as_str()
            .ok_or_else(|| type_mismatch(path, JsonType::String, JsonType::of(value)))
    }

    fn require_f64(
        obj: &serde_json::Map<String, serde_json::Value>,
        path: &'static str,
    ) -> Result<f64, ParseError> {
        let value = obj.get(path_tail(path)).ok_or_else(|| missing(path))?;
        value
            .as_f64()
            .ok_or_else(|| type_mismatch(path, JsonType::Number, JsonType::of(value)))
    }

    fn require_u64(
        obj: &serde_json::Map<String, serde_json::Value>,
        path: &'static str,
    ) -> Result<u64, ParseError> {
        let value = obj.get(path_tail(path)).ok_or_else(|| missing(path))?;
        value
            .as_u64()
            .ok_or_else(|| type_mismatch(path, JsonType::Number, JsonType::of(value)))
    }

    fn path_tail(path: &str) -> &str {
        path.rsplit('.').next().unwrap_or(path)
    }

    fn missing(path: impl Into<String>) -> ParseError {
        ParseError::MissingField {
            tool: TOOL,
            path: path.into(),
        }
    }

    fn type_mismatch(path: impl Into<String>, expected: JsonType, got: JsonType) -> ParseError {
        ParseError::TypeMismatch {
            tool: TOOL,
            path: path.into(),
            expected,
            got,
        }
    }

    fn invalid_value(path: impl Into<String>, reason: &'static str) -> ParseError {
        ParseError::InvalidValue {
            tool: TOOL,
            path: path.into(),
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pct(v: f32) -> Percent {
        Percent::new(v).expect("in range")
    }

    #[test]
    fn percent_new_rejects_out_of_range() {
        assert!(Percent::new(-0.1).is_none());
        assert!(Percent::new(100.1).is_none());
        assert!(Percent::new(f32::NAN).is_none());
    }

    #[test]
    fn percent_from_f64_clamped_clamps_finite_values_and_rejects_nan() {
        assert_eq!(Percent::from_f64_clamped(150.0).unwrap().value(), 100.0);
        assert_eq!(Percent::from_f64_clamped(-5.0).unwrap().value(), 0.0);
        assert_eq!(Percent::from_f64_clamped(100.0).unwrap().value(), 100.0);
        assert_eq!(Percent::from_f64_clamped(0.0).unwrap().value(), 0.0);
        assert_eq!(Percent::from_f64_clamped(42.5).unwrap().value(), 42.5);
        // Tiny overshoot is the shape claude-code#37163 actually emits.
        // Strict `from_f64` rejects this because its range check runs
        // on the f64 before narrowing, so `100.0000001` is flagged
        // even though it would narrow to exactly `100.0` as f32. The
        // clamped variant accepts the out-of-range value and pins it
        // to 100.0.
        assert_eq!(
            Percent::from_f64_clamped(100.0000001).unwrap().value(),
            100.0
        );
        // NaN is the only input that still fails: `f64::clamp` treats
        // NaN as identity, which would poison downstream math. Reject
        // so the caller can surface InvalidValue.
        assert!(Percent::from_f64_clamped(f64::NAN).is_none());
        // Infinity clamps to the nearest bound under IEEE-754 ordering
        // — pinning explicitly in case a future stdlib change shifts
        // the semantics.
        assert_eq!(
            Percent::from_f64_clamped(f64::INFINITY).unwrap().value(),
            100.0
        );
        assert_eq!(
            Percent::from_f64_clamped(f64::NEG_INFINITY)
                .unwrap()
                .value(),
            0.0
        );
    }

    #[test]
    fn percent_from_f64_rejects_values_that_would_narrow_into_range() {
        // 100.0000001 as f32 rounds to exactly 100.0. from_f64 validates
        // before narrowing so this is rejected rather than silently passing.
        assert!(Percent::from_f64(100.0000001).is_none());
        assert!(Percent::from_f64(-0.0000001).is_none());
        assert!(Percent::from_f64(f64::NAN).is_none());
        assert!(Percent::from_f64(100.0).is_some());
        assert!(Percent::from_f64(0.0).is_some());
    }

    #[test]
    fn percent_complement_stays_in_range() {
        assert_eq!(pct(42.0).complement().value(), 58.0);
        assert_eq!(pct(0.0).complement().value(), 100.0);
        assert_eq!(pct(100.0).complement().value(), 0.0);
    }

    #[test]
    fn parses_minimal_claude_payload() {
        let json = br#"{
            "model": { "id": "x", "display_name": "Claude Test" },
            "workspace": {
                "current_dir": ".",
                "project_dir": "/home/dev/linesmith",
                "added_dirs": [],
                "git_worktree": null
            }
        }"#;
        let ctx = parse(json).expect("parse ok");
        assert_eq!(ctx.tool, Tool::ClaudeCode);
        assert_eq!(ctx.model.display_name, "Claude Test");
        assert_eq!(
            ctx.workspace.project_dir.to_str(),
            Some("/home/dev/linesmith")
        );
        assert!(ctx.workspace.git_worktree.is_none());
        assert!(ctx.context_window.is_none());
    }

    #[test]
    fn parses_payload_with_worktree() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": {
                "project_dir": "/repo",
                "git_worktree": { "name": "main", "path": "/wt/main" }
            }
        }"#;
        let ctx = parse(json).expect("parse ok");
        let wt = ctx.workspace.git_worktree.expect("worktree");
        assert_eq!(wt.name, "main");
        assert_eq!(wt.path, PathBuf::from("/wt/main"));
    }

    #[test]
    fn git_worktree_absent_key_treated_as_none() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" }
        }"#;
        let ctx = parse(json).expect("parse ok");
        assert!(ctx.workspace.git_worktree.is_none());
    }

    #[test]
    fn parses_context_window() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 42.5,
                "remaining_percentage": 57.5,
                "context_window_size": 200000,
                "total_input_tokens": 12345,
                "total_output_tokens": 6789
            }
        }"#;
        let ctx = parse(json).expect("parse ok");
        let cw = ctx.context_window.expect("context_window");
        assert_eq!(cw.used.value(), 42.5);
        assert_eq!(cw.remaining().value(), 57.5);
        assert_eq!(cw.size, 200_000);
        assert_eq!(cw.total_input_tokens, 12_345);
        assert_eq!(cw.total_output_tokens, 6_789);
    }

    #[test]
    fn used_percentage_above_100_clamps_instead_of_rejecting() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 150,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        let ctx = parse(json).expect("clamp succeeds");
        let cw = ctx.context_window.expect("context_window present");
        assert_eq!(cw.used.value(), 100.0);
    }

    #[test]
    fn used_percentage_fractional_overshoot_clamps_to_100() {
        // Catch a regression that routes the raw f64 through `as i64`
        // or `.floor()` before clamping.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 101.7,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        let ctx = parse(json).expect("clamp succeeds");
        let cw = ctx.context_window.expect("context_window present");
        assert_eq!(cw.used.value(), 100.0);
    }

    #[test]
    fn used_percentage_below_0_rejects_as_invalid_value() {
        // Negative percentages aren't a documented Claude Code state —
        // treat them as a corrupted payload and surface InvalidValue
        // so the failure is loud, instead of silently clamping to 0%.
        // The above-100 case is different (known upstream bug in
        // claude-code#37163) and clamps via the companion test.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": -5.0,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        match parse(json).expect_err("should reject") {
            ParseError::InvalidValue { path, .. } => {
                assert_eq!(path, "context_window.used_percentage");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn used_percentage_in_range_passes_through_unchanged() {
        // Clamp must not distort values that were already in range.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 42.5,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        let ctx = parse(json).expect("in-range succeeds");
        let cw = ctx.context_window.expect("context_window present");
        assert_eq!(cw.used.value(), 42.5);
    }

    #[test]
    fn rejects_missing_used_percentage_as_missing_field() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        match parse(json).expect_err("should reject") {
            ParseError::MissingField { path, .. } => {
                assert_eq!(path, "context_window.used_percentage");
            }
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_type_used_percentage_as_type_mismatch() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": "42",
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        match parse(json).expect_err("should reject") {
            ParseError::TypeMismatch {
                path,
                expected,
                got,
                ..
            } => {
                assert_eq!(path, "context_window.used_percentage");
                assert_eq!(expected, JsonType::Number);
                assert_eq!(got, JsonType::String);
            }
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn pre_first_api_call_payload_renders_other_segments() {
        // Real captured CC 2.1.119 payload from the ~15s
        // pre-first-API-call window where `used_percentage` is null.
        // Asserts structurally (not value-pinned) so re-capturing the
        // fixture from a different account/session doesn't break the
        // test for reasons unrelated to the contract under check.
        let bytes = include_bytes!("../tests/fixtures/claude_pre_first_api_call.json");
        let ctx = parse(bytes).expect("parse must succeed despite null context_window leaves");
        assert!(!ctx.model.display_name.is_empty(), "model must parse");
        assert!(
            !ctx.workspace.project_dir.as_os_str().is_empty(),
            "workspace must parse"
        );
        assert!(
            ctx.context_window.is_none(),
            "context_window with null leaves must hide, not surface bogus zeros"
        );
        assert!(ctx.cost.is_some(), "cost segment must still render");
        assert_eq!(ctx.effort, Some(EffortLevel::XHigh));
    }

    #[test]
    fn used_percentage_null_hides_context_window_segment() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": null,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        let ctx = parse(json).expect("null used_percentage must not fail the whole parse");
        assert!(ctx.context_window.is_none());
    }

    #[test]
    fn null_context_window_size_hides_segment() {
        // Defensive-sweep guard: not observed in the wild, but locks in
        // the contract so a future schema wobble doesn't tank the parse.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 12.5,
                "context_window_size": null,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        let ctx = parse(json).expect("null size must not fail the whole parse");
        assert!(ctx.context_window.is_none());
    }

    #[test]
    fn null_total_input_tokens_hides_segment() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 12.5,
                "context_window_size": 200000,
                "total_input_tokens": null,
                "total_output_tokens": 0
            }
        }"#;
        let ctx = parse(json).expect("null total_input_tokens must not fail the whole parse");
        assert!(ctx.context_window.is_none());
    }

    #[test]
    fn null_total_output_tokens_hides_segment() {
        // Symmetric guard against a copy-paste regression that drops
        // `total_output_tokens` from the loop array in parse_context_window.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 12.5,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": null
            }
        }"#;
        let ctx = parse(json).expect("null total_output_tokens must not fail the whole parse");
        assert!(ctx.context_window.is_none());
    }

    #[test]
    fn context_window_explicit_null_treated_as_none() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": null
        }"#;
        let ctx = parse(json).expect("parse ok");
        assert!(ctx.context_window.is_none());
    }

    #[test]
    fn current_usage_absent_is_none() {
        // Schema doesn't guarantee the key is present inside a
        // context_window object; treat missing the same as explicit
        // `null` so a future schema variation parses cleanly.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 42.5,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        let ctx = parse(json).expect("parse ok");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.current_usage.is_none());
    }

    #[test]
    fn current_usage_null_is_none() {
        // Claude Code emits `current_usage: null` before the first API
        // call in a session; round-trip to Option::None.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 0,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "current_usage": null
            }
        }"#;
        let ctx = parse(json).expect("parse ok");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.current_usage.is_none());
    }

    #[test]
    fn current_usage_present_parses_all_four_fields() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 12.4,
                "context_window_size": 200000,
                "total_input_tokens": 24800,
                "total_output_tokens": 3200,
                "current_usage": {
                    "input_tokens": 2000,
                    "output_tokens": 500,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 500
                }
            }
        }"#;
        let ctx = parse(json).expect("parse ok");
        let cw = ctx.context_window.expect("context_window present");
        let usage = cw.current_usage.expect("current_usage present");
        assert_eq!(usage.input_tokens, 2000);
        assert_eq!(usage.output_tokens, 500);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 500);
    }

    #[test]
    fn current_usage_non_object_is_type_mismatch() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 0,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "current_usage": "not an object"
            }
        }"#;
        match parse(json).expect_err("should reject") {
            ParseError::TypeMismatch { path, .. } => {
                assert_eq!(path, "context_window.current_usage");
            }
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn current_usage_missing_inner_field_is_missing_field() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 0,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "current_usage": {
                    "input_tokens": 100,
                    "output_tokens": 50
                }
            }
        }"#;
        match parse(json).expect_err("should reject") {
            ParseError::MissingField { path, .. } => {
                assert_eq!(
                    path,
                    "context_window.current_usage.cache_creation_input_tokens"
                );
            }
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn current_usage_inner_wrong_type_is_type_mismatch_at_nested_path() {
        // Lock the error-path provenance for nested fields: a non-
        // number inner value should surface as TypeMismatch with the
        // full `context_window.current_usage.<field>` path, not the
        // outer `context_window.current_usage`.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 0,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "current_usage": {
                    "input_tokens": "200",
                    "output_tokens": 50,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            }
        }"#;
        match parse(json).expect_err("should reject") {
            ParseError::TypeMismatch { path, .. } => {
                assert_eq!(path, "context_window.current_usage.input_tokens");
            }
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn rejects_wrong_type_git_worktree_as_type_mismatch() {
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": {
                "project_dir": "/repo",
                "git_worktree": "main"
            }
        }"#;
        match parse(json).expect_err("should reject") {
            ParseError::TypeMismatch {
                path,
                expected,
                got,
                ..
            } => {
                assert_eq!(path, "workspace.git_worktree");
                assert_eq!(expected, JsonType::Object);
                assert_eq!(got, JsonType::String);
            }
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn rejects_missing_model() {
        let json = br#"{
            "workspace": { "project_dir": "/repo" }
        }"#;
        let err = parse(json).expect_err("should reject");
        assert!(matches!(err, ParseError::MissingField { .. }));
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(
            parse(b"{not json"),
            Err(ParseError::InvalidJson { .. })
        ));
    }

    #[test]
    fn malformed_json_carries_exact_source_position() {
        // The `}` at line 2, column 10 is where serde_json bails.
        let ParseError::InvalidJson { location, .. } = parse(b"{\n  \"bad\": }").unwrap_err()
        else {
            panic!("expected InvalidJson");
        };
        let pos = location.expect("position populated for positional errors");
        assert_eq!(pos.line, 2);
        assert_eq!(pos.column, 10);
    }

    #[test]
    fn json_type_of_maps_each_variant() {
        use serde_json::Value;
        assert_eq!(
            JsonType::of(&Value::Object(Default::default())),
            JsonType::Object
        );
        assert_eq!(JsonType::of(&Value::Array(vec![])), JsonType::Array);
        assert_eq!(JsonType::of(&Value::String("x".into())), JsonType::String);
        assert_eq!(JsonType::of(&Value::from(42)), JsonType::Number);
        assert_eq!(JsonType::of(&Value::Bool(true)), JsonType::Bool);
        assert_eq!(JsonType::of(&Value::Null), JsonType::Null);
    }

    #[test]
    fn parse_error_display_formats_root_path_readably() {
        // When the root JSON is not an object, the path is empty.
        let err = parse(b"[]").expect_err("array at root rejected");
        let display = err.to_string();
        assert!(display.contains("<root>"), "got {display:?}");
    }

    // --- cost error paths ---

    #[test]
    fn cost_absent_treated_as_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
        assert!(parse(bytes).expect("ok").cost.is_none());
    }

    #[test]
    fn cost_explicit_null_treated_as_none() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"cost":null}"#;
        assert!(parse(bytes).expect("ok").cost.is_none());
    }

    #[test]
    fn cost_wrong_type_rejected_as_type_mismatch() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"cost":"nope"}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch { path, .. } => assert_eq!(path, "cost"),
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn cost_missing_sub_field_rejected_as_missing_field() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
            "cost":{"total_cost_usd":1.0,"total_duration_ms":0,"total_api_duration_ms":0,"total_lines_added":0}}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::MissingField { path, .. } => assert_eq!(path, "cost.total_lines_removed"),
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn cost_lines_added_accepts_large_value_without_truncation() {
        // Regression guard for slice-3 review fix: fields were previously
        // narrowed to u32, silently truncating at 4.29B.
        let bytes = format!(
            r#"{{"model":{{"display_name":"X"}},"workspace":{{"project_dir":"/r"}},
               "cost":{{"total_cost_usd":0.0,"total_duration_ms":0,"total_api_duration_ms":0,
                        "total_lines_added":{n},"total_lines_removed":0}}}}"#,
            n = 5_000_000_000u64
        );
        let ctx = parse(bytes.as_bytes()).expect("parse ok");
        assert_eq!(ctx.cost.expect("cost").total_lines_added, 5_000_000_000u64);
    }

    // --- effort error paths ---

    #[test]
    fn effort_object_form_parses() {
        // Canonical shape as of Claude Code 2.1.x.
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":"xhigh"}}"#;
        let ctx = parse(bytes).expect("parse ok");
        assert_eq!(ctx.effort, Some(EffortLevel::XHigh));
    }

    #[test]
    fn effort_bare_string_still_parses() {
        // Back-compat for tools that emit a bare-string form.
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":"high"}"#;
        let ctx = parse(bytes).expect("parse ok");
        assert_eq!(ctx.effort, Some(EffortLevel::High));
    }

    #[test]
    fn effort_object_missing_level_surfaces_missing_field_with_full_path() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{}}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::MissingField { path, .. } => assert_eq!(path, "effort.level"),
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn effort_object_non_string_level_surfaces_type_mismatch_with_full_path() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":42}}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch {
                path,
                expected,
                got,
                ..
            } => {
                assert_eq!(path, "effort.level");
                assert_eq!(expected, JsonType::String);
                assert_eq!(got, JsonType::Number);
            }
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn effort_object_null_level_maps_to_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":null}}"#;
        let ctx = parse(bytes).expect("parse ok");
        assert_eq!(ctx.effort, None);
    }

    #[test]
    fn effort_top_level_null_maps_to_none() {
        // Locks the outer early-return in parse_effort. A refactor that
        // dropped the explicit is_null() check and delegated to the
        // object/string match would regress this into a TypeMismatch.
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":null}"#;
        let ctx = parse(bytes).expect("parse ok");
        assert_eq!(ctx.effort, None);
    }

    #[test]
    fn effort_non_object_non_string_rejected_as_type_mismatch() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":42}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch {
                path,
                expected,
                got,
                ..
            } => {
                assert_eq!(path, "effort");
                assert_eq!(expected, JsonType::Object);
                assert_eq!(got, JsonType::Number);
            }
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn effort_object_unknown_level_rejected_as_invalid_value_with_full_path() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":"ultra"}}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::InvalidValue { path, reason, .. } => {
                assert_eq!(path, "effort.level");
                assert!(
                    reason.contains("low"),
                    "reason should list known values, got {reason:?}"
                );
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn effort_unknown_string_rejected_as_invalid_value() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":"ultra"}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::InvalidValue { path, reason, .. } => {
                assert_eq!(path, "effort");
                assert!(
                    reason.contains("low"),
                    "reason should list known values, got {reason:?}"
                );
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    // --- vim / output_style / agent ---

    #[test]
    fn parses_vim_object_form() {
        let bytes = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/r" },
            "vim": { "mode": "insert" }
        }"#;
        let ctx = parse(bytes).expect("ok");
        assert_eq!(ctx.vim, Some(VimMode::Insert));
    }

    #[test]
    fn parses_vim_string_form_for_compat() {
        let bytes = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/r" },
            "vim": "visual"
        }"#;
        let ctx = parse(bytes).expect("ok");
        assert_eq!(ctx.vim, Some(VimMode::Visual));
    }

    #[test]
    fn vim_absent_or_null_yields_none() {
        let absent = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
        assert_eq!(parse(absent).unwrap().vim, None);
        let null = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":null}"#;
        assert_eq!(parse(null).unwrap().vim, None);
        let null_mode = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{"mode":null}}"#;
        assert_eq!(parse(null_mode).unwrap().vim, None);
    }

    #[test]
    fn vim_unknown_mode_degrades_segment_not_whole_parse() {
        // An unknown vim mode (e.g. a future CC `select` or `terminal`)
        // must NOT abort the whole parse — the rest of the statusline
        // would render blank for an opt-in informational segment. Warn
        // and degrade to None instead. Lock the contract so a refactor
        // that re-introduces `InvalidValue` here regresses loudly.
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{"mode":"surrogate"}}"#;
        let ctx = parse(bytes).expect("unknown vim mode must not fail parse");
        assert_eq!(ctx.vim, None);
    }

    #[test]
    fn parses_output_style() {
        let bytes = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/r" },
            "output_style": { "name": "concise" }
        }"#;
        let ctx = parse(bytes).expect("ok");
        let style = ctx.output_style.expect("present");
        assert_eq!(style.name, "concise");
    }

    #[test]
    fn output_style_absent_or_null_yields_none() {
        let absent = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
        assert!(parse(absent).unwrap().output_style.is_none());
        let null = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":null}"#;
        assert!(parse(null).unwrap().output_style.is_none());
        let null_name = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":{"name":null}}"#;
        assert!(parse(null_name).unwrap().output_style.is_none());
        // Object without `name` is tolerated as None so the schema can grow.
        let no_name =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":{}}"#;
        assert!(parse(no_name).unwrap().output_style.is_none());
        let empty = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":{"name":""}}"#;
        assert!(parse(empty).unwrap().output_style.is_none());
    }

    #[test]
    fn output_style_name_typed_wrong_rejected() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":{"name":42}}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch { path, .. } => assert_eq!(path, "output_style.name"),
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn parses_agent_name() {
        let bytes = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/r" },
            "agent": { "name": "research" }
        }"#;
        let ctx = parse(bytes).expect("ok");
        assert_eq!(ctx.agent_name.as_deref(), Some("research"));
    }

    #[test]
    fn agent_absent_null_or_empty_yields_none() {
        let absent = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
        assert!(parse(absent).unwrap().agent_name.is_none());
        let null =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":null}"#;
        assert!(parse(null).unwrap().agent_name.is_none());
        let empty = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":{"name":""}}"#;
        assert!(parse(empty).unwrap().agent_name.is_none());
        let no_name =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":{}}"#;
        assert!(parse(no_name).unwrap().agent_name.is_none());
    }

    #[test]
    fn vim_object_missing_mode_surfaces_missing_field() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{}}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::MissingField { path, .. } => assert_eq!(path, "vim.mode"),
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn vim_object_non_string_mode_surfaces_type_mismatch() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{"mode":42}}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch { path, .. } => assert_eq!(path, "vim.mode"),
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn vim_non_object_non_string_rejected_as_type_mismatch() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":42}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch { path, .. } => assert_eq!(path, "vim"),
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn output_style_non_object_rejected_as_type_mismatch() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":"concise"}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch { path, .. } => assert_eq!(path, "output_style"),
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn agent_non_object_rejected_as_type_mismatch() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":"research"}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch { path, .. } => assert_eq!(path, "agent"),
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn agent_name_typed_wrong_rejected_as_type_mismatch() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":{"name":42}}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch { path, .. } => assert_eq!(path, "agent.name"),
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }
}
