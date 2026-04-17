# Competitor Landscape: Claude Code Status Line Tools

- Date: 2026-04-17
- Author: Claude Code research agent session
- Scope: Survey existing status line implementations for Claude Code (and adjacent shell prompt tools) to identify strengths, gaps, and differentiation opportunities for linesmith.

## Question

What status line tools exist for Claude Code today? What do the leaders do well, what's missing, and what's worth borrowing from adjacent shell-prompt tools like Starship and Powerlevel10k?

## Sources

- [ccstatusline (sirmalloc)](https://github.com/sirmalloc/ccstatusline)
- [CCometixLine (Haleclipse)](https://github.com/Haleclipse/CCometixLine)
- [claude-powerline (Owloops)](https://github.com/Owloops/claude-powerline)
- [cc-statusline (chongdashu)](https://github.com/chongdashu/cc-statusline)
- [claude-powerline-rust (david-strejc)](https://github.com/david-strejc/claude-powerline-rust)
- [claudia-statusline (hagan)](https://github.com/hagan/claudia-statusline)
- [claude-statusline (felipeelias)](https://github.com/felipeelias/claude-statusline)
- [claude-statusline-powerline (spences10)](https://github.com/spences10/claude-statusline-powerline)
- [ClaudeCodeStatusLine (daniel3303)](https://github.com/daniel3303/ClaudeCodeStatusLine)
- [oh-my-claude (ssenart)](https://github.com/ssenart/oh-my-claude)
- [claude-code-statusline (rz1989s)](https://github.com/rz1989s/claude-code-statusline)
- [claude-statusline (hell0github)](https://github.com/hell0github/claude-statusline)
- [claude-hud (jarrodwatts)](https://github.com/jarrodwatts/claude-hud)
- [Starship](https://starship.rs) · [Powerlevel10k](https://github.com/romkatv/powerlevel10k) · [Oh My Posh](https://ohmyposh.dev)

## Findings

### The leader: ccstatusline (reference)

- **7.7k stars**, TypeScript, Node+Bun, 47 contributors
- Settings in `~/.config/ccstatusline/settings.json`, managed by a React/Ink TUI
- Widget catalog: Model, Session (cost/duration/skills), Token (input/output/speed/weekly/context), Git (17 variants incl. PR links, worktrees, forks, OSC8 IDE links), System (memory, vim mode, account email)
- **Strengths:** deepest widget catalog, powerline + flex separators, multi-line support, truecolor, auto Nerd Font install
- **Weaknesses:** no plugin API — widgets are hardcoded in TS. Cold-start via `npx` can be slow. Theming only via in-app builder.

### Rust contenders (performance-focused)

- **CCometixLine** — 2.7k stars, Rust, TOML config, 5 themes. Fast, interactive TUI, Claude Code patcher. Weak: only 4 default segments, no VCS beyond git.
- **claudia-statusline** — 23 stars, Rust + TOML + SQLite. 11 themes, 5 layout presets, hook-based compaction detection (~600x faster than transcript parsing), optional Turso cloud sync. Most architecturally interesting Rust entry.
- **claude-powerline-rust** — 12 stars, Rust port of Owloops claiming 8.4x speed (1260ms→150ms) via SIMD JSON + parallel aggregation. Early stage.

### TypeScript alternatives

- **claude-powerline (Owloops)** — 1k stars, Node 18+, JSON config with XDG support, hot-reload, 11+ segments incl. tmux, native rate-limit polling, 6 themes, 4 styles (minimal/powerline/capsule/tui). Strong competitor to ccstatusline.
- **claude-statusline-powerline (spences10)** — 31 stars, JSON+IntelliSense schema, SQLite analytics, 12 themes, 9 separator styles. The JSON schema with IntelliSense is a standout idea.
- **cc-statusline (chongdashu)** — 566 stars, TS CLI that generates a self-contained shell script (~45-80ms). Great onboarding ("three questions"); weak extensibility.

### Shell & Go

- **felipeelias/claude-statusline** — Go, TOML, 6 presets (catppuccin, gruvbox-rainbow, tokyo-night). Single binary, zero deps. Best distribution story. Missing: TUI, persistent stats.
- **hell0github/claude-statusline** — 22 stars, bash, rigorous 3-stage architecture (collect→compute→render), atomic cache writes, weekly calibration.
- **daniel3303/ClaudeCodeStatusLine** — 422 stars, shell+PowerShell, strongest Windows/PS parity, real rate-limit API via OAuth.
- **ssenart/oh-my-claude** — 11 stars, shell + oh-my-posh theme file (reuses OMP renderer — clever).
- **rz1989s/claude-code-statusline** — 425 stars, bash, 227-setting `Config.toml`, 18 modules, sub-50ms cache, prayer-time integration (unique).

### Different category: claude-hud

- **19.8k stars** (!), TS/JS plugin, three presets (Full/Essential/Minimal)
- Not a traditional statusline — shows active tools, running subagents, todo progress
- Proves demand extends beyond "pretty git branch" to **live event visibility**

### Adjacent: shell prompt tools worth learning from

- **Starship (Rust)** — module auto-activation based on context. Each module emits segments via a StringFormatter with variables/styles/conditionals. Weakness: slow custom-shell modules (Python 169ms vs git 29ms per user report). `starship explain` as a debug tool is worth copying.
- **Powerlevel10k** — _instant prompt_ + _gitstatus_ (persistent daemon) are the gold standard for perceived perf. Async segment rendering and cache-then-update beat "make everything fast synchronously." Flaw: tight coupling / private APIs make extension hard.
- **Oh My Posh** — portable JSON theme format is why `oh-my-claude` can reuse it. Lesson: theme interchange format > bespoke DSL.

### Comparison matrix

| Tool                  | Stars | Lang          | Config        | Segments     | Themes              | Plugin API        | Perf                    | Notable               |
| --------------------- | ----- | ------------- | ------------- | ------------ | ------------------- | ----------------- | ----------------------- | --------------------- |
| ccstatusline          | 7.7k  | TS (Node/Bun) | JSON+TUI      | 40+          | truecolor+powerline | No                | Medium (npx cold-start) | Widest widget set     |
| CCometixLine          | 2.7k  | Rust          | TOML          | 4+custom     | 5 built-in          | No                | Fast                    | CC patcher            |
| claude-powerline      | 1.0k  | TS (Node18+)  | JSON+XDG      | 11+          | 6                   | Shell-append only | Medium                  | Hot reload, tmux      |
| cc-statusline         | 566   | TS→shell      | Generated .sh | ~8           | None                | No                | ~45-80ms                | Easiest setup         |
| rz1989s               | 425   | bash          | TOML (227)    | 18           | 3                   | Module system     | <50ms (cache)           | Prayer times          |
| daniel3303            | 422   | bash+PS       | JSON          | ~7           | color-coded         | No                | Fast                    | Real rate-limits      |
| spences10             | 31    | TS            | JSON+schema   | 6+           | 12                  | No                | Medium                  | IntelliSense schema   |
| claudia-statusline    | 23    | Rust          | TOML+SQLite   | ~10          | 11                  | Hook-based        | Very fast               | Cloud sync            |
| hell0github           | 22    | bash          | JSON          | ~8           | multi-layer         | No                | Fast                    | 3-stage arch          |
| oh-my-claude          | 11    | shell         | OMP JSON      | 7            | via OMP             | via OMP           | Medium                  | Reuses OMP            |
| claude-powerline-rust | 12    | Rust          | JSON+CLI      | ~10          | 5                   | No                | 150ms                   | SIMD JSON             |
| felipeelias           | low   | Go            | TOML          | 5+2          | 6                   | No                | Fast                    | Single binary         |
| claude-hud            | 19.8k | TS/JS         | JSON          | event-driven | 256-color           | plugin itself     | Medium                  | Tool/agent visibility |

## Conclusions

The ecosystem is crowded but not saturated. Critical gaps across **all** tools:

1. **No real plugin API** — every tool hardcodes widgets. Users file issues asking for custom segments and are told "PR welcome to the core."
2. **No shared theme format** — each tool reinvents TOML/JSON schemas.
3. **No async/daemon model** like gitstatus — cold-start dominates on Node-based tools.
4. **Weak `claude-hud`-style event visibility** — stats only, no tool/agent activity.
5. **Uneven Windows support.**
6. **Cost/rate-limit data sources are inconsistent** — JSONL parsing vs native API vs ccusage.

The leaders (ccstatusline, claude-powerline) have the most segments but the weakest architecture for extension. The Rust contenders have the best perf but the thinnest feature sets. Nobody has combined depth + extensibility + performance.

## Implications / actions

- Supports [ADR-0001: Use Rust](../adrs/0001-use-rust-for-runtime.md) — Rust tools gaining share is a signal not a coincidence
- Supports [ADR-0003: Segment/Widget System](../adrs/0003-segment-widget-system.md) with rich layout capabilities
- Supports [ADR-0004: Rhai for Plugins](../adrs/0004-rhai-for-plugins.md) — the plugin-API gap is the biggest architectural opportunity
- Inspires borrowing from Starship: `explain` command, module auto-activation, StringFormatter
- Inspires borrowing from p10k: cache-then-update pattern for expensive segments

## Open questions

- Is the `claude-hud` "live events" signal an orthogonal product or something we should absorb as a segment category?
- Will users abandon TypeScript-based tools _en masse_ once Rust equivalents catch up on features, or does the ccstatusline install base create lock-in?
- Can we build a theme format portable enough that `oh-my-claude`-style reuse of Oh My Posh themes becomes viable?
