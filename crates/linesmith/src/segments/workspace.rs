//! Directory / worktree hybrid segment.
//!
//! - Inside a linked git worktree: `{repo}/{worktree_name}`
//! - Any other repo kind or outside git: project-dir basename
//! - Project dir has no usable basename: hidden
//!
//! Worktree name comes from [`DataContext::git`] (gix discovery) rather
//! than the stdin passthrough, so every git-aware segment agrees on one
//! source of truth.

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::{DataContext, DataDep, RepoKind};
use crate::theme::Role;

pub struct WorkspaceSegment;

/// Lowest non-zero priority in the built-in set: orientation ("where am
/// I?") survives nearly all width pressure.
const PRIORITY: u8 = 16;

impl Segment for WorkspaceSegment {
    fn data_deps(&self) -> &'static [DataDep] {
        &[DataDep::Git]
    }

    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(repo_name) = ctx
            .status
            .workspace
            .project_dir
            .file_name()
            .and_then(|s| s.to_str())
        else {
            crate::lsm_debug!("workspace: status.workspace.project_dir has no basename; hiding");
            return Ok(None);
        };

        let text = match &*ctx.git() {
            Ok(Some(gc)) => match &gc.repo_kind {
                RepoKind::LinkedWorktree { name } => format!("{repo_name}/{name}"),
                // Wildcard keeps parity with git_branch's SemVer posture
                // on the shared #[non_exhaustive] enum: a new variant
                // renders like a regular checkout until either segment
                // opts it in.
                _ => repo_name.to_string(),
            },
            // Data layer already logged any gix error to stderr; fall
            // through so orientation survives.
            Ok(None) | Err(_) => repo_name.to_string(),
        };

        Ok(Some(RenderedSegment::new(text).with_role(Role::Info)))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY).with_truncatable(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_context::{GitContext, GitError, Head, RepoKind};
    use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn status(project_dir: &str) -> StatusContext {
        StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "Claude Test".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from(project_dir),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            raw: Arc::new(serde_json::Value::Null),
        }
    }

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx_with_git(project_dir: &str, git: Result<Option<GitContext>, GitError>) -> DataContext {
        let dc = DataContext::with_cwd(status(project_dir), None);
        dc.preseed_git(git).expect("fresh onceCell");
        dc
    }

    fn linked_worktree(name: &str) -> GitContext {
        GitContext::new(
            RepoKind::LinkedWorktree { name: name.into() },
            PathBuf::from(format!("/repo/.git/worktrees/{name}")),
            Head::Branch(name.into()),
        )
    }

    #[test]
    fn renders_directory_outside_repo() {
        let dc = ctx_with_git("/home/dev/linesmith", Ok(None));
        assert_eq!(
            WorkspaceSegment.render(&dc, &rc()).unwrap(),
            Some(RenderedSegment::new("linesmith").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_basename_in_main_checkout() {
        let gc = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/home/dev/linesmith/.git"),
            Head::Branch("main".into()),
        );
        let dc = ctx_with_git("/home/dev/linesmith", Ok(Some(gc)));
        assert_eq!(
            WorkspaceSegment.render(&dc, &rc()).unwrap(),
            Some(RenderedSegment::new("linesmith").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_hybrid_inside_linked_worktree() {
        let dc = ctx_with_git(
            "/home/dev/linesmith",
            Ok(Some(linked_worktree("feat-segments"))),
        );
        assert_eq!(
            WorkspaceSegment.render(&dc, &rc()).unwrap(),
            Some(RenderedSegment::new("linesmith/feat-segments").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_worktree_name_containing_slash_verbatim() {
        // Branch-backed worktrees commonly have `/` in their names. We
        // render verbatim (no escape, no truncation); downstream readers
        // interpret "repo/path-with-slashes" unambiguously in practice.
        let dc = ctx_with_git(
            "/home/dev/linesmith",
            Ok(Some(linked_worktree("feature/auth"))),
        );
        assert_eq!(
            WorkspaceSegment.render(&dc, &rc()).unwrap(),
            Some(RenderedSegment::new("linesmith/feature/auth").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_basename_in_bare_repo() {
        let gc = GitContext::new(
            RepoKind::Bare,
            PathBuf::from("/srv/repos/linesmith.git"),
            Head::Unborn {
                symbolic_ref: "main".into(),
            },
        );
        let dc = ctx_with_git("/home/dev/linesmith", Ok(Some(gc)));
        assert_eq!(
            WorkspaceSegment.render(&dc, &rc()).unwrap(),
            Some(RenderedSegment::new("linesmith").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_basename_in_submodule() {
        let gc = GitContext::new(
            RepoKind::Submodule,
            PathBuf::from("/home/dev/parent/.git/modules/linesmith"),
            Head::Branch("main".into()),
        );
        let dc = ctx_with_git("/home/dev/linesmith", Ok(Some(gc)));
        assert_eq!(
            WorkspaceSegment.render(&dc, &rc()).unwrap(),
            Some(RenderedSegment::new("linesmith").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_basename_on_gix_corrupt_repo() {
        // A gix failure (safe.directory rejection, corrupt index, etc.)
        // must not blank the basename; orientation still matters.
        let err = GitError::CorruptRepo {
            path: PathBuf::from("/home/dev/linesmith"),
            message: "synthetic".into(),
        };
        let dc = ctx_with_git("/home/dev/linesmith", Err(err));
        assert_eq!(
            WorkspaceSegment.render(&dc, &rc()).unwrap(),
            Some(RenderedSegment::new("linesmith").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_basename_on_gix_walk_failed() {
        // Pin the second GitError variant too — wildcard `Err(_)` in
        // render must treat every error the same, but the spec
        // distinguishes CorruptRepo from WalkFailed and a future
        // refactor could split the arms.
        let err = GitError::WalkFailed {
            path: PathBuf::from("/home/dev/linesmith"),
            message: "synthetic".into(),
        };
        let dc = ctx_with_git("/home/dev/linesmith", Err(err));
        assert_eq!(
            WorkspaceSegment.render(&dc, &rc()).unwrap(),
            Some(RenderedSegment::new("linesmith").with_role(Role::Info))
        );
    }

    #[test]
    fn hidden_when_project_dir_has_no_basename() {
        let dc = ctx_with_git("/", Ok(None));
        assert_eq!(WorkspaceSegment.render(&dc, &rc()).unwrap(), None);
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(WorkspaceSegment.defaults().priority, PRIORITY);
    }

    #[test]
    fn declares_git_data_dep() {
        assert_eq!(WorkspaceSegment.data_deps(), &[DataDep::Git]);
    }

    #[test]
    fn hostile_worktree_name_is_stripped_of_control_chars() {
        let dc = ctx_with_git(
            "/home/dev/linesmith",
            Ok(Some(linked_worktree("evil\x1b[2J"))),
        );
        let rendered = WorkspaceSegment
            .render(&dc, &rc())
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
        let dc = ctx_with_git("/tmp/\x1b]0;pwn\x07evil", Ok(None));
        let rendered = WorkspaceSegment
            .render(&dc, &rc())
            .unwrap()
            .expect("renders");
        assert_eq!(rendered.text(), "]0;pwnevil");
        assert!(!rendered.text().contains('\x1b'));
        assert!(!rendered.text().contains('\x07'));
    }
}
