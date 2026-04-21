# Data Fetching

- Status: draft
- Version: 0.1.1
- Last updated: 2026-04-19
- Driving ADRs: [ADR-0009](../adrs/0009-json-parsing-stack.md), [ADR-0010](../adrs/0010-data-fetching-architecture.md), [ADR-0012](../adrs/0012-per-process-execution.md)

## Overview

The data-fetching layer is the bridge between external sources (stdin, settings, `~/.claude.json`, JSONL transcripts, macOS Keychain, the OAuth usage endpoint) and segment render code. It owns caching, lazy loading, and error propagation so that segments can treat data access as a cheap typed read.

This spec defines:

1. The `DataContext` runtime struct shared across all segments in a single invocation
2. How segments declare which data sources they need
3. Per-source fetch and cache strategies (mtime, byte-offset, multi-tier)
4. Error propagation and caching-of-errors semantics
5. Atomic write and schema-versioning conventions for our own cache files

The spec does NOT cover: segment rendering ([segment-system.md](segment-system.md)), config parsing ([config.md](config.md)), credential reading details ([credentials.md](credentials.md)), or specific segment contracts ([rate-limit-segments.md](rate-limit-segments.md)).

## Requirements

### Functional

- `DataContext` exposes all data sources as typed accessors
- Segments declare their data dependencies statically (at compile time, via trait method)
- Runtime computes the union of declared dependencies from the enabled segment list; fetches only those sources
- File-backed sources (`settings.json`, `~/.claude.json`, `sessions/*.json`) cache on `mtime` and reparse only on change
- JSONL transcripts read via byte-offset incremental tail; never re-read whole files
- OAuth endpoint data uses a three-tier cache: in-memory `OnceCell` → disk file (`~/.cache/linesmith/usage.json`) → disk lock (`~/.cache/linesmith/usage.lock`)
- Credentials memoize for the process lifetime (single `security` subprocess call on macOS, single file read elsewhere)
- All our own cache files carry a top-level `schema_version` field; version mismatch → treat as cache miss, don't error
- Cache writes are atomic: write to a tempfile in the same directory, then rename over the target
- `linesmith doctor` can inspect every cache file and every memoized value for diagnostics

### Non-functional

- Cold-start target <20ms per invocation when all enabled segments miss zero cache layers
- Mtime stat must be the common-case path for file-backed sources (measured: <10μs on warm page cache)
- Byte-offset JSONL tail must scale as O(new bytes), not O(file size)
- Errors are cached alongside values (same `OnceCell`), so a single failed source never re-tries within the same process
- Concurrent linesmith processes on the same machine must not stampede the OAuth endpoint: lock file coordination with 30s TTL
- Partial parsing: each source's Rust struct declares only fields that linesmith actually consumes; serde silently ignores the rest ([ADR-0009](../adrs/0009-json-parsing-stack.md))

## Interface / Contract

### `DataContext`

The shared struct threaded through every segment's `render()` call.

```rust
pub struct DataContext {
    /// Parsed stdin payload per [input-schema.md](input-schema.md).
    /// Always populated eagerly on `new()`.
    pub status: StatusContext,

    settings:     OnceCell<Arc<Result<Settings, SettingsError>>>,
    claude_json:  OnceCell<Arc<Result<ClaudeJson, ClaudeJsonError>>>,
    jsonl:        OnceCell<Arc<Result<JsonlAggregate, JsonlError>>>,
    usage:        OnceCell<Arc<Result<UsageData, UsageError>>>,
    credentials:  OnceCell<Arc<Result<Credentials, CredentialError>>>,
    sessions:     OnceCell<Arc<Result<LiveSessions, SessionError>>>,
    git:          OnceCell<Arc<Result<Option<GitContext>, GitError>>>,
}

impl DataContext {
    pub fn new(status: StatusContext) -> Self;

    // Accessors return Arc<Result<...>> so error state is cached and shareable.
    pub fn settings(&self)    -> Arc<Result<Settings,    SettingsError>>;
    pub fn claude_json(&self) -> Arc<Result<ClaudeJson,  ClaudeJsonError>>;
    pub fn jsonl(&self)       -> Arc<Result<JsonlAggregate, JsonlError>>;
    pub fn usage(&self)       -> Arc<Result<UsageData,   UsageError>>;
    pub fn credentials(&self) -> Arc<Result<Credentials, CredentialError>>;
    pub fn sessions(&self)    -> Arc<Result<LiveSessions, SessionError>>;
    /// `Ok(None)` when cwd is not inside a git repo. `Ok(Some(_))` for
    /// main checkouts, linked worktrees, and bare repos (distinguished
    /// via `GitContext.repo_kind`). Inner `OnceCell`s inside
    /// `GitContext` defer dirty-scan and upstream-walk cost to segments
    /// that actually render those fields.
    pub fn git(&self)         -> Arc<Result<Option<GitContext>, GitError>>;
}
```

