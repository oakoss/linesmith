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

A single static Rust binary (~3-5MB, <20ms cold start) reads Claude Code's status line JSON on stdin and renders a composable line with a rich segment system: priority, width hints, conditional visibility, caching, async, and sub-composition. A [rhai](https://rhai.rs/)-based extension API for user-authored segments is on the roadmap (see `lsm-ewa`). Themes are role-based (Catppuccin-compatible). The input schema is tool-agnostic; it works with Claude Code and Qwen Code today, with Codex CLI and GitHub Copilot CLI ready to slot in when they ship compatible APIs.

See [`docs/adrs/`](docs/adrs/) for the decision rationale behind each piece and [`docs/research/`](docs/research/) for the research that drove those decisions.

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
  "rate_limit",
]
```

Built-in segments available in v0.1.0: `model`, `workspace`, `context_window`, `cost`, `effort`, `rate_limit`, `rate_limit_5h`, `rate_limit_7d`. The git-segment family (`git_branch`, dirty, ahead/behind) and the visual `context_bar` widget are tracked for v0.2+.

Custom segments via [rhai](https://rhai.rs/) scripts dropped in `~/.config/linesmith/segments/` are on the roadmap (see `lsm-ewa`); v0.1 ships the runtime scaffolding without the loader wired up.

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
