//! `StatusContext` models the parsed Claude Code statusline JSON.
//! Rate-limit windows, cost, vim state, effort, output-style, and agent
//! fields are added as segments start consuming them; see
//! `docs/specs/input-schema.md` for the full contract.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

/// The canonical, tool-agnostic input to the rendering pipeline. `Arc`
/// around `raw` keeps `StatusContext::clone` at O(1) when segments cache.
#[derive(Debug, Clone)]
pub struct StatusContext {
    pub tool: Tool,
    pub model: ModelInfo,
    pub workspace: WorkspaceInfo,
    pub context_window: Option<ContextWindow>,
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
        ContextWindow, GitWorktree, JsonType, ModelInfo, ParseError, Percent, StatusContext, Tool,
        WorkspaceInfo,
    };
    use std::path::PathBuf;
    use std::sync::Arc;

    const TOOL: Tool = Tool::ClaudeCode;

    pub fn normalize(raw: Arc<serde_json::Value>) -> Result<StatusContext, ParseError> {
        let root = expect_object(&raw, "")?;

        let model = parse_model(root)?;
        let workspace = parse_workspace(root)?;
        let context_window = parse_context_window(root)?;

        Ok(StatusContext {
            tool: TOOL,
            model,
            workspace,
            context_window,
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
}