`status` is populated eagerly on `new()` (it's the input parameter; there's nothing to lazy-load). `StatusContext` is the parsed-stdin type defined in [input-schema.md](input-schema.md) and already referenced by [segment-system.md](segment-system.md)'s `Segment::render`. All other fields are lazy `OnceCell`s populated on first access.

Returning `Arc<Result<T, E>>` rather than `Result<&T, &E>` lets segments hold the data across the render call without tying lifetimes to `&self`.

### Segment dependency declaration

Segments opt in to their data sources via an additional `data_deps()` method on the existing `Segment` trait defined in [segment-system.md](segment-system.md). This spec does NOT redefine the trait; it extends it:

```rust
// Addition to the canonical Segment trait in segment-system.md.
// Render signature and other methods remain as segment-system.md specifies
// (ctx: &DataContext in v0.3; RenderResult return; Send bound; defaults/cache_policy/children).
pub trait Segment {
    /// Which data sources this segment reads from `DataContext`.
    /// Runtime fetches only the union of declared deps across enabled
    /// segments.
    fn data_deps(&self) -> &'static [DataDep] {
        &[DataDep::Status]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataDep {
    /// The parsed stdin payload (always available via `ctx.status`).
    Status,
    Settings,
    ClaudeJson,
    Jsonl,
    Usage,
    Credentials,
    Sessions,
    /// Git repository state (branch, dirty, ahead/behind, worktree kind).
    /// See [git-segments.md](git-segments.md) for the `GitContext` shape.
    Git,
}
```

During render, segments receive `&DataContext` directly. `StatusContext` (the parsed stdin payload from [input-schema.md](input-schema.md)) is accessible as `ctx.status`. This is a signature change from segment-system.md v0.2's `render(&self, ctx: &StatusContext) -> RenderResult` to `render(&self, ctx: &DataContext) -> RenderResult`; DataContext owns StatusContext as a field, so segments that only need stdin data read `ctx.status.<field>` without functional loss. Passing `&DataContext` avoids a self-referential type (DataContext owns StatusContext, so StatusContext cannot borrow `&DataContext` without creating a construction-order cycle).

The runtime computes `enabled_segments.iter().flat_map(|s| s.data_deps()).collect::<HashSet<_>>()` once at startup. Sources not in that set are never touched — their `OnceCell`s stay empty, no file I/O, no subprocess, no HTTP.

Dependency declarations are static and compile-time-known. Segments that need data only conditionally (e.g., `ContextBar` needs JSONL only post-`/compact`) declare the superset.

**Follow-up:** [segment-system.md](segment-system.md) currently shows `render(&self, ctx: &StatusContext)` without acknowledging `DataContext`. When that spec revs to v0.3, change the render signature to `render(&self, ctx: &DataContext) -> RenderResult` and add the `data_deps()` method. StatusContext stays accessible via `ctx.status`. Tracked as lsm-thm.

### File-backed source cache (`settings.json`, `~/.claude.json`, `sessions/*.json`)

Every file-backed source goes through a shared helper:

```rust
pub fn read_mtime_cached<T, E>(
    path: &Path,
    cache: &Mutex<Option<(SystemTime, Arc<T>)>>,
    parse: impl FnOnce(&str) -> Result<T, E>,
) -> Result<Arc<T>, FileReadError<E>>;
```

Semantics:

1. `stat(path)` to read `mtime`. If this fails with `NotFound`, return `FileReadError::NotFound` (semantic distinction from IO errors).
2. Compare against cached `mtime`. If equal, return the cached `Arc<T>` without reading.
3. Read the file, parse via `parse`, wrap in `Arc`, store `(mtime, Arc::clone(&value))` in the cache, return the `Arc`.
4. Parse errors are propagated through `FileReadError::Parse(E)`.

