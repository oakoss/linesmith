# linesmith

A Rust status line tool for Claude Code (and other AI coding CLIs) with plugin API, role-based themes, and correctness-first context/rate-limit/worktree handling.

**Status:** Active development (v0.2.x). Segment system, role-based theming, TUI config editor, `doctor` diagnostics, rhai plugin runtime, and cargo-dist multi-platform builds all ship.

> **AGENTS.md is the source of truth for AI agent instructions in this project.** `CLAUDE.md` contains only `@AGENTS.md` to import this file. Do not edit `CLAUDE.md` directly or duplicate content across both files. Agents that read `AGENTS.md` (Codex, OpenAI Codex CLI, Cursor, etc.) and Claude Code (via the `@` import) see the same content.

## Pipeline

```text
research  →  ideas  →  ADRs  →  specs  →  beads
(what is)  (what if)  (what we)  (how we)  (who does)
                      (will do)  (build)   (what)
```

- **Research** captures surveys, deep dives, and competitive analysis (see `docs/research/`)
- **Ideas** explore possibilities (see `docs/ideas/`; promoted ideas move to `docs/ideas/archived/`)
- **ADRs** resolve questions in MADR v4.0 format (see `docs/adrs/`, immutable once accepted)
- **Specs** formalize decisions into implementation contracts (see `docs/specs/`)
- **Beads** tracks execution work (`bd ready`)

See `docs/README.md` for the full pipeline description and promotion rules.

## Docs Workflow

### When to write which doc

| Trigger                                                                  | Doc type        | Location                              |
| ------------------------------------------------------------------------ | --------------- | ------------------------------------- |
| Significant research session (surveys, deep dives, competitive analysis) | Research note   | `docs/research/{descriptive-name}.md` |
| Exploratory "what if we did X?" thought                                  | Idea            | `docs/ideas/NNNN-{kebab-name}.md`     |
| Any architectural decision that shapes implementation                    | ADR (MADR v4.0) | `docs/adrs/NNNN-{kebab-name}.md`      |
| Implementation contract for a feature area                               | Spec            | `docs/specs/{feature-area}.md`        |

### Mechanics

- **Start from the template.** Copy `0000-template.md` in the target folder, rename it, fill it in. Do not write from scratch.
- **Numbering.** ADRs and ideas use zero-padded 4-digit prefixes (`0001-`, `0002-`, ...); pick the next available number. Research docs use descriptive names. Specs use feature-area names.
- **Cross-link.** ADRs cite their driving research docs and list related ADRs. Specs cite their driving ADRs. Research docs cite the ADRs they should drive.

### Promotion flow

- **Idea → ADR.** When an idea hardens into a decision, create the ADR, then move the original idea to `docs/ideas/archived/` and update its `Promoted to:` field with the ADR link.
- **ADR → Spec.** Once an ADR is accepted, any feature area it governs gets a spec.
- **Spec → Beads.** A spec is the contract; beads issues are the execution. Create an epic per spec, tasks per deliverable.

### ADR immutability

**Accepted ADRs are immutable.** If the decision changes, write a new ADR with status `accepted` and update the old one's status to `superseded by [ADR-NNNN]`. This preserves the reasoning trail. Do not rewrite an accepted ADR in place.

**Supersede vs. amend.** Supersession replaces a whole decision. When a later ADR revises only part of one — a struct shape, a single clause — while the rest stands, it **amends** instead: the new ADR carries an `Amends: [ADR-NNNN] — <what changed, what stands>` line and the old one gains a matching `Amended by:` line. Adding that pointer is the only edit an accepted ADR ever takes, and it is the same kind of edit supersession already requires; the body stays untouched. A backlink is permitted metadata, not a rewrite, and does not violate immutability. ADR-0011 carries pointers from both ADR-0013 and ADR-0030 this way.

## Tooling

