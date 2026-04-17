# linesmith

A Rust status line for Claude Code and other AI coding CLIs. Fast cold start, real plugin API, role-based themes, and correctness-first handling of context, rate limits, and worktrees.

> **Status:** bootstrap phase. Docs and tooling foundation complete; Rust scaffold and v0.1 pending.

## Why

Existing Claude Code status line tools have real gaps:

- **Trust and resources.** `npx -y @latest` auto-executes unreviewed code and can spawn 30+ Node processes at 3GB RAM under rapid use.
- **Correctness.** Context %, rate limits, and worktree state all break in common edge cases (1M context, `/compact`, `/resume`, 429s, parallel worktrees).
- **No plugin API.** Every existing tool hardcodes its widgets. Custom segments require forking.
- **No curated onboarding.** Users repeatedly ask for "copy a preset and go."

linesmith is built to close those gaps.

## Design in one paragraph

A single static Rust binary (~3-5MB, <20ms cold start) that reads Claude Code's status line JSON on stdin and renders a composable line with a rich segment system (priority, width hints, conditional visibility, caching, async, sub-composition). User extensions are written in [rhai](https://rhai.rs/) and loaded from a config directory. Themes are role-based (Catppuccin-compatible). Input schema is tool-agnostic — it works with Claude Code and Qwen Code today, with Codex CLI and GitHub Copilot CLI ready to slot in when they ship compatible APIs.

See [`docs/adrs/`](docs/adrs/) for the decision rationale behind each piece and [`docs/research/`](docs/research/) for the research that drove those decisions.

## Install

Not yet — v0.1 has not shipped. Once it does, install will be one of:

```sh
# macOS / Linux
curl -sSf https://linesmith.sh/install.sh | sh

# Homebrew (macOS, Linux)
brew install oakoss/linesmith/linesmith

# Windows PowerShell
powershell -c "irm https://linesmith.sh/install.ps1 | iex"

# Rust users
cargo install linesmith
```

Distribution is handled by [`cargo-dist`](https://opensource.axo.dev/cargo-dist/).

## Configure (planned)

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
  "workspace",      # directory/worktree hybrid
  "git_branch",
  "context_window",
  "cost",
  "rate_limit",
]

[segments.context_window]
style = "bar"
width = 10
```

Drop a `.rhai` file in `~/.config/linesmith/segments/` to add a custom segment.

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

Commit style: [Conventional Commits](https://www.conventionalcommits.org/) enforced by [cog](https://docs.cocogitto.io/).

## License

[Mozilla Public License 2.0](LICENSE). The core stays open source; any future plugin ecosystem can ship under any license.