The `Mutex` protects against theoretical concurrent access from a future multi-threaded render path; today's per-process model is single-threaded on this path.

### JSONL incremental tail

```rust
pub struct JsonlTailer {
    path: PathBuf,
    last_offset: u64,
    last_size: u64,
}

impl JsonlTailer {
    pub fn new(path: PathBuf) -> Self;

    /// Reads new lines since last call. Returns successfully parsed entries;
    /// malformed lines are logged and skipped (never fail the batch).
    pub fn read_new<T: DeserializeOwned>(&mut self) -> Result<Vec<T>, JsonlError>;
}

/// Aggregate of JSONL transcript data used by the usage fallback
/// and future segments (effort detection, session metrics, etc.).
/// The concrete shape — 5h block buckets, 7d window rollups, per-line
/// record schema, project-root cascade, dedup semantics — is owned by
/// [jsonl-aggregation.md](jsonl-aggregation.md). For this spec's
/// purposes, `JsonlAggregate` is the type returned by `ctx.jsonl()`
/// and `JsonlError` is its failure variant.
pub struct JsonlAggregate { /* see jsonl-aggregation.md */ }
pub struct JsonlError    { /* see jsonl-aggregation.md */ }
```

Semantics:

1. `stat(path)`. If `size < last_size`, the file was truncated/rotated: reset `last_offset = 0`.
2. `seek(SeekFrom::Start(last_offset))`.
3. Read lines via `BufReader::read_line`. For each complete line (newline-terminated), parse via `serde_json::from_str`. Malformed lines are logged at `warn!` and skipped.
4. Advance `last_offset` only on `\n`-terminated lines. Partial trailing lines (no newline) do not advance the offset; they'll be re-read next invocation.
5. Return the parsed batch.

Per-process model: `JsonlTailer` is created fresh per invocation; `last_offset = 0` at start. For daemon mode, `last_offset` persists across requests.

### OAuth usage cache stack

Three-tier per [ADR-0010](../adrs/0010-data-fetching-architecture.md):

| Tier        | Location                         | TTL  | Purpose                                                                                                        |
| ----------- | -------------------------------- | ---- | -------------------------------------------------------------------------------------------------------------- |
| Memory      | `DataContext.usage` (`OnceCell`) | —    | Same-process re-reads (multiple segments reading `ctx.usage()`)                                                |
| Disk (data) | `~/.cache/linesmith/usage.json`  | 180s | Cross-invocation cache (configurable via `usage.cache_duration`; see [config.md](config.md) §Top-level schema) |
| Disk (lock) | `~/.cache/linesmith/usage.lock`  | 30s  | Cross-process API spam prevention                                                                              |

Cache file shape:

```json
{
  "schema_version": 1,
  "cached_at": "2026-04-19T18:23:12Z",
  "data": {
    "five_hour":  { "utilization": 22.0, "resets_at": "..." },
    "seven_day":  { "utilization": 33.0, "resets_at": "..." },
    "seven_day_sonnet": { "utilization": 0.0, "resets_at": "..." },
    "extra_usage": { "is_enabled": false, ... },
    "unknown_buckets": { "omelette_promotional": null, ... }
  },
  "error": null
}
```

`data` and `error` are mutually exclusive (one or the other is non-null). Error cache uses a shorter TTL (30s default) so transient failures recover quickly.

Lock file shape:

```json
{
  "blocked_until": 1744826592,
  "error": "rate-limited"
}
```

`blocked_until` is a Unix timestamp. Legacy fallback: if the file is not valid JSON, use `mtime + 30s` as the implicit `blocked_until`.

### Credential memoization

See [credentials.md](credentials.md) for the cascade details. From the data-fetching layer's perspective: credentials read exactly once per process, cached in `DataContext.credentials`.

### Atomic writes

All our own cache writes use this pattern:

```rust
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let mut tmp = NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut tmp, value)?;
    tmp.flush()?;
    tmp.persist(path)?;
    Ok(())
}
```

`tempfile::NamedTempFile::persist` is rename-on-Unix, `MoveFileEx` with `MOVEFILE_REPLACE_EXISTING` on Windows. Both are atomic at the filesystem level.

### Schema versioning

