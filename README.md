# linesmith

[![crates.io](https://img.shields.io/crates/v/linesmith.svg)](https://crates.io/crates/linesmith)
[![docs.rs](https://img.shields.io/docsrs/linesmith)](https://docs.rs/linesmith)
[![GitHub release](https://img.shields.io/github/v/release/oakoss/linesmith)](https://github.com/oakoss/linesmith/releases/latest)
[![License: MIT](https://img.shields.io/crates/l/linesmith.svg)](LICENSE)

A Rust status line for Claude Code and other AI coding CLIs. Fast cold start, real plugin API, role-based themes, and correctness-first handling of context, rate limits, and worktrees.

> **Status:** pre-1.0. Minor version bumps (`0.x.y` → `0.x+1.0`) may include breaking changes; patch bumps are bug fixes only (per [semver posture](docs/specs/release-process.md#tag-format-and-semver-posture)).

## Why

Existing Claude Code status line tools have real gaps:

- **Trust and resources.** `npx -y @latest` auto-executes unreviewed code and can spawn 30+ Node processes at 3GB RAM under rapid use.
- **Correctness.** Context %, rate limits, and worktree state all break in common edge cases (1M context, `/compact`, `/resume`, 429s, parallel worktrees).
- **No plugin API.** Every existing tool hardcodes its widgets. Custom segments require forking.
- **No curated onboarding.** Users repeatedly ask for "copy a preset and go."

linesmith is built to close those gaps.

## Design in one paragraph

A single static Rust binary (~3-5MB, <20ms cold start) reads Claude Code's status line JSON on stdin and renders a composable line with a rich segment system: priority, width hints, conditional visibility, caching, async, and sub-composition. A [rhai](https://rhai.rs/)-based extension API for user-authored segments has runtime scaffolding shipped; the script loader lands in v0.2 (see `lsm-ewa`). Themes are role-based (Catppuccin-compatible). The input schema is tool-agnostic; it works with Claude Code and Qwen Code today, with Codex CLI and GitHub Copilot CLI ready to slot in when they ship compatible APIs.

See [`docs/adrs/`](docs/adrs/) for the decision rationale behind each piece and [`docs/research/`](docs/research/) for the research that drove those decisions.

## What you'll see

The default segment line, plain and powerline:

```text
# space separator (default)
Opus 4.7 (1M)  8% · 1M  $2.34  xhigh  main * ↑1  linesmith

# powerline separator (Nerd Font U+E0B0 chevron; renders as ` ` to
# Nerd-Font terminals, tofu otherwise. Configure via [layout_options].separator.)
Opus 4.7 (1M)   8% · 1M   $2.34   xhigh   main * ↑1   linesmith
```

Segments use role-based theming (foreground, accent, muted, success, error, etc.) that adapts to the terminal's color support. Auto-detection respects `NO_COLOR`, `TERM`, and `COLORTERM`; set `[layout_options].color = "always"` to force ANSI through pipes (Claude Code captures stdout, so this is the common setting).

[OSC 8 hyperlinks](https://gist.github.com/egmontkob/eb114294efbcd5adb1944c9f3cb5feda) are wired end-to-end: any segment that sets `Style.hyperlink` (today, plugins; built-in workspace/branch links are tracked for v0.2) renders as a clickable link in supporting terminals, with detection via [`supports-hyperlinks`](https://crates.io/crates/supports-hyperlinks). Non-supporting terminals see plain text.

## Install

Pick whichever fits your setup:

```sh
# macOS / Linux shell installer (no Rust toolchain needed)
curl -LsSf https://github.com/oakoss/linesmith/releases/latest/download/linesmith-installer.sh | sh

# Windows x86_64 PowerShell installer
powershell -ExecutionPolicy ByPass -c "irm https://github.com/oakoss/linesmith/releases/latest/download/linesmith-installer.ps1 | iex"

# Homebrew (macOS, Linux)
brew install oakoss/tap/linesmith

# Rust toolchain users (any platform, including Windows ARM64)
cargo install linesmith
```

Prebuilt binaries also live on the [GitHub Releases page](https://github.com/oakoss/linesmith/releases/latest) for direct download. Supported target triples: `aarch64-apple-darwin`, `x86_64-apple-darwin`, `aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, `x86_64-unknown-linux-musl`, `x86_64-pc-windows-msvc`.

**Windows ARM64:** no prebuilt binary yet due to a cross-compile toolchain limitation. Use `cargo install linesmith` instead — building from source works.

**macOS first-run:** unsigned binaries hit Gatekeeper on first launch. Clear the quarantine flag with:

```sh
xattr -d com.apple.quarantine "$(command -v linesmith)"
```

**Verify release provenance** (SLSA attestations ship with prebuilt artifacts — binaries downloaded from the GitHub Release, installed via the shell/PowerShell installer, or poured by Homebrew. Does not apply to `cargo install linesmith` output since that's compiled locally):

```sh
# Works for binaries from the shell installer, PowerShell installer,
# or Homebrew. For a direct download, verify the archive itself.
gh attestation verify "$(command -v linesmith)" --owner oakoss
```

Distribution is handled by [`cargo-dist`](https://opensource.axo.dev/cargo-dist/); see [`docs/specs/release-process.md`](docs/specs/release-process.md) for the pipeline contract.

## Configure

Wire linesmith into Claude Code's `~/.claude/settings.json`:

```json
{
  "statusLine": {
    "type": "command",
    "command": "linesmith",
    "padding": 0
  }
}
```

linesmith reads its own config from `~/.config/linesmith/config.toml`:

```toml
theme = "catppuccin-mocha"

[line]
segments = [
  "model",
  "workspace",
  "context_window",
  "cost",
  "rate_limit_5h",
]

[layout_options]
# "always" forces ANSI through pipe boundaries (Claude Code captures
# stdout, so auto-detection picks "no color" without this).
color = "always"

# Inter-segment separator. Reserved keywords: "space" (default),
# "powerline" (Nerd Font U+E0B0 chevron), "" (no separator).
# Anything else renders as a literal: " | ", " · ", "->", etc.
separator = "powerline"

# Cell count for the powerline chevron (1 or 2). Most modern Nerd
# Fonts render U+E0B0 as 1 cell; bump to 2 if the chevron looks
# squashed in your terminal.
powerline_width = 1
```

Default segments (rendered when no `[line].segments` is set): `model`, `context_window`, `cost`, `effort`, `git_branch` (with dirty `*` and ahead/behind `↑`/`↓`), `workspace`. Opt-ins include `rate_limit_5h` / `rate_limit_7d` (current usage), `rate_limit_5h_reset` / `rate_limit_7d_reset` (countdown to window reset), `version` (Claude Code CLI version), token counts, session duration, and `vim` / `agent` / `output_style` mode indicators. The visual `context_bar` widget is wired but rough — tracked for v0.2 polish. See [`docs/specs/segment-system.md`](docs/specs/segment-system.md) for the full inventory and per-segment knobs.

Custom segments via [rhai](https://rhai.rs/) scripts dropped in `~/.config/linesmith/segments/` are on the roadmap (see `lsm-ewa`); v0.1 ships the runtime scaffolding (the registry, the engine, the trait wiring) without the script loader.

### Themes

Eleven built-in themes ship in v0.1; pick one with `theme = "<name>"` at the top of your config:

| Theme                                                             | Notes                                     |
| ----------------------------------------------------------------- | ----------------------------------------- |
| `default`                                                         | ANSI-16 fallback, works on every terminal |
| `minimal`                                                         | Dim/muted only, no accent colors          |
| `catppuccin-latte`                                                | Light flavor                              |
| `catppuccin-frappe` / `catppuccin-macchiato` / `catppuccin-mocha` | Dark flavors                              |
| `dracula`                                                         | High-contrast purple/cyan                 |
| `nord`                                                            | Cool blue/gray                            |
| `gruvbox`                                                         | Warm retro palette                        |
| `tokyo-night`                                                     | Storm variant                             |
| `rose-pine`                                                       | Main flavor                               |

User themes (custom palettes via TOML) drop into `~/.config/linesmith/themes/`. See [`docs/specs/theming.md`](docs/specs/theming.md) for the role schema and palette source attribution.

## How fast

linesmith targets `<20ms` per invocation, end-to-end from process spawn to rendered stdout. Two complementary measurements cover that:

- **End-to-end wall-clock**: `mise run bench:cold-start` runs [hyperfine](https://github.com/sharkdp/hyperfine) against the release binary with a real fixture payload on stdin. Captures the user-perceived signal — process spawn + ELF load + crate-level static init + render. Run on demand against any local build.
- **Render-path regression detector**: `mise run bench` (Criterion, `crates/linesmith/benches/render.rs`) measures the post-spawn render hot path in three configurations (minimal / default preset / all built-ins minus segments with `DataDep::Usage`). Local-only — runner variance on GitHub-hosted CI overwhelms the regression delta worth catching, so this isn't gated automatically. See [`docs/research/perf-benchmarking-survey.md`](docs/research/perf-benchmarking-survey.md) for the rationale and the v0.2+ candidate (CodSpeed) for managed bare-metal CI gating.

## Documentation

| Folder                             | Contents                                                                                                                  |
| ---------------------------------- | ------------------------------------------------------------------------------------------------------------------------- |
| [`docs/research/`](docs/research/) | Surveys and deep dives (API contract, competitor landscape, user demand, Rust crate survey, cross-tool support)           |
| [`docs/adrs/`](docs/adrs/)         | Architecture Decision Records (MADR v4.0) — why Rust, why rhai, how segments compose, theming model, schema, distribution |
| [`docs/specs/`](docs/specs/)       | Implementation contracts (written as features are built)                                                                  |
| [`docs/ideas/`](docs/ideas/)       | Exploratory notes                                                                                                         |
| [`AGENTS.md`](AGENTS.md)           | Instructions for AI coding agents working on this project                                                                 |

See [`docs/README.md`](docs/README.md) for the full pipeline: how research flows to ideas to ADRs to specs to tasks.

## Development

Managed by [mise](https://mise.jdx.dev/):

```sh
mise install              # install all tools (rust, node, prettier, markdownlint, lefthook, cog)
lefthook install          # install git hooks (pre-commit fmt/lint, commit-msg conventional)
mise run check            # run all checks (fmt + lint + Rust)
mise run test             # run all tests
```

Commit style: [Conventional Commits](https://www.conventionalcommits.org/) enforced by [cog](https://docs.cocogitto.io/). Full contribution workflow — bead tracker conventions, commit scope list, spec pipeline — lives in [`AGENTS.md`](AGENTS.md). Cutting a release? See [`docs/ops/release-runbook.md`](docs/ops/release-runbook.md).

## License

[MIT](LICENSE). The core stays open source; any future plugin ecosystem can ship under any license.
