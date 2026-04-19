# Data fetching strategy: efficient reads, cache discipline, rate-limit safety

- Date: 2026-04-18
- Author: Jace Babin (w/ Claude Code)
- Scope: How linesmith should read every data source identified in `claude-data-files.md` without taxing the system or hitting Anthropic API rate limits. Defines per-source cost expectations, caching tiers, invalidation rules, and the segment-driven lazy-load model.

## Question

linesmith runs per-prompt with a 300ms debounce and a <20ms cold-start target. The data sources we want to surface have wildly different read costs — sub-millisecond file reads versus subprocess Keychain access versus rate-limited HTTP. What's the architectural strategy that keeps the statusline fast, respects API rate limits, and stays correct in the face of stale data?

## Sources

- `claude-data-files.md` — the file/Keychain layout we need to read from
- `ccstatusline-widget-internals.md` — ccstatusline's HTTP cache stack, 429 handling
- `ccometixline-rust-patterns.md` — CCometixLine's simpler file cache, anti-patterns
- `jsonl-data-source.md` — JSONL aggregation and incremental-tail considerations

## Findings

### 1. Per-source cost matrix

| Source                                   | Typical read cost           | Per-prompt safe? | Caching strategy                         |
| ---------------------------------------- | --------------------------- | ---------------- | ---------------------------------------- |
| Stdin payload                            | ~0ms                        | yes              | None — input is the cache                |
| `~/.claude/settings.json`                | <1ms                        | yes              | mtime stat + reparse only on change      |
| `{project_root}/.claude/settings.json`   | <1ms                        | yes              | mtime stat + reparse                     |
| `~/.claude.json` (~100-200KB)            | 5-20ms                      | borderline       | mtime stat + reparse only on change      |
| `~/.claude/sessions/{pid}.json`          | <1ms                        | yes              | mtime stat + reparse                     |
| JSONL transcripts                        | 10ms-1s+                    | **no, naively**  | **Incremental tail** + byte-offset       |
| macOS Keychain via `security` subprocess | 50-200ms                    | **no**           | Memoize for process lifetime             |
| `~/.claude/.credentials.json` (Linux)    | <1ms                        | yes              | Memoize for process lifetime             |
| OAuth `/api/oauth/usage` endpoint        | 100ms-1s + **rate limited** | **no**           | Two-tier cache + lock file + 429 backoff |

The two operations that _cannot_ run unguarded per-prompt are JSONL full reads and the OAuth endpoint call. Everything else is either free or cheap enough that an mtime stat is sufficient.

### 2. Segment-driven lazy loading

The biggest win: **don't fetch what no enabled segment needs.**

- The runtime first reads the user's enabled segments from the merged config
- Each segment declares its data dependencies (e.g., `RateLimit5h` depends on `OAuthUsageData`; `Cost` depends only on stdin)
- The runtime computes the union of declared dependencies
- It fetches _only_ sources in that union, in parallel where possible

If a user runs `linesmith` with a config that only renders `model + cost + workspace`, no Keychain read happens, no endpoint call happens, no JSONL scan happens. Cold-start drops to a couple of milliseconds.

This is the architectural hinge. Most other optimizations are smaller wins layered on top.

### 3. mtime-based cache invalidation for files

Every file source above gets the same pattern (pseudocode — real impl will need `Entry`-style API or interior mutability to satisfy the borrow checker):

```rust
fn read_cached<T>(path: &Path, cache: &mut Option<(SystemTime, T)>) -> Result<&T> {
    let mtime = fs::metadata(path)?.modified()?;
    if cache.as_ref().map(|(t, _)| *t == mtime).unwrap_or(false) {
        return Ok(&cache.as_ref().unwrap().1);
    }
    let parsed = parse(fs::read_to_string(path)?)?;
    *cache = Some((mtime, parsed));
    Ok(&cache.as_ref().unwrap().1)
}
```

`fs::metadata().modified()` is microseconds — almost always cached by the OS. Reparsing only happens when the file has actually changed. For `~/.claude.json` (the 100-200KB outlier) this is the difference between 5-20ms and ~10μs in the steady-state.

For a per-prompt CLI, the cache is process-scoped (the process exits after each render). For a daemon (future), the cache outlives the prompt and the savings compound.

### 4. JSONL incremental tail

JSONL transcripts grow without bound — long sessions can hit megabytes. Reading the whole file every prompt is unacceptable.

Pattern:

