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

/// Parse a Claude Code statusline JSON payload into a `StatusContext`.
///
/// Currently dispatches to the Claude normalizer unconditionally;
/// tool-detection heuristics are added when a second tool is wired in.
///
/// # Errors
///
/// Per ADR-0014, sub-field failures degrade to `Option::None` with
/// `lsm_warn!` rather than propagating through `Result`. `parse` only
/// returns `Err` for catastrophic failures: `ParseError::InvalidJson`
/// on malformed JSON, `TypeMismatch` when the root is not a JSON
/// object, and `InvalidValue` for a `used_percentage` < 0 (carve-out
/// for undocumented CC corruption signals; NaN is rejected upstream
/// by `serde_json` as `InvalidJson`).
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

    pub(super) fn normalize(raw: Arc<serde_json::Value>) -> Result<StatusContext, ParseError> {
        let root = expect_object(&raw, "")?;

        let model = parse_model(root);
        let workspace = parse_workspace(root);
        let context_window = parse_context_window(root)?;
        let cost = parse_cost(root)?;
        let effort = parse_effort(root)?;
        let vim = parse_vim(root)?;
        let output_style = parse_output_style(root)?;
        let agent_name = parse_agent_name(root)?;
        let version = parse_version(root)?;

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
            version,
            raw,
        })
    }

    /// ADR-0014: any sub-field failure (missing wrapper, non-object,
    /// missing/null/non-string `display_name`) downgrades the field
    /// to `None` with an `lsm_warn!` carrying the JSON path. Segments
    /// hide; unrelated segments still render.
    fn parse_model(root: &serde_json::Map<String, serde_json::Value>) -> Option<ModelInfo> {
        let value = root.get("model")?;
        if value.is_null() {
            return None;
        }
        let model = match value.as_object() {
            Some(o) => o,
            None => {
                crate::lsm_warn!(
                    "model: expected object, got {:?}; degrading to None (possible CC schema drift)",
                    JsonType::of(value)
                );
                return None;
            }
        };
        let Some(name_value) = model.get("display_name") else {
            crate::lsm_warn!("model.display_name: missing; degrading to None");
            return None;
        };
        if name_value.is_null() {
            return None;
        }
        let Some(display_name) = name_value.as_str() else {
            crate::lsm_warn!(
                "model.display_name: expected string, got {:?}; degrading to None",
                JsonType::of(name_value)
            );
            return None;
        };
        Some(ModelInfo {
            display_name: display_name.to_owned(),
        })
    }

    /// ADR-0014: any sub-field failure downgrades to `None` + warn.
    /// `git_worktree` still degrades independently — a malformed
    /// worktree shouldn't hide the project_dir basename.
    fn parse_workspace(root: &serde_json::Map<String, serde_json::Value>) -> Option<WorkspaceInfo> {
        let value = root.get("workspace")?;
        if value.is_null() {
            return None;
        }
        let workspace = match value.as_object() {
            Some(o) => o,
            None => {
                crate::lsm_warn!(
                    "workspace: expected object, got {:?}; degrading to None (possible CC schema drift)",
                    JsonType::of(value)
                );
                return None;
            }
        };
        let Some(dir_value) = workspace.get("project_dir") else {
            crate::lsm_warn!("workspace.project_dir: missing; degrading to None");
            return None;
        };
        if dir_value.is_null() {
            return None;
        }
        let Some(project_dir_str) = dir_value.as_str() else {
            crate::lsm_warn!(
                "workspace.project_dir: expected string, got {:?}; degrading to None",
                JsonType::of(dir_value)
            );
            return None;
        };

        let git_worktree = match workspace.get("git_worktree") {
            Some(serde_json::Value::Null) | None => None,
            Some(serde_json::Value::Object(obj)) => parse_git_worktree(obj),
            Some(other) => {
                crate::lsm_warn!(
                    "workspace.git_worktree: expected object, got {:?}; degrading to None (worktree only)",
                    JsonType::of(other)
                );
                None
            }
        };

        Some(WorkspaceInfo {
            project_dir: PathBuf::from(project_dir_str),
            git_worktree,
        })
    }

    fn parse_git_worktree(obj: &serde_json::Map<String, serde_json::Value>) -> Option<GitWorktree> {
        let name = string_leaf(obj, "workspace.git_worktree.name")?;
        let path = string_leaf(obj, "workspace.git_worktree.path")?;
        // Empty strings are silent: CC sometimes emits `""` for
        // unset fields. A non-string drift already warned in
        // `string_leaf` before reaching here.
        if name.is_empty() || path.is_empty() {
            return None;
        }
        Some(GitWorktree {
            name: name.to_owned(),
            path: PathBuf::from(path),
        })
    }

    /// Tolerant string reader. Missing or null → silent `None`
    /// (documented "field unset" shape). Non-string → `lsm_warn!` +
    /// `None` to surface schema drift.
    fn string_leaf<'a>(
        obj: &'a serde_json::Map<String, serde_json::Value>,
        path: &'static str,
    ) -> Option<&'a str> {
        let value = obj.get(path_tail(path))?;
        if value.is_null() {
            return None;
        }
        match value.as_str() {
            Some(s) => Some(s),
            None => {
                crate::lsm_warn!(
                    "{path}: expected string, got {:?}; degrading to None",
                    JsonType::of(value)
                );
                None
            }
        }
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
        let Some(cw) = value.as_object() else {
            crate::lsm_warn!(
                "context_window: expected object, got {:?}; degrading to None",
                JsonType::of(value)
            );
            return Ok(None);
        };

        // ADR-0014: each leaf degrades independently. A null
        // `used_percentage` (the documented pre-first-API-call shape)
        // no longer hides peers like `current_usage` or `size`.
        let used = parse_used_percentage(cw)?;
        let size = parse_size(cw);
        let total_input_tokens = try_u64_required(cw, "context_window.total_input_tokens");
        let total_output_tokens = try_u64_required(cw, "context_window.total_output_tokens");
        let current_usage = parse_current_usage(cw)?;

        let window = ContextWindow {
            used,
            size,
            total_input_tokens,
            total_output_tokens,
            current_usage,
        };
        // Collapse to `None` when every leaf failed so the plugin
        // contract `ctx.status.context_window != ()` round-trips: a
        // non-`()` map must always have at least one readable leaf.
        if context_window_is_empty(&window) {
            return Ok(None);
        }
        Ok(Some(window))
    }

    fn context_window_is_empty(cw: &ContextWindow) -> bool {
        cw.used.is_none()
            && cw.size.is_none()
            && cw.total_input_tokens.is_none()
            && cw.total_output_tokens.is_none()
            && cw.current_usage.is_none()
    }

    /// Parse `context_window.used_percentage` into `Option<Percent>`.
    /// - missing / null → `Ok(None)`, silent (documented pre-first-
    ///   API-call shape; warning would spam at every fresh session)
    /// - non-number → `Ok(None)` + warn (schema drift)
    /// - in-range value → `Ok(Some(_))`
    /// - >100 → clamp to 100 + warn (claude-code#37163)
    /// - <0 or NaN → `Err(InvalidValue)`. Carve-out from ADR-0014's
    ///   warn-and-degrade default: a negative is undocumented and
    ///   most likely a corrupted payload; surfacing loud catches
    ///   real upstream breakage that "Some(0)" or warn+None would mask.
    fn parse_used_percentage(
        cw: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<Percent>, ParseError> {
        let Some(value) = cw.get("used_percentage") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let Some(used_raw) = value.as_f64() else {
            crate::lsm_warn!(
                "context_window.used_percentage: expected number, got {:?}; degrading leaf to None",
                JsonType::of(value)
            );
            return Ok(None);
        };
        if used_raw > 100.0 {
            crate::lsm_warn!("context_window.used_percentage = {used_raw} > 100; clamping to 100");
            return Ok(Some(
                Percent::from_f64_clamped(used_raw)
                    .expect("non-NaN value > 100 clamps successfully"),
            ));
        }
        match Percent::from_f64(used_raw) {
            Some(p) => Ok(Some(p)),
            None => Err(invalid_value(
                "context_window.used_percentage",
                "percentage must be a number in [0, 100]",
            )),
        }
    }

    /// Parse `context_window_size` into `Option<u32>`. Narrows `u64`
    /// to ADR-0014's `u32`; warns and degrades on overflow rather
    /// than wrapping. No real CC context window comes anywhere close.
    fn parse_size(cw: &serde_json::Map<String, serde_json::Value>) -> Option<u32> {
        let raw = try_u64_required(cw, "context_window.context_window_size")?;
        match u32::try_from(raw) {
            Ok(n) => Some(n),
            Err(_) => {
                crate::lsm_warn!(
                    "context_window.context_window_size = {raw} exceeds u32::MAX; degrading leaf to None"
                );
                None
            }
        }
    }

    /// Tolerant `u64` reader for *contracted* leaves (CC contract
    /// guarantees the key is present and non-null). Missing or null
    /// here is schema drift; warn so the channel surfaces upstream
    /// changes. Use `try_u64_optional` for documented "may be absent"
    /// fields like `current_usage.*`.
    fn try_u64_required(
        obj: &serde_json::Map<String, serde_json::Value>,
        path: &'static str,
    ) -> Option<u64> {
        let Some(value) = obj.get(path_tail(path)) else {
            crate::lsm_warn!("{path}: missing; degrading leaf to None (possible CC schema drift)");
            return None;
        };
        if value.is_null() {
            crate::lsm_warn!("{path}: null; degrading leaf to None (possible CC schema drift)");
            return None;
        }
        match value.as_u64() {
            Some(n) => Some(n),
            None => {
                crate::lsm_warn!(
                    "{path}: expected unsigned integer, got {:?}; degrading leaf to None",
                    JsonType::of(value)
                );
                None
            }
        }
    }

    /// Tolerant `u64` reader for *optional* leaves where missing/null
    /// is documented (e.g. `current_usage.*` before the first API
    /// call). Silent on absence; warn on type drift.
    fn try_u64_optional(
        obj: &serde_json::Map<String, serde_json::Value>,
        path: &'static str,
    ) -> Option<u64> {
        let value = obj.get(path_tail(path))?;
        if value.is_null() {
            return None;
        }
        match value.as_u64() {
            Some(n) => Some(n),
            None => {
                crate::lsm_warn!(
                    "{path}: expected unsigned integer, got {:?}; degrading leaf to None",
                    JsonType::of(value)
                );
                None
            }
        }
    }

    /// Tolerant `f64` reader for *contracted* leaves. Mirrors
    /// `try_u64_required`. JSON syntax can't represent NaN or ±Inf
    /// (`serde_json` rejects them as `InvalidJson` at parse time),
    /// so a non-finite check here would be unreachable through
    /// `parse()` — omitted intentionally.
    fn try_f64_required(
        obj: &serde_json::Map<String, serde_json::Value>,
        path: &'static str,
    ) -> Option<f64> {
        let Some(value) = obj.get(path_tail(path)) else {
            crate::lsm_warn!("{path}: missing; degrading leaf to None (possible CC schema drift)");
            return None;
        };
        if value.is_null() {
            crate::lsm_warn!("{path}: null; degrading leaf to None (possible CC schema drift)");
            return None;
        }
        let Some(n) = value.as_f64() else {
            crate::lsm_warn!(
                "{path}: expected number, got {:?}; degrading leaf to None",
                JsonType::of(value)
            );
            return None;
        };
        Some(n)
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
        let Some(obj) = value.as_object() else {
            crate::lsm_warn!(
                "context_window.current_usage: expected object, got {:?}; degrading to None",
                JsonType::of(value)
            );
            return Ok(None);
        };
        // ADR-0014: TurnUsage's leaves are non-Option, so any leaf
        // failure collapses the whole TurnUsage to None. Type drift
        // warns via `try_u64_optional`; missing/null leaves stay
        // silent because `current_usage` itself is documented to be
        // null pre-first-API-call.
        let Some(input_tokens) = try_u64_optional(obj, "context_window.current_usage.input_tokens")
        else {
            return Ok(None);
        };
        let Some(output_tokens) =
            try_u64_optional(obj, "context_window.current_usage.output_tokens")
        else {
            return Ok(None);
        };
        let Some(cache_creation_input_tokens) = try_u64_optional(
            obj,
            "context_window.current_usage.cache_creation_input_tokens",
        ) else {
            return Ok(None);
        };
        let Some(cache_read_input_tokens) =
            try_u64_optional(obj, "context_window.current_usage.cache_read_input_tokens")
        else {
            return Ok(None);
        };
        Ok(Some(TurnUsage {
            input_tokens,
            output_tokens,
            cache_creation_input_tokens,
            cache_read_input_tokens,
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
        let Some(cost) = value.as_object() else {
            crate::lsm_warn!(
                "cost: expected object, got {:?}; degrading to None",
                JsonType::of(value)
            );
            return Ok(None);
        };

        // Per ADR-0014, leaves degrade independently. CC contract
        // guarantees these keys when `cost` is present, so missing/null
        // is schema drift and warns at the leaf.
        let metrics = CostMetrics {
            total_cost_usd: try_f64_required(cost, "cost.total_cost_usd"),
            total_duration_ms: try_u64_required(cost, "cost.total_duration_ms"),
            total_api_duration_ms: try_u64_required(cost, "cost.total_api_duration_ms"),
            total_lines_added: try_u64_required(cost, "cost.total_lines_added"),
            total_lines_removed: try_u64_required(cost, "cost.total_lines_removed"),
        };
        // If every leaf failed, collapse to `None` so the plugin
        // contract `ctx.status.cost != ()` (per plugin-api.md) round-
        // trips correctly: a non-`()` cost map must always have at
        // least one readable leaf.
        if cost_is_empty(&metrics) {
            return Ok(None);
        }
        Ok(Some(metrics))
    }

    fn cost_is_empty(c: &CostMetrics) -> bool {
        c.total_cost_usd.is_none()
            && c.total_duration_ms.is_none()
            && c.total_api_duration_ms.is_none()
            && c.total_lines_added.is_none()
            && c.total_lines_removed.is_none()
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
        // Canonical CC 2.1.x emits `effort: { level: "xhigh" }`. A bare
        // string is tolerated for forward/backward compat. Per ADR-0014,
        // every failure path warn-and-degrades — symmetric with parse_vim.
        let (raw, path): (&str, &'static str) = match value {
            serde_json::Value::Object(obj) => {
                let Some(level) = obj.get("level") else {
                    crate::lsm_warn!(
                        "effort: wrapper present but `level` missing; degrading to None (possible CC schema drift)"
                    );
                    return Ok(None);
                };
                if level.is_null() {
                    return Ok(None);
                }
                let Some(s) = level.as_str() else {
                    crate::lsm_warn!(
                        "effort.level: expected string, got {:?}; degrading to None",
                        JsonType::of(level)
                    );
                    return Ok(None);
                };
                (s, "effort.level")
            }
            serde_json::Value::String(s) => (s.as_str(), "effort"),
            other => {
                crate::lsm_warn!(
                    "effort: expected object or string, got {:?}; degrading to None",
                    JsonType::of(other)
                );
                return Ok(None);
            }
        };
        match raw.parse::<EffortLevel>() {
            Ok(level) => Ok(Some(level)),
            Err(()) => {
                crate::lsm_warn!(
                    "effort: unknown level {raw:?} at {path}; degrading to None (possible CC schema drift — known: low, medium, high, max, xhigh)"
                );
                Ok(None)
            }
        }
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
        // tolerated for forward/backward compat. Per ADR-0014, every
        // failure path warn-and-degrades.
        let (raw, path): (&str, &'static str) = match value {
            serde_json::Value::Object(obj) => {
                let Some(mode) = obj.get("mode") else {
                    crate::lsm_warn!(
                        "vim: wrapper present but `mode` missing; degrading to None (possible CC schema drift)"
                    );
                    return Ok(None);
                };
                if mode.is_null() {
                    return Ok(None);
                }
                let Some(s) = mode.as_str() else {
                    crate::lsm_warn!(
                        "vim.mode: expected string, got {:?}; degrading to None",
                        JsonType::of(mode)
                    );
                    return Ok(None);
                };
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
                crate::lsm_warn!(
                    "vim: expected object or string, got {:?}; degrading to None",
                    JsonType::of(other)
                );
                return Ok(None);
            }
        };
        // Unknown vim modes degrade the segment, not the whole render:
        // `vim` is opt-in and informational, so a future CC mode (e.g.
        // `select`, `terminal`) shouldn't blank the statusline.
        match raw.parse::<VimMode>() {
            Ok(mode) => Ok(Some(mode)),
            Err(()) => {
                crate::lsm_warn!(
                    "vim: unknown mode {raw:?} at {path}; degrading to None (possible CC schema drift — known: normal, insert, visual, command, replace)"
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
        let Some(obj) = value.as_object() else {
            crate::lsm_warn!(
                "output_style: expected object, got {:?}; degrading to None",
                JsonType::of(value)
            );
            return Ok(None);
        };
        // Tolerate a missing `name` as None so the schema can grow,
        // but warn: the wrapper is present, so a future CC rename of
        // `name` would otherwise dark-ship this segment without a
        // diagnostic trail.
        let Some(name_value) = obj.get("name") else {
            crate::lsm_warn!(
                "output_style: wrapper present but `name` field missing; degrading to None (possible CC schema drift)"
            );
            return Ok(None);
        };
        if name_value.is_null() {
            return Ok(None);
        }
        let Some(name) = name_value.as_str() else {
            crate::lsm_warn!(
                "output_style.name: expected string, got {:?}; degrading to None",
                JsonType::of(name_value)
            );
            return Ok(None);
        };
        if name.is_empty() {
            return Ok(None);
        }
        Ok(Some(OutputStyle {
            name: name.to_owned(),
        }))
    }

    fn parse_version(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<String>, ParseError> {
        let Some(value) = root.get("version") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let Some(raw) = value.as_str() else {
            crate::lsm_warn!(
                "version: expected string, got {:?}; degrading to None",
                JsonType::of(value)
            );
            return Ok(None);
        };
        // Trim and fold whitespace-only / empty to None — the
        // empty-payload contract should treat `"  "` the same as `""`
        // and `null` rather than rendering a blank-looking version.
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        Ok(Some(trimmed.to_owned()))
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
        let Some(obj) = value.as_object() else {
            crate::lsm_warn!(
                "agent: expected object, got {:?}; degrading to None",
                JsonType::of(value)
            );
            return Ok(None);
        };
        // Same drift-detection rationale as `parse_output_style`: warn
        // when the wrapper is present but `name` is absent.
        let Some(name_value) = obj.get("name") else {
            crate::lsm_warn!(
                "agent: wrapper present but `name` field missing; degrading to None (possible CC schema drift)"
            );
            return Ok(None);
        };
        if name_value.is_null() {
            return Ok(None);
        }
        let Some(name) = name_value.as_str() else {
            crate::lsm_warn!(
                "agent.name: expected string, got {:?}; degrading to None",
                JsonType::of(name_value)
            );
            return Ok(None);
        };
        if name.is_empty() {
            return Ok(None);
        }
        Ok(Some(name.to_owned()))
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

    fn path_tail(path: &str) -> &str {
        path.rsplit('.').next().unwrap_or(path)
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
        let model = ctx.model.expect("model");
        assert_eq!(model.display_name, "Claude Test");
        let workspace = ctx.workspace.expect("workspace");
        assert_eq!(workspace.project_dir.to_str(), Some("/home/dev/linesmith"));
        assert!(workspace.git_worktree.is_none());
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
        let wt = ctx
            .workspace
            .expect("workspace")
            .git_worktree
            .expect("worktree");
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
        assert!(ctx.workspace.expect("workspace").git_worktree.is_none());
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
        assert_eq!(cw.used.expect("used").value(), 42.5);
        assert_eq!(cw.remaining().expect("remaining").value(), 57.5);
        assert_eq!(cw.size, Some(200_000));
        assert_eq!(cw.total_input_tokens, Some(12_345));
        assert_eq!(cw.total_output_tokens, Some(6_789));
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
        assert_eq!(cw.used.expect("used").value(), 100.0);
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
        assert_eq!(cw.used.expect("used").value(), 100.0);
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
        assert_eq!(cw.used.expect("used").value(), 42.5);
    }

    #[test]
    fn missing_used_percentage_degrades_leaf_to_none() {
        // ADR-0014: missing leaf no longer aborts the parse; only the
        // affected leaf goes None and peers like `size` survive.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        let ctx = parse(json).expect("missing leaf must not fail the whole parse");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.used.is_none());
        assert_eq!(cw.size, Some(200_000));
    }

    #[test]
    fn wrong_type_used_percentage_degrades_leaf_to_none() {
        // ADR-0014: type drift on a leaf no longer aborts the parse.
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
        let ctx = parse(json).expect("type-drift leaf must not fail the whole parse");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.used.is_none());
        assert_eq!(cw.size, Some(200_000));
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
        assert!(
            !ctx.model.expect("model").display_name.is_empty(),
            "model must parse"
        );
        assert!(
            !ctx.workspace
                .expect("workspace")
                .project_dir
                .as_os_str()
                .is_empty(),
            "workspace must parse"
        );
        // Per ADR-0014, leaves degrade individually: `used` is null
        // pre-first-call but `size` and `total_*_tokens` survive.
        // Segments with a hard dependency on `used` (the textual
        // `context_window` segment) hide; segments that only need
        // peer leaves keep rendering.
        let cw = ctx
            .context_window
            .expect("context_window present with partial leaves");
        assert!(cw.used.is_none(), "used_percentage was null in payload");
        assert!(cw.size.is_some(), "size populated in payload");
        assert!(ctx.cost.is_some(), "cost segment must still render");
        assert_eq!(ctx.effort, Some(EffortLevel::XHigh));
    }

    #[test]
    fn null_used_percentage_degrades_leaf_only() {
        // ADR-0014: peers (`size`, `total_*_tokens`) survive a null
        // `used_percentage`, including the documented pre-first-API-
        // call shape. The textual segment hides; tokens / current_usage
        // peers remain consumable.
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
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.used.is_none());
        assert_eq!(cw.size, Some(200_000));
        assert_eq!(cw.total_input_tokens, Some(0));
    }

    #[test]
    fn null_context_window_size_degrades_leaf_only() {
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
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.size.is_none());
        assert!(cw.used.is_some(), "used survives even when size is null");
    }

    #[test]
    fn null_total_input_tokens_degrades_leaf_only() {
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
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.total_input_tokens.is_none());
        assert_eq!(
            cw.total_output_tokens,
            Some(0),
            "peer leaves survive isolated null"
        );
    }

    #[test]
    fn null_total_output_tokens_degrades_leaf_only() {
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
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.total_output_tokens.is_none());
        assert_eq!(cw.total_input_tokens, Some(0));
    }

    #[test]
    fn current_usage_survives_when_peer_leaf_is_null() {
        // ADR-0014: per-leaf isolation — current_usage must not be
        // dropped just because a sibling totals counter is null.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 50.0,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": null,
                "current_usage": {
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            }
        }"#;
        let ctx = parse(json).expect("partial null must not drop current_usage");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.total_output_tokens.is_none());
        let usage = cw.current_usage.expect("current_usage preserved");
        assert_eq!(usage.input_tokens, 100);
    }

    #[test]
    fn context_window_size_above_u32_max_degrades_leaf_only() {
        // ADR-0014 narrows `size` to `u32`. A future hypothetical
        // CC payload exceeding u32::MAX must degrade the leaf to
        // None (not wrap via `as u32`) and let peers survive.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 12.5,
                "context_window_size": 4294967296,
                "total_input_tokens": 0,
                "total_output_tokens": 0
            }
        }"#;
        let ctx = parse(json).expect("u32 overflow must not fail the whole parse");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.size.is_none(), "size leaf degraded on overflow");
        assert!(cw.used.is_some(), "peer leaf survives");
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
    fn current_usage_non_object_degrades_to_none() {
        // ADR-0014: a non-object current_usage no longer aborts the
        // whole parse; the leaf goes None and peers survive.
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
        let ctx = parse(json).expect("non-object current_usage must not fail the whole parse");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.current_usage.is_none());
        assert_eq!(cw.size, Some(200_000));
    }

    #[test]
    fn current_usage_missing_inner_field_degrades_to_none() {
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
        let ctx = parse(json).expect("partial current_usage must not fail the whole parse");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.current_usage.is_none());
    }

    #[test]
    fn current_usage_inner_wrong_type_degrades_to_none() {
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
        let ctx = parse(json).expect("type-drift inner field must not fail the whole parse");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.current_usage.is_none());
    }

    #[test]
    fn current_usage_inner_null_collapses_whole_turn_usage() {
        // Pin TurnUsage's all-or-nothing semantics: a single null
        // leaf collapses the whole `current_usage` to None (per
        // parse_current_usage's let-else cascade), but peers
        // outside `current_usage` (size, used) survive.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 50.0,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "current_usage": {
                    "input_tokens": null,
                    "output_tokens": 50,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            }
        }"#;
        let ctx = parse(json).expect("partial null inside current_usage must not fail parse");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.current_usage.is_none());
        assert_eq!(cw.size, Some(200_000));
        assert!(cw.used.is_some());
    }

    #[test]
    fn current_usage_inner_missing_collapses_whole_turn_usage() {
        // Symmetric to the null-leaf test, but pinned at the call
        // site (parse_current_usage's let-else cascade), not at the
        // helper. `try_u64_optional` collapses missing and null into
        // the same silent `None` branch today; if a future refactor
        // splits them (e.g. logging a warn on missing), this test
        // catches the call-site behavior change without forcing the
        // helper's internal control flow into the test surface.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/repo" },
            "context_window": {
                "used_percentage": 50.0,
                "context_window_size": 200000,
                "total_input_tokens": 0,
                "total_output_tokens": 0,
                "current_usage": {
                    "output_tokens": 50,
                    "cache_creation_input_tokens": 0,
                    "cache_read_input_tokens": 0
                }
            }
        }"#;
        let ctx = parse(json).expect("missing leaf inside current_usage must not fail parse");
        let cw = ctx.context_window.expect("context_window present");
        assert!(cw.current_usage.is_none());
        assert_eq!(cw.size, Some(200_000));
    }

    #[test]
    fn cost_total_cost_usd_accepts_zero_and_tiny_positive() {
        // Defends try_f64_required's success path against a future
        // over-tightening like `n != 0.0` or `n > 0.0`. Zero is valid
        // (no API calls yet); a tiny non-zero positive must also
        // round-trip. serde_json rejects literals at f64::MAX as
        // "number out of range" — see `out_of_range_number_rejected_
        // at_json_layer` for the upper bound.
        for &val in &[0.0_f64, 1e-300_f64] {
            let bytes = format!(
                r#"{{"model":{{"display_name":"X"}},"workspace":{{"project_dir":"/r"}},
                     "cost":{{"total_cost_usd":{val},"total_duration_ms":0,"total_api_duration_ms":0,
                              "total_lines_added":0,"total_lines_removed":0}}}}"#
            );
            let ctx = parse(bytes.as_bytes()).expect("finite f64 must round-trip");
            let cost = ctx.cost.expect("cost present");
            assert_eq!(cost.total_cost_usd, Some(val));
        }
    }

    #[test]
    fn wrong_type_git_worktree_degrades_to_none() {
        // ADR-0014: malformed git_worktree no longer aborts the whole
        // parse; the worktree goes None and the project_dir survives.
        let json = br#"{
            "model": { "display_name": "X" },
            "workspace": {
                "project_dir": "/repo",
                "git_worktree": "main"
            }
        }"#;
        let ctx = parse(json).expect("malformed worktree must not fail the whole parse");
        let workspace = ctx.workspace.expect("workspace present");
        assert!(workspace.git_worktree.is_none());
        assert_eq!(workspace.project_dir.to_str(), Some("/repo"));
    }

    #[test]
    fn missing_model_degrades_to_none() {
        // ADR-0014: a payload without `model` no longer aborts the
        // whole parse; the field goes None and segments hide.
        let json = br#"{
            "workspace": { "project_dir": "/repo" }
        }"#;
        let ctx = parse(json).expect("missing model must not fail the whole parse");
        assert!(ctx.model.is_none());
        assert!(ctx.workspace.is_some());
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(
            parse(b"{not json"),
            Err(ParseError::InvalidJson { .. })
        ));
    }

    #[test]
    fn empty_object_payload_returns_all_none_top_level() {
        // ADR-0014 confirmation: parse(b"{}") must succeed with every
        // top-level Option field None, only `tool` and `raw` populated.
        // This is the post-isolation contract — no field is required
        // for parse to succeed on a syntactically valid object root.
        let ctx = parse(b"{}").expect("empty object must parse");
        assert_eq!(ctx.tool, Tool::ClaudeCode);
        assert!(ctx.model.is_none());
        assert!(ctx.workspace.is_none());
        assert!(ctx.context_window.is_none());
        assert!(ctx.cost.is_none());
        assert_eq!(ctx.effort, None);
        assert_eq!(ctx.vim, None);
        assert!(ctx.output_style.is_none());
        assert!(ctx.agent_name.is_none());
        assert!(ctx.version.is_none());
        // raw round-trips the empty object so plugins can still query
        // ctx.status.raw paths without a None guard.
        assert!(ctx.raw.is_object());
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
    fn cost_wrong_type_degrades_to_none() {
        // ADR-0014: a non-object cost wrapper no longer aborts the
        // whole parse; the field goes None and peers survive.
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"cost":"nope"}"#;
        let ctx = parse(bytes).expect("non-object cost must not fail the whole parse");
        assert!(ctx.cost.is_none());
        assert!(ctx.model.is_some());
    }

    #[test]
    fn cost_missing_sub_field_degrades_leaf_only() {
        // ADR-0014: missing cost leaf no longer aborts the parse;
        // the leaf goes None, peers populate from the payload.
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
            "cost":{"total_cost_usd":1.0,"total_duration_ms":0,"total_api_duration_ms":0,"total_lines_added":0}}"#;
        let ctx = parse(bytes).expect("missing leaf must not fail the whole parse");
        let cost = ctx.cost.expect("cost present");
        assert!(cost.total_lines_removed.is_none());
        assert_eq!(cost.total_cost_usd, Some(1.0));
        assert_eq!(cost.total_lines_added, Some(0));
    }

    #[test]
    fn cost_total_cost_usd_non_numeric_degrades_leaf_only() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
            "cost":{"total_cost_usd":"oops","total_duration_ms":0,"total_api_duration_ms":0,
                     "total_lines_added":0,"total_lines_removed":0}}"#;
        let ctx = parse(bytes).expect("type-drift leaf must not fail the whole parse");
        let cost = ctx.cost.expect("cost present");
        assert!(cost.total_cost_usd.is_none());
        assert_eq!(cost.total_duration_ms, Some(0));
    }

    #[test]
    fn cost_wrapper_with_all_leaves_drift_collapses_to_none() {
        // Plugin contract regression guard: ctx.status.cost != () must
        // always imply at least one readable leaf. A wrapper whose
        // every leaf is malformed collapses to None rather than
        // surfacing an all-`()` cost map to rhai consumers.
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
            "cost":{"total_cost_usd":"a","total_duration_ms":"b","total_api_duration_ms":"c",
                     "total_lines_added":"d","total_lines_removed":"e"}}"#;
        let ctx = parse(bytes).expect("all-leaves-drift cost must not fail the whole parse");
        assert!(ctx.cost.is_none());
    }

    #[test]
    fn context_window_wrapper_with_all_leaves_drift_collapses_to_none() {
        // Same plugin contract for context_window.
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
            "context_window":{"used_percentage":"a","context_window_size":"b",
                               "total_input_tokens":"c","total_output_tokens":"d"}}"#;
        let ctx = parse(bytes).expect("all-leaves-drift context_window must not fail the parse");
        assert!(ctx.context_window.is_none());
    }

    #[test]
    fn out_of_range_number_rejected_at_json_layer() {
        // Pins the JSON-syntax barrier: `1e500` overflows f64 and
        // serde_json rejects it as InvalidJson before our normalizer
        // sees it. Documents why try_f64_required has no `is_finite()`
        // guard — that branch would be unreachable through parse().
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},
            "cost":{"total_cost_usd":1e500,"total_duration_ms":0,"total_api_duration_ms":0,
                     "total_lines_added":0,"total_lines_removed":0}}"#;
        match parse(bytes).expect_err("serde_json rejects out-of-range numbers") {
            ParseError::InvalidJson { .. } => {}
            other => panic!("expected InvalidJson, got {other:?}"),
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
        assert_eq!(
            ctx.cost.expect("cost").total_lines_added,
            Some(5_000_000_000u64)
        );
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
    fn effort_object_missing_level_degrades_to_none() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{}}"#;
        let ctx = parse(bytes).expect("missing effort.level must not fail the whole parse");
        assert_eq!(ctx.effort, None);
        assert!(ctx.model.is_some());
    }

    #[test]
    fn effort_object_non_string_level_degrades_to_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":42}}"#;
        let ctx = parse(bytes).expect("type-drift effort.level must not fail the whole parse");
        assert_eq!(ctx.effort, None);
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
    fn effort_non_object_non_string_degrades_to_none() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":42}"#;
        let ctx = parse(bytes).expect("non-object/non-string effort must not fail the whole parse");
        assert_eq!(ctx.effort, None);
    }

    #[test]
    fn effort_object_unknown_level_degrades_to_none() {
        // ADR-0014: unknown effort variant warns + degrades, symmetric
        // with parse_vim. A future CC level shouldn't blank the line.
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":{"level":"ultra"}}"#;
        let ctx = parse(bytes).expect("unknown effort variant must not fail the whole parse");
        assert_eq!(ctx.effort, None);
    }

    #[test]
    fn effort_unknown_string_degrades_to_none() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":"ultra"}"#;
        let ctx = parse(bytes).expect("unknown string effort must not fail the whole parse");
        assert_eq!(ctx.effort, None);
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
    fn output_style_name_typed_wrong_degrades_to_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":{"name":42}}"#;
        let ctx = parse(bytes).expect("type-drift output_style.name must not fail the whole parse");
        assert_eq!(ctx.output_style, None);
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
    fn vim_object_missing_mode_degrades_to_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{}}"#;
        let ctx = parse(bytes).expect("missing vim.mode must not fail the whole parse");
        assert_eq!(ctx.vim, None);
    }

    #[test]
    fn vim_object_non_string_mode_degrades_to_none() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":{"mode":42}}"#;
        let ctx = parse(bytes).expect("type-drift vim.mode must not fail the whole parse");
        assert_eq!(ctx.vim, None);
    }

    #[test]
    fn vim_non_object_non_string_degrades_to_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"vim":42}"#;
        let ctx = parse(bytes).expect("non-object/non-string vim must not fail the whole parse");
        assert_eq!(ctx.vim, None);
    }

    #[test]
    fn output_style_non_object_degrades_to_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"output_style":"concise"}"#;
        let ctx = parse(bytes).expect("non-object output_style must not fail the whole parse");
        assert_eq!(ctx.output_style, None);
    }

    #[test]
    fn agent_non_object_degrades_to_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":"research"}"#;
        let ctx = parse(bytes).expect("non-object agent must not fail the whole parse");
        assert!(ctx.agent_name.is_none());
    }

    #[test]
    fn agent_name_typed_wrong_degrades_to_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"agent":{"name":42}}"#;
        let ctx = parse(bytes).expect("type-drift agent.name must not fail the whole parse");
        assert!(ctx.agent_name.is_none());
    }

    #[test]
    fn parses_top_level_version_string() {
        // Pinned against the official CC docs at code.claude.com/docs/en/statusline
        // (verified 2026-04-28): `version` is a top-level string field
        // on the statusline payload.
        let bytes = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/r" },
            "version": "2.1.90"
        }"#;
        assert_eq!(parse(bytes).unwrap().version.as_deref(), Some("2.1.90"));
    }

    #[test]
    fn version_absent_or_null_or_empty_yields_none() {
        let absent = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"}}"#;
        assert!(parse(absent).unwrap().version.is_none());
        let null =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"version":null}"#;
        assert!(parse(null).unwrap().version.is_none());
        let empty =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"version":""}"#;
        assert!(parse(empty).unwrap().version.is_none());
        // Whitespace-only is treated like empty: rendering "v   " is
        // worse than hiding the segment.
        let ws =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"version":"   "}"#;
        assert!(parse(ws).unwrap().version.is_none());
    }

    #[test]
    fn version_surrounding_whitespace_is_trimmed() {
        // Defensive: if CC ever ships padded version strings, the
        // segment shouldn't render `v  2.1.90  `.
        let bytes = br#"{
            "model": { "display_name": "X" },
            "workspace": { "project_dir": "/r" },
            "version": "  2.1.90  "
        }"#;
        assert_eq!(parse(bytes).unwrap().version.as_deref(), Some("2.1.90"));
    }

    #[test]
    fn version_typed_wrong_degrades_to_none() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"version":42}"#;
        let ctx = parse(bytes).expect("type-drift version must not fail the whole parse");
        assert!(ctx.version.is_none());
    }
}