Managed by [mise](https://mise.jdx.dev/). Run `mise install` to get all tools.

| Command                           | Purpose                                     |
| --------------------------------- | ------------------------------------------- |
| `mise run check`                  | All checks (fmt + lint + Rust)              |
| `mise run test`                   | All tests                                   |
| `mise run bench`                  | All benchmarks                              |
| `mise run fmt`                    | Format non-Rust files (prettier)            |
| `mise run lint`                   | Lint markdown (markdownlint-cli2)           |
| `cargo fmt`                       | Format Rust                                 |
| `cargo clippy`                    | Rust linting                                |
| `knope document-change`           | Author a changeset file under `.changeset/` |
| `knope prepare-release --dry-run` | Preview the next release PR's diff locally  |

Git hooks installed via `lefthook install`. `pre-commit` runs fmt/lint on staged files; `commit-msg` verifies conventional commit format via `cog`.

### Testing against a live statusline

Point `~/.claude/settings.json`'s `statusLine.command` at
`target/debug/linesmith` and rebuild with `cargo build -p linesmith`.
Debug costs ~15ms per render against ~150ms dominated by I/O (endpoint,
JSONL aggregation, git), so a release build is not worth the compile
wait while iterating. Back up the file first — it is outside the repo and
nothing else records what it pointed at.

**Isolate the cache.** `XDG_CACHE_HOME=/tmp/lsm-test ./target/debug/linesmith`
keeps test runs off `~/.cache/linesmith/usage.json`, which the real
statusline is reading. Editing or deleting that file to force a code path
perturbs the thing you are trying to observe.

**Do not loop renders against `/api/oauth/usage`.** It rate-limits
aggressively, and its cooldown outlasts the 30s lock file, so every retry
returns 429 and pushes `blocked_until` further out. A handful of rapid
invocations locks the account out for the better part of an hour; the
cascade then falls back to JSONL and the whole line switches from
percentages to raw token counts (`~5h: 384M`), with the reset segments
hiding because a rolling window has no hard reset. That is spec'd
behavior, not breakage — see [ADR-0013](docs/adrs/0013-jsonl-fallback-carries-token-counts.md).

**Segment not appearing after a change? Read `cached_at` first.** A cache
written by an older binary deserializes new typed fields to `None`, so the
segment hides until the 180s TTL expires
([ADR-0030](docs/adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md)
§Decision Outcome). Two separate debugging detours during `lsm-zgju`
started by mistaking this for a defect. `LINESMITH_LOG=debug` prints the
hide reason per segment and is the fastest way to tell these apart.

### Releases

Knope drives release automation per [ADR-0027](docs/adrs/0027-knope-for-release-automation.md) — per-crate versioning, per-crate CHANGELOG, cross-manifest dep-pin updates by name. Release-PR merge fires `knope-release.yml`, which tags each bumped package and creates its GitHub Release; each `<crate>/v*` tag push then fires its own `knope-release.yml` run that publishes to crates.io (it can't ride the merge run — crates.io rejects `pull_request_target`); the `linesmith/v*` tag additionally fires cargo-dist's `release.yml` for binary builds, which the library tags deliberately don't. Expect several runs per release — one publish run per bumped crate, some of which may report as cancelled rather than failed once three or more tags land, since each run publishes the whole workspace and the `knope-publish` group keeps only one pending. Exact counts are in `docs/ops/release-runbook.md`. A `verify-published` job fails the release if the registry doesn't catch up. Full contract in `docs/specs/release-process.md`; day-of-release steps in `docs/ops/release-runbook.md`.

Drop a changeset file via `knope document-change` when the conventional-commit subject doesn't fully capture release impact (e.g. a `refactor:` that's actually breaking, or a `feat:` whose per-package effect isn't obvious from the scope). Conventional commits remain the primary signal; changesets supplement.

## Non-Interactive Shell Commands

**ALWAYS use non-interactive flags** with file operations to avoid hanging on confirmation prompts. Shell commands like `cp`, `mv`, and `rm` may be aliased to `-i` (interactive) mode, causing agents to hang waiting for y/n input.

```bash
# Force overwrite without prompting
cp -f source dest           # NOT: cp source dest
mv -f source dest           # NOT: mv source dest
rm -f file                  # NOT: rm file

# For recursive operations
rm -rf directory            # NOT: rm -r directory
cp -rf source dest          # NOT: cp -r source dest
```

**Other commands that may prompt:**

- `scp` — use `-o BatchMode=yes`
- `ssh` — use `-o BatchMode=yes`
- `apt-get` — use `-y` flag
- `brew` — use `HOMEBREW_NO_AUTO_UPDATE=1` env var

## Architecture

Detailed architecture lives in `docs/adrs/` and `docs/specs/`. High-level:

- **Runtime:** Rust single binary (~3-5MB stripped), <20ms cold start target
- **Segment/widget system:** priority, width hints, conditional visibility, caching, async, composition
- **Plugin runtime:** rhai embedded scripting (sub-ms init, sandboxed, pure Rust)
- **Theming:** role-based semantic colors (Catppuccin-compatible)
- **Input schema:** tool-agnostic union of Claude Code + Qwen Code fields with per-tool normalizers
- **Distribution:** cargo-dist for multi-platform binaries (macOS/Linux/Windows)

See `docs/adrs/0001-use-rust-for-runtime.md` through `0007-cargo-dist-distribution.md` for decision rationale.

## Task Tracking

Beads (`bd`) tracks all implementation work. Issue prefix: `lsm-`.

- **`bd ready`** before starting — claim work, check for blockers
- **`bd update <id> --claim`** when starting an issue
- **`bd close <id> --reason="..."`** when completing
- **`bd remember "insight"`** for persistent knowledge across sessions (search with `bd memories <keyword>`)
- **`bd prime`** for full workflow context and command reference
- **TodoWrite / TaskCreate** — fine for in-session progress tracking; mirror anything new into beads at session end (see "Session Completion").

## Worktree Workflow

Background Claude Code sessions isolate code edits into a git worktree under `.claude/worktrees/` (the `worktree.bgIsolation` default). `.claude/settings.json` sets `worktree.baseRef = "head"` so worktrees carry unpushed local commits — no missing state when syncing back.

### Worktree vs. plain branch

Not every change needs a worktree. Pick by size and concurrency:

- **Small, single-track change** (a doc fix, a one-liner, a tightly-scoped edit) — branch off `main` (`git switch -c <branch> main` creates it from `main` regardless of what's checked out; refresh `main` first if it's stale with `git switch main && git pull --ff-only`), commit, push, PR. Worktree setup/cleanup overhead and the footguns below aren't worth it. Exception: a **background** session code-file edit (Rust/JSON/TOML) trips the bgIsolation guard regardless of size and is forced into a worktree; doc-only edits and interactive sessions branch freely.
- **Big work item and/or parallel work** (a substantial bead, or several beads in flight at once) — use a worktree so isolated checkouts don't collide. The trigger is concurrency or churn across the checkout; the worktree-specific subsections below (Naming convention onward) cover that path.

Either way every change ships through a PR (`main` is protected); the only choice is worktree vs. plain branch.

On the plain-branch path:

- **Name** the branch after the bead (`git switch -c lsm-xyz main`) for beaded work, or a short kebab descriptor (`git switch -c docs-branch-policy main`) for ad-hoc work. Unlike `EnterWorktree`, `git switch -c` takes your name verbatim — no `worktree-` prefix is added.
- **Claim** the bead once you're on the branch (`bd update lsm-xyz --claim`). Issue state lives in the bd database rather than a tracked file, so the claim isn't tied to a branch and needs no staging. Non-beaded changes (most doc fixes) skip the claim.
- **Cleanup** is `git switch main && git branch -D <branch>` once the PR merges — no `git worktree remove`, so the plain-branch path skips this workflow's most dangerous step and its data-loss footguns.

### When the bgIsolation guard fires

The harness only blocks code-file edits (Rust, JSON, TOML); markdown / doc-only edits pass through to the main checkout without triggering:

> This background session hasn't isolated its changes yet. Call EnterWorktree first so edits land in a worktree instead of the shared checkout.

Use `EnterWorktree(name: <bd-id>)` to satisfy the guard when it fires (a code edit in a background session). The guard is a backstop for background isolation, not a mandate to worktree every edit — for small or doc-only changes that don't trip it, prefer a plain branch per **Worktree vs. plain branch** above.

### Naming convention

One bead, one worktree, one branch — use the bare bead ID as the name:

```text
EnterWorktree(name: "lsm-hhb")
```

Creates `.claude/worktrees/lsm-hhb/` on branch `worktree-lsm-hhb`. Short, scannable in `git worktree list`, maps back to `bd show lsm-hhb`. The bead title is the descriptor; don't repeat it in the name. For ad-hoc non-beaded work, use a short kebab descriptor **without** the `worktree-` prefix (e.g., `workflow-docs`, not `worktree-workflow-docs`) — the harness adds the prefix to the branch name automatically, so a `worktree-`-prefixed input produces `worktree-worktree-<name>`.

From the shell, `claude --worktree <bd-id>` is the user-driven equivalent of `EnterWorktree(name: <bd-id>)` — same `.claude/worktrees/<bd-id>/` path, same `worktree-<bd-id>` branch.

### Bd flow inside the worktree

The bd database auto-shares across worktrees via git-common-dir discovery — every worktree sees the same DB as main without manual setup. Issue state lives in the database and replicates through the Dolt remote, not through any file in the tree, so claiming and closing produce no diff to stage.

```bash
# Inside the worktree:
bd update lsm-xyz --claim
# ... edit, test, review-cycle ...
bd close lsm-xyz --reason="<one-line summary of what shipped>"
git add <files>
git commit -m "feat(scope): subject

lsm-xyz"
git push -u origin worktree-lsm-xyz

# Open a PR; squash is the only merge method enabled on the repo:
gh pr create --title "feat(scope): subject" --body "$(cat <<'EOF'
## Summary
- ...
EOF
)"

# CodeRabbit posts a commit status as well as a PR review; `--watch`
# blocks on that status alongside CI.
gh pr checks --watch

# `--watch` only waits on checks present when it starts, and CodeRabbit's
# status lands ~20s after the push, so absence must fail rather than read
# as approval. `jq -e` exits 1 when the status is missing or not yet
# green. A green status can still mean "review skipped" — read the
# findings below before trusting it.
gh pr checks --json name,state |
  jq -e 'any(.[]; .name == "CodeRabbit" and .state == "SUCCESS")'

# CodeRabbit's findings are inline review comments, which `gh pr view
# --comments` doesn't print; it only shows the walkthrough and review
# bodies. The API call below is what surfaces the findings themselves.
gh pr view --comments
gh api --paginate "repos/{owner}/{repo}/pulls/$(gh pr view --json number -q .number)/comments" \
  --jq '.[] | "\(.path):\(.line // .original_line)\n\(.body)\n"'

# Act on the findings, then resolve their threads: the repo ruleset sets
# `required_review_thread_resolution`, so an unresolved thread blocks the
# merge. A PR with zero findings resolves trivially, which is why the
# assertion above still carries the "did the review run" half.
gh pr merge --squash               # remote branch auto-deletes (delete_branch_on_merge=true); local cleanup below
```

Exit the worktree via the Claude Code tool (`ExitWorktree(action: "keep")`) so the branch + dir stay on disk for cleanup. Then back in main:

```bash
git pull --ff-only origin main
git fetch --prune origin           # prune the now-deleted remote tracking ref
git worktree remove .claude/worktrees/lsm-xyz
# Squash merge creates a new commit on main; the worktree branch is NOT an
# ancestor of it, so `git branch -d` will refuse. Use -D after confirming the
# PR landed (via `gh pr view <N> --json mergedAt` or the gh output above).
git branch -D worktree-lsm-xyz
```

The PR path honors the repo's branch protection (`Changes must be made through a pull request`), the required `CI Summary` check, and `required_review_thread_resolution` — pushing straight to main bypasses all three. Thread resolution is set on the repo ruleset while the org ruleset leaves it off; GitHub applies the stricter of the two, so an unresolved review thread blocks merge here even though the org rule alone wouldn't. Squash collapses any review-cycle iter-commits into one clean main commit; the squash commit body is built from concatenated commit messages (`squash_merge_commit_message: COMMIT_MESSAGES`), so the `lsm-xyz` footer in your worktree commit lands in the squash commit naturally — no PR-body workaround needed.

**AI review on PRs is CodeRabbit.** It reviews automatically on PR open and on each push, reads this file as review guidance, and reports a `CodeRabbit` commit status alongside the CI checks. `@coderabbitai review` re-runs it on demand. Local pre-push review (`/review-cycle:review`, `/code-review`) is separate and still the first line — CodeRabbit is the backstop for what reaches a PR unreviewed.

**Don't pass `--auto` to `gh pr merge`.** Native auto-merge waits only on _required_ status checks, and the repo requires just `CI Summary`. CodeRabbit's status isn't required, so `--auto` can land a PR while the review is still queued. `gh pr checks --watch` blocks on every reported check including that status, which is why the manual sequence above waits on it and then merges. That stays true until CodeRabbit's status becomes a required check (tracked in `lsm-qhg5`); once it does, `--auto` becomes safe and this entry can go.

If main moves while CI is running, the `strict_required_status_checks_policy` blocks merge until the PR branch is updated; run `gh pr update-branch` to refresh.

Every change ships through the PR path — doc-only diffs included. `## Rules` ("Never push unless explicitly asked") and the org's `Changes must be made through a pull request` rule both apply uniformly.

### Cleanup safety

Worktree auto-cleanup has documented data loss (anthropics/claude-code#46444, anthropics/claude-code#48927, anthropics/claude-code#38287, anthropics/claude-code#51596, anthropics/claude-code#27753). Treat the cleanup step as the most dangerous in the workflow — push or merge before deleting, and prefer the safer commands.

- **Safest**: `bd worktree remove <name>` — checks for uncommitted changes, stashes, AND unpushed commits.
- **Safe**: `git worktree remove <path>` and `claude rm <id>` — refuse to delete worktrees with uncommitted changes (don't catch stashes / unpushed commits).
- **Footgun**: `Ctrl+X` twice in agent view (`claude agents`) deletes Claude-created worktrees **including any uncommitted changes**. Avoid unless the worktree is genuinely clean.

`bd worktree list` (or `git worktree list`) shows what's outstanding; `bd worktree info` inside a worktree confirms branch + bd state before removing.

Spawned agents with `isolation: worktree` can drift onto main-repo files. Audit `git status` in BOTH the worktree AND the main repo before trusting that work is contained. The `isolation: worktree` frontmatter is also silently ignored in some invocations (anthropics/claude-code#50357) — don't rely on it as the sole safeguard.

### Anti-patterns

- **Kitchen-sink session.** One worktree, one bead, one PR. Reusing a worktree for unrelated tasks pollutes context and risks the cleanup step nuking work for the wrong reason. `/clear` and a fresh `EnterWorktree` for the next bead.
- **`--auto` merge while an AI reviewer is non-required.** `gh pr merge --auto` waits only on required checks, so it can land a PR before CodeRabbit's review posts. Use `gh pr checks --watch`, then a manual `gh pr merge --squash`.
- **Trusting subagent `isolation: worktree` alone.** It's been observed to be silently ignored; combine with session-level `bgIsolation` and `git status` audits.
- **Pre-commit / lefthook hooks that assume single working dir.** Hooks that write to `./tmp`, hardcode paths, or share lock files collide across worktrees. Audit `lefthook.yml` when adding hooks.
- **Auto-merging external contributor PRs based on AI review alone.** Spoofed git identity has fooled AI reviewers into approving malicious PRs (manifold.security). Keep human approval for contributor PRs even if CodeRabbit signs off. This holds regardless of whether CodeRabbit's status later becomes a required check: requiring it gates _timing_, not identity, so it does nothing to stop a spoofed-identity PR from collecting an AI approval.

## Rules

- **Never commit proactively.** Wait for the user's go-ahead.
- **Never push** unless explicitly asked.
- **Read before writing.** Understand existing docs/code before modifying.
- **Conventions are law.** Follow `docs/README.md` for the docs pipeline, MADR v4.0 for ADRs, per-folder templates for new docs.
- **No empty docs.** Every idea needs a problem statement, every ADR needs considered options + rationale, every spec needs interface + behavior.
- **Scope is explicit.** Include "What This Is Not" when the boundary matters.
- **Decisions go in ADRs.** Don't resolve contradictions inline; write a new ADR (superseding the old one if needed).
- **One logical change per commit.** Split independent concerns.

## Commit Style

Conventional commits: `type(scope): short description`

**Types:** `docs`, `chore`, `feat`, `fix`, `refactor`, `test`, `perf`, `ci`

**Scopes (indicative):** `ideas`, `adr`, `spec`, `docs`, `readme`, `config`, `beads`, `core`, `plugins`, `themes`, `segments`, `cli`, `tui`, `doctor`, `ci`, `repo`. Scopes that bump a published crate are pinned in `knope.toml`'s `[packages.*]` blocks (`core` → linesmith-core, `plugins` → linesmith-plugin, `cli`/`tui`/`segments`/`themes`/`config`/`doctor` → linesmith). Doc / meta scopes (`adr`, `spec`, `docs`, `readme`, `ideas`, `beads`, `ci`, `repo`) pair with non-bumping commit types (`docs`/`chore`/`ci`/`test`) and aren't claimed by any package.

Beads issue references go in the commit footer as a bare `lsm-xyz`, not in the subject line. Commits not tied to a beads issue (meta / workflow / CI / version bumps) have **no** footer — don't invent one.

### Body

Optional; wrap at 72 characters. When present:

- **Short bullets** for the material changes (what ships).
- **A sentence or two of prose** for any WHY that isn't obvious from the code or diff — cross-cutting effects, non-obvious tradeoffs, or context a future bisecter will need.

Skip the body entirely for self-explanatory commits. Don't enumerate tests, narrate the code, or list follow-up beads — those live in `bd ready`.

```text
feat(core): implement stdin JSON parsing

Parse Claude Code statusline JSON payload into a typed StatusContext
with Option<T> for nullable fields (current_usage, rate_limits).

lsm-sgh
```

Close reasons (`bd close <id> --reason="..."`) describe **what shipped**, not **which commit** shipped it. Don't embed commit SHAs — they rot on rebase while subjects survive. Reference the work, not the byte:

```text
# Good
bd close lsm-aql --reason="Layout engine: priority-drop, width hints, grapheme-aware truncation. 9 follow-ups filed."

# Bad
bd close lsm-aql --reason="Shipped in ca91f3a"
```

### Closing beads issues

Close the bead when the work is done. Issue state lives in the bd
database and reaches the remote via `bd dolt push`, so there is nothing
to stage and no ordering constraint against the commit.

```text
# 1. Finish the work, then:
bd close lsm-xyz --reason="<one-line summary of what shipped>"

# 2. Stage and commit the code, with lsm-xyz as the bare footer:
git add <files>
git commit -m "feat(scope): ..."
```

`bd dolt push` replicates issue state to the remote, and it does so
explicitly — the `pre-push` hook does not (verified 2026-08-05: a hook
run left `refs/dolt/data` unchanged; an explicit push advanced it). Run
it when you want the close visible to other checkouts.

Do NOT make a separate `chore(beads):` commit to record the close. Issue
state never enters git, so there is nothing for such a commit to carry.

## Session Completion

When ending a work session:

1. **File issues for remaining work.** Run `bd create ...` for anything that needs follow-up.
2. **Run quality gates** if code changed: `mise run check` or a relevant subset.
3. **Update issue status.** Close finished work with `bd close <id>`; update in-progress items.
4. **Sync beads.** Run `bd dolt push` — it does not need the go-ahead `git push` does. It publishes issue metadata, triggers no CI or release, and Dolt is versioned, so the blast radius is nothing like a code push. The `pre-push` hook covers the common case, but it only fires when code is pushed; a session that only changes issues still needs the explicit command, and an unpushed database exists on one machine and won't surface on its own.
5. **Commit if the user asks.** Do not commit proactively.

Beads replicates through a Dolt remote (`origin` → `git+ssh://git@github.com/oakoss/linesmith.git`), configured as `sync.remote` in `.beads/config.yaml`. It stores the database in `refs/dolt/data` and a `__dolt_remote_info__` branch inside this repo; `.beads/issues.jsonl` is a local export and is not tracked. Code and issue state push separately: `git push` for one, `bd dolt push` for the other.

Pull the other direction at session start, or after a `git pull` that
brought in someone else's work — nothing delivers it automatically:

```bash
bd dolt pull
```

To hydrate a fresh clone or a new machine, `bd init` alone is enough: it reads
`sync.remote` from `.beads/config.yaml` and pulls from the Dolt remote itself.

```bash
bd init --prefix lsm
```

**`bd init` rewrites the repo, and its damage needs undoing.** It appends a
managed block to both `AGENTS.md` and `CLAUDE.md` (violating the rule that
`CLAUDE.md` holds only `@AGENTS.md`), overwrites `.claude/settings.json`,
`.gitignore`, and `.beads/hooks/*`, sets `core.hooksPath` to `.beads/hooks` —
which disables lefthook, silently dropping `cargo fmt`, `markdownlint`,
`prettier`, and `cog` commit-msg validation — and commits all of it. On a
throwaway clone:

```bash
git reset --hard HEAD~1              # drop bd init's commit
git config --unset-all --local core.hooksPath
lefthook install                     # restore the real hooks
```

Only if `sync.remote` is missing from the config does the remote need adding by
hand, followed by a pull:

```bash
bd dolt remote add origin git+ssh://git@github.com/oakoss/linesmith.git
bd dolt pull                         # errors with "no beads database found" if bd init hasn't run
```

## Comment policy

Comments are useful when they add value. Keep them clean and minimal.

A good comment:

- Is accurate (matches the code; remove if stale)
- Earns its place (explains WHY or non-obvious context, not WHAT)
- Is concise (one or two lines unless documenting a complex invariant)

Avoid:

- Restating what the code does
- Section markers like `// ===== HELPERS =====`
- Hedge words, apologies, "obviously", "basically", "just"
- "Note:" / "Important:" prefixes when surrounding text already conveys importance
- TODOs without ticket references
- Cross-references that belong in the PR description ("added for X", "used by Y")
- Multi-line comments on trivial code
- AI-flavored phrasings ("Here we...", "Let's...", "This...")

When in doubt: keep the comment, but make it tighter.

## Fix-vs-defer policy

When addressing review findings (from the review-cycle skill, PR comments, or any other reviewer):

Default to fixing inline. Defer to a follow-up only if:

- The fix is substantially more work than writing the follow-up itself
- The fix requires architectural changes spanning files outside this PR scope
- The fix requires a new dependency or schema migration not in this PR
- The fix would invalidate unrelated tests

If you can describe the fix in one sentence, do the fix.

When deferring, briefly state which criterion above applies.
