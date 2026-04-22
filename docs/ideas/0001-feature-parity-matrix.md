# Feature-Parity Matrix

- Status: active
- Date: 2026-04-17
- Author: Jace
- Promoted to: (living document — not slated for promotion to ADR; serves as the roadmap input to specs and releases)

## The idea

Enumerate every feature across the leading Claude Code status line tools, tag each with a linesmith decision, and use this matrix as the roadmap input to our foundational specs and release scoping. The matrix is the answer to the question "what does v0.1 / v0.2 / v1.0 look like?"

## Why it matters

- **v0.1 ships fast** if we're explicit about what's in and what waits
- **Specs are architected for the full target**, even when v0.1 implements a subset; prevents rewrites
- **Differentiators are visible** (plugin API, correctness) instead of drowned in parity items
- **Public roadmap**: contributors and early users know where we are and where we're going

## Tags

Each row is tagged with one of:

- **v0.1**: ships at first release (foundation + highest-leverage parity items)
- **v0.2+**: post-v0.1 parity drive; included in roadmap but not v0.1 blocker
- **Differentiate**: linesmith ships this better than any competitor (our wedge)
- **Defer**: not scoped; may happen if demand materializes
- **Skip**: explicitly out of scope (tangential or conflicts with project direction)

Competitor columns use abbreviations: **cc** = ccstatusline (sirmalloc), **cpl** = claude-powerline (Owloops), **ccx** = CCometixLine (Haleclipse), **ccs** = cc-statusline (chongdashu), **hud** = claude-hud, **cs** = claudia-statusline (hagan), **fe** = felipeelias/claude-statusline, **rz** = rz1989s/claude-code-statusline, **daniel** = daniel3303/ClaudeCodeStatusLine.

---

## Segments

### Model / Session

| Feature                                          | cc  | cpl | ccx | hud | Other  | linesmith |
| ------------------------------------------------ | --- | --- | --- | --- | ------ | --------- |
| Current model name                               | ✓   | ✓   | ✓   | ✓   | all    | **v0.1**  |
| Model provider badge (API / Pro / Max / Bedrock) | ✓   |     |     |     | daniel | v0.2+     |
| Session name / ID                                | ✓   | ✓   |     | ✓   | cs     | v0.2+     |
| Session cost (USD)                               | ✓   | ✓   | ✓   | ✓   | all    | **v0.1**  |
| Session duration                                 | ✓   | ✓   | ✓   |     | cs, rz | **v0.1**  |
| Lines added / removed                            | ✓   | ✓   |     |     | daniel | v0.2+     |
| Last-task duration (p10k-style)                  |     |     |     |     |        | v0.2+     |
| Account email / username                         | ✓   |     |     |     |        | Defer     |

### Token / Context

| Feature                                      | cc  | cpl | ccx | hud | Other | linesmith                                                              |
| -------------------------------------------- | --- | --- | --- | --- | ----- | ---------------------------------------------------------------------- |
| Context window percentage                    | ✓   | ✓   | ✓   | ✓   | all   | **v0.1 — correct across 1M / compact / resume / 429s** (Differentiate) |
| Context window bar (visual)                  | ✓   | ✓   |     |     | rz    | **v0.1 — size configurable** (Differentiate)                           |
| Total input / output tokens                  | ✓   | ✓   | ✓   |     |       | **v0.1**                                                               |
| Token speed (tokens/sec)                     | ✓   |     |     |     |       | v0.2+                                                                  |
| Weekly / rolling token totals                | ✓   |     |     |     | rz    | v0.2+                                                                  |
| Cache-read / cache-creation tokens           | ✓   |     |     |     |       | v0.2+                                                                  |
| **Cache-hit countdown** (5-min expiry timer) |     |     |     |     |       | **Differentiate**                                                      |

### Rate Limits (Pro/Max)

| Feature                                 | cc  | cpl | ccx | daniel | Other | linesmith                                   |
| --------------------------------------- | --- | --- | --- | ------ | ----- | ------------------------------------------- |
| 5-hour usage percentage                 | ✓   | ✓   |     | ✓      |       | **v0.1**                                    |
| 5-hour resets-at countdown              | ✓   | ✓   |     | ✓      |       | **v0.1**                                    |
| 7-day usage percentage                  | ✓   | ✓   |     | ✓      |       | **v0.1**                                    |
| 7-day resets-at countdown               | ✓   | ✓   |     | ✓      |       | **v0.1**                                    |
| Scrape from API headers (ccusage-style) |     |     |     | ✓      |       | **v0.1 — tier-independent** (Differentiate) |
| Burn rate ($/h)                         |     |     |     |        | rz    | v0.2+                                       |
| **Survival projection** (`→full ~52m`)  |     |     |     |        |       | **Differentiate**                           |
| Peak / off-peak scheduler awareness     |     |     |     |        |       | Defer                                       |

