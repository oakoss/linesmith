# Git Segments

- Status: draft
- Version: 0.1.1
- Last updated: 2026-04-21
- Driving ADRs: [ADR-0001](../adrs/0001-use-rust-for-runtime.md), [ADR-0003](../adrs/0003-segment-widget-system.md), [ADR-0010](../adrs/0010-data-fetching-architecture.md)

## Overview

`git_branch` is linesmith's git-state segment: branch name, dirty indicator, and ahead/behind counters rendered as one visual unit. This spec defines the rendering contract, config schema, data-fetch strategy, and edge-case handling.

The segment uses `gix` (pure-Rust git implementation) per [ADR-0001](../adrs/0001-use-rust-for-runtime.md)'s stack choice. All git work happens through `gix` — no `git` subprocess calls. A shared `GitContext` populated via `DataContext` lets multiple future git-aware segments reuse a single repo walk per invocation.

This spec does NOT cover: worktree-name rendering inside the `workspace` segment (tracked separately as lsm-sog); future per-segment git widgets like `git_stash`, `git_state` (merge/rebase), `git_branch_group`; or submodule-specific state.

**Coordination with the `workspace` segment:** `workspace` currently reads `ctx.status.workspace.git_worktree` from the stdin passthrough. Once lsm-sog lands, `workspace` switches to reading `GitContext.repo_kind` from this spec's shared `ctx.git()` (so there's one source of truth for worktree name). When both `workspace` and `git_branch` are enabled on a linked worktree, `workspace` renders the worktree name and `git_branch` renders the branch — they intentionally show different things, no suppression logic.

## Requirements

### Functional

- Render the current branch name as the segment's primary text
- Fall back to a short SHA (configurable length) when HEAD is detached
- Render a dirty indicator when staged / unstaged / untracked changes exist (configurable: glyph, show-when-clean, per-state counts)
- Render ahead/behind counters relative to the tracked upstream when one exists (e.g., `↑2 ↓1`); hide when both are zero (configurable: show-zero)
- Hide the entire segment when the current directory is not in a git repository
- Handle linked worktrees (`git worktree add`) — the `.git` file case — by resolving the per-worktree HEAD, not the main checkout's
- Handle bare repositories — hide the segment entirely (no working tree means no meaningful branch/dirty state)
- Populate a shared `GitContext` once per invocation so future git-aware segments don't repeat the gix walk
- Declare `DataDep::Git` so the runtime fetches git state only when at least one git segment is enabled

### Non-functional

- <5ms warm (repo already in OS page cache); <15ms cold (first fetch of the invocation). Both must fit within the <20ms overall cold-start budget
- Caching via DataContext's OnceCell pattern — gix walk runs at most once per invocation regardless of how many git segments are enabled
- No allocations on the steady-state render path beyond the output `String`
- Graceful on large repos: avoid full-repo `git status` scans when possible; prefer `gix`'s diffless dirty detection
- Cross-platform: paths with backslashes on Windows, case-insensitive filesystems, line-ending normalization. Reuse gix's native handling; don't re-invent

## Interface / Contract

### Config schema

```toml
[segments.git_branch]
enabled = true
icon = ""                      # optional prefix (Nerd Font glyph or text); default empty
label = ""                     # optional label; default empty (branch name stands alone)
max_length = 40                # truncate branch name beyond this cell count (min 1)
truncation_marker = "…"        # appended when truncated
short_sha_length = 7           # characters for detached-HEAD short SHA (1..=40)

[segments.git_branch.dirty]
enabled = true                 # include dirty indicator
format = "indicator"           # "indicator" | "counts" | "hidden"
indicator = "*"                # when format = "indicator" and dirty
clean_indicator = ""           # when format = "indicator" and clean; "" hides
staged_icon = "+"              # when format = "counts": "+3 ~2 ?1"
unstaged_icon = "~"
untracked_icon = "?"
count_hide_zero = true         # in "counts" mode, hide a category when its count is zero

[segments.git_branch.ahead_behind]
enabled = true                 # include ahead/behind counters
ahead_format = "↑{n}"
behind_format = "↓{n}"
hide_when_zero = true          # hide ahead/behind entirely when both are 0
hide_when_no_upstream = true   # hide when branch has no tracked upstream (true = hide; false = render "?")
```

