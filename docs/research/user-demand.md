# User Demand for Claude Code Status Lines

- Date: 2026-04-17
- Author: Claude Code research agent session
- Scope: Identify what Claude Code users actually want from a status line and what frustrates them about existing tools, so linesmith's feature priorities are grounded in real demand rather than assumptions.

## Question

What features do Claude Code users want from a status line? What pain points do existing tools fail to solve? Rank by signal strength (independent-user reactions, duplicate issues, cross-source mentions).

## Sources

- [ccstatusline issues](https://github.com/sirmalloc/ccstatusline/issues) (100+ reviewed)
- [anthropics/claude-code issues](https://github.com/anthropics/claude-code/issues) (60+ statusline-tagged)
- [Hacker News](https://news.ycombinator.com/item?id=47660817)
- Community blog posts: [dandoescode.com](https://www.dandoescode.com/blog/claude-code-custom-statusline), [jeradbitner.com](https://jeradbitner.com/blog/claude-code-statusline), [aihero.dev](https://www.aihero.dev/creating-the-perfect-claude-code-status-line), [ovidiueftimie.substack.com](https://ovidiueftimie.substack.com/p/claude-code-status-lines-that-actually), [felipeelias.github.io](https://felipeelias.github.io/2026/03/17/claude-statusline.html)
- Competing tools: [claude-powerline](https://github.com/Owloops/claude-powerline), [CCometixLine](https://github.com/Haleclipse/CCometixLine), [claude-code-usage-bar](https://github.com/leeguooooo/claude-code-usage-bar), [ccusage](https://ccusage.com/guide/statusline)

Reddit was partially bot-blocked; signal summarized via indexed snippets rather than direct thread content. X/Twitter yielded no distinctive voice beyond GitHub cross-posts.

## Findings

### Most-requested features (ranked by signal strength)

1. **Rate-limit / plan-usage quota in statusline JSON**: **the loudest signal in the entire corpus.**
   - [anthropics/claude-code#8412](https://github.com/anthropics/claude-code/issues/8412) has **44 thumbs-up reactions**
   - 10+ duplicate issues: #15844 (14 reactions), #22221 (11), #30341, #34074, #35747, #37338, #38946, #48279, #46329, #47574
   - Users want session-5h + weekly-7d % exposed so lines can show `Usage 92% · resets in 2h 13m` ([#41739](https://github.com/anthropics/claude-code/issues/41739))
   - Today people scrape API response headers or `ccusage`; Anthropic hasn't shipped it, which is the #1 sore spot

2. **Effort / thinking-mode level in JSON**: **second-highest dupe count.**
   - [#49630](https://github.com/anthropics/claude-code/issues/49630), #49754, #45786, #44842, #41985, #39399, #38392, #37764, #37701, #36187, #31415, #41049, #42016 (13+ separate asks)
   - Users want live `/effort` level (low/medium/high/max/xhigh) visible
   - ccstatusline has an `Effort` widget but [#239](https://github.com/sirmalloc/ccstatusline/issues/239) shows it doesn't update when user runs `/effort max`; CC never re-emits

3. **Accurate context-window tracking**: repeatedly broken.
   - ccstatusline #233, #251, #164, #171, #180, #193, #146, #109, #92, #100 all report wrong % for 1M-context Opus/Sonnet, post-`/compact`, post-`/resume`, or during 429s (#97, #204)
   - Users say: "the context %, when it works, is the #1 reason I installed this" ([jeradbitner.com](https://jeradbitner.com/blog/claude-code-statusline), [aihero.dev](https://www.aihero.dev/creating-the-perfect-claude-code-status-line))

4. **Git worktree awareness**: heavily requested, only partially served.
   - ccstatusline #176 (git detection failed in worktrees), [#190](https://github.com/sirmalloc/ccstatusline/issues/190) (**model leaks across worktrees**: 4 reactions, still open)
   - [dandoescode.com](https://www.dandoescode.com/blog/claude-code-custom-statusline) and [felipeelias.github.io](https://felipeelias.github.io/2026/03/17/claude-statusline.html) frame worktrees as the core motivating use case
   - Anthropic added `workspace.git_worktree` in 2.1.x but cross-session isolation (per-worktree model, branch, cost) remains broken

5. **Presets / themes / zero-config onboarding.**
   - [ccstatusline#42](https://github.com/sirmalloc/ccstatusline/issues/42) (5 reactions), #226 (4), #277 (4)
   - Universal complaint: "great TUI, but I want to copy a preset and go." No tool ships a gallery.

6. **Burn-rate + reset countdown + survival projections.**
   - [anthropics/claude-code#43271](https://github.com/anthropics/claude-code/issues/43271) (AndrewTKent's `claude-statusline` mockup), [leeguooooo/claude-code-usage-bar](https://github.com/leeguooooo/claude-code-usage-bar), ccusage all converge on: `$5.30/h · →full ~52m · ✓18m`
   - Under-served niche

7. **Active-skill / active-agent / MCP visibility.**
   - [anthropics/claude-code#16078](https://github.com/anthropics/claude-code/issues/16078) (22 reactions), [ccstatusline#114](https://github.com/sirmalloc/ccstatusline/issues/114) (7), #299, #40739
   - Power users running many custom skills/subagents have no indicator of what's firing
   - `claude-hud` (19.8k stars) exists specifically to fill this gap

8. **Cache-hit / cache-creation tokens.**
   - ccstatusline #271, #21, #300 (5-minute prompt-cache expiry countdown)
   - Power users chasing cost want to see when cache is about to evaporate

### Top complaints about existing tools

- **`npx -y ccstatusline@latest` is a supply-chain liability AND a CPU hog**
  - [ccstatusline#298](https://github.com/sirmalloc/ccstatusline/issues/298): auto-executing `@latest` with no review (4 reactions)
  - [#103](https://github.com/sirmalloc/ccstatusline/issues/103) + [#22](https://github.com/sirmalloc/ccstatusline/issues/22) + [chongdashu/cc-statusline PR #4](https://github.com/chongdashu/cc-statusline/pull/4): **30+ concurrent node processes, 3GB RAM, 300% CPU** from rapid spawning
  - Everyone who's been bitten asks for a **persistent daemon** or **native binary**; hence CCometixLine in Rust gaining traction

- **Context/usage percentages are wrong more often than right** (see signal #3 above)

- **Terminal/rendering brittleness**
  - Truncation at large fonts (#312)
  - Powerline glyph corruption (#162, #31)
  - Windows Unicode (#186, #62)
  - IntelliJ/node-pty width bugs (#67, #262)
  - "No git" shown inside worktrees (#35, #176)
  - Cross-terminal reliability is the **most-filed bug category**

- **TUI depth vs. overwhelm**: reviewers explicitly warn "resist adding excessive widgets." TUI is loved for depth but has no defaults.

- **Doesn't refresh on lifecycle events.** Anthropic-side issues #37163 (after compact), #36683 (during long turns), #40362 (after `/clear`), #48445 (refreshInterval doesn't repaint), #29411 (resumed sessions).

### Under-served use cases

- **Per-worktree isolated state**: no tool survives parallel worktrees on different models
- **Usage projections with "survive" indicator**: only AndrewTKent's mockup in #43271
- **Clickable OSC-8 hyperlinks** for branch/PR/plan file (partial in ccstatusline #188; broken in tmux per #37216)
- **Account/provider indicator**: ccstatusline #47, #64 (API vs Pro vs Max vs Bedrock/Vertex)
- **Plan-mode file + progress widget**: #41906
- **Last-task duration** (p10k-style): ccstatusline #77

### Surprising insights

- **Energy/CO2 indicator has real demand** (ccstatusline #247, 2 reactions, cites arXiv paper)
- **Peak/off-peak usage** ([#243](https://github.com/sirmalloc/ccstatusline/issues/243)): Anthropic's double-usage off-peak promos created a scheduling-aware widget need no one anticipated
- **Users actively distrust npx-based installs**: #298 is framed as blocking, not theoretical. Statically-compiled binary (Rust/Go) is now a **competitive moat, not a nice-to-have**
- **The control channel is one-way**: [#44245](https://github.com/anthropics/claude-code/issues/44245) asks to let the statusline _set_ session color/name back to CC; no tool can do this
- **"Self-aware" status messages** ([#40453](https://github.com/anthropics/claude-code/issues/40453)): people use interrupt just to ask "are you stuck?". A heartbeat/what-am-I-doing signal would displace that behavior

## Conclusions

The ecosystem's top-requested features cluster around **correctness** (rate-limits, context %, effort level, worktree isolation) more than **features** (new widget types). The #1 and #3 complaints are _the same data being shown wrong_, not _missing functionality_.

Supply-chain and resource concerns (npx spawns, RAM) are real enough that users are migrating to Rust tools even at cost of feature depth. Presets and onboarding are under-served across the board.

## Implications / actions

- Drives [ADR-0001: Use Rust](../adrs/0001-use-rust-for-runtime.md): native binary isn't just a perf win, it's trust
- Drives linesmith's **correctness-first** positioning for v0.1:
  - Context % accurate across 1M, `/compact`, `/resume`, 429s
  - Rate-limit scraping from API headers like ccusage does
  - Worktree isolation as a primary concept
- Ship 5-10 curated presets from v0.1 — onboarding gap is massive
- Consider `claude-hud`-style live event segments as a stretch goal after core stats land
- Cache-hit countdown is a small-signal but cheap win if we're already parsing the token fields

## Open questions

- What proportion of ccstatusline's 7.7k users are on Pro/Max vs API-key? (affects rate-limit feature prioritization)
- Is Anthropic planning to expose rate limits natively in the statusline JSON? (#8412) — if yes, timeline affects our header-scraping priority
- Do `claude-hud` users also run a statusline, or is it one-or-the-other?