Every cache file carries `"schema_version": u32`. Read semantics:

```rust
if cache.schema_version != CURRENT_SCHEMA_VERSION {
    // treat as cache miss; do not error
    return Ok(None);
}
```

Incompatible changes bump `CURRENT_SCHEMA_VERSION` in a follow-up. Old cache files are silently discarded on next read.

## Behavior

### Lazy fetch flow

1. `main()` parses stdin into `StdinPayload`, constructs `DataContext::new(stdin)`.
2. Runtime enumerates enabled segments from the merged config.
3. Runtime computes `deps: HashSet<DataDep>` by folding `segment.data_deps()` across enabled segments.
4. Runtime calls `ctx.settings()`, `ctx.claude_json()`, etc. for each `DataDep` in `deps`. (This populates the `OnceCell`s.)
5. Segments then render concurrently (future: thread pool) or sequentially (v0.1), each reading from the pre-populated `DataContext`.

The runtime does NOT call accessors for undeclared deps, so those `OnceCell`s stay empty and no I/O happens.

### Error propagation

- Each accessor returns `Arc<Result<T, E>>`.
- Errors are cached in the `OnceCell` on first access and re-returned on subsequent calls (no retries within the process).
- Segment render code decides how to display errors (rate-limit segments render `[No credentials]`, context-window segments might hide entirely).
- The data-fetching layer itself NEVER returns synthesized default values to hide errors. Hidden errors masquerading as zero-utilization would mislead users.

### OAuth fallback cascade

`DataContext.usage()` internally implements the cascade from [ADR-0011](../adrs/0011-rate-limit-data-source.md) §Fallback cascade. Cache reads happen BEFORE credentials so fresh-cache hits don't trigger a Keychain subprocess or macOS permission prompt:

1. Check in-memory `OnceCell` (always a cache hit after first call within the process).
2. Check disk cache file (`~/.cache/linesmith/usage.json`); if fresh and `schema_version` matches, populate memory cache and return.
3. Check lock file; if active AND disk cache has stale data, return stale (no credential read needed for this path either).
4. Read credentials via `ctx.credentials()`. Only reached when we genuinely need to call the endpoint.
5. If credentials missing: record `NoCredentials` error, skip to JSONL fallback (step 7).
6. Hit the endpoint with 2s timeout. On 200, cache + return. On 429/timeout/network error with no stale cache, skip to JSONL fallback.
7. JSONL fallback: scan `~/.claude/projects/*/*.jsonl`, aggregate into 5h blocks, 7d windows.
8. If JSONL also empty: surface the original error (`NoCredentials`, `Timeout`, `RateLimited`, etc.).

Credentials are resolved before the endpoint call (step 4 before step 6), preserving the ADR-0011 rule that `NoCredentials` is never masked as `Timeout`. This spec deliberately reorders the pure cache reads (steps 1-3) ahead of credentials compared to ADR-0011's step-2 placement, because cache hits and stale-lock serving have no need of a token — that avoids the Keychain subprocess on cache hits without weakening the `NoCredentials`-distinct-from-`Timeout` guarantee. When ADR-0011's cascade is re-read strictly in order, only the endpoint-fetch path ever reaches credentials anyway, so the behavior is equivalent even though the numbered ordering differs.

JSONL-derived values are tagged `UsageData::source = UsageSource::Jsonl` so segments can display a visible indicator (prefix `~` or dim color per segment spec).

### Concurrent processes

Multiple CC sessions invoke linesmith concurrently. The disk lock file is the coordination point, using the same atomic-rename pattern as `atomic_write_json` above:

1. Process A writes `{blocked_until, error}` to `.lock.tmp-{pid}` and renames to `.lock`. The rename is atomic on Unix and Windows alike.
2. Process B reads `.lock`; if `now < blocked_until`, it serves stale data from the data-cache file.
3. When A completes (success or failure), it writes a new lock with the updated `blocked_until`. On a clean success with a valid cache write, the lock is left with `blocked_until = now + cache_duration` so other processes see the fresh data through the cache rather than retrying.

Rename-based locking avoids platform-specific `flock`/`LockFileEx` gymnastics and is the same pattern the data-cache writer uses; one helper serves both.

## Edge cases