Branch-rendering keys (`max_length`, `truncation_marker`,
`short_sha_length`) live at the top level of `[segments.git_branch]`
rather than a nested `[.branch]` table. Flattening matches the
implementation's `Config` shape and keeps the simple knobs one level
shallower. Nested tables (`dirty`, `ahead_behind`) remain nested
because they group multiple related settings. `detached_style` is
deferred to a follow-up (tag-aware rendering is not in v0.1.1).

Rendered examples, default config:

```text
main                  # clean, no upstream
main *                # clean → dirty
feature/auth *  ↑2    # dirty + ahead of upstream
feature/auth *  ↑2 ↓1 # dirty + both ahead and behind
(a3f9b72)             # detached HEAD (short SHA)
main +3 ~2 ?1         # counts mode
```

### Data dependency

`git_branch` declares:

```rust
fn data_deps(&self) -> &'static [DataDep] {
    &[DataDep::Git]
}
```

`DataDep::Git` is a new variant added to the enum in [data-fetching.md](data-fetching.md) at the same time this spec is implemented. `DataContext.git()` returns `Arc<Result<Option<GitContext>, GitError>>`:

- `Ok(None)` — cwd is not in a git repo; segment hides
- `Ok(Some(gc))` — repo found, `gc` populated
- `Err(e)` — gix failed (corrupt repo, permission denied, `safe.directory` trust rejection, &c); segment hides. The error `Display` is written to stderr with the `linesmith:` prefix so a user running from a terminal sees the cause. A future render mode can surface a `[git error]` marker once the structured logger (lsm-cgg) lets segments opt into inline error messaging

Multiple future git segments share the same `Arc<GitContext>` without re-walking the repo.

### `GitContext` type

```rust
pub struct GitContext {
    /// Which repo we opened: the main checkout, a linked worktree, or a bare repo.
    pub repo_kind: RepoKind,

    /// Absolute path to the repository directory. `.git` dir for main checkouts,
    /// the `.git/worktrees/<name>/` dir for linked worktrees, the bare repo path itself.
    pub repo_path: PathBuf,

    /// Resolved HEAD.
    pub head: Head,

    /// Dirty state, populated lazily on first access. Dirty scans can be
    /// expensive on large repos; gated behind a closure so segments that
    /// don't render dirty (e.g., a future branch-only segment) skip the cost.
    pub dirty: OnceCell<Arc<DirtyState>>,

    /// Upstream relationship, populated lazily.
    pub upstream: OnceCell<Arc<Option<UpstreamState>>>,
}

pub enum RepoKind {
    Main,
    LinkedWorktree { name: String },
    Bare,
}

pub enum Head {
    Branch(String),           // "main", "feature/auth", etc.
    Detached(ObjectId),       // short-SHA rendered by the segment
    /// Fresh init, no commits yet. The inner string is the HEAD
    /// symbolic-ref target — `init.defaultBranch` or whatever
    /// `refs/heads/NAME` HEAD points at. Segments render this name
    /// so users see `main` or `master` instead of a blank.
    Unborn { symbolic_ref: String },
}

pub struct DirtyState {
    pub staged: usize,
    pub unstaged: usize,
    pub untracked: usize,
    pub any: bool,            // true iff staged + unstaged + untracked > 0
}

pub struct UpstreamState {
    pub ahead: u32,
    pub behind: u32,
    pub upstream_branch: String,   // "origin/main" etc.
}

pub enum GitError {
    CorruptRepo { path: PathBuf, message: String },
    WalkFailed  { path: PathBuf, message: String },
}
```

**Note on `message: String`:** the implementation stringifies the
underlying `gix` cause at the error-construction boundary so `GitError`
stays `Clone + PartialEq + Eq`. `DataContext` memoizes git state as
`Arc<Result<Option<GitContext>, GitError>>`, and that `Arc<Result<...>>`
boundary requires `Clone`. Trade-off: the structured source chain
terminates at the string. No statusline render path branches on inner
`gix` variants, so no behavior is lost.

Why `OnceCell` inside `GitContext` on top of `OnceCell` in `DataContext`: two tiers of laziness. The outer layer decides whether to open the repo at all; the inner layer decides whether to pay for a dirty scan. Even inside a git repo, a config rendering only branch + ahead/behind skips the dirty walk entirely.

### Repo discovery

```rust
fn resolve_repo(cwd: &Path) -> Result<Option<GitContext>, GitError>;
```

1. `gix::discover(cwd)` walks up from `cwd` looking for `.git`. Returns the repository on success, `None` if not in a repo.
2. Distinguish `RepoKind`:
   - `.git/` is a directory → `RepoKind::Main`
   - `.git` is a file containing `gitdir: ...` → `RepoKind::LinkedWorktree { name: <dir-name-of-gitdir> }`
   - The discovered path has no worktree (bare) → `RepoKind::Bare`
