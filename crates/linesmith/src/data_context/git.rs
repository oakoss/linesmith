//! Git repository inspection state via `gix`.
//!
//! Canonical definition: `docs/specs/git-segments.md` §GitContext type.
//!
//! Two tiers of laziness. [`DataContext::git`](super::DataContext::git)
//! decides whether to open the repo at all. Once opened, [`GitContext`]
//! exposes lazy [`dirty`](GitContext::dirty) and
//! [`upstream`](GitContext::upstream) accessors so segments that don't
//! read those fields skip the scan entirely.

use std::cell::OnceCell;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::errors::GitError;

/// Which flavor of repository `gix::discover` found.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepoKind {
    /// A regular checkout with a `.git/` directory.
    Main,
    /// A linked worktree (`.git` is a file with `gitdir: ...`). `name`
    /// is the per-worktree directory basename (`.git/worktrees/<name>/`).
    LinkedWorktree { name: String },
    /// A bare repository. `git_branch` hides on this kind (no working
    /// tree means no dirty state).
    Bare,
    /// A submodule checkout. Treated like `Main` for rendering
    /// purposes — it has a working tree and a HEAD — but carried as a
    /// distinct variant so segments that need to style submodules
    /// differently don't have to re-classify.
    Submodule,
}

/// Resolved HEAD state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Head {
    /// HEAD → `refs/heads/<name>` with at least one commit. The
    /// `String` is the short name (prefix stripped).
    Branch(String),
    /// Detached HEAD at a specific object.
    Detached(gix::ObjectId),
    /// Fresh `git init` with no commits. `symbolic_ref` is the short
    /// name HEAD points at (whatever `init.defaultBranch` resolves to).
    Unborn { symbolic_ref: String },
    /// HEAD points at a ref outside `refs/heads/` (e.g. a remote-
    /// tracking ref or a tag). `full_name` is the unstripped refname.
    OtherRef { full_name: String },
}

impl Head {
    /// Short plugin-facing tag used in the rhai ctx mirror.
    #[must_use]
    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::Branch(_) => "branch",
            Self::Detached(_) => "detached",
            Self::Unborn { .. } => "unborn",
            Self::OtherRef { .. } => "other_ref",
        }
    }
}

/// Dirty-state result.
///
/// - `Clean` — no tracked modifications and no untracked files.
/// - `Dirty(None)` — fast-path indicator mode: scan short-circuited on
///   the first dirty entry, so counts were not collected.
/// - `Dirty(Some(counts))` — full-scan counts mode.
///
/// The two `Dirty` forms are distinct plugin-facing states so a
/// future counts-mode renderer can reliably tell "counts not yet
/// computed" apart from "zero of this category."
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum DirtyState {
    #[default]
    Clean,
    Dirty(Option<DirtyCounts>),
}

impl DirtyState {
    /// True iff the working tree has any modification or untracked file.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        matches!(self, Self::Dirty(_))
    }
}

/// Per-category dirty counts. Populated only in full-scan (counts)
/// mode; fast-path indicator scans leave this absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct DirtyCounts {
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
}

/// Upstream-tracking branch comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UpstreamState {
    pub ahead: u32,
    pub behind: u32,
    pub upstream_branch: String,
}

/// Git state shared across all `git_*` segments for the duration of
/// one render invocation. Populated once by
/// [`resolve_repo`] and held behind an
/// [`Arc`](std::sync::Arc) in [`DataContext`](super::DataContext).
#[non_exhaustive]
pub struct GitContext {
    /// Which repo flavor was discovered.
    pub repo_kind: RepoKind,
    /// Absolute path to the repository directory (the `.git` dir for
    /// main, the `.git/worktrees/<name>/` dir for linked worktrees,
    /// the repo path itself for bare).
    pub repo_path: PathBuf,
    /// Resolved HEAD.
    pub head: Head,

    dirty: OnceCell<Arc<DirtyState>>,
    upstream: OnceCell<Arc<Option<UpstreamState>>>,
    /// Kept for lazy dirty / upstream resolution. `gix::Repository`
    /// is `Send` (with the `parallel` feature) but not `Sync`; the
    /// render path is single-threaded, so `OnceCell` suffices.
    repo: Option<gix::Repository>,
}

impl std::fmt::Debug for GitContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitContext")
            .field("repo_kind", &self.repo_kind)
            .field("repo_path", &self.repo_path)
            .field("head", &self.head)
            .field("dirty", &self.dirty.get().map(|arc| &**arc))
            .field("upstream", &self.upstream.get().map(|arc| &**arc))
            .finish_non_exhaustive()
    }
}

impl GitContext {
    /// Construct a [`GitContext`] from pre-resolved fields without
    /// opening a repo. Lazy `dirty` / `upstream` accessors then
    /// return their defaults (empty dirty state, no upstream)
    /// because no `gix::Repository` is held. Pair with
    /// [`DataContext::preseed_git`](super::DataContext::preseed_git).
    #[must_use]
    pub fn new(repo_kind: RepoKind, repo_path: PathBuf, head: Head) -> Self {
        Self {
            repo_kind,
            repo_path,
            head,
            dirty: OnceCell::new(),
            upstream: OnceCell::new(),
            repo: None,
        }
    }