- **Stale lock file** (process crashed mid-fetch): next process sees `now > blocked_until`, treats as expired, proceeds to fetch.
- **Cache file corrupt** (malformed JSON, partial write): read returns parse error → treat as cache miss, fetch fresh, overwrite.
- **Cache file `schema_version` mismatch**: treat as miss, overwrite on next successful fetch.
- **JSONL file truncated during read**: `last_size` regression detected, `last_offset` reset to 0.
- **JSONL file grows past `last_offset` with malformed lines in between**: malformed lines logged + skipped; offset still advances past them so the next tail doesn't re-encounter.
- **`~/.claude.json` missing** (fresh install): `FileReadError::NotFound` returned; segments depending on it render fallback (e.g., no account info).
- **Keychain subprocess fails** (macOS permission denied, `security` not on PATH): `CredentialError::Subprocess`; cascade falls through to file-based read.
- **Clock skew** (system time moved backwards, cache `cached_at > now`): treat as stale, refetch.
- **Zero-sized cache file** (previous process died mid-write with non-atomic writes): parse error → cache miss. Atomic writes prevent this on well-behaved filesystems.
- **Symlinks in cache dir**: allowed; `fs::metadata` follows them by default.

## Testing strategy

- **Unit tests per helper:**
  - `read_mtime_cached`: hit returns cached `Arc`, miss re-parses, `NotFound` handled, parse error surfaces
  - `JsonlTailer::read_new`: empty file, single line, many lines, truncation, partial trailing line, malformed line
  - `atomic_write_json`: happy path, then verify with concurrent reader seeing either old or new content (never partial)
  - Schema-version mismatch: explicitly set `schema_version = 999`, assert cache-miss behavior
  - Lock file: stale (expired), active, malformed

- **Integration tests:**
  - `DataContext` with every combination of enabled/disabled sources, assert only declared sources are read (use a fake filesystem that errors on undeclared access)
  - OAuth fallback cascade with a fake endpoint that returns 200, 429 (with and without `Retry-After`), timeout, network error; verify each branch of the cascade
  - Concurrent process simulation: two linesmith invocations racing on the lock file

- **Snapshot tests:**
  - Cache file shape (serialized) against a golden file, to catch unintended schema changes
  - Error message format across the `UsageError` variants

- **Property tests (nice-to-have for v0.1):**
  - `JsonlTailer` invariants: `new_offset >= old_offset`, `new_offset <= file_size`, parsed entries ⊆ entries between `[old_offset, new_offset)`

## Open questions

- **Daemon-compat API shape.** This spec defines `DataContext` as a per-process struct. A daemon would need `Arc<RwLock<DataContext>>` or equivalent, with additional methods to invalidate individual sources on file-watch events. Defer the daemon API to a follow-up spec when [ADR-0012](../adrs/0012-per-process-execution.md) is revisited.
- **Cross-source failure isolation.** If `settings.json` is malformed, should `usage()` still succeed? This spec says yes (sources are independent and errors don't cross-contaminate), but we should stress-test once the implementation exists.
- **Parallel fetching of expensive sources.** Keychain read (~50-200ms) and OAuth endpoint (~100ms-1s) could run in parallel if both are needed. v0.1 does sequential for simplicity; revisit if measured cold-start exceeds target.
- **Partial-struct rot.** `ClaudeJson` declares only the fields we use (`oauthAccount`, `mcpServers`, `projects`). If Anthropic renames `oauthAccount.billingType`, our partial struct silently misses the rename. Mitigation: surface parse errors loudly (`#[serde(deny_unknown_fields)]` is not feasible due to forward-compat needs, but missing-field errors will fire).
- **Cache-file permissions.** On Unix, `usage.json` contains no secrets (OAuth token lives in Keychain / `.credentials.json`, not in our cache). But on multi-user systems, should the cache directory be mode `700`? Defer to credentials spec for consistency.

## Change log

- 2026-04-19: initial draft (v0.1). Sets out `DataContext` shape, segment dependency declaration, per-source fetch strategies, OAuth fallback cascade, and atomic-write/schema-version conventions. Driven by ADR-0009, ADR-0010, ADR-0012.
- 2026-04-19: v0.1.1 additive update. Adds `DataDep::Git` + `DataContext::git()` accessor + `GitContext`/`GitError` type pointers to [git-segments.md](git-segments.md). No behavior change for existing sources; git-aware segments opt in via `DataDep::Git`.