1. Track `last_byte_offset` per JSONL path (in process memory; persist across prompts only in daemon mode)
2. Each invocation: `seek(last_byte_offset)`, read to EOF
3. Parse only the new lines
4. Update `last_byte_offset` and merge new entries into the in-memory aggregate

For aggregations like 5h block usage, the rolling state can be recomputed cheaply from a windowed entry buffer. For per-message lookups (e.g., "last `/effort` marker"), tail-with-fallback: scan the new tail; if not found, scan the file backwards to a configurable limit (e.g., 200 lines or 100KB).

ccusage's `_session-blocks.ts` aggregator works this way. ccstatusline's JSONL fallback inherits the same property.

### 5. OAuth endpoint: ccstatusline's full cache stack

The endpoint is the only rate-limited source. Adopt ccstatusline's multi-tier stack verbatim:

| Tier        | Location                           | TTL      | Purpose                                       |
| ----------- | ---------------------------------- | -------- | --------------------------------------------- |
| Memory      | module-level `OnceCell<UsageData>` | 180s     | Intra-process re-reads (segment dependencies) |
| Disk (data) | `~/.cache/linesmith/usage.json`    | 180-300s | Cross-invocation cache                        |
| Disk (lock) | `~/.cache/linesmith/usage.lock`    | 30s      | "Don't retry the API more than 1×/30s"        |

Plus error handling:

- **429 response:** honor `Retry-After` (integer seconds or HTTP date). Default 300s if header missing. Write lock with `{blockedUntil, error: "rate-limited"}`.
- **Timeout / network error:** serve stale cache if available; otherwise short-cache the error (30s) so we don't hammer.
- **Auth-failure distinct from timeout:** resolve credentials _before_ the lock check so `no-credentials` isn't masked as `timeout`.
- **Stale-while-revalidate:** if a cached value exists and is past TTL but the API call fails, return the stale value rather than nothing.

CCometixLine ships without lock file or 429 handling — that's an under-engineered spot for a per-prompt tool. ccstatusline's design is the right starting point.

### 6. Memoize Keychain and credential reads

The macOS `security` subprocess fork costs 50-200ms — orders of magnitude more than any file read. Read the OAuth token _once_ per process (or per daemon lifetime), keep the parsed `UsageCredentials` struct in a `OnceCell`, and never re-fork during the prompt.

Same applies to `~/.claude/.credentials.json` on Linux/Windows — even though the file read is sub-millisecond, the credential parsing and token-shape validation should run exactly once per process.

This memoization is safe because:

- Credentials don't rotate during a single statusline render
- Token expiration is handled by the endpoint returning 401 (caught by error handler), not by us pre-validating `expiresAt`
- If the user logs out mid-session, the next _process_ invocation picks up the change

### 7. Compile-time User-Agent

CCometixLine shells out to `npm view @anthropic-ai/claude-code version` for every cache miss to populate `User-Agent: claude-code/{version}`. NPM cold-starts are 300-500ms even on a network-cached lookup. This is misallocated work in a fast-path binary.

linesmith should hardcode at build time:

```rust
const USER_AGENT: &str = concat!("linesmith/", env!("CARGO_PKG_VERSION"));
```

If endpoint behavior depends on UA spoofing (currently no evidence it does), expose it as a config option, not the default.

### 8. Async vs sync

`ureq` (sync, blocking, pure-Rust) is the right HTTP client. The async crowd (`reqwest`, `hyper`) carries a tokio dependency that adds ~100ms cold-start cost from runtime initialization — a poor trade for a CLI that makes one HTTP call.

For parallel file I/O, sync threads (`std::thread::spawn` or `rayon`) are sufficient. Most file reads are <1ms; spawning a thread to overlap two of them is rarely worth it. The exception: if a JSONL incremental tail and an OAuth endpoint call are both required, kick off the network call first (longest pole) and do the JSONL tail in parallel.

### 9. Concurrent statusline invocations

The 300ms debounce caps practical invocation rate at ~3/sec per CC session. Multiple CC sessions on the same machine multiply this. The disk lock file (`usage.lock`) provides cross-process serialization for the OAuth endpoint — if process A is mid-fetch, process B serves stale and skips the call.

For file caches, the OS handles concurrent reads safely. Writes (cache updates) should use `tempfile + atomic rename` to avoid partial-write corruption that another process might read.

### 10. Daemon mode (post-v0.1)

If the per-process model bumps against the <20ms target despite all of the above, the next architectural step is a long-lived daemon:

- Daemon owns all caches and file-watch invalidation (`notify` crate)
- Statusline binary becomes a thin RPC client (Unix socket → daemon → render)
- Cache reuse compounds across prompts; cold-start cost is paid once at daemon launch
- File watches (notify on `~/.claude.json`, JSONL, settings) eliminate even the `stat()` cost

