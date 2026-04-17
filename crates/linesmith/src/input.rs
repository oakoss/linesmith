//! `StatusContext` models the fields built-in segments consume. See
//! `docs/specs/input-schema.md` for the full model (Percent newtype,
//! RateLimits enum, Tool detection) that this grows into as segments
//! land.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct StatusContext {
    pub model: ModelInfo,
    pub workspace: WorkspaceInfo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelInfo {
    pub display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkspaceInfo {
    pub project_dir: PathBuf,
    #[serde(default)]
    pub git_worktree: Option<GitWorktree>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitWorktree {
    pub name: String,
    pub path: PathBuf,
}

/// Parse a Claude Code statusline JSON payload.
///
/// # Errors
///
/// Returns `ParseError::InvalidJson` if the input isn't valid JSON
/// matching the minimal schema this crate currently supports.
pub fn parse(input: &[u8]) -> Result<StatusContext, ParseError> {
    serde_json::from_slice(input).map_err(|err| ParseError::InvalidJson(err.to_string()))
}

#[derive(Debug)]
#[non_exhaustive]
pub enum ParseError {
    InvalidJson(String),
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidJson(msg) => write!(f, "invalid JSON: {msg}"),
        }
    }
}

impl std::error::Error for ParseError {}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(ctx.model.display_name, "Claude Test");
        assert_eq!(
            ctx.workspace.project_dir.to_str(),
            Some("/home/dev/linesmith")
        );
        assert!(ctx.workspace.git_worktree.is_none());
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
    fn rejects_malformed_json() {
        assert!(matches!(
            parse(b"{not json"),
            Err(ParseError::InvalidJson(_))
        ));
    }
}
