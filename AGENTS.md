# linesmith

A Rust status line tool for Claude Code (and other AI coding CLIs) with plugin API, role-based themes, and correctness-first context/rate-limit/worktree handling.

**Status:** Bootstrap phase. Docs and tooling foundation complete; Rust scaffold pending.

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

### Releases

Knope drives release automation per [ADR-0027](docs/adrs/0027-knope-for-release-automation.md) — per-crate versioning, per-crate CHANGELOG, cross-manifest dep-pin updates by name. Release-PR merge fires `knope-release.yml` (tags + crates.io publish); the `linesmith/v*` tag push then fires cargo-dist's `release.yml` for binary builds. Full contract in `docs/specs/release-process.md`; day-of-release steps in `docs/ops/release-runbook.md`.

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

### When the bgIsolation guard fires

The harness only blocks code-file edits (Rust, JSON, TOML); markdown / doc-only edits pass through to the main checkout without triggering:

> This background session hasn't isolated its changes yet. Call EnterWorktree first so edits land in a worktree instead of the shared checkout.

Use `EnterWorktree(name: <bd-id>)` to satisfy the guard when it fires, and call it proactively for doc-only changes too — every commit ships through a PR (next section), and that needs a branch separate from `main`. The guard is a backstop, not the only reason to isolate.

### Naming convention

One bead, one worktree, one branch — use the bare bead ID as the name:

```text
EnterWorktree(name: "lsm-hhb")
```

Creates `.claude/worktrees/lsm-hhb/` on branch `worktree-lsm-hhb`. Short, scannable in `git worktree list`, maps back to `bd show lsm-hhb`. The bead title is the descriptor; don't repeat it in the name. For ad-hoc non-beaded work, use a short kebab descriptor **without** the `worktree-` prefix (e.g., `workflow-docs`, not `worktree-workflow-docs`) — the harness adds the prefix to the branch name automatically, so a `worktree-`-prefixed input produces `worktree-worktree-<name>`.

From the shell, `claude --worktree <bd-id>` is the user-driven equivalent of `EnterWorktree(name: <bd-id>)` — same `.claude/worktrees/<bd-id>/` path, same `worktree-<bd-id>` branch.

### Bd flow inside the worktree

The bd database auto-shares across worktrees via git-common-dir discovery — every worktree sees the same DB as main without manual setup. The `.beads/issues.jsonl` file is per-checkout (it tracks per-branch on-disk state), so the close-then-commit pattern still produces clean diffs at merge time.

Claim **inside** the worktree, not in main — keeps main's `.beads/issues.jsonl` clean and avoids a stale-staged-jsonl conflict at sync time. The atomic close-then-commit pattern from `## Task Tracking` applies; the worktree adds a push + PR step at the end.

```bash
# Inside the worktree:
bd update lsm-xyz --claim
# ... edit, test, review-cycle ...
bd close lsm-xyz --reason="<one-line summary of what shipped>"
# If `git status` shows .beads/issues.jsonl unchanged after bd close
# (60s auto-export throttle hit), force the write:
#   bd export -o .beads/issues.jsonl
git add <files> .beads/issues.jsonl
git commit -m "feat(scope): subject

lsm-xyz"
git push -u origin worktree-lsm-xyz

# Open a PR; squash is the only merge method enabled on the repo:
gh pr create --title "feat(scope): subject" --body "$(cat <<'EOF'
## Summary
- ...
EOF
)"

# Wait for CI, then confirm Copilot has posted before merging:
gh pr checks --watch               # blocks until CI Summary completes; does NOT wait for Copilot

# Copilot review is async — poll up to 10m, or open the PR in a browser
# to confirm visually. No GNU `timeout` dependency; exits 1 on timeout.
deadline=$(($(date +%s) + 600))
until gh pr view --json reviews \
    -q '.reviews[] | select(.author.login | test("copilot"; "i"))' \
    | grep -q .; do
  (($(date +%s) < deadline)) || {
    echo "Copilot review not detected within 10m; open the PR page to check before merging" >&2
    exit 1
  }
  sleep 15
done

gh pr view --comments              # read Copilot's review notes
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

The PR path honors the repo's branch protection (`Changes must be made through a pull request`) and required `CI Summary` check — pushing straight to main bypasses both. Squash collapses any review-cycle iter-commits into one clean main commit; the squash commit body is built from concatenated commit messages (`squash_merge_commit_message: COMMIT_MESSAGES`), so the `lsm-xyz` footer in your worktree commit lands in the squash commit naturally — no PR-body workaround needed.

**Don't pass `--auto` to `gh pr merge`** while the org ruleset has `copilot_code_review` enabled: Copilot review is a non-blocking _request_, not a required status check, so `--auto` will fire as soon as CI passes and may land before Copilot has posted its review. Manual merge after `gh pr view --comments` confirms both signals are in.

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
- **`--auto` merge while Copilot review is enabled.** Copilot is a non-blocking review request — `gh pr merge --auto` will land the PR before Copilot has posted. Use manual `gh pr merge --squash` after reading `gh pr view --comments`.
- **Trusting subagent `isolation: worktree` alone.** It's been observed to be silently ignored; combine with session-level `bgIsolation` and `git status` audits.
- **Pre-commit / lefthook hooks that assume single working dir.** Hooks that write to `./tmp`, hardcode paths, or share lock files collide across worktrees. Audit `lefthook.yml` when adding hooks.
- **Auto-merging external contributor PRs based on AI review alone.** Spoofed git identity has fooled AI reviewers into approving malicious PRs (manifold.security). Keep human approval for contributor PRs even if Copilot signs off; `--auto` is only safe on your own branches.

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

Close the bead **before** staging so the feat/fix commit captures the
code AND the close atomically. The pre-commit hook (`bd hooks run
pre-commit`) flushes any pending jsonl export into the same commit.

```text
# 1. Finish the work, then:
bd close lsm-xyz --reason="<one-line summary of what shipped>"

# 2. Stage code + the jsonl bd just updated:
git add <files> .beads/issues.jsonl

# 3. Commit with lsm-xyz as the bare footer (see above):
git commit -m "feat(scope): ..."
```

If `git status` shows `.beads/issues.jsonl` unchanged after `bd close`
(60s auto-export throttle hit), force the write before staging:

```text
bd export -o .beads/issues.jsonl
git add .beads/issues.jsonl
```

Do NOT make a separate `chore(beads):` commit just to record the close;
the pre-commit hook keeps code and issue state in the same commit.

## Session Completion

When ending a work session:

1. **File issues for remaining work.** Run `bd create ...` for anything that needs follow-up.
2. **Run quality gates** if code changed: `mise run check` or a relevant subset.
3. **Update issue status.** Close finished work with `bd close <id>`; update in-progress items.
4. **Commit if the user asks.** Do not commit proactively.

Note: beads state syncs via the git-tracked `.beads/issues.jsonl` (no Dolt remote). After committing, `git push` to `origin` (`git@github.com:oakoss/linesmith.git`) is the only sync step — only push when the user asks.

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