This is explicitly **out of scope for v0.1**. The per-process model with the optimizations above should hit our cold-start target on its own. Document as a future bead under the performance epic when we have measurements proving it's needed.

### 11. Cache-busting and configuration knobs

Per ccstatusline + CCometixLine practice, expose:

| Config knob                    | Default             | Purpose                                              |
| ------------------------------ | ------------------- | ---------------------------------------------------- |
| `usage.api_base_url`           | `api.anthropic.com` | Self-hosters, proxies, beta endpoints                |
| `usage.cache_duration_seconds` | 180                 | Tradeoff: freshness vs API politeness                |
| `usage.timeout_seconds`        | 2                   | CLI-friendly default; tighter than ccstatusline's 5s |
| `usage.lock_max_age_seconds`   | 30                  | Per-machine API spam prevention                      |
| `cache.enabled`                | true                | Disable for tests / debugging                        |

User can lengthen TTLs to be polite to the endpoint or shorten for development/testing. The lock file mechanism stays on by default.

## Conclusions

1. **Segment-driven lazy loading is the biggest win.** No enabled segment needs the endpoint? No call. Cold-start collapses to file-only operations.
2. **mtime caching makes file reads effectively free** in steady-state. Stat is microseconds; reparse only on change.
3. **JSONL must be tailed, never fully re-read.** Track byte offset, read deltas. This is the difference between 10ms and 1s on long sessions.
4. **Adopt ccstatusline's OAuth cache stack verbatim.** Memory + file + lock + 429 backoff + stale-on-error. CCometixLine's simpler approach (file cache only, no lock, no 429 handling) is under-engineered for a per-prompt tool.
5. **Memoize Keychain reads.** The 50-200ms `security` subprocess is the single biggest cost we can eliminate.
6. **Skip the npm-subprocess UA anti-pattern.** Use `env!("CARGO_PKG_VERSION")`.
7. **Sync I/O is fine for v0.1.** ureq + std::fs without tokio. Async would add cold-start cost without clear benefit.
8. **Daemon mode is the post-v0.1 escape hatch.** File-watch invalidation + RPC client. Don't build it speculatively; only when measurements demand it.

## Implications / actions

- **Promoted to [ADR-0010](../adrs/0010-data-fetching-architecture.md)** — segment-driven lazy load + per-source strategy matrix. Implementation work below flows from that decision.
- **Wire segment dependency declaration** into the runtime — the lazy-load model needs each segment to opt into its data sources rather than have the runtime fetch everything proactively.
- **Implement an `mtime + reparse` helper crate** (or module) that all file readers share. Reduces boilerplate and centralizes cache invalidation.
- **Build the OAuth fetcher with the full ccstatusline cache stack** — file cache, lock file, 429 handling, stale-on-error, distinct error states. File as a slice under `lsm-y6m`.
- **JSONL incremental tail helper** belongs in the same shared utility module as the mtime cache. Both are foundational.
- **Cache-config schema** (URL, TTLs, timeout, lock TTL) needs to land in the config spec when written.
- **Performance-budget test** — once we have anything end-to-end, measure cold-start per-segment-set and validate the <20ms target. File a bead for the test harness.
- **Document the daemon-mode escape hatch** in the architecture spec, but don't build it.
- **Update `lsm-y6m` ADR** (when written) to cite this strategy doc — rate-limit segment cannot ship without the cache stack described here.

## Open questions

- **Atomic file-writes pattern in Rust.** `tempfile + persist` is the standard, but on Windows the rename semantics differ. Worth a small spike when we implement the cache writer.
- **JSONL byte-offset persistence across daemon restarts.** If/when we build the daemon, do we persist the offset map to `~/.cache/linesmith/jsonl-offsets.json` or recompute on launch? Latter is simpler, former is faster.
- **Endpoint behavior on staleness.** If we serve a 5-minute-stale `utilization` value, does the user notice? Worth a UX test with shorter and longer TTLs once the segment ships.
- **`notify` cross-platform reliability.** The Rust `notify` crate has some platform quirks (especially on macOS for symlinked watch targets). Defer evaluation to the daemon-mode work.
- **Concurrent CC sessions hitting the endpoint.** With 5 CC sessions running, even with the lock file, each session's first cache miss could hit the endpoint within seconds of each other. We should validate the endpoint doesn't penalize this — and consider extending the lock semantics to "shared-fate" across sessions if it does.
