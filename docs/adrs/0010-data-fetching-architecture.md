# Fetch data with segment-driven lazy loading, mtime file caching, and byte-offset JSONL tails

- Status: accepted
- Date: 2026-04-18
- Deciders: Jace

## Context and Problem Statement

linesmith runs per-prompt with a 300ms debounce and a <20ms cold-start target. The data sources we want to surface span wildly different read costs: sub-millisecond file reads, 50-200ms Keychain subprocesses, rate-limited HTTP. How do we structure data fetching so the statusline stays fast, respects Anthropic's API rate limits, handles JSONL growth without re-reading full transcripts, and stays correct when cache invalidates mid-session?

## Decision Drivers

- Cold-start budget <20ms per invocation
- OAuth `/api/oauth/usage` endpoint is rate-limited — must not be called unguarded per prompt
- JSONL transcripts grow without bound (MB+ on long sessions) — naive full-file read destroys the budget
- Different sources have different freshness requirements (stdin always fresh; rate-limit data valid for minutes; credentials stable for process lifetime)
- Multiple CC sessions on one machine can hit the API concurrently — cross-process coordination needed for the endpoint
- Contributor friendliness: the pattern should use conventional Rust idioms, not a custom async runtime stack

## Considered Options

- **Eager-load everything**: fetch every source at startup. Simple; violates cold-start budget the first time any expensive source is needed
- **Segment-driven lazy loading**: enabled segments declare their data dependencies; runtime fetches only the union. Skip anything unused
- **Daemon with persistent caches**: long-lived process owns caches; thin client spawns per prompt. Fast but adds IPC, lifecycle, crash recovery as new subsystems (see [ADR-0012](0012-per-process-execution.md))
- **Async I/O with tokio**: parallelize network + file reads via async runtime. Adds ~100ms cold-start from runtime init; async doesn't help when our network call is a single endpoint

## Decision Outcome

Chosen option: **segment-driven lazy loading with a tiered cache strategy per source**, because it kills the biggest cost (unnecessary OAuth endpoint calls) before optimizing anything else, matches the per-process execution model ([ADR-0012](0012-per-process-execution.md)), and uses conventional sync I/O that contributors recognize.

### Per-source strategy

| Source                                   | Read cost               | Strategy                                                                                                 |
| ---------------------------------------- | ----------------------- | -------------------------------------------------------------------------------------------------------- |
| Stdin payload                            | ~0ms                    | Parse once; always fresh                                                                                 |
| `~/.claude/settings.json` + overlays     | <1ms                    | `fs::metadata().modified()` + reparse only on change                                                     |
| `~/.claude.json` (~125KB)                | 5-20ms                  | Same mtime pattern; partial struct per [ADR-0009](0009-json-parsing-stack.md)                            |
| `~/.claude/sessions/{pid}.json`          | <1ms                    | mtime cached                                                                                             |
| JSONL transcripts                        | 10ms-1s+                | **Byte-offset incremental tail** via `BufReader::read_line`                                              |
| macOS Keychain via `security` subprocess | 50-200ms                | Memoize for entire process lifetime                                                                      |
| `~/.claude/.credentials.json` (Linux)    | <1ms                    | Memoize for entire process lifetime                                                                      |
| OAuth `/api/oauth/usage`                 | 100ms-1s + rate-limited | **Multi-tier cache:** memory (180s) + disk data (180-300s) + disk lock (30s) + 429 `Retry-After` backoff |

### Runtime shape

```text
DataContext {
  stdin:       StdinPayload,                   // always loaded
  settings:    OnceCell<Arc<Settings>>,        // lazy, mtime-cached
  claude:      OnceCell<Arc<ClaudeJson>>,      // lazy, mtime-cached
  jsonl:       OnceCell<Arc<JsonlAggregate>>,  // lazy, incremental tail
  usage:       OnceCell<Arc<UsageData>>,       // lazy, multi-tier cache
  credentials: OnceCell<Arc<Credentials>>,     // lazy, memoized
}
```

