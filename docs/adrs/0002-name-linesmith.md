# Name the project `linesmith`

- Status: accepted
- Date: 2026-04-17
- Deciders: Jace

## Context and Problem Statement

We need a product name. The obvious candidate (`ccstatusline`) collides with the dominant npm package (`sirmalloc/ccstatusline`, 7.7k stars). The name shapes positioning: a `cc` prefix locks us to Claude Code, while a standalone brand opens the door to tool-agnostic positioning as Qwen, Codex, and Copilot adopt the same contract (see `research/cross-tool-statusline-support.md`).

## Decision Drivers

- No conflict on crates.io, npm, or GitHub repo namespace
- Tool-agnostic positioning — the statusline contract is becoming a cross-vendor standard
- Memorable, pronounceable, searchable as a unique term
- Narrative fit — "forge/craft your statusline" aligns with our extensibility-first positioning
- Brand room to grow (potential for `linesmith-themes`, `linesmith-plugins`, etc.)
- GitHub org (`oakoss`) carries the umbrella brand; product name can focus on function

## Considered Options

- `ccbar`, `ccline`, `cchud` — `cc`-prefixed
- `statline`, `statsmith` — descriptive standalone
- `linesmith` — craft-themed standalone
- `bladeline` — Rust-themed, distinctive but aggressive
- Other abstract brands (`cairn`, `lumen`, `glyph`, `vantage`) — mostly taken on crates.io
- Invented portmanteaus (`ccforge`, `ccink`, `ccstripe`, `ccglimmer`, etc.)

## Decision Outcome

Chosen option: **`linesmith`** (shipped as `oakoss/linesmith`), because it satisfies every decision driver: it's available across crates.io, npm, and GitHub; the craft narrative ("smith your statusline") aligns with our extensibility/plugin positioning; it's tool-agnostic as the ecosystem standardizes on the `statusLine` contract; and as a unique term it will own its own SEO over time.

### Consequences

- Good, because the name is available everywhere and we own a clean namespace
- Good, because the craft theme reinforces "build/customize/extend" — the core differentiator
- Good, because we're positioned for Qwen/Codex/Copilot as they ship compatible APIs
- Good, because `linesmith-themes` / `linesmith-plugins` extensions fit the brand naturally
- Bad, because zero-discovery SEO today — a new Claude Code user searching "status line" won't find us via organic search initially
- Bad, because `cc`-prefixed tools benefit from the Claude Code search space; we'll need to lean on docs, READMEs, and community presence
- Neutral, because beads issue prefix is `lsm-` (8-char limit) — an acceptable abbreviation

### Confirmation

Revisit if:

- The cross-tool statusline standard stalls and Claude Code remains the only tool with a rich API for 12+ months
- User feedback strongly signals confusion with other `line*` projects (unlikely but possible)
- SEO struggle becomes persistent after v0.1 launch despite docs investment

## Pros and Cons of the Options

### `linesmith`

- Good: available across crates.io, npm, GitHub; craft narrative; tool-agnostic; unique term
- Good: 9 characters — pronounceable, typable
- Good: "smith" is productive for ecosystem naming (themes, plugins, presets)
- Bad: zero initial SEO discoverability
- Bad: "line" is generic; possible future name collision with vim plugins

### `ccbar`, `ccline`, `cchud` (cc-prefixed)

- Good: immediately searchable for Claude Code users
- Bad: all three have multiple existing Claude Code statusline repos (crowded namespace — see competitor research)
- Bad: locks us to Claude Code as the ecosystem goes cross-tool
- Bad: users would confuse us with the npm ccstatusline tool

### `statline` / `statsmith`

- Good: descriptive, obvious purpose
- Bad: "statline" competes with generic vim/tmux statusline results
- Bad: weaker as a standalone brand

### `bladeline`

- Good: Rust-flavored, distinctive
- Bad: tonally aggressive; may not age well
- Bad: unfamiliar — requires users to memorize without hook

### Abstract brands (`cairn`, `lumen`, `glyph`, `vantage`, etc.)

- Good: evocative, strong brands
- Bad: all taken on crates.io

## More Information

- Driven by: `research/cross-tool-statusline-support.md` (tool-agnostic positioning), `research/competitor-landscape.md` (cc-namespace crowding)
- GitHub: [oakoss/linesmith](https://github.com/oakoss/linesmith) (to be created)
- Beads issue prefix: `lsm-` (abbreviation, 4 chars)
