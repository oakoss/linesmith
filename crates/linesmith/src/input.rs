//! `StatusContext` models the parsed Claude Code statusline JSON.
//! Rate-limit windows, cost, vim state, effort, output-style, and agent
//! fields are added as segments start consuming them; see
//! `docs/specs/input-schema.md` for the full contract.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{DateTime, Utc};

/// The canonical, tool-agnostic input to the rendering pipeline. `Arc`
/// around `raw` keeps `StatusContext::clone` at O(1) when segments cache.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StatusContext {
    pub tool: Tool,
    pub model: ModelInfo,
    pub workspace: WorkspaceInfo,
    pub context_window: Option<ContextWindow>,
    pub cost: Option<CostMetrics>,
    pub rate_limits: Option<RateLimits>,
    pub effort: Option<EffortLevel>,
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
}

impl ContextWindow {
    /// Percentage remaining; always consistent with `used`.
    #[must_use]
    pub fn remaining(&self) -> Percent {
        self.used.complement()
    }
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

/// Rate-limit windows exposed to paid tiers. A `Some(RateLimits)` on
/// `StatusContext` always carries at least one window; the `(None, None)`
/// state is unrepresentable per ADR-0008.
#[derive(Debug, Clone, Copy)]
pub enum RateLimits {
    FiveHourOnly(RateLimitWindow),
    SevenDayOnly(RateLimitWindow),
    Both {
        five_hour: RateLimitWindow,
        seven_day: RateLimitWindow,
    },
}

impl RateLimits {
    #[must_use]
    pub fn five_hour(&self) -> Option<&RateLimitWindow> {
        match self {
            Self::FiveHourOnly(w) | Self::Both { five_hour: w, .. } => Some(w),
            Self::SevenDayOnly(_) => None,
        }
    }

