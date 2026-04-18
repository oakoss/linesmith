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

| Command          | Purpose                           |
| ---------------- | --------------------------------- |
| `mise run check` | All checks (fmt + lint + Rust)    |
| `mise run test`  | All tests                         |
| `mise run bench` | All benchmarks                    |
| `mise run fmt`   | Format non-Rust files (prettier)  |
| `mise run lint`  | Lint markdown (markdownlint-cli2) |
| `cargo fmt`      | Format Rust                       |
| `cargo clippy`   | Rust linting                      |

Git hooks installed via `lefthook install`. `pre-commit` runs fmt/lint on staged files; `commit-msg` verifies conventional commit format via `cog`.

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
- **Do NOT** use TodoWrite, TaskCreate, or markdown TODO lists; beads is the source of truth

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

**Scopes (indicative):** `ideas`, `adr`, `spec`, `docs`, `readme`, `config`, `beads`, `core`, `plugins`, `themes`, `segments`, `ci`, `repo`

Beads issue references go in the commit footer as a bare `lsm-xyz`, not in the subject line. Commits not tied to a beads issue (meta / workflow / CI / version bumps) have **no** footer — don't invent one.

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

Note: this project is local-only until a GitHub remote is added. Beads state syncs via the git-tracked `.beads/issues.jsonl` (no Dolt remote); `git push` is the only sync step.
