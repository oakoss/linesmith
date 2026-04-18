//! Directory / worktree hybrid segment.
//!
//! - Inside a git worktree with a non-empty name: `{repo}/{worktree_name}`
//! - Regular git repo or outside git: the project-dir basename
//! - Project dir has no usable basename, or worktree name is empty: hidden

use super::{RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::input::StatusContext;

pub struct WorkspaceSegment;

/// Lowest non-zero priority in the built-in set: orientation ("where am
/// I?") survives nearly all width pressure.
const PRIORITY: u8 = 16;

impl Segment for WorkspaceSegment {
    fn render(&self, ctx: &StatusContext) -> RenderResult {
        let Some(repo_name) = ctx
            .workspace
            .project_dir
            .file_name()
            .and_then(|s| s.to_str())
        else {
            return Ok(None);
        };

        if let Some(worktree) = &ctx.workspace.git_worktree {
            if worktree.name.is_empty() {
                return Ok(None);
            }
            return Ok(Some(RenderedSegment::new(format!(
                "{repo_name}/{}",
                worktree.name
            ))));
        }

        Ok(Some(RenderedSegment::new(repo_name)))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
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
            WorkspaceSegment.render(&ctx(None)).unwrap(),
            Some(RenderedSegment::new("linesmith"))
        );
    }

    #[test]
    fn renders_hybrid_inside_worktree() {
        assert_eq!(
            WorkspaceSegment
                .render(&ctx(Some(worktree("feat-segments"))))
                .unwrap(),
            Some(RenderedSegment::new("linesmith/feat-segments"))
        );
    }

    #[test]
    fn renders_worktree_name_containing_slash_verbatim() {
        // Branch-backed worktrees commonly have `/` in their names. We
        // render verbatim (no escape, no truncation); downstream readers
        // interpret "repo/path-with-slashes" unambiguously in practice.
        assert_eq!(
            WorkspaceSegment
                .render(&ctx(Some(worktree("feature/auth"))))
                .unwrap(),
            Some(RenderedSegment::new("linesmith/feature/auth"))
        );
    }

    #[test]
    fn hidden_when_project_dir_has_no_basename() {
        let mut c = ctx(None);
        c.workspace.project_dir = PathBuf::from("/");
        assert_eq!(WorkspaceSegment.render(&c).unwrap(), None);
    }

    #[test]
    fn hidden_when_worktree_name_is_empty() {
        assert_eq!(
            WorkspaceSegment.render(&ctx(Some(worktree("")))).unwrap(),
            None
        );
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(WorkspaceSegment.defaults().priority, PRIORITY);
    }

    #[test]
    fn hostile_worktree_name_is_stripped_of_control_chars() {
        let rendered = WorkspaceSegment
            .render(&ctx(Some(worktree("evil\x1b[2J"))))
            .unwrap()
            .expect("renders");
        assert_eq!(rendered.text(), "linesmith/evil[2J");
        assert!(!rendered.text().contains('\x1b'));
    }

    #[test]
    fn hostile_project_dir_basename_is_stripped_of_control_chars() {
        // Separate code path from worktree: project-dir basename,
        // payload varied to OSC-set-title + BEL so the two tests
        // cover distinct escape families.
        let mut c = ctx(None);
        c.workspace.project_dir = PathBuf::from("/tmp/\x1b]0;pwn\x07evil");
        let rendered = WorkspaceSegment.render(&c).unwrap().expect("renders");
        assert_eq!(rendered.text(), "]0;pwnevil");
        assert!(!rendered.text().contains('\x1b'));
        assert!(!rendered.text().contains('\x07'));
    }
}
