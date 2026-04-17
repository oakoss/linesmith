# Cross-Tool Status Line Support

- Date: 2026-04-17
- Author: Claude Code research agent session
- Scope: Determine which AI coding CLI tools currently support user-configurable status lines (like Claude Code's `statusLine`), to decide whether linesmith should be Claude-only or tool-agnostic.

## Question

Is Claude Code the only AI coding CLI that supports a user-customizable status line? If others exist or are coming, should linesmith be designed tool-agnostic?

## Sources

- [Claude Code statusline docs](https://code.claude.com/docs/en/statusline)
- [Codex CLI config reference](https://developers.openai.com/codex/config-reference)
- [Codex issue #17827 (statusLine feature request)](https://github.com/openai/codex/issues/17827)
- [Codex issue #14043 (custom statusline widget API)](https://github.com/openai/codex/issues/14043)
- [Qwen Code status line docs](https://qwenlm.github.io/qwen-code-docs/en/users/features/status-line/)
- [Gemini CLI configuration](https://geminicli.com/docs/reference/configuration/)
- [Aider configuration](https://aider.chat/docs/config.html)
- [opencode TUI config](https://opencode.ai/docs/tui/)
- [Cline customization discussion #2867](https://github.com/cline/cline/discussions/2867)
- [Copilot CLI issue #1311 (status line)](https://github.com/github/copilot-cli/issues/1311)
- [Copilot CLI issue #2329 (customize prompt symbols)](https://github.com/github/copilot-cli/issues/2329)
- [codex-hud external emulator](https://github.com/fwyc0573/codex-hud)
- [avatorl/copilot-cli-statusline](https://github.com/avatorl/copilot-cli-statusline)

## Findings

### Support matrix

| #   | Tool                      | Custom statusLine?                   | Contract                                                                                                                                                                                                                                            |
| --- | ------------------------- | ------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 1   | **Claude Code**           | Yes (baseline)                       | `statusLine` in `~/.claude/settings.json`; `type: "command"` runs a shell cmd, receives full JSON on stdin, prints text to stdout                                                                                                                   |
| 2   | **Qwen Code**             | **Yes — Claude-compatible contract** | `ui.statusLine` in `~/.qwen/settings.json`; shell command receives JSON on stdin with `session_id`, `version`, `model.display_name`, `context_window`, `workspace.current_dir`, `git.branch`, metrics, `vim.mode`. Near-identical shape to Claude's |
| 3   | **OpenAI Codex CLI**      | No (fixed items only)                | `tui.status_line` in `config.toml` is an ordered list of built-in identifiers. Issue #17827 explicitly requests Claude-style custom commands; not yet implemented                                                                                   |
| 4   | **GitHub Copilot CLI**    | No (requested)                       | Prompt glyph hardcoded; issues #1311, #2329 request it; `avatorl/copilot-cli-statusline` (PowerShell) is an external workaround                                                                                                                     |
| 5   | **Gemini CLI**            | No                                   | Footer shows a fixed configurable list of item IDs; no stdin-JSON custom command                                                                                                                                                                    |
| 6   | **Aider**                 | No                                   | No status line API; only CLI/YAML config flags                                                                                                                                                                                                      |
| 7   | **opencode (sst)**        | No                                   | `tui.json` exposes theme, keybinds, diff style; no custom status command                                                                                                                                                                            |
| 8   | **Cline**                 | No                                   | VSCode status bar item only; open community request #2867 for customization                                                                                                                                                                         |
| 9   | **Cursor**                | No (user-space only)                 | IDE status bar; third-party extensions like `cursor-stats` add indicators, but no first-class JSON-in/text-out hook                                                                                                                                 |
| 10  | **Windsurf**              | No                                   | IDE hover UI; no user-scriptable status line                                                                                                                                                                                                        |
| 11  | **Continue.dev**          | No                                   | Status bar item is a toggle button only                                                                                                                                                                                                             |
| 12  | **Cody (Sourcegraph)**    | No                                   | Built-in status bar icon; no custom command hook                                                                                                                                                                                                    |
| 13  | **Warp terminal**         | Partial (non-AI)                     | Custom "context chips" are shell-prompt customization, not AI-session JSON hook                                                                                                                                                                     |
| 14  | **Roo Code**              | No                                   | Standard VSCode settings; no status-line API                                                                                                                                                                                                        |
| 15  | **Goose (block)**         | No                                   | Theme/priority env vars only (`GOOSE_CLI_THEME`, `GOOSE_CLI_MIN_PRIORITY`)                                                                                                                                                                          |
| 16  | **Crush (charmbracelet)** | No                                   | Bubble Tea TUI with built-in progress bar; no hook                                                                                                                                                                                                  |

### Emerging de-facto standard

- **No formal cross-tool spec exists**, but Qwen Code's `ui.statusLine` is a **deliberate clone** of Claude's contract — same `type: "command"` shape, JSON-on-stdin, text-on-stdout. That's two tools with a compatible contract today.
- **Codex CLI issue #17827** explicitly proposes adopting Claude's shape (`statusLine` in `config.toml`, JSON stdin, ANSI-text stdout). Issues #13660 and #14043 are related. Community project `fwyc0573/codex-hud` emulates it externally.
- **GitHub Copilot CLI** has parallel requests (#1311, #2329) with `avatorl/copilot-cli-statusline` as out-of-band workaround.
- **Gemini CLI** has no extensibility point suitable for emulating a custom status line — custom slash commands are prompt-injection TOML files, not stdout renderers.

### IDE-based tools are structurally out of scope

Cline, Cursor, Windsurf, Continue, Cody, Roo Code — all are IDE plugins without a stdin-JSON contract and unlikely to grow one. Different category of tool.

## Conclusions

This is becoming a **cross-tool standard**, not a Claude-only feature. Today:

- 2 tools ship the contract (Claude, Qwen)
- 2 more have active feature requests adopting Claude's shape (Codex, Copilot)

A tool with Claude-specific design locks itself out of half the likely future market. Conversely, a tool designed around the **union schema** with per-vendor normalizers is:

- Fully functional for Claude Code today
- Trivially functional for Qwen today (schema is already ~95% compatible)
- Ready to absorb Codex and Copilot the day their features ship

## Implications / actions

- Drives [ADR-0002: Name linesmith](../adrs/0002-name-linesmith.md) — standalone tool-agnostic brand, not `cc`-prefixed
- Drives [ADR-0006: Tool-Agnostic JSON Schema](../adrs/0006-tool-agnostic-json-schema.md) — union of Claude + Qwen fields with thin per-tool normalizer
- Ship Claude preset at v0.1; Qwen preset is nearly free (can ship alongside)
- Stub Codex / Copilot presets that activate the day those tools ship their APIs

## Open questions

- Will Codex land a Claude-compatible contract or invent a different shape? (Issue text suggests Claude-compatible; needs tracking)
- Will Gemini CLI eventually add extensibility? Currently the only mainstream CLI with no path forward.
- Is there value in being "the tool that works across Claude / Qwen / Codex / Copilot" as a marketing angle, or do users only care about the one tool they use?