    /// Dirty state, scanned lazily on first access. Returns
    /// [`DirtyState::Clean`] when no repo handle is held.
    ///
    /// The scan covers untracked files and tracked modifications.
    /// HEAD↔index (staged-only) changes are not detected — gix 0.67
    /// does not expose that comparison; see lsm-u5h for the
    /// follow-up.
    #[must_use]
    pub fn dirty(&self) -> Arc<DirtyState> {
        self.dirty
            .get_or_init(|| match &self.repo {
                Some(repo) => Arc::new(compute_dirty(repo).unwrap_or_else(|err| {
                    // Diagnostic-only stderr until lsm-cgg (logger)
                    // lands; a silent false-clean would mask real
                    // gix failures like index corruption.
                    let _ = writeln!(
                        io::stderr().lock(),
                        "linesmith: git dirty scan failed: {err}"
                    );
                    DirtyState::Clean
                })),
                None => Arc::new(DirtyState::Clean),
            })
            .clone()
    }

    /// Upstream-tracking state. Returns `Arc<None>` when no
    /// upstream has been populated; ahead/behind renderers treat
    /// `None` the same as "no upstream configured."
    #[must_use]
    pub fn upstream(&self) -> Arc<Option<UpstreamState>> {
        self.upstream.get_or_init(|| Arc::new(None)).clone()
    }
}

/// Walk up from `cwd` looking for a repository. Returns `Ok(None)`
/// only for the legitimate "no repo here" cases (no `.git` found
/// walking up). Permission errors, trust rejections (`safe.directory`),
/// ceiling-dir misconfig, and path-input errors surface as
/// [`GitError::CorruptRepo`] so they reach the user instead of
/// silently hiding the segment.
pub fn resolve_repo(cwd: &Path) -> Result<Option<GitContext>, GitError> {
    let repo = match gix::discover(cwd) {
        Ok(r) => r,
        Err(gix::discover::Error::Discover(inner)) => {
            use gix::discover::upwards::Error as U;
            match inner {
                U::NoGitRepository { .. }
                | U::NoGitRepositoryWithinCeiling { .. }
                | U::NoGitRepositoryWithinFs { .. } => return Ok(None),
                other => {
                    return Err(GitError::CorruptRepo {
                        path: cwd.to_path_buf(),
                        message: other.to_string(),
                    });
                }
            }
        }
        Err(e) => {
            return Err(GitError::CorruptRepo {
                path: cwd.to_path_buf(),
                message: e.to_string(),
            });
        }
    };

    let repo_kind = classify_kind(&repo);
    let repo_path = repo.git_dir().to_path_buf();
    let head = resolve_head(&repo).map_err(|e| GitError::WalkFailed {
        path: repo_path.clone(),
        message: e,
    })?;

    Ok(Some(GitContext {
        repo_kind,
        repo_path,
        head,
        dirty: OnceCell::new(),
        upstream: OnceCell::new(),
        repo: Some(repo),
    }))
}

/// Fast-path dirty check: short-circuits on the first status entry.
/// Covers untracked + worktree-vs-index (unstaged). Missing:
/// HEAD↔index (staged) per gix 0.67's own TODO on `is_dirty`;
/// tracked in lsm-u5h.
fn compute_dirty(repo: &gix::Repository) -> Result<DirtyState, Box<dyn std::error::Error>> {
    use gix::status::UntrackedFiles;

    let platform = repo
        .status(gix::progress::Discard)?
        .untracked_files(UntrackedFiles::Collapsed)
        .index_worktree_rewrites(None);
    for item in platform.into_index_worktree_iter(Vec::new())? {
        if item.is_ok() {
            return Ok(DirtyState::Dirty(None));
        }
    }
    Ok(DirtyState::Clean)
}

fn classify_kind(repo: &gix::Repository) -> RepoKind {
    match repo.kind() {
        gix::repository::Kind::Bare => RepoKind::Bare,
        gix::repository::Kind::WorkTree { is_linked: false } => RepoKind::Main,
        gix::repository::Kind::WorkTree { is_linked: true } => {
            // `.git/worktrees/<name>/` — basename of the gitdir is the
            // per-worktree label.
            let name = repo
                .git_dir()
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            RepoKind::LinkedWorktree { name }
        }
        gix::repository::Kind::Submodule => RepoKind::Submodule,
    }
}

