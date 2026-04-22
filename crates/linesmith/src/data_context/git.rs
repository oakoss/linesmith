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
    /// A submodule checkout. Has a working tree and HEAD like
    /// `Main`, but carried as a distinct variant so segments that
    /// want to style submodules differently don't re-classify.
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
/// - `Dirty(None)` — indicator mode: scan short-circuited on the
///   first dirty entry, so counts were not collected.
/// - `Dirty(Some(counts))` — full-scan counts mode.
///
/// The two `Dirty` forms are distinct plugin-facing states so
/// counts-mode renderers can tell "counts not yet computed" apart
/// from "zero of this category."
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

/// Per-category dirty counts. Populated only in counts mode;
/// indicator-mode scans leave this absent.
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
    /// HEAD↔index (staged-only) changes are not detected because
    /// gix 0.67 doesn't expose that comparison.
    #[must_use]
    pub fn dirty(&self) -> Arc<DirtyState> {
        self.dirty
            .get_or_init(|| match &self.repo {
                Some(repo) => Arc::new(compute_dirty(repo).unwrap_or_else(|err| {
                    // Silent false-clean would mask real gix failures
                    // (e.g. index corruption); write the cause through.
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

    /// Pre-populate the `upstream` OnceCell so tests can exercise
    /// rendering paths without a real fixture repo. Returns `Err`
    /// via [`OnceCell::set`]'s semantics if the cell was already
    /// populated.
    pub fn preseed_upstream(
        &self,
        value: Option<UpstreamState>,
    ) -> Result<(), Arc<Option<UpstreamState>>> {
        self.upstream.set(Arc::new(value))
    }

    /// Upstream-tracking state, scanned lazily on first access.
    /// Returns `Arc<None>` when the branch has no tracking upstream,
    /// when HEAD is detached / unborn / an `OtherRef`, or when no
    /// repo handle is held. Ahead/behind renderers treat `Arc<None>`
    /// the same as "no upstream configured."
    #[must_use]
    pub fn upstream(&self) -> Arc<Option<UpstreamState>> {
        self.upstream
            .get_or_init(|| match &self.repo {
                Some(repo) => Arc::new(compute_upstream(repo, &self.head).unwrap_or_else(|err| {
                    let _ = writeln!(
                        io::stderr().lock(),
                        "linesmith: git ahead/behind scan failed: {err}"
                    );
                    None
                })),
                None => Arc::new(None),
            })
            .clone()
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

/// Indicator-mode dirty scan: short-circuits on the first status
/// entry. Covers untracked + worktree-vs-index (unstaged). Misses
/// HEAD↔index (staged-only) per gix 0.67's own TODO on `is_dirty`.
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

/// Resolve upstream tracking branch + count ahead/behind relative to
/// HEAD. Returns `Ok(None)` when there is no upstream configured or
/// HEAD cannot tip a branch (detached, unborn, other-ref).
fn compute_upstream(
    repo: &gix::Repository,
    head: &Head,
) -> Result<Option<UpstreamState>, Box<dyn std::error::Error>> {
    let Head::Branch(_) = head else {
        return Ok(None);
    };

    let head_ref = match repo.head_ref()? {
        Some(r) => r,
        None => return Ok(None),
    };
    let upstream_ref_name = match head_ref.remote_tracking_ref_name(gix::remote::Direction::Fetch) {
        Some(Ok(name)) => name.into_owned(),
        Some(Err(e)) => return Err(Box::new(e)),
        None => return Ok(None),
    };

    let mut upstream_ref = match repo.try_find_reference(upstream_ref_name.as_ref())? {
        Some(r) => r,
        None => return Ok(None),
    };
    let upstream_oid = upstream_ref.peel_to_id_in_place()?.detach();
    let head_oid = head_ref.id().detach();

    // Explicit merge-base + `selected()` filter avoids gix's
    // `with_pruned` which is commit-time-based and flakes when
    // commits share a timestamp. Walk each tip, skipping commits
    // reachable from the merge-base.
    let (ahead, behind) = match repo.merge_base(head_oid, upstream_oid) {
        Ok(base) => {
            let base_oid = base.detach();
            let ahead = count_ancestors_excluding(repo, head_oid, base_oid)?;
            let behind = count_ancestors_excluding(repo, upstream_oid, base_oid)?;
            (ahead, behind)
        }
        // No common ancestor (unrelated histories) — treat every
        // commit on each side as its own divergence.
        Err(_) => {
            let ahead = repo.rev_walk([head_oid]).all()?.count();
            let behind = repo.rev_walk([upstream_oid]).all()?.count();
            (ahead, behind)
        }
    };

    let upstream_branch = upstream_ref_name.as_bstr().to_string();
    let upstream_branch = upstream_branch
        .strip_prefix("refs/remotes/")
        .map_or(upstream_branch.clone(), str::to_string);

    Ok(Some(UpstreamState {
        ahead: u32::try_from(ahead).unwrap_or(u32::MAX),
        behind: u32::try_from(behind).unwrap_or(u32::MAX),
        upstream_branch,
    }))
}

/// Count commits reachable from `tip` but not from `stop` (and not
/// `stop` itself). BFS from `tip`; queue all parents except those
/// reachable from `stop`.
fn count_ancestors_excluding(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    stop: gix::ObjectId,
) -> Result<usize, Box<dyn std::error::Error>> {
    use std::collections::HashSet;
    if tip == stop {
        return Ok(0);
    }
    // Pre-load every ancestor of `stop` into an exclusion set so we
    // can short-circuit walks into shared history.
    let mut excluded: HashSet<gix::ObjectId> = HashSet::new();
    excluded.insert(stop);
    for info in repo.rev_walk([stop]).all()? {
        excluded.insert(info?.id);
    }

    let mut visited: HashSet<gix::ObjectId> = HashSet::new();
    let mut count = 0usize;
    for info in repo.rev_walk([tip]).all()? {
        let info = info?;
        if excluded.contains(&info.id) {
            continue;
        }
        if visited.insert(info.id) {
            count += 1;
        }
    }
    Ok(count)
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
        // Fixture setup shells out to the `git` binary; fabricating
        // an index + initial commit via gix would take dozens of
        // lines per test. Production code paths stay gix-only.
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
    fn upstream_is_none_when_no_gix_repo_held() {
        let ctx = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/tmp/.git"),
            Head::Branch("main".into()),
        );
        assert!(ctx.upstream().is_none());
    }

    #[test]
    fn upstream_is_none_when_no_tracking_branch_configured() {
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert!(
            ctx.upstream().is_none(),
            "expected None without upstream, got {:?}",
            ctx.upstream()
        );
    }

    /// Build local + bare-remote fixture with HEAD tracking
    /// `origin/main`. `local_commits` extra commits on top of the
    /// shared base stay local (ahead); `remote_commits` land in the
    /// bare remote and are fetched without updating HEAD (behind).
    fn fixture_with_upstream<'a>(
        local: &'a TempDir,
        remote: &'a TempDir,
        local_commits: usize,
        remote_commits: usize,
    ) -> &'a Path {
        use std::fs;
        use std::process::Command;
        let bare = remote.path();
        let path = local.path();
        Command::new("git")
            .args(["init", "--bare", "--quiet", "--initial-branch=main"])
            .current_dir(bare)
            .status()
            .expect("init bare");
        Command::new("git")
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(path)
            .status()
            .expect("init local");
        // Shared base commit.
        fs::write(path.join("f"), "base").expect("write base");
        run_git(path, &["add", "f"]);
        run_git_commit(path, "base");
        run_git(
            path,
            &["remote", "add", "origin", bare.to_str().expect("utf8 path")],
        );
        run_git(path, &["push", "-u", "origin", "main", "--quiet"]);
        // Diverge: stack `local_commits` on HEAD that aren't pushed.
        for i in 0..local_commits {
            fs::write(path.join("f"), format!("local-{i}")).expect("write");
            run_git(path, &["add", "f"]);
            run_git_commit(path, &format!("local {i}"));
        }
        // Diverge other side: clone from bare into a unique path
        // (so parallel tests don't collide), add commits, push back,
        // then fetch in the local repo so origin/main moves ahead.
        if remote_commits > 0 {
            let other_tmp = TempDir::new().expect("other tmp");
            let other = other_tmp.path().join("clone");
            Command::new("git")
                .args(["clone", "--quiet"])
                .arg(bare)
                .arg(&other)
                .status()
                .expect("clone");
            for i in 0..remote_commits {
                fs::write(other.join("g"), format!("remote-{i}")).expect("write");
                run_git(&other, &["add", "g"]);
                run_git_commit(&other, &format!("remote {i}"));
            }
            run_git(&other, &["push", "--quiet"]);
            run_git(path, &["fetch", "--quiet"]);
            drop(other_tmp);
        }
        path
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        use std::process::Command;
        let status = Command::new("git")
            .args(["-C"])
            .arg(cwd)
            .args(args)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }

    fn run_git_commit(cwd: &Path, msg: &str) {
        use std::process::Command;
        let status = Command::new("git")
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "-C"])
            .arg(cwd)
            .args(["commit", "-m", msg, "--quiet"])
            .status()
            .expect("git commit");
        assert!(status.success(), "git commit failed in {cwd:?}");
    }

    #[test]
    fn upstream_reports_zero_ahead_zero_behind_when_in_sync() {
        let local = TempDir::new().expect("local");
        let remote = TempDir::new().expect("remote");
        let path = fixture_with_upstream(&local, &remote, 0, 0);
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        let upstream = ctx.upstream();
        let state = upstream.as_ref().as_ref().expect("some upstream");
        assert_eq!(state.ahead, 0);
        assert_eq!(state.behind, 0);
        assert_eq!(state.upstream_branch, "origin/main");
    }

    #[test]
    fn upstream_reports_ahead_only_when_local_leads() {
        let local = TempDir::new().expect("local");
        let remote = TempDir::new().expect("remote");
        let path = fixture_with_upstream(&local, &remote, 2, 0);
        use std::process::Command;
        let log = Command::new("git")
            .args(["-C"])
            .arg(path)
            .args(["log", "--all", "--oneline", "--decorate", "--graph"])
            .output()
            .expect("log");
        eprintln!("{}", String::from_utf8_lossy(&log.stdout));
        let head = Command::new("git")
            .args(["-C"])
            .arg(path)
            .args(["rev-parse", "HEAD", "origin/main"])
            .output()
            .expect("rev-parse");
        eprintln!("rev-parse: {}", String::from_utf8_lossy(&head.stdout));
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        let upstream = ctx.upstream();
        let state = upstream.as_ref().as_ref().expect("some upstream");
        assert_eq!(state.ahead, 2);
        assert_eq!(state.behind, 0);
    }

    #[test]
    fn upstream_reports_behind_only_when_remote_leads() {
        let local = TempDir::new().expect("local");
        let remote = TempDir::new().expect("remote");
        let path = fixture_with_upstream(&local, &remote, 0, 3);
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        let upstream = ctx.upstream();
        let state = upstream.as_ref().as_ref().expect("some upstream");
        assert_eq!(state.ahead, 0);
        assert_eq!(state.behind, 3);
    }

    #[test]
    fn upstream_reports_both_when_diverged() {
        let local = TempDir::new().expect("local");
        let remote = TempDir::new().expect("remote");
        let path = fixture_with_upstream(&local, &remote, 2, 3);
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        let upstream = ctx.upstream();
        let state = upstream.as_ref().as_ref().expect("some upstream");
        assert_eq!(state.ahead, 2);
        assert_eq!(state.behind, 3);
    }

    #[test]
    fn upstream_is_none_on_detached_head() {
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        // Detach HEAD at the current commit.
        run_git(path, &["checkout", "--detach", "HEAD"]);
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert!(matches!(ctx.head, Head::Detached(_)));
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