    #[must_use]
    pub fn seven_day(&self) -> Option<&RateLimitWindow> {
        match self {
            Self::SevenDayOnly(w) | Self::Both { seven_day: w, .. } => Some(w),
            Self::FiveHourOnly(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RateLimitWindow {
    pub used: Percent,
    pub resets_at: DateTime<Utc>,
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

/// Percentage in `0.0..=100.0`. Construction outside that range returns
/// `None` so normalizers can translate to `ParseError::InvalidValue`.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
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
    /// invariant (e.g. `Percent::new` rejected a number outside `0..=100`).
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
        ContextWindow, CostMetrics, EffortLevel, GitWorktree, JsonType, ModelInfo, ParseError,
        Percent, RateLimitWindow, RateLimits, StatusContext, Tool, WorkspaceInfo,
    };
    use chrono::{DateTime, Utc};
    use std::path::PathBuf;
    use std::sync::Arc;

    const TOOL: Tool = Tool::ClaudeCode;

    pub fn normalize(raw: Arc<serde_json::Value>) -> Result<StatusContext, ParseError> {
        let root = expect_object(&raw, "")?;

        let model = parse_model(root)?;
        let workspace = parse_workspace(root)?;
        let context_window = parse_context_window(root)?;
        let cost = parse_cost(root)?;
        let rate_limits = parse_rate_limits(root)?;
        let effort = parse_effort(root)?;

        Ok(StatusContext {
            tool: TOOL,
            model,
            workspace,
            context_window,
            cost,
            rate_limits,
            effort,
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

        let used_raw = require_f64(cw, "context_window.used_percentage")?;
        let used = Percent::from_f64(used_raw).ok_or_else(|| {
            invalid_value(
                "context_window.used_percentage",
                "percentage must be in 0.0..=100.0",
            )
        })?;

        let size = require_u64(cw, "context_window.context_window_size")?;
        let total_input_tokens = require_u64(cw, "context_window.total_input_tokens")?;
        let total_output_tokens = require_u64(cw, "context_window.total_output_tokens")?;

        Ok(Some(ContextWindow {
            used,
            size,
            total_input_tokens,
            total_output_tokens,
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

    fn parse_rate_limits(
        root: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<Option<RateLimits>, ParseError> {
        let Some(value) = root.get("rate_limits") else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let obj = expect_object(value, "rate_limits")?;

        let five_hour = parse_rate_window(obj, "five_hour", "rate_limits.five_hour")?;
        let seven_day = parse_rate_window(obj, "seven_day", "rate_limits.seven_day")?;

        Ok(match (five_hour, seven_day) {
            (Some(f), Some(s)) => Some(RateLimits::Both {
                five_hour: f,
                seven_day: s,
            }),
            (Some(f), None) => Some(RateLimits::FiveHourOnly(f)),
            (None, Some(s)) => Some(RateLimits::SevenDayOnly(s)),
            (None, None) => None,
        })
    }

    fn parse_rate_window(
        obj: &serde_json::Map<String, serde_json::Value>,
        key: &str,
        path: &'static str,
    ) -> Result<Option<RateLimitWindow>, ParseError> {
        let Some(value) = obj.get(key) else {
            return Ok(None);
        };
        if value.is_null() {
            return Ok(None);
        }
        let window = expect_object(value, path)?;

        let used_raw = require_f64_at(
            window,
            "used_percentage",
            &format!("{path}.used_percentage"),
        )?;
        let used = Percent::from_f64(used_raw).ok_or_else(|| {
            invalid_value(
                format!("{path}.used_percentage"),
                "percentage must be in 0.0..=100.0",
            )
        })?;

        let resets_at_str = require_string_at(window, "resets_at", &format!("{path}.resets_at"))?;
        let resets_at = DateTime::parse_from_rfc3339(resets_at_str)
            .map_err(|err| ParseError::NormalizerError {
                tool: TOOL,
                message: format!("rate_limits.{key}.resets_at is not RFC 3339: {err}"),
            })?
            .with_timezone(&Utc);

        Ok(Some(RateLimitWindow { used, resets_at }))
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
        let raw = value
            .as_str()
            .ok_or_else(|| type_mismatch("effort", JsonType::String, JsonType::of(value)))?;
        raw.parse::<EffortLevel>()
            .map(Some)
            .map_err(|()| invalid_value("effort", "expected one of: low, medium, high, max, xhigh"))
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

    /// Variants that take an explicit key (lookup) and path (error
    /// reporting) separately; used when the path is runtime-computed
    /// (rate_limits.{five_hour|seven_day}.*).
    fn require_string_at<'a>(
        obj: &'a serde_json::Map<String, serde_json::Value>,
        key: &str,
        path: &str,
    ) -> Result<&'a str, ParseError> {
        let value = obj.get(key).ok_or_else(|| missing(path.to_owned()))?;
        value
            .as_str()
            .ok_or_else(|| type_mismatch(path, JsonType::String, JsonType::of(value)))
    }

    fn require_f64_at(
        obj: &serde_json::Map<String, serde_json::Value>,
        key: &str,
        path: &str,
    ) -> Result<f64, ParseError> {
        let value = obj.get(key).ok_or_else(|| missing(path.to_owned()))?;
        value
            .as_f64()
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
    fn rejects_out_of_range_percentage_as_invalid_value() {
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
        match parse(json).expect_err("should reject") {
            ParseError::InvalidValue { path, reason, .. } => {
                assert_eq!(path, "context_window.used_percentage");
                assert!(reason.contains("0.0..=100.0"));
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
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

    // --- rate_limits variant matrix ---

    fn base_payload_with_rate_limits(body: &str) -> Vec<u8> {
        format!(
            r#"{{"model":{{"display_name":"X"}},"workspace":{{"project_dir":"/r"}},"rate_limits":{body}}}"#
        )
        .into_bytes()
    }

    #[test]
    fn rate_limits_with_both_windows_parses_as_both() {
        let bytes = base_payload_with_rate_limits(
            r#"{"five_hour":{"used_percentage":35.0,"resets_at":"2099-01-01T00:00:00Z"},
                "seven_day":{"used_percentage":12.0,"resets_at":"2099-01-08T00:00:00Z"}}"#,
        );
        let ctx = parse(&bytes).expect("parse ok");
        assert!(matches!(ctx.rate_limits, Some(RateLimits::Both { .. })));
    }

    #[test]
    fn rate_limits_with_only_five_hour_parses_as_five_hour_only() {
        let bytes = base_payload_with_rate_limits(
            r#"{"five_hour":{"used_percentage":35.0,"resets_at":"2099-01-01T00:00:00Z"}}"#,
        );
        let ctx = parse(&bytes).expect("parse ok");
        assert!(matches!(ctx.rate_limits, Some(RateLimits::FiveHourOnly(_))));
    }

    #[test]
    fn rate_limits_with_only_seven_day_parses_as_seven_day_only() {
        let bytes = base_payload_with_rate_limits(
            r#"{"seven_day":{"used_percentage":12.0,"resets_at":"2099-01-08T00:00:00Z"}}"#,
        );
        let ctx = parse(&bytes).expect("parse ok");
        assert!(matches!(ctx.rate_limits, Some(RateLimits::SevenDayOnly(_))));
    }

    #[test]
    fn rate_limits_empty_object_collapses_to_none() {
        // Forgiving parse: an empty rate_limits object is treated the same
        // as the key being absent. If Claude ever regresses to emitting {}
        // when a window should be present we'd silently hide — acceptable
        // tradeoff today (see slice 3 review). Lock current behavior.
        let bytes = base_payload_with_rate_limits("{}");
        let ctx = parse(&bytes).expect("parse ok");
        assert!(ctx.rate_limits.is_none());
    }

    #[test]
    fn rate_limits_explicit_null_treated_as_none() {
        let bytes = br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"rate_limits":null}"#;
        let ctx = parse(bytes).expect("parse ok");
        assert!(ctx.rate_limits.is_none());
    }

    // --- rate_window error paths ---

    #[test]
    fn rate_window_rejects_malformed_rfc3339_as_normalizer_error() {
        let bytes = base_payload_with_rate_limits(
            r#"{"five_hour":{"used_percentage":35.0,"resets_at":"not-a-date"}}"#,
        );
        match parse(&bytes).expect_err("should reject") {
            ParseError::NormalizerError { message, .. } => {
                assert!(message.contains("RFC 3339"), "got {message:?}");
            }
            other => panic!("expected NormalizerError, got {other:?}"),
        }
    }

    #[test]
    fn rate_window_accepts_non_z_timezone_offset() {
        let bytes = base_payload_with_rate_limits(
            r#"{"five_hour":{"used_percentage":35.0,"resets_at":"2099-04-17T19:30:00+02:00"}}"#,
        );
        let ctx = parse(&bytes).expect("parse ok");
        let rl = ctx.rate_limits.expect("rate_limits");
        let window = rl.five_hour().expect("five_hour");
        // +02:00 19:30 == Z 17:30
        assert_eq!(window.resets_at.to_rfc3339(), "2099-04-17T17:30:00+00:00");
    }

    #[test]
    fn rate_window_rejects_non_string_resets_at_as_type_mismatch() {
        let bytes = base_payload_with_rate_limits(
            r#"{"five_hour":{"used_percentage":35.0,"resets_at":42}}"#,
        );
        match parse(&bytes).expect_err("should reject") {
            ParseError::TypeMismatch {
                path,
                expected,
                got,
                ..
            } => {
                assert_eq!(path, "rate_limits.five_hour.resets_at");
                assert_eq!(expected, JsonType::String);
                assert_eq!(got, JsonType::Number);
            }
            other => panic!("expected TypeMismatch, got {other:?}"),
        }
    }

    #[test]
    fn rate_window_rejects_missing_used_percentage_as_missing_field() {
        let bytes =
            base_payload_with_rate_limits(r#"{"five_hour":{"resets_at":"2099-01-01T00:00:00Z"}}"#);
        match parse(&bytes).expect_err("should reject") {
            ParseError::MissingField { path, .. } => {
                assert_eq!(path, "rate_limits.five_hour.used_percentage");
            }
            other => panic!("expected MissingField, got {other:?}"),
        }
    }

    #[test]
    fn rate_window_rejects_out_of_range_percent_as_invalid_value() {
        let bytes = base_payload_with_rate_limits(
            r#"{"five_hour":{"used_percentage":150,"resets_at":"2099-01-01T00:00:00Z"}}"#,
        );
        match parse(&bytes).expect_err("should reject") {
            ParseError::InvalidValue { path, .. } => {
                assert_eq!(path, "rate_limits.five_hour.used_percentage");
            }
            other => panic!("expected InvalidValue, got {other:?}"),
        }
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
    fn effort_non_string_rejected_as_type_mismatch() {
        let bytes =
            br#"{"model":{"display_name":"X"},"workspace":{"project_dir":"/r"},"effort":42}"#;
        match parse(bytes).expect_err("rejected") {
            ParseError::TypeMismatch { path, .. } => assert_eq!(path, "effort"),
            other => panic!("expected TypeMismatch, got {other:?}"),
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
}
