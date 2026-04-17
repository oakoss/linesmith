# Use Rust for the linesmith runtime

- Status: accepted
- Date: 2026-04-17
- Deciders: Jace

## Context and Problem Statement

linesmith is a status line tool that will spawn as a fresh process on every Claude Code prompt (post-300ms debounce). Users complain vocally about existing Node/TypeScript implementations causing 30+ concurrent processes, 3GB RAM, and 300% CPU under rapid use. There is also a trust problem: `npx -y ccstatusline@latest` auto-executes unreviewed code on every invocation. What runtime should we build on?

## Decision Drivers

- Cold start budget <20ms
- Single-binary distribution (no auto-updating npm/npx execution)
- Supply-chain trustworthiness — users actively distrust `npx -y @latest`
- Competitive moat — Rust-based tools are gaining share on exactly this axis
- Plugin system fit — embedded scripting runtimes (rhai, mlua) integrate cleanly with Rust
- Long-term ecosystem bet — we're willing to trade dev velocity for a better ceiling

## Considered Options

- **Rust** — ~15ms cold start, ~3-5MB stripped binary, static single-binary
- **TypeScript + Bun** (`bun build --compile`) — ~30ms cold start, ~50MB binary, keeps TS iteration speed
- **Go** — ~20ms cold start, ~10MB binary, simpler than Rust but no skill reuse
- **Node** — ~150ms cold start, needs runtime installed, supply-chain risk
- **Shell (bash/zsh)** — fastest to write, no portability story, terminal-parsing brittleness

## Decision Outcome

Chosen option: **Rust**, because it's the only option that simultaneously hits the <20ms cold-start budget, produces a single static binary under 10MB, and gives us first-class integration with embedded plugin runtimes (rhai). The supply-chain concern is meaningful enough that Rust's distribution story is not just a nice-to-have — it's a competitive differentiator that users are actively seeking out (see `research/user-demand.md` and the migration toward CCometixLine, claudia-statusline, felipeelias's Go tool).

### Consequences

- Good, because cold start will be the fastest in the ecosystem (<20ms target)
- Good, because a single ~3-5MB static binary eliminates the `npx -y @latest` complaint entirely
- Good, because the `gix` + `rhai` + `owo-colors` + `serde_json` stack is proven and pure-Rust, which simplifies cross-compilation via `cross`
- Good, because Rust cross-compilation via cargo-dist is mature — macOS universal, Linux glibc+musl, Windows MSVC all ship cleanly
- Bad, because development velocity is 2-3x slower than TypeScript, especially for UI-adjacent work
- Bad, because the plugin authoring story for user-written code in native Rust is much heavier than embedded scripting — we mitigate this in [ADR-0004](0004-rhai-for-plugins.md) with rhai
- Bad, because community contributors familiar with TypeScript statuslines won't trivially port over
- Neutral, because binary size (~3-5MB) sits between Go (~10MB) and C — not a differentiator either way

### Confirmation

Revisit if:

- Observed cold start exceeds 30ms in realistic benchmarks (segments + cache + plugin execution)
- Binary size exceeds 10MB after feature gating
- Plugin authoring friction in rhai prevents meaningful community contribution (in which case revisit [ADR-0004](0004-rhai-for-plugins.md), not this ADR)

## Pros and Cons of the Options

### Rust

- Good: fastest cold start, smallest static binary, native plugin runtime fit
- Good: `cargo-dist` ecosystem is mature for multi-platform releases
- Good: existing Rust crates (`gix`, `rhai`, `owo-colors`) cover our needs without heavy dependencies
- Bad: steeper dev learning curve, slower iteration
- Bad: fewer potential contributors from the existing Claude Code statusline community

### TypeScript + Bun

- Good: 80% of the Rust perf win with 20% of the effort
- Good: keeps TS iteration speed; plugin authors write familiar TS
- Good: Bun's `--compile` produces a ~50MB standalone binary — kills the npx complaint
- Bad: ~30ms cold start vs Rust's ~15ms — noticeable at the margin
- Bad: ~50MB binary vs ~5MB Rust — 10x larger, matters for Homebrew/distribution perception
- Bad: Bun ecosystem is still maturing; edge cases around FFI and platform binaries

### Go

- Good: fast cold start (~20ms), simple language, mature cross-compilation
- Good: single static binary (~10MB), no runtime dependency
- Bad: no Rust skill reuse for Jace (currently working on other Rust projects)
- Bad: weaker embedded scripting story (yaegi exists but is heavier than rhai)
- Bad: less satisfying to work in for this project scope

### Node (plain)

- Good: ubiquitous, every Claude Code user has it
- Bad: 150ms cold start — unacceptable
- Bad: the exact stack whose supply-chain and resource problems motivated this project
- Bad: no static binary without heavy tooling (pkg/nexe are deprecated or problematic)

### Shell (bash/zsh)

- Good: fastest possible to prototype, zero deps
- Bad: no plugin story
- Bad: cross-platform is miserable (Windows PowerShell fork required)
- Bad: JSON parsing via `jq` adds dependency, and string manipulation is brittle
- Bad: no path to ship rich features (caching, themes, segment composition)

## More Information

- Driven by: `research/user-demand.md` (npx complaints, Rust migration trend), `research/competitor-landscape.md` (Rust tools gaining share), `research/rust-crate-survey.md` (stack viability)
- Related ADRs: [ADR-0004](0004-rhai-for-plugins.md) (plugin runtime), [ADR-0007](0007-cargo-dist-distribution.md) (distribution tooling)
