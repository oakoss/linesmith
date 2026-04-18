//! Directory / worktree hybrid segment.
//!
//! - Inside a git worktree with a non-empty name: `{repo}/{worktree_name}`
//! - Regular git repo or outside git: the project-dir basename
//! - Project dir has no usable basename, or worktree name is empty: hidden

use super::{RenderedSegment, Segment};
use crate::input::StatusContext;

pub struct WorkspaceSegment;

impl Segment for WorkspaceSegment {
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment> {
        let repo_name = ctx
            .workspace
            .project_dir
            .file_name()
            .and_then(|s| s.to_str())?;

        if let Some(worktree) = &ctx.workspace.git_worktree {
            if worktree.name.is_empty() {
                return None;
            }
            return Some(RenderedSegment::new(format!(
                "{repo_name}/{}",
                worktree.name
            )));
        }

        Some(RenderedSegment::new(repo_name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{GitWorktree, ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx(worktree: Option<GitWorktree>) -> StatusContext {
        StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "Claude Test".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/home/dev/linesmith"),
                git_worktree: worktree,
            },
            context_window: None,
            cost: None,
            rate_limits: None,
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
        }
    }

    fn worktree(name: &str) -> GitWorktree {
        GitWorktree {
            name: name.into(),
            path: PathBuf::from(format!("/home/dev/linesmith-worktrees/{name}")),
        }
    }

    #[test]
    fn renders_directory_outside_worktree() {
        assert_eq!(
            WorkspaceSegment.render(&ctx(None)),
            Some(RenderedSegment::new("linesmith"))
        );
    }

    #[test]
    fn renders_hybrid_inside_worktree() {
        assert_eq!(
            WorkspaceSegment.render(&ctx(Some(worktree("feat-segments")))),
            Some(RenderedSegment::new("linesmith/feat-segments"))
        );
    }

    #[test]
    fn renders_worktree_name_containing_slash_verbatim() {
        // Branch-backed worktrees commonly have `/` in their names. We
        // render verbatim (no escape, no truncation); downstream readers
        // interpret "repo/path-with-slashes" unambiguously in practice.
        assert_eq!(
            WorkspaceSegment.render(&ctx(Some(worktree("feature/auth")))),
            Some(RenderedSegment::new("linesmith/feature/auth"))
        );
    }

    #[test]
    fn hidden_when_project_dir_has_no_basename() {
        let mut c = ctx(None);
        c.workspace.project_dir = PathBuf::from("/");
        assert_eq!(WorkspaceSegment.render(&c), None);
    }

    #[test]
    fn hidden_when_worktree_name_is_empty() {
        assert_eq!(WorkspaceSegment.render(&ctx(Some(worktree("")))), None);
    }
}