1. Runtime reads the enabled-segment list from the merged config
2. Each segment declares its data dependencies (e.g., `RateLimit5h` depends on `OAuthUsageData`; `Cost` depends only on stdin)
3. Runtime computes the union; fetches only sources in that union, in parallel where helpful
4. Each segment reads from the `DataContext` via shared `Arc<T>` references — one parse per source per process

### Consequences

- Good, because a config with only `model + cost + workspace` segments skips Keychain, the endpoint, and JSONL entirely — cold-start drops to a couple of ms
- Good, because the multi-tier OAuth cache stack (memory + file + lock + 429 backoff) is a direct port of ccstatusline's battle-tested pattern
- Good, because `mtime` caching makes file reads effectively free in the steady state (stat is microseconds; reparse only on change)
- Good, because byte-offset JSONL tailing turns O(file-size) reads into O(new-bytes) reads — the difference between 10ms and 1s on long sessions
- Good, because sync I/O keeps the cold-start path short and the code conventional
- Bad, because the segment-dependency declaration adds a small amount of indirection in the type system (each segment specifies its deps)
- Bad, because cross-process coordination via the disk lock file is messier than a daemon-mediated single cache; acceptable for v0.1
- Neutral, because the architecture composes cleanly with a future daemon mode (per ADR-0012) — daemon would own the `DataContext` and serve via RPC, but segment-dependency declarations and per-source strategies carry over

### Confirmation

Revisit if:

- Cold-start measurements show >20ms despite all optimizations above — signal that daemon mode ([ADR-0012](0012-per-process-execution.md)) needs to land sooner
- JSONL incremental tail proves unreliable under file rotation or concurrent writes — may need to fall back to range reads or file-watch events
- The OAuth endpoint's rate-limit policy tightens (Anthropic could reduce throughput) — may need to tighten default cache TTL from 180s to 300s+

## Pros and Cons of the Options

### Segment-driven lazy loading

- Good: smallest possible fetch footprint per prompt
- Good: plain Rust stdlib types (`OnceCell`, `Arc`) plus closures over segment lists
- Good: the pattern scales: adding a new source means adding a new `OnceCell` field
- Bad: segment authors must remember to declare their data dependencies (lint-able)

### Eager-load everything

- Good: dead simple — no dependency graph, no lazy cells
- Bad: users on a minimal preset still pay for every source
- Bad: violates cold-start budget on any invocation that touches the OAuth endpoint

### Daemon with persistent caches

- Good: sub-millisecond client latency; best possible experience for users with many CC sessions
- Good: OAuth endpoint cache hit across all sessions for free
- Bad: four hard subsystems (IPC, lifecycle, version skew, crash recovery) — weeks of implementation, months of operational complexity
- Bad: no competitor uses a daemon (ccstatusline, CCometixLine, claudia-statusline, claude-powerline all per-process)
- Deferred to v0.2+ per [ADR-0012](0012-per-process-execution.md)

### Async I/O with tokio

- Good: parallel file + network I/O
- Bad: tokio runtime init costs ~100ms cold-start — larger than the file reads we'd parallelize
- Bad: async adds complexity that doesn't pay off when our network path is a single endpoint

## More Information

- Primary source: [`docs/research/data-fetching-strategy.md`](../research/data-fetching-strategy.md) — per-source cost matrix, caching patterns, full rationale
- Supporting: [`docs/research/claude-data-files.md`](../research/claude-data-files.md) — the data sources this architecture fetches from
- Supporting: [`docs/research/ccstatusline-widget-internals.md`](../research/ccstatusline-widget-internals.md) — the OAuth multi-tier cache stack we adopt
- Depends on: [ADR-0009](0009-json-parsing-stack.md) — parsing strategy is serde_json + partial structs
- Related: [ADR-0012](0012-per-process-execution.md) — per-process execution model; defers daemon mode
- Will drive: [ADR-0011](0011-rate-limit-data-source.md) — rate-limit segment uses this architecture
- Will drive: [`specs/data-fetching.md`](../specs/data-fetching.md) — interface contracts for `DataContext` and segment-dependency declaration