### Git

| Feature                                     | cc  | cpl | ccx | Other | linesmith                                               |
| ------------------------------------------- | --- | --- | --- | ----- | ------------------------------------------------------- |
| Branch name                                 | ✓   | ✓   | ✓   | all   | **v0.1**                                                |
| Dirty indicator (modified/added/deleted)    | ✓   | ✓   | ✓   | most  | **v0.1**                                                |
| Ahead / behind origin                       | ✓   | ✓   | ✓   | some  | **v0.1**                                                |
| Stash count                                 | ✓   |     |     |       | v0.2+                                                   |
| **Worktree name + branch (hybrid segment)** | ✓   | ✓   |     | cs    | **v0.1 — per-worktree state isolation** (Differentiate) |
| PR number / link                            | ✓   |     |     |       | v0.2+                                                   |
| Fork indicator                              | ✓   |     |     |       | v0.2+                                                   |
| Remote / provider (GitHub/GitLab icon)      | ✓   |     |     |       | v0.2+                                                   |
| Uncommitted / untracked counts              | ✓   | ✓   |     |       | v0.2+                                                   |
| Conflict indicator                          | ✓   |     |     |       | v0.2+                                                   |

### Directory / Workspace

| Feature                                   | cc  | cpl | ccx | Other | linesmith                  |
| ----------------------------------------- | --- | --- | --- | ----- | -------------------------- |
| Current directory name                    | ✓   | ✓   | ✓   | all   | **v0.1**                   |
| Project root (relative path)              | ✓   | ✓   |     | cs    | v0.2+                      |
| Added dirs (multi-root)                   | ✓   |     |     |       | v0.2+                      |
| Directory / worktree hybrid (auto-switch) | ✓   |     |     |       | **v0.1 — primary segment** |

### System

| Feature                | cc  | cpl | ccx | Other  | linesmith |
| ---------------------- | --- | --- | --- | ------ | --------- |
| Memory usage (MB / %)  | ✓   |     |     |        | v0.2+     |
| Vim mode indicator     | ✓   | ✓   |     | cs     | v0.2+     |
| Output style indicator | ✓   |     |     |        | v0.2+     |
| OS / hostname          |     |     |     | daniel | Skip      |
| Time of day            |     |     |     | rz     | Defer     |
| Prayer times           |     |     |     | rz     | Skip      |
| Energy / CO2 estimate  |     |     |     |        | Defer     |

### Agents / Tools / MCP

| Feature                                             | cc  | hud | Other | linesmith |
| --------------------------------------------------- | --- | --- | ----- | --------- |
| Active skills list                                  |     | ✓   |       | v0.2+     |
| Active subagents                                    |     | ✓   |       | v0.2+     |
| Active tools (firing right now)                     |     | ✓   |       | v0.2+     |
| MCP server status                                   |     | ✓   |       | v0.2+     |
| Todo progress (claude-hud-style)                    |     | ✓   |       | Defer     |
| Effort / thinking level (low/medium/high/max/xhigh) | ✓   |     |       | **v0.1**  |
| Agent identity / name                               | ✓   | ✓   |       | v0.2+     |

### Plan Mode

| Feature             | cc  | Other | linesmith |
| ------------------- | --- | ----- | --------- |
| Plan mode indicator |     |       | v0.2+     |
| Active plan file    |     |       | v0.2+     |
| Plan progress       |     |       | v0.2+     |

### Multiplexer Integration

| Feature                      | cpl | rz  | Other | linesmith |
| ---------------------------- | --- | --- | ----- | --------- |
| tmux pane title              | ✓   |     |       | v0.2+     |
| tmux status-line integration | ✓   |     |       | v0.2+     |
| zellij integration           |     |     |       | Defer     |

---

## Themes