fn resolve_head(repo: &gix::Repository) -> Result<Head, String> {
    let head = repo.head().map_err(|e| e.to_string())?;
    match head.kind {
        gix::head::Kind::Symbolic(reference) => {
            let full = reference.name.as_bstr().to_string();
            match full.strip_prefix("refs/heads/") {
                Some(short) => Ok(Head::Branch(short.to_string())),
                None => Ok(Head::OtherRef { full_name: full }),
            }
        }
        gix::head::Kind::Detached { target, peeled: _ } => Ok(Head::Detached(target)),
        gix::head::Kind::Unborn(refname) => {
            let full = refname.as_bstr().to_string();
            match full.strip_prefix("refs/heads/") {
                Some(short) => Ok(Head::Unborn {
                    symbolic_ref: short.to_string(),
                }),
                None => Ok(Head::OtherRef { full_name: full }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn init_repo(dir: &Path) -> gix::Repository {
        gix::init(dir).expect("gix::init")
    }

    #[test]
    fn non_repo_directory_returns_ok_none() {
        let tmp = TempDir::new().expect("tmp");
        // Nested subdir with no .git anywhere up the chain. `gix`
        // discover walks up the tempdir which lives under /var/folders;
        // tempdirs do not have .git parents on any OS we support.
        let sub = tmp.path().join("nested");
        fs::create_dir_all(&sub).expect("mkdir");
        assert!(resolve_repo(&sub).expect("resolve").is_none());
    }

    #[test]
    fn main_checkout_classifies_as_main() {
        let tmp = TempDir::new().expect("tmp");
        init_repo(tmp.path());
        let ctx = resolve_repo(tmp.path()).expect("resolve").expect("some");
        assert_eq!(ctx.repo_kind, RepoKind::Main);
    }

    #[test]
    fn bare_repo_classifies_as_bare() {
        let tmp = TempDir::new().expect("tmp");
        gix::init_bare(tmp.path()).expect("init_bare");
        let ctx = resolve_repo(tmp.path()).expect("resolve").expect("some");
        assert_eq!(ctx.repo_kind, RepoKind::Bare);
    }

    #[test]
    fn unborn_head_reports_symbolic_ref_target() {
        let tmp = TempDir::new().expect("tmp");
        init_repo(tmp.path());
        let ctx = resolve_repo(tmp.path()).expect("resolve").expect("some");
        match &ctx.head {
            Head::Unborn { symbolic_ref } => {
                // `gix::init` defaults to `main` unless init.defaultBranch
                // is configured; we accept either `main` or `master` so
                // this runs on systems with legacy defaults.
                assert!(
                    symbolic_ref == "main" || symbolic_ref == "master",
                    "unexpected default branch: {symbolic_ref}"
                );
            }
            other => panic!("expected Unborn, got {other:?}"),
        }
    }

    #[test]
    fn dirty_is_clean_when_no_gix_repo_held() {
        let ctx = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/tmp/.git"),
            Head::Branch("main".into()),
        );
        assert_eq!(*ctx.dirty(), DirtyState::Clean);
    }

    /// Build a repo with one committed tracked file. Returns the
    /// fixture path so callers can add untracked files / modify
    /// tracked ones and re-scan.
    fn fixture_with_commit(tmp: &TempDir) -> &Path {
        use std::fs;
        use std::process::Command;
        let path = tmp.path();
        // Shelling out to `git` for test-fixture setup is acceptable
        // per our AGENTS policy: the production code path stays
        // gix-only, but fabricating an index + HEAD via gix's mid-
        // level APIs would be dozens of lines of boilerplate per
        // test. Fixture prep runs once per test and only in `cfg(test)`.
        Command::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(path)
            .status()
            .expect("git init");
        Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-C"])
            .arg(path)
            .args(["commit", "--allow-empty", "-m", "seed", "--quiet"])
            .status()
            .expect("git commit");
        fs::write(path.join("tracked.txt"), "v1").expect("write");
        Command::new("git")
            .args(["-C"])
            .arg(path)
            .args(["add", "tracked.txt"])
            .status()
            .expect("git add");
        Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(["-C"])
            .arg(path)
            .args(["commit", "-m", "tracked", "--quiet"])
            .status()
            .expect("git commit");
        path
    }

    #[test]
    fn dirty_detects_untracked_file() {
        use std::fs;
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        fs::write(path.join("new.txt"), "hello").expect("write");
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert!(
            ctx.dirty().is_dirty(),
            "expected dirty on untracked, got {:?}",
            ctx.dirty()
        );
    }

    #[test]
    fn dirty_detects_modified_tracked_file() {
        use std::fs;
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        fs::write(path.join("tracked.txt"), "modified").expect("write");
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert!(
            ctx.dirty().is_dirty(),
            "expected dirty on modified tracked, got {:?}",
            ctx.dirty()
        );
    }

    #[test]
    fn dirty_is_clean_on_committed_repo_with_no_changes() {
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert_eq!(*ctx.dirty(), DirtyState::Clean);
    }

    #[test]
    fn upstream_is_none_until_walker_lands() {
        let ctx = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/tmp/.git"),
            Head::Branch("main".into()),
        );
        assert!(ctx.upstream().is_none());
    }

    #[test]
    fn head_kind_str_covers_every_variant() {
        assert_eq!(Head::Branch("x".into()).kind_str(), "branch");
        assert_eq!(
            Head::Detached(gix::ObjectId::null(gix::hash::Kind::Sha1)).kind_str(),
            "detached"
        );
        assert_eq!(
            Head::Unborn {
                symbolic_ref: "main".into()
            }
            .kind_str(),
            "unborn"
        );
        assert_eq!(
            Head::OtherRef {
                full_name: "refs/remotes/origin/main".into()
            }
            .kind_str(),
            "other_ref"
        );
    }
}
