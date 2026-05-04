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
mod tests;