3. For bare repos, return `Ok(Some(GitContext { repo_kind: Bare, head: Unborn, ... }))` with empty dirty/upstream cells. The `git_branch` segment will hide on `repo_kind == Bare`.

### Render semantics

```rust
fn render(&self, ctx: &DataContext) -> RenderResult {
    match &*ctx.git() {
        // Hide on error; cause has already been logged to stderr by
        // the data-layer scan (see §Data dependency).
        Err(_) | Ok(None) => Ok(None),
        Ok(Some(gc)) if matches!(gc.repo_kind, RepoKind::Bare) => Ok(None),
        Ok(Some(gc)) => {
            let parts = self.assemble(gc);              // branch | dirty | ahead_behind
            Ok(Some(parts.into_rendered_segment()))
        }
    }
}
```

Render output is a single `RenderedSegment` with multiple `StyledRun`s so each part takes its own color role: branch name in `git.branch`, dirty marker in `git.dirty`, ahead/behind counters in `git.ahead` / `git.behind`. Roles resolve via [theming.md](theming.md).

**Implementation prerequisite:** the multi-run rendering shape requires [segment-system.md](segment-system.md) v0.3's `RenderedSegment { runs: Vec<StyledRun>, ... }` to land in code. The current crate scaffolding (`crates/linesmith/src/segments/mod.rs`) still carries the earlier single-`style` shape. If multi-run hasn't landed when `git_branch` is implemented, fall back to one role for the whole composite (the segment's default role) and file a follow-up to finish the multi-run migration.

## Behavior

### Fetch ordering

1. Runtime sees `DataDep::Git` in the enabled-segments union → calls `ctx.git()` during prefetch.
2. `resolve_repo(cwd)` runs once, stored in `DataContext.git` OnceCell.
3. Render call hits the cached `Arc<GitContext>` instantly.
4. Inside render, if dirty info is needed, `gc.dirty.get_or_init(...)` runs the dirty walk. This is what the spec's <5ms warm / <15ms cold budgets are measured against.

### Dirty detection strategy

- Prefer `gix::Repository::status_platform()` with `.untracked_files(Untracked::Collapsed)` — it's the diffless fast path
- Staged = index entries that differ from HEAD
- Unstaged = index entries that differ from the working tree
- Untracked = working-tree paths absent from the index and not `.gitignore`d
- Abort the scan early (`gix` supports this) once `any` is determined in "indicator" mode — no need to count every file
- In "counts" mode, full scan is required; consider a TTL cache in a later rev if users report slowness

### Ahead/behind computation

- If `head` is `Detached` or `Unborn`: no upstream exists; `upstream` OnceCell resolves to `Arc::new(None)`. Segment behavior below applies.
- If `head` is `Branch`, resolve the upstream ref for the current branch via gix's upstream lookup. On success, use gix's ahead/behind walker with the HEAD and upstream tips and store the result as `Some(UpstreamState { ahead, behind, upstream_branch })`. On missing upstream, store `None`.
- Segment rendering then depends on config:
  - Upstream present, `ahead == 0 && behind == 0`: hidden if `hide_when_zero = true`, otherwise rendered as `↑0 ↓0`
  - Upstream present, non-zero: rendered per `ahead_format` / `behind_format`
  - Upstream absent (`Arc<None>`) AND `hide_when_no_upstream = true`: ahead/behind part omitted from the segment output
  - Upstream absent AND `hide_when_no_upstream = false`: render the literal fallback marker `?` (i.e., `main *  ?`) so the user sees the branch has no tracking configured

The data model distinguishes "upstream absent" (`Arc<None>`) from "upstream not yet computed" (`OnceCell` empty) so `hide_when_no_upstream = false` can render the distinct `?` marker; both states are reachable and represent different things.

### Truncation

- `max_length` is cell count (not byte count). Use the same grapheme-aware width routine as the rest of the layout engine ([segment-system.md](segment-system.md) §Layout intent).
- When the branch name alone exceeds `max_length`, truncate from the middle (keep prefix + suffix) with `truncation_marker` between them. Middle truncation preserves both the `feature/` prefix and the ticket ID suffix, which users rely on.

### Color roles

Roles this segment may set on its styled runs (resolved via [theming.md](theming.md)):

| Role                  | When                                                      |
| --------------------- | --------------------------------------------------------- |
| `git.branch`          | The branch-name run (normal state)                        |
| `git.branch.detached` | The short-SHA run when HEAD is detached                   |
| `git.dirty`           | The dirty indicator/counts run                            |
| `git.clean`           | The clean indicator (when `clean_indicator` is non-empty) |
| `git.ahead`           | The ahead counter run                                     |
| `git.behind`          | The behind counter run                                    |

Themes that don't define these roles fall back to the theme's default text color.

## Edge cases

| Case                                                     | Handling                                                                                                                                                             |
| -------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| cwd outside any git repo                                 | `ctx.git()` → `Ok(None)`; segment hides                                                                                                                              |
| Bare repo                                                | `Ok(Some(gc))` with `repo_kind: Bare`; segment hides (no working tree to describe)                                                                                   |
| Linked worktree (`.git` is a file)                       | `repo_kind: LinkedWorktree { name }`; HEAD/dirty/upstream resolved for the worktree, NOT the main checkout                                                           |
| Detached HEAD                                            | `head: Detached(oid)`; segment renders `(oid.short())` wrapped per config                                                                                            |
| Unborn branch (fresh `git init`, no commits)             | `head: Unborn`; segment renders the HEAD symbolic-ref target (whatever `init.defaultBranch` resolved to — `main`, `master`, etc.) with no dirty/ahead-behind affixes |
| Branch name longer than `max_length`                     | Middle-truncate per config                                                                                                                                           |
| No upstream configured                                   | ahead/behind hidden when `hide_when_no_upstream = true`; renders `?` when false                                                                                      |
| Very large repo (dirty scan >100ms)                      | The dirty `OnceCell` still only runs once per invocation; for repeated invocations across sessions, add a future TTL cache (filed separately)                        |
| Corrupt `.git/index`                                     | `GitError::WalkFailed`; segment hides and writes the cause to stderr                                                                                                 |
| Permission denied on `.git/`                             | `GitError::CorruptRepo`; same rendering and stderr behavior                                                                                                          |
| `safe.directory` trust rejection                         | `GitError::CorruptRepo`; hidden, cause on stderr so the user can run `git config --global --add safe.directory ...`                                                  |
| Submodules                                               | `RepoKind::Submodule`; segment renders the submodule's branch / dirty state like any other checkout                                                                  |
| HEAD points at a non-`refs/heads/` ref                   | `Head::OtherRef { full_name }`; segment renders the full refname (middle-truncated)                                                                                  |
| Packed refs only (no loose refs)                         | gix handles transparently; no special case in this spec                                                                                                              |
| Merge/rebase/bisect in progress                          | Out of scope for v0.1 (future `git_state` segment); `git_branch` renders the HEAD branch unchanged                                                                   |
| Windows case-insensitive filesystem                      | gix normalizes per platform; segment sees the canonical form                                                                                                         |
| Symlinked cwd pointing into a repo                       | gix `discover` follows symlinks; repo found as expected                                                                                                              |
| Repo inside a repo (e.g., vendored dep)                  | `discover` returns the innermost repo. Same as git CLI behavior                                                                                                      |
| `gix` discover exceeds ceiling (e.g., `.git` 50 dirs up) | `gix` returns `None`; segment hides. Default ceiling is well above typical layouts                                                                                   |

## Testing strategy

Follows `AGENTS.md`: inline `#[cfg(test)] mod tests` for unit tests, `tests/` for integration, `insta` for snapshots. Fixtures live under `crates/linesmith/tests/fixtures/git/` generated via `gix::init()` and direct object-database manipulation (no shelling out to the `git` binary).

### Unit tests (inline)

- `resolve_repo` returns `Ok(None)` for a non-repo directory
- `resolve_repo` distinguishes `Main` / `LinkedWorktree` / `Bare`
- Truncation: `max_length = 10` on `"feature/authentication-v3"` middle-truncates correctly with grapheme-aware width
- Dirty-indicator mode short-circuits after first dirty file detected
- Ahead/behind with no upstream returns `None`
- Detached HEAD renders short SHA at configured length
- Clean indicator rendering (configured and empty string)

### Integration tests (`tests/git_branch.rs`)

Fixture scenarios under `tests/fixtures/git/`:

- `clean/` — clean repo on `main`, no upstream, no changes
- `dirty-staged/` — one staged file
- `dirty-unstaged/` — one unstaged file
- `dirty-untracked/` — one untracked file
- `dirty-all/` — staged + unstaged + untracked
- `ahead-2/` — two commits ahead of `origin/main`
- `behind-3/` — three commits behind
- `ahead-behind/` — both
- `detached/` — detached HEAD on a specific commit
- `unborn/` — fresh `git init` with no commits
- `worktree-linked/` — main checkout + one linked worktree, tests exercise the worktree's cwd
- `bare/` — bare repo, segment hides

Each fixture runs the full render pipeline (stdin → config → segment render → output) and snapshots the output with `insta`.

### Snapshot tests

- Each fixture × each render mode (indicator / counts) × each config preset (default, minimal, verbose)
- Terminal-width matrix: 40 / 80 / 120 / 200 cells to exercise truncation

### Benchmarks (criterion)

- Cold fetch: fresh repo, new DataContext, assert <15ms p95
- Warm fetch: same invocation, second segment asks for `ctx.git()`, assert effectively zero-cost (same Arc returned)
- Large-repo dirty scan: 10k-file fixture, indicator mode, assert <5ms p95 (short-circuit proves itself)

## Open questions

- **Dirty-mode caching across invocations.** The per-invocation `OnceCell` gives warm performance within a single render, but a long-lived session on a large repo will still pay the cold cost on every prompt. Revisit with a TTL or file-watch-based cache when measurements show a problem.
- **Tag resolution on detached HEAD.** `detached_style = "sha_with_tag"` is in the config schema but not required for v0.1. gix supports walking back to find the nearest tag; a tag lookup adds ~ms of work. Defer to a follow-up bead.
- **Merge/rebase/bisect state display.** `git_state` segment is out of scope for v0.1; `git_branch` renders HEAD's branch-name unchanged during an in-progress operation. Future segment spec to cover.
- **Stash count.** `git_stash` segment is out of scope for v0.1; add a follow-up spec when demand signals it.

## Change log

- 2026-04-21 (v0.1.1): several reconciliations between the v0.1
  draft and the shipped lsm-4cf implementation:
  - `GitError` variants carry `message: String` instead of
    structured `cause: gix::open::Error` / `gix::revwalk::Error`.
    `DataContext` memoizes git state as
    `Arc<Result<Option<GitContext>, GitError>>`, and that boundary
    requires `Clone + PartialEq + Eq`. Structured source chain is
    the only thing lost.
  - `Err(_)` in the segment render path **hides** rather than
    emitting a `[git error]` marker. The cause is written to stderr
    (the existing `linesmith:` diagnostic channel). A future render
    mode can opt back into inline error text once the structured
    logger (lsm-cgg) lands.
  - Branch-rendering knobs (`max_length`, `truncation_marker`,
    `short_sha_length`) moved from the nested `[segments.git_branch.branch]`
    table up to `[segments.git_branch]` directly. The dirty /
    ahead_behind sub-tables remain nested.
  - `RepoKind` gains a `Submodule` variant so submodule checkouts
    can be styled distinctly later without re-classification; today
    the segment renders them identically to `Main`.
  - `Head` gains an `OtherRef { full_name }` variant for HEADs
    pointing outside `refs/heads/` (remote-tracking branches, tags,
    etc.). `Head::Branch` now strictly holds short local-branch
    names.
  - `DirtyState` is now an enum (`Clean` |
    `Dirty(Option<DirtyCounts>)`) instead of a flat struct, so the
    fast-path "dirty but counts not computed" state is explicit
    rather than encoded in a bool+zero-counts denormalization.
  - The dirty scan now includes untracked files via
    `status().untracked_files(Collapsed)`. gix 0.67's own
    `Repository::is_dirty()` is not used because it excludes
    untracked files and (per its own TODO) doesn't compare HEAD to
    the index. HEAD↔index (staged-only) detection is still missing;
    tracked in lsm-u5h.
  - `resolve_repo` now matches `gix::discover::upwards::Error` inner
    variants. Only the three genuine "no repo here" kinds
    (`NoGitRepository*`) become `Ok(None)`; trust rejections
    (`safe.directory`), ceiling-dir misconfig, and permission errors
    all become `GitError::CorruptRepo` so the user sees the cause
    on stderr.
- 2026-04-19: initial draft (v0.1). Defines the `git_branch` segment's rendering contract (branch + dirty + ahead/behind as one visual unit), config schema, `GitContext` data type populated via `DataContext::git()` / `DataDep::Git`, repo discovery via `gix::discover`, worktree-aware resolution, edge-case taxonomy, and integration test plan. Driven by ADR-0001 (gix), ADR-0003 (segment system), ADR-0010 (data-fetching).