| Theme                        | cc  | cpl | ccx | cs  | fe  | linesmith                    |
| ---------------------------- | --- | --- | --- | --- | --- | ---------------------------- |
| Default (neutral / terminal) | ✓   | ✓   | ✓   | ✓   | ✓   | **v0.1**                     |
| Minimal (no color)           | ✓   | ✓   |     | ✓   |     | **v0.1**                     |
| Catppuccin Latte             | ✓   |     |     | ✓   | ✓   | **v0.1**                     |
| Catppuccin Frappé            | ✓   |     |     | ✓   |     | **v0.1**                     |
| Catppuccin Macchiato         | ✓   |     |     | ✓   |     | **v0.1**                     |
| Catppuccin Mocha             | ✓   |     |     | ✓   | ✓   | **v0.1**                     |
| Dracula                      | ✓   | ✓   |     | ✓   |     | v0.2+                        |
| Nord                         | ✓   | ✓   | ✓   | ✓   |     | v0.2+                        |
| Gruvbox (dark + light)       | ✓   | ✓   | ✓   | ✓   | ✓   | v0.2+                        |
| Tokyo Night                  | ✓   | ✓   |     | ✓   | ✓   | v0.2+                        |
| Rose Pine                    | ✓   |     |     | ✓   |     | v0.2+                        |
| Solarized (dark + light)     | ✓   |     |     | ✓   |     | v0.2+                        |
| One Dark                     | ✓   |     |     |     |     | v0.2+                        |
| Material                     | ✓   |     |     |     |     | Defer                        |
| Powerline-dark               |     |     | ✓   |     |     | v0.2+                        |
| Cometix (CCx's own)          |     |     | ✓   |     |     | Skip                         |
| User-authored themes         | ✓   | ✓   |     | ✓   |     | **v0.1 — TOML file drop-in** |
| OMP JSON theme import        |     |     |     |     |     | Defer                        |

---

## Styles / Separators

| Style                                 | cc  | cpl | Other   | linesmith |
| ------------------------------------- | --- | --- | ------- | --------- |
| Plain (space-separated)               | ✓   | ✓   | all     | **v0.1**  |
| Powerline (triangle chevron)          | ✓   | ✓   | ccx     | **v0.1**  |
| Capsule (rounded)                     |     | ✓   |         | v0.2+     |
| Flex separator (stretch to width)     | ✓   |     |         | v0.2+     |
| Flame                                 |     |     | spences | Defer     |
| Wave                                  |     |     | spences | Defer     |
| Diamond                               |     |     | spences | Defer     |
| Slash / backslash                     |     |     | spences | Defer     |
| Pixelated                             |     |     | spences | Defer     |
| Multi-line (2-3 rows)                 | ✓   | ✓   |         | **v0.1**  |
| TUI-style (claude-powerline TUI mode) |     | ✓   |         | Defer     |

---

## Layout / Rendering

| Feature                                                 | cc      | cpl     | ccx | Other | linesmith         |
| ------------------------------------------------------- | ------- | ------- | --- | ----- | ----------------- |
| Terminal width detection                                | ✓       | ✓       |     | some  | **v0.1**          |
| **Priority-based truncation** (drops low-prio segments) |         |         |     |       | **Differentiate** |
| Per-segment min/max width hints                         |         |         |     |       | **Differentiate** |
| Conditional visibility (hide when N/A)                  | partial | partial |     |       | **v0.1**          |
| Sub-composed widgets (git group)                        |         |         |     |       | **Differentiate** |
| Truecolor (24-bit)                                      | ✓       | ✓       | ✓   | all   | **v0.1**          |
| 256-color fallback                                      | ✓       | ✓       | ✓   | all   | **v0.1**          |
| NO_COLOR / FORCE_COLOR                                  | ✓       | ✓       | ✓   | most  | **v0.1**          |
| Nerd Font glyph support                                 | ✓       | ✓       | ✓   | all   | **v0.1**          |
| Nerd Font auto-install prompt                           | ✓       |         |     |       | Defer             |
| OSC 8 hyperlinks (branch → PR URL etc.)                 | partial |         |     |       | **v0.1**          |
| Padding (top/bottom)                                    | ✓       |         |     |       | **v0.1**          |
| Caching for expensive segments                          | some    |         |     |       | **v0.1**          |
| Async prefetch (background refresh)                     |         |         |     |       | v0.2+             |

---

## Config

| Feature                                         | cc  | cpl | ccx | Other   | linesmith          |
| ----------------------------------------------- | --- | --- | --- | ------- | ------------------ |
| JSON config                                     | ✓   | ✓   |     | cs      | v0.2+              |
| TOML config                                     |     |     | ✓   | fe, rz  | **v0.1 — primary** |
| YAML config                                     |     |     |     |         | Skip               |
| XDG config location                             | ✓   | ✓   | ✓   | fe      | **v0.1**           |
| Config file schema (JSON Schema / IntelliSense) |     |     |     | spences | **v0.1**           |
| `--config <path>` flag                          | ✓   | ✓   | ✓   | most    | **v0.1**           |
| Env var overrides                               | ✓   |     |     | rz      | **v0.1**           |
| Hot reload on config change                     |     | ✓   |     |         | v0.2+              |
| Live preview during editing                     | ✓   |     | ✓   |         | v0.2+              |
| Per-segment config override                     | ✓   | ✓   |     |         | **v0.1**           |
| Validation with clear error messages            | ✓   | ✓   |     |         | **v0.1**           |

---

## Plugins / Extensibility

| Feature                                                     | cc  | cpl | rz  | Other | linesmith                |
| ----------------------------------------------------------- | --- | --- | --- | ----- | ------------------------ |
| **User-defined segments (scripting)**                       |     |     |     |       | **Differentiate (rhai)** |
| Plugin discovery from `~/.config/linesmith/segments/*.rhai` |     |     |     |       | **v0.1**                 |
| Plugin sandbox (no shell exec, no FS writes)                |     |     |     |       | **v0.1**                 |
| Segment composition API for plugins                         |     |     |     |       | **v0.1**                 |
| Plugin hot reload                                           |     |     |     |       | v0.2+                    |
| Plugin registry / marketplace                               |     |     |     |       | Defer                    |
| WASM plugins (language-agnostic)                            |     |     |     |       | Defer                    |
| Shell-script widgets (Starship-style)                       |     |     |     | rz    | Skip                     |
| Module system (rz1989s-style)                               |     |     | ✓   |       | Skip                     |

---

## Onboarding / UX

| Feature                                       | cc             | ccx | ccs | Other | linesmith             |
| --------------------------------------------- | -------------- | --- | --- | ----- | --------------------- |
| Interactive TUI config builder                | ✓              | ✓   |     |       | v0.2+                 |
| Sequential-prompt config wizard (dialoguer)   |                |     | ✓   |       | **v0.1**              |
| `linesmith init` command                      |                |     | ✓   | fe    | **v0.1**              |
| Curated preset gallery                        |                |     |     |       | **v0.1 — 5+ presets** |
| `linesmith presets list / apply <name>`       |                |     |     |       | **v0.1**              |
| `linesmith doctor` (health check)             |                |     |     |       | **v0.1**              |
| `linesmith explain <segment>` (debug)         | (starship has) |     |     |       | v0.2+                 |
| One-command install (wire to Claude settings) |                |     | ✓   |       | **v0.1**              |

---

## Correctness / Edge Cases

| Concern                                             | cc            | cpl     | ccx | Other   | linesmith         |
| --------------------------------------------------- | ------------- | ------- | --- | ------- | ----------------- |
| **Context % correct in 1M context**                 | ✗ (broken)    | ✗       | ✗   | ✗       | **Differentiate** |
| **Context % correct post-/compact**                 | ✗             | ✗       |     | ✗       | **Differentiate** |
| **Context % correct post-/resume**                  | ✗             | ✗       |     | ✗       | **Differentiate** |
| **Context % correct during 429s**                   | ✗             |         |     |         | **Differentiate** |
| Rate limits source (endpoint + JSONL fallback)      | partial       |         |     |         | **v0.1**          |
| Render unchanged when no Rust code yet              | n/a           | n/a     | n/a | n/a     | **v0.1**          |
| Graceful no-op when no git                          | ✓             | ✓       | ✗   | mixed   | **v0.1**          |
| Worktree detection (`.git` as file)                 | partial       | partial |     | partial | **v0.1**          |
| **Per-worktree state isolation (no cross-leakage)** | ✗ (#190)      | partial |     |         | **Differentiate** |
| Windows terminal compatibility                      | partial       | partial | ✓   | daniel  | **v0.1**          |
| tmux rendering                                      | partial       | ✓       |     | ✓       | **v0.1**          |
| IntelliJ / VS Code integrated terminal              | partial (#67) |         |     |         | **v0.1**          |

---

## Distribution

| Feature                         | cc  | cpl | ccx | Other  | linesmith                    |
| ------------------------------- | --- | --- | --- | ------ | ---------------------------- |
| npm install                     | ✓   | ✓   |     | ccs    | **Skip (intentional)**       |
| Homebrew tap                    | ✓   |     | ✓   | fe     | **v0.1**                     |
| `curl \| sh` installer          |     |     | ✓   | fe     | **v0.1**                     |
| PowerShell installer (Windows)  |     |     | ✓   | daniel | **v0.1**                     |
| Cargo install                   |     |     | ✓   |        | **v0.1**                     |
| Single static binary            |     |     | ✓   | fe     | **v0.1**                     |
| Pre-built binaries per platform |     |     | ✓   | fe     | **v0.1**                     |
| Scoop (Windows)                 |     |     |     |        | v0.2+                        |
| Auto-update check               | ✓   |     |     |        | Defer                        |
| `npx -y` run                    | ✓   |     |     | ccs    | **Skip (supply-chain risk)** |

---

## Tool Support

| Tool                                   | cc  | cpl | ccx | Other | linesmith                             |
| -------------------------------------- | --- | --- | --- | ----- | ------------------------------------- |
| Claude Code                            | ✓   | ✓   | ✓   | all   | **v0.1**                              |
| Qwen Code (near-identical JSON schema) |     |     |     |       | **v0.1 (cheap)**                      |
| OpenAI Codex CLI                       |     |     |     |       | v0.2+ (when CCX ships statusLine API) |
| GitHub Copilot CLI                     |     |     |     |       | v0.2+ (when shipped)                  |
| Gemini CLI                             |     |     |     |       | Skip (no extensibility hook)          |
| Aider                                  |     |     |     |       | Skip                                  |
| opencode                               |     |     |     |       | Skip                                  |
| IDE tools (Cline/Cursor/Windsurf/etc.) | n/a | n/a | n/a | n/a   | Skip (structurally different)         |

---

## Telemetry / Analytics / Extras

| Feature                                  | cc  | cs  | rz  | Other | linesmith             |
| ---------------------------------------- | --- | --- | --- | ----- | --------------------- |
| SQLite usage analytics                   |     | ✓   |     |       | Defer                 |
| Turso / cloud sync of analytics          |     | ✓   |     |       | Defer                 |
| Zero telemetry (never phone home)        |     |     |     |       | **v0.1 — guaranteed** |
| Compaction detection (hook-based)        |     | ✓   |     |       | v0.2+                 |
| JSONL transcript parsing for metrics     | ✓   | ✓   |     |       | v0.2+                 |
| Claude Code patcher (ccx's custom thing) |     |     |     | ccx   | Skip                  |

---

## Developer Tooling

| Feature                                  | Some competitors | linesmith |
| ---------------------------------------- | ---------------- | --------- |
| Cargo workspace                          | ccx, cs          | **v0.1**  |
| Clippy clean (deny(warnings))            | ccx              | **v0.1**  |
| Unit tests for every segment             | partial          | **v0.1**  |
| Integration tests (golden JSON → output) |                  | **v0.1**  |
| Benchmarks (cold start)                  |                  | **v0.1**  |
| CI (fmt / lint / test)                   | all              | **v0.1**  |
| Cross-compile (cargo-dist)               | partial          | **v0.1**  |
| Docs site                                | ccs, cs, ccx     | v0.2+     |

---

## v0.1 scope summary

**Segments (count ≈ 11):** model, context %, context bar, cost, session duration, directory/worktree hybrid, git branch (with dirty + ahead/behind), rate limit 5h, rate limit 7d, rate limit (combined 5h+7d; users pick combined OR individual, not both), effort level (renders hidden until Claude Code emits the field live).

**Themes (count ≈ 6):** default, minimal, Catppuccin Latte / Frappé / Macchiato / Mocha. (All 4 Catppuccin flavors are nearly-free once the role vocabulary is defined.)

**Styles:** plain + powerline + multi-line support. Capsule and flex come v0.2+.

**Differentiators active in v0.1:**

- Plugin API (rhai): working, even if small segment library
- Correctness: context %, rate limits, worktree isolation done right
- Priority-based layout truncation
- Cache-hit countdown segment
- Sub-composed git group

**Onboarding:** `linesmith init` + 5 presets + `linesmith doctor`.

**Distribution:** single binary via cargo-dist (macOS/Linux/Windows), Homebrew tap, curl installer, `cargo install`.

**Tool support:** Claude Code + Qwen Code (free ride given the schema overlap).

## Open questions

- Do we ship Catppuccin as 4 separate themes or one theme with a `flavor` selector?
- What's the minimum viable `linesmith doctor` output? (git detection, Nerd Font check, terminal capabilities, bd sanity)
- Should the first release be v0.1.0 or v0.0.1? (tracer-bullet says v0.1; marketing says v0.0.1 signals "preview")
- Do we accept theme PRs from day one, or wait for a stable role vocabulary first?

## Related work

- [Research: competitor landscape](../research/competitor-landscape.md)
- [Research: user demand](../research/user-demand.md)
- [ADR-0003: segment/widget system](../adrs/0003-segment-widget-system.md)
- [ADR-0005: role-based themes](../adrs/0005-role-based-themes.md)

## Living document

This matrix is updated as:

- New competitor features are discovered
- Scope decisions change (Defer → v0.2, Skip → Defer, etc.)
- Differentiators prove out or fall through

It does **not** get promoted to an ADR; it's a roadmap planning doc, not a single decision. Individual scope bumps (e.g., "we're deferring X") may warrant ADRs; the matrix is where those decisions surface first.
