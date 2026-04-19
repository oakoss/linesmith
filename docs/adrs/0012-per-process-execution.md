# Ship linesmith as a per-process invocation; defer daemon mode to v0.2+

- Status: accepted
- Date: 2026-04-18
- Deciders: Jace

## Context and Problem Statement

Claude Code's statusline protocol spawns the statusline binary per prompt (300ms debounced). This invocation model invites two execution strategies: a short-lived per-process binary that does its work and exits, or a long-lived daemon with a thin client that sends the stdin payload over RPC and receives the rendered line. The daemon model has real advantages (shared caches across CC sessions, file-watch cache invalidation, sub-millisecond client latency) but meaningful costs (IPC protocol, lifecycle management, crash recovery, version skew). Which execution model should v0.1 ship?

## Decision Drivers

- Time-to-v0.1: the repo is in bootstrap phase with no Rust binary yet; scope creep is existential
- Cold-start budget <20ms — per-process model must hit this on its own merits
- Competitor precedent — what execution model do the tools with existing users use?
- Operational complexity for end users — a daemon introduces "is it running?" support questions
- Reversibility — can we switch to daemon mode later without rewriting segment code?

## Considered Options

- **Per-process invocation (chosen)**: each statusline call spawns a fresh `linesmith` binary; caches live in process memory + on-disk files; no persistent state machine
- **Daemon from day one**: long-lived user-scoped daemon (`~/.cache/linesmith/daemon.sock`); statusline binary is a thin RPC client; daemon owns all caches and file-watches
- **Hybrid: optional daemon**: per-process default, daemon as opt-in via `linesmith daemon start`

## Decision Outcome

Chosen option: **per-process invocation for v0.1**, because (a) every Claude Code statusline tool with existing users ships per-process (ccstatusline, CCometixLine, claudia-statusline, claude-powerline — four different language choices, identical model), (b) the data-fetching architecture in [ADR-0010](0010-data-fetching-architecture.md) hits the <20ms cold-start target without a daemon, and (c) adding a daemon now creates four hard subsystems (IPC, lifecycle, version skew, crash recovery) that each warrant their own research + design work before a binary even exists.

Daemon mode is documented as the v0.2+ escape hatch. Data-fetching patterns from ADR-0010 (segment-driven lazy loading, `DataContext` with `OnceCell`+`Arc` sharing) compose cleanly with a future daemon — the daemon would own the `DataContext` and serve renders via RPC without changing segment code.

### Consequences

- Good, because v0.1 scope stays focused on shipping a working statusline, not building process-management infrastructure
- Good, because operational model matches every existing Claude Code statusline tool — no novel "where's the daemon" support burden
- Good, because every invocation sees a clean credential/config/cache state — no stale state accumulating across long-running processes
- Good, because data-fetching architecture is daemon-ready: `DataContext`'s lazy-loaded fields and segment dependency declarations port cleanly to a daemon owning those fields
- Bad, because cross-process OAuth endpoint coordination requires the disk lock file from [ADR-0011](0011-rate-limit-data-source.md) rather than a daemon-held single cache
- Bad, because users with many concurrent CC sessions pay startup cost per prompt per session (vs a daemon serving all of them from warm caches)
- Bad, because file-watch invalidation (via `notify`) isn't viable without a persistent process — we fall back to mtime-stat polling
- Neutral, because the reversibility test passes: moving to daemon later is an additive architectural decision, not a rewrite

### Confirmation

Revisit if any of these fire:

- Cold-start measurements in real-world use consistently exceed the <20ms target despite partial-struct parsing ([ADR-0009](0009-json-parsing-stack.md)) and lazy loading ([ADR-0010](0010-data-fetching-architecture.md))
- Multiple concurrent CC sessions consistently thunder-herd the OAuth endpoint despite the disk lock file from [ADR-0011](0011-rate-limit-data-source.md)
- User demand emerges for features that require a long-lived process (e.g., live burn-rate updates between prompts)
- A competitor ships daemon mode and demonstrates a qualitative UX improvement (sub-ms updates, cross-session sync)

File a P4 bead when the scaffold compiles so the escape hatch isn't forgotten: "Evaluate daemon mode if cold-start regression observed in production use."

## Pros and Cons of the Options

### Per-process invocation (chosen)

- Good: simple — no IPC, no lifecycle, no crash recovery
- Good: matches every existing Claude Code statusline tool's model
- Good: every invocation starts clean; no persistent-state bugs
- Good: operationally invisible to end users
- Bad: startup cost paid every prompt
- Bad: caches are file-scoped, not process-scoped — slightly less efficient than memory caches

### Daemon from day one

- Good: sub-millisecond client latency
- Good: shared caches across all CC sessions; OAuth endpoint hit once per machine per 180s
- Good: file-watch invalidation (via `notify`) replaces mtime-stat polling
- Bad: four hard subsystems (IPC protocol, lifecycle, version skew, crash recovery) — each its own epic
- Bad: zero competitor precedent — novel UX burden for end users
- Bad: permission edge cases (different `$USER`, sudo, multi-user systems)
- Bad: daemon updates require coordinated client/daemon restart
- Bad: kills v0.1 scope

### Hybrid: optional daemon

- Good: users who want the daemon can opt in without forcing complexity on others
- Bad: ships both code paths — same complexity as "daemon from day one" plus the per-process path
- Bad: bifurcates support ("did you try with the daemon?")
- Bad: premature — solves a problem we haven't measured

## More Information

- Primary source: [`docs/research/data-fetching-strategy.md`](../research/data-fetching-strategy.md) §10 — daemon-mode tradeoffs and architecture sketch
- Supporting: session discussion 2026-04-18 covering multi-CC-session daemon architecture (per-session state keyed by session_id; shared OAuth cache)
- Related: [ADR-0010](0010-data-fetching-architecture.md) — data-fetching architecture that composes with both per-process and future daemon modes
- Competitor reference: ccstatusline (TS, per-process), CCometixLine (Rust, per-process), claudia-statusline (Rust+SQLite, per-process), claude-powerline (TS, per-process) — all ship per-process models
