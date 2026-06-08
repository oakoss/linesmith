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
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::error::GitError;

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
/// - `Dirty(Some(counts))` — full-scan counts mode. Invariant: at
///   least one category is non-zero. [`compute_dirty_counts`] collapses
///   an all-zero tally to `Clean`, so `Dirty(Some(DirtyCounts::default()))`
///   is not a state the scan produces; only a hand-built preseed can
///   forge it.
///
/// The two `Dirty` forms are kept distinct so a counts-mode renderer
/// can tell "counts not collected" (indicator scan) apart from "zero
/// of this category."
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
    /// Counts-mode dirty state. A cell of its own (not shared with
    /// `dirty`) so an indicator-mode read can't poison a later
    /// counts-mode read with a count-less `Dirty(None)`, regardless of
    /// which segment renders first.
    dirty_counts: OnceCell<Arc<DirtyState>>,
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
            .field("dirty_counts", &self.dirty_counts.get().map(|arc| &**arc))
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
            dirty_counts: OnceCell::new(),
            upstream: OnceCell::new(),
            repo: None,
        }
    }

    /// Dirty state in indicator mode, scanned lazily on first access.
    /// Returns [`DirtyState::Clean`] when no repo handle is held.
    ///
    /// The scan short-circuits on the first dirty entry and yields
    /// [`DirtyState::Dirty(None)`](DirtyState::Dirty) — it covers
    /// untracked files and tracked modifications but never collects
    /// per-category counts. Use [`Self::dirty_counts`] when the caller
    /// needs the staged/unstaged/untracked breakdown.
    #[must_use]
    pub fn dirty(&self) -> Arc<DirtyState> {
        self.dirty
            .get_or_init(|| match &self.repo {
                Some(repo) => Arc::new(compute_dirty(repo).unwrap_or_else(|err| {
                    // Silent false-clean would mask real gix failures
                    // (e.g. index corruption); route through the
                    // logger so `LINESMITH_LOG=off` can suppress it.
                    crate::lsm_warn!("git dirty scan failed: {err}");
                    DirtyState::Clean
                })),
                None => Arc::new(DirtyState::Clean),
            })
            .clone()
    }

    /// Dirty state in counts mode, scanned lazily on first access.
    /// Returns [`DirtyState::Clean`] when no repo handle is held, and
    /// otherwise [`DirtyState::Dirty(Some(counts))`](DirtyState::Dirty)
    /// with a per-category file breakdown.
    ///
    /// Costlier than [`Self::dirty`]: the scan has no early-exit and
    /// adds the HEAD↔index comparison so staged-only changes are
    /// counted (available since gix 0.83's combined status iterator).
    /// Cached in a dedicated cell, so calling both accessors runs two
    /// scans — only counts-mode segments should reach for this one.
    ///
    /// The two accessors can legitimately disagree: a staged-only
    /// change makes `dirty()` report `Clean` (early-exit path skips
    /// the HEAD↔index diff) while `dirty_counts()` reports
    /// `Dirty(Some { staged: 1, .. })`. Don't mix them expecting
    /// agreement.
    #[must_use]
    pub fn dirty_counts(&self) -> Arc<DirtyState> {
        self.dirty_counts
            .get_or_init(|| match &self.repo {
                Some(repo) => Arc::new(compute_dirty_counts(repo).unwrap_or_else(|err| {
                    crate::lsm_warn!("git dirty counts scan failed: {err}");
                    DirtyState::Clean
                })),
                None => Arc::new(DirtyState::Clean),
            })
            .clone()
    }

    /// Pre-populate the counts-mode `dirty_counts` OnceCell with an
    /// explicit value, bypassing the real scan. Returns `Err` when the
    /// cell was already populated.
    pub fn preseed_dirty_counts_state(&self, value: DirtyState) -> Result<(), Arc<DirtyState>> {
        self.dirty_counts.set(Arc::new(value))
    }

    /// Pre-populate the `upstream` OnceCell with an explicit value,
    /// bypassing the real walker. Returns `Err` via
    /// [`OnceCell::set`]'s semantics when the cell was already
    /// populated.
    pub fn preseed_upstream(
        &self,
        value: Option<UpstreamState>,
    ) -> Result<(), Arc<Option<UpstreamState>>> {
        self.upstream.set(Arc::new(value))
    }

    /// Pre-populate the `dirty` OnceCell with an explicit value,
    /// bypassing the real scan. Same `OnceCell::set` semantics as
    /// [`Self::preseed_upstream`].
    pub fn preseed_dirty_state(&self, value: DirtyState) -> Result<(), Arc<DirtyState>> {
        self.dirty.set(Arc::new(value))
    }

    /// Upstream-tracking state, scanned lazily on first access.
    ///
    /// Returns `Arc<None>` in five distinct cases:
    /// 1. HEAD is detached / unborn / an `OtherRef`.
    /// 2. The branch has no tracking upstream configured.
    /// 3. The configured tracking ref has no local object (never
    ///    fetched, or remote pruned).
    /// 4. The repo is shallow — ancestor walks truncate at the
    ///    shallow frontier and would silently undercount.
    /// 5. HEAD and upstream share no merge base (unrelated histories)
    ///    OR `gix` failed partway through (corrupt index, cache open
    ///    failure, ...). In the failure case the cause is written to
    ///    stderr with the `linesmith:` prefix on the first read.
    ///
    /// Cases 1-4 render identically to ahead/behind segments (no
    /// upstream). Case 5 deliberately fuses "walker failed" into "no
    /// upstream" — distinguishing them in the plugin mirror requires
    /// a structured variant (follow-up).
    #[must_use]
    pub fn upstream(&self) -> Arc<Option<UpstreamState>> {
        self.upstream
            .get_or_init(|| match &self.repo {
                Some(repo) => Arc::new(compute_upstream(repo, &self.head).unwrap_or_else(|err| {
                    crate::lsm_warn!("git ahead/behind scan failed: {err}");
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
        dirty_counts: OnceCell::new(),
        upstream: OnceCell::new(),
        repo: Some(repo),
    }))
}

/// Indicator-mode dirty scan: short-circuits on the first status
/// entry. Covers untracked + worktree-vs-index (unstaged) only, since
/// the early-exit `into_index_worktree_iter` skips the HEAD↔index
/// comparison. Staged-only changes therefore don't flip the indicator;
/// [`compute_dirty_counts`] does include them. Keeping indicator mode
/// on the cheaper iterator preserves the <20ms cold-start budget.
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

/// Counts-mode dirty scan: full status walk (no early-exit) over the
/// combined HEAD↔index↔worktree iterator, tallying files per category.
///
/// - `TreeIndex` change → staged (HEAD differs from index)
/// - `IndexWorktree::Modification` → unstaged (index differs from worktree)
/// - `IndexWorktree::DirectoryContents` → untracked
///
/// A file staged AND further modified in the worktree counts toward
/// both `staged` and `unstaged`, matching `git status`'s two-column
/// view. Untracked directories collapse to one entry
/// ([`UntrackedFiles::Collapsed`]) per `git-segments.md`.
/// `index_worktree_rewrites(None)` disables rewrite tracking on the
/// worktree side, so an unstaged rename surfaces as delete + add; a
/// staged rename may still arrive as a single `TreeIndex` change, which
/// counts as one staged file (matching `git status`'s `R` line).
fn compute_dirty_counts(repo: &gix::Repository) -> Result<DirtyState, Box<dyn std::error::Error>> {
    use gix::status::index_worktree::Item as IwItem;
    use gix::status::Item;
    use gix::status::UntrackedFiles;

    let platform = repo
        .status(gix::progress::Discard)?
        .untracked_files(UntrackedFiles::Collapsed)
        .index_worktree_rewrites(None);

    let mut counts = DirtyCounts::default();
    for item in platform.into_iter(Vec::new())? {
        // Per-item errors are best-effort: a single unreadable entry
        // skips that file rather than failing the whole scan closed to
        // `Clean` (which would show a dirty repo as clean — the
        // dangerous direction). Unlike the indicator path's
        // short-circuit, a skip here undercounts, so leave a
        // debug-gated breadcrumb for "count says 2 but I have 3".
        let item = match item {
            Ok(item) => item,
            Err(err) => {
                crate::lsm_debug!("git dirty counts: skipping unreadable status entry: {err}");
                continue;
            }
        };
        match item {
            Item::TreeIndex(_) => counts.staged += 1,
            Item::IndexWorktree(IwItem::Modification { .. }) => counts.unstaged += 1,
            Item::IndexWorktree(IwItem::DirectoryContents { .. }) => counts.untracked += 1,
            // Rewrites are disabled above; ignore defensively if one slips through.
            Item::IndexWorktree(IwItem::Rewrite { .. }) => {}
        }
    }

    if counts == DirtyCounts::default() {
        Ok(DirtyState::Clean)
    } else {
        Ok(DirtyState::Dirty(Some(counts)))
    }
}

/// Resolve the tracking branch for `head` and count its ahead/behind
/// commits relative to HEAD. Returns `Ok(None)` for:
/// - HEAD not on a local branch
/// - no upstream configured on the branch
/// - tracking ref configured but not present locally (never fetched)
/// - shallow clones, where ancestor walks are truncated at the
///   shallow frontier and counts would be wrong
/// - unrelated histories (HEAD and upstream share no merge base)
fn compute_upstream(
    repo: &gix::Repository,
    head: &Head,
) -> Result<Option<UpstreamState>, Box<dyn std::error::Error>> {
    let Head::Branch(_) = head else {
        return Ok(None);
    };

    if repo.is_shallow() {
        return Ok(None);
    }

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
    let upstream_oid = upstream_ref.peel_to_id()?.detach();
    let head_oid = head_ref.id().detach();

    // Explicit merge_base + manual exclusion avoids gix's
    // `with_pruned`: that path's `ByCommitTimeCutoff` sort collides
    // when two commits share a committer-date second, which breaks
    // ahead/behind counts non-deterministically.
    let (ahead, behind) = match repo.merge_base(head_oid, upstream_oid) {
        Ok(base) => {
            let base_oid = base.detach();
            let ahead = count_ancestors_excluding(repo, head_oid, base_oid)?;
            let behind = count_ancestors_excluding(repo, upstream_oid, base_oid)?;
            (ahead, behind)
        }
        // Unrelated histories → hide per spec §Ahead/behind
        // computation. Other merge_base errors (cache open, walker
        // crash) bubble so the outer accessor surfaces them.
        Err(gix::repository::merge_base::Error::NotFound { .. }) => return Ok(None),
        Err(e) => return Err(Box::new(e)),
    };

    let full_name = upstream_ref_name.as_bstr().to_string();
    let upstream_branch = match full_name.strip_prefix("refs/remotes/") {
        Some(short) => short.to_string(),
        None => {
            crate::lsm_warn!(
                "upstream ref {full_name} is outside refs/remotes/; rendering full refname"
            );
            full_name
        }
    };

    Ok(Some(UpstreamState {
        ahead: u32::try_from(ahead).map_err(|_| {
            Box::<dyn std::error::Error>::from(format!("ahead count {ahead} overflows u32"))
        })?,
        behind: u32::try_from(behind).map_err(|_| {
            Box::<dyn std::error::Error>::from(format!("behind count {behind} overflows u32"))
        })?,
        upstream_branch,
    }))
}

/// Count commits reachable from `tip` but not from `stop` (and not
/// `stop` itself). No visited-set needed: `gix::rev_walk` emits
/// each OID at most once per walk.
fn count_ancestors_excluding(
    repo: &gix::Repository,
    tip: gix::ObjectId,
    stop: gix::ObjectId,
) -> Result<usize, Box<dyn std::error::Error>> {
    use std::collections::HashSet;
    if tip == stop {
        return Ok(0);
    }
    let mut excluded: HashSet<gix::ObjectId> = HashSet::new();
    excluded.insert(stop);
    for info in repo.rev_walk([stop]).all()? {
        excluded.insert(info?.id);
    }

    let mut count = 0usize;
    for info in repo.rev_walk([tip]).all()? {
        if !excluded.contains(&info?.id) {
            count += 1;
        }
    }
    Ok(count)
}

fn classify_kind(repo: &gix::Repository) -> RepoKind {
    // gix 0.82 consolidated `Kind::Bare` and `Kind::WorkTree { is_linked: false }`
    // into `Kind::Common`; bare-ness now reads from `Repository::is_bare()`
    // which consults the loaded config rather than the directory layout.
    if repo.is_bare() {
        return RepoKind::Bare;
    }
    match repo.kind() {
        gix::repository::Kind::Common => RepoKind::Main,
        gix::repository::Kind::LinkedWorkTree => {
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

    /// Fabricate the on-disk layout `git worktree add` produces without
    /// shelling out. Primary has a real `.git/`; the worktree checkout has
    /// a `.git` file pointing at `<primary>/.git/worktrees/<name>/`, which
    /// holds the per-worktree `HEAD`, `commondir`, and `gitdir`.
    fn hand_built_linked_worktree(name: &str, primary: &Path, wt_root: &Path) -> PathBuf {
        let primary_git = primary.join(".git");
        fs::create_dir_all(primary_git.join("refs/heads")).expect("mkdir refs/heads");
        fs::create_dir_all(primary_git.join("objects")).expect("mkdir objects");
        fs::write(primary_git.join("HEAD"), "ref: refs/heads/main\n").expect("write primary HEAD");

        let admin_dir = primary_git.join("worktrees").join(name);
        fs::create_dir_all(&admin_dir).expect("mkdir admin");
        let worktree_branch = format!("wt-{name}");
        fs::write(
            admin_dir.join("HEAD"),
            format!("ref: refs/heads/{worktree_branch}\n"),
        )
        .expect("write admin HEAD");
        fs::write(admin_dir.join("commondir"), "../..\n").expect("write commondir");

        let worktree_dir = wt_root.join(name);
        fs::create_dir_all(&worktree_dir).expect("mkdir worktree");
        fs::write(
            admin_dir.join("gitdir"),
            format!("{}\n", worktree_dir.join(".git").display()),
        )
        .expect("write gitdir");

        fs::write(
            worktree_dir.join(".git"),
            format!("gitdir: {}\n", admin_dir.display()),
        )
        .expect("write .git pointer");

        worktree_dir
    }

    #[test]
    fn resolve_repo_classifies_hand_built_linked_worktree() {
        let primary_tmp = TempDir::new().expect("primary");
        let wt_tmp = TempDir::new().expect("wt root");
        let worktree = hand_built_linked_worktree("feat-abc", primary_tmp.path(), wt_tmp.path());

        let ctx = resolve_repo(&worktree).expect("resolve").expect("some");

        let RepoKind::LinkedWorktree { name } = &ctx.repo_kind else {
            panic!("expected LinkedWorktree, got {:?}", ctx.repo_kind);
        };
        assert_eq!(name, "feat-abc");
        assert!(
            ctx.repo_path.ends_with("worktrees/feat-abc"),
            "repo_path should point at the per-worktree admin dir, got {:?}",
            ctx.repo_path
        );
        match &ctx.head {
            Head::Unborn { symbolic_ref } => assert_eq!(
                symbolic_ref, "wt-feat-abc",
                "head must come from the worktree admin HEAD, not the primary's"
            ),
            other => panic!("expected Unborn(wt-feat-abc), got {other:?}"),
        }
    }

    #[test]
    fn classify_kind_returns_basename_for_real_linked_worktree() {
        let primary = TempDir::new().expect("primary");
        let wt_parent = TempDir::new().expect("wt parent");
        run_git_init(primary.path());
        run_git_commit_allow_empty(primary.path(), "seed");
        let worktree_dir = wt_parent.path().join("feat-real-wt");
        run_git(
            primary.path(),
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feat-real-wt",
                worktree_dir.to_str().expect("utf8 path"),
            ],
        );

        let ctx = resolve_repo(&worktree_dir).expect("resolve").expect("some");
        let RepoKind::LinkedWorktree { name } = &ctx.repo_kind else {
            panic!("expected LinkedWorktree, got {:?}", ctx.repo_kind);
        };
        assert_eq!(name, "feat-real-wt");
        match &ctx.head {
            Head::Branch(b) => assert_eq!(b, "feat-real-wt"),
            other => panic!("expected Branch(feat-real-wt), got {other:?}"),
        }
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
        let path = tmp.path();
        // Fixture setup shells out to the `git` binary; fabricating
        // an index + initial commit via gix would take dozens of
        // lines per test. Production code paths stay gix-only.
        run_git_init(path);
        run_git_commit_allow_empty(path, "seed");
        fs::write(path.join("tracked.txt"), "v1").expect("write");
        run_git(path, &["add", "tracked.txt"]);
        run_git_commit(path, "tracked");
        path
    }

    fn run_git_init(path: &Path) {
        use std::process::Command;
        let mut cmd = Command::new("git");
        isolated_git_env(&mut cmd);
        let status = cmd
            .args(["init", "--quiet", "--initial-branch=main"])
            .current_dir(path)
            .status()
            .expect("git init");
        assert!(status.success(), "git init failed in {path:?}");
    }

    fn run_git_init_bare(path: &Path) {
        use std::process::Command;
        let mut cmd = Command::new("git");
        isolated_git_env(&mut cmd);
        let status = cmd
            .args(["init", "--bare", "--quiet", "--initial-branch=main"])
            .current_dir(path)
            .status()
            .expect("git init --bare");
        assert!(status.success(), "git init --bare failed in {path:?}");
    }

    fn run_git_commit_allow_empty(cwd: &Path, msg: &str) {
        use std::process::Command;
        let mut cmd = Command::new("git");
        isolated_git_env(&mut cmd);
        let status = cmd
            .args(["-c", "user.email=t@t", "-c", "user.name=t", "-C"])
            .arg(cwd)
            .args(["commit", "--allow-empty", "-m", msg, "--quiet"])
            .status()
            .expect("git commit");
        assert!(
            status.success(),
            "git commit --allow-empty failed in {cwd:?}"
        );
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
    fn dirty_counts_is_clean_on_committed_repo_with_no_changes() {
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert_eq!(*ctx.dirty_counts(), DirtyState::Clean);
    }

    #[test]
    fn dirty_counts_counts_staged_file() {
        use std::fs;
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        // A brand-new file added to the index differs from HEAD →
        // staged. gix 0.83's HEAD↔index comparison is what makes this
        // visible; gix 0.67 missed it.
        fs::write(path.join("staged.txt"), "new").expect("write");
        run_git(path, &["add", "staged.txt"]);
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert_eq!(
            *ctx.dirty_counts(),
            DirtyState::Dirty(Some(DirtyCounts {
                staged: 1,
                unstaged: 0,
                untracked: 0,
            }))
        );
    }

    #[test]
    fn dirty_counts_counts_unstaged_file() {
        use std::fs;
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        fs::write(path.join("tracked.txt"), "modified").expect("write");
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert_eq!(
            *ctx.dirty_counts(),
            DirtyState::Dirty(Some(DirtyCounts {
                staged: 0,
                unstaged: 1,
                untracked: 0,
            }))
        );
    }

    #[test]
    fn dirty_counts_counts_untracked_file() {
        use std::fs;
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        fs::write(path.join("new.txt"), "hello").expect("write");
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert_eq!(
            *ctx.dirty_counts(),
            DirtyState::Dirty(Some(DirtyCounts {
                staged: 0,
                unstaged: 0,
                untracked: 1,
            }))
        );
    }

    #[test]
    fn dirty_counts_tallies_all_three_categories() {
        use std::fs;
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        // Staged: a new file added to the index.
        fs::write(path.join("staged.txt"), "s").expect("write");
        run_git(path, &["add", "staged.txt"]);
        // Unstaged: modify the committed tracked file without adding.
        fs::write(path.join("tracked.txt"), "modified").expect("write");
        // Untracked: a new file left out of the index.
        fs::write(path.join("untracked.txt"), "u").expect("write");
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert_eq!(
            *ctx.dirty_counts(),
            DirtyState::Dirty(Some(DirtyCounts {
                staged: 1,
                unstaged: 1,
                untracked: 1,
            }))
        );
    }

    #[test]
    fn dirty_counts_is_clean_when_no_gix_repo_held() {
        let ctx = GitContext::new(
            RepoKind::Main,
            PathBuf::from("/tmp/.git"),
            Head::Branch("main".into()),
        );
        assert_eq!(*ctx.dirty_counts(), DirtyState::Clean);
    }

    #[test]
    fn dirty_counts_tallies_same_file_staged_and_modified_in_both_columns() {
        use std::fs;
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        // Stage a change to the committed file, then modify it again in
        // the worktree without re-adding: `git status` shows it in both
        // columns (HEAD↔index AND index↔worktree). The combined scan
        // must emit both items so the file counts toward staged + unstaged.
        fs::write(path.join("tracked.txt"), "staged change").expect("write");
        run_git(path, &["add", "tracked.txt"]);
        fs::write(path.join("tracked.txt"), "further worktree change").expect("write");
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        assert_eq!(
            *ctx.dirty_counts(),
            DirtyState::Dirty(Some(DirtyCounts {
                staged: 1,
                unstaged: 1,
                untracked: 0,
            }))
        );
    }

    #[test]
    fn dirty_and_dirty_counts_use_independent_cells() {
        use std::fs;
        let tmp = TempDir::new().expect("tmp");
        let path = fixture_with_commit(&tmp);
        // Staged-only change plus an untracked file: the untracked file
        // is what the early-exit indicator scan catches.
        fs::write(path.join("staged.txt"), "s").expect("write");
        run_git(path, &["add", "staged.txt"]);
        fs::write(path.join("untracked.txt"), "u").expect("write");
        let ctx = resolve_repo(path).expect("resolve").expect("some");
        // Indicator scan runs first and caches a count-less Dirty(None)
        // in its own cell.
        assert_eq!(*ctx.dirty(), DirtyState::Dirty(None));
        // The counts cell must not inherit that count-less state — it
        // runs its own full scan and sees the staged file too.
        assert_eq!(
            *ctx.dirty_counts(),
            DirtyState::Dirty(Some(DirtyCounts {
                staged: 1,
                unstaged: 0,
                untracked: 1,
            }))
        );
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
        run_git_init_bare(bare);
        run_git_init(path);
        fs::write(path.join("f"), "base").expect("write base");
        run_git(path, &["add", "f"]);
        run_git_commit(path, "base");
        run_git(
            path,
            &["remote", "add", "origin", bare.to_str().expect("utf8 path")],
        );
        run_git(path, &["push", "-u", "origin", "main", "--quiet"]);
        for i in 0..local_commits {
            fs::write(path.join("f"), format!("local-{i}")).expect("write");
            run_git(path, &["add", "f"]);
            run_git_commit(path, &format!("local {i}"));
        }
        // Diverge from the remote side by cloning into a unique
        // TempDir (so parallel tests don't collide), adding commits
        // there, pushing back, and fetching locally.
        if remote_commits > 0 {
            let other_tmp = TempDir::new().expect("other tmp");
            let other = other_tmp.path().join("clone");
            let mut clone_cmd = Command::new("git");
            isolated_git_env(&mut clone_cmd);
            let status = clone_cmd
                .args(["clone", "--quiet"])
                .arg(bare)
                .arg(&other)
                .status()
                .expect("clone");
            assert!(status.success(), "git clone failed");
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

    /// Env vars that neutralize the test host's global / system git
    /// config. A dev with `commit.gpgsign = true`, `core.hooksPath`,
    /// or `safe.directory` denials set globally would otherwise see
    /// spurious fixture failures unrelated to the code under test.
    fn isolated_git_env(cmd: &mut std::process::Command) {
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "commit.gpgsign=false"])
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(["-c", "init.defaultBranch=main"]);
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        use std::process::Command;
        let mut cmd = Command::new("git");
        isolated_git_env(&mut cmd);
        let status = cmd.args(["-C"]).arg(cwd).args(args).status().expect("git");
        assert!(status.success(), "git {args:?} failed in {cwd:?}");
    }

    fn run_git_commit(cwd: &Path, msg: &str) {
        use std::process::Command;
        let mut cmd = Command::new("git");
        isolated_git_env(&mut cmd);
        let status = cmd
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
