# Data Fetching

- Status: draft
- Version: 0.1.2
- Last updated: 2026-04-22
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

`DataContext::usage()` surfaces a `UsageData` enum whose variant encodes provenance. Per [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md):

```rust
#[non_exhaustive]
pub enum UsageData {
    Endpoint(EndpointUsage),
    Jsonl(JsonlUsage),
}

#[non_exhaustive]
pub struct EndpointUsage {
    pub five_hour:            Option<UsageBucket>,
    pub seven_day:            Option<UsageBucket>,
    pub seven_day_opus:       Option<UsageBucket>,
    pub seven_day_sonnet:     Option<UsageBucket>,
    pub seven_day_oauth_apps: Option<UsageBucket>,

    /// Model-scoped and aggregate limits. Per [ADR-0030](../adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md)
    /// this is where per-model usage now arrives; the `seven_day_*`
    /// fields above are null in practice.
    pub limits: Option<Vec<UsageLimit>>,
    pub extra_usage:          Option<ExtraUsage>,
    /// Landing zone for keys nothing depends on. Per
    /// [ADR-0030](../adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md),
    /// a key a segment comes to depend on is promoted out of here
    /// into a typed field; this is not a supported read path.
    pub unknown_buckets:      HashMap<String, serde_json::Value>,
}

/// `seven_day` is always populated (zero-valued on empty transcripts);
/// `five_hour` is `None` when the current 5h block has no activity.
pub struct JsonlUsage {
    pub(crate) five_hour: Option<FiveHourWindow>,
    pub(crate) seven_day: SevenDayWindow,
}

pub struct FiveHourWindow {
    pub(crate) tokens:  TokenCounts,   // four-category breakdown owned by aggregator; segments call `.total()`
    pub(crate) ends_at: Timestamp, // invariant: ends_at == block.start + SignedDuration::from_hours(5)
}

pub struct SevenDayWindow {
    pub(crate) tokens: TokenCounts,    // no reset_at — rolling window, no hard reset
}
```

`UsageLimit` models one element of the `limits` array:

All five derive `Debug, Clone, Deserialize, Serialize, PartialEq`, matching
`ExtraUsage`; the two enums add `Copy`, which the structs cannot have because
`LimitModel` owns `String`s. `Serialize` is load-bearing — these round-trip
through `usage.json`.

```rust
#[non_exhaustive]
pub struct UsageLimit {
    /// Selection key. `weekly_scoped` is the model-scoped bucket
    /// `rate_limit_7d_model` renders; `session` / `weekly_all` duplicate
    /// `five_hour` / `seven_day` and are ignored.
    pub kind: LimitKind,

    /// Arrives as a JSON integer (`82`) where `UsageBucket::utilization`
    /// arrives as a float; both route through `deserialize_clamped_percent`,
    /// which reads either and clamps to `[0, 100]`.
    #[serde(deserialize_with = "deserialize_clamped_percent")]
    pub percent: Percent,

    #[serde(default)]
    pub resets_at: Option<Timestamp>,

    /// Populated only for `kind == WeeklyScoped` in every capture to date.
    /// The correlation is not enforced by the type.
    #[serde(default)]
    pub scope: Option<LimitScope>,

    /// Not consulted when rendering — see [rate-limit-segments.md](rate-limit-segments.md)
    /// §`rate_limit_7d_model`. Modelled so plugins can reach it.
    #[serde(default)]
    pub severity: LimitSeverity,
}

#[non_exhaustive]
pub struct LimitScope {
    #[serde(default)]
    pub model: Option<LimitModel>,
}

#[non_exhaustive]
pub struct LimitModel {
    /// The family name (`"Fable"`), not the stdin `display_name` (`"Fable 5"`).
    #[serde(default)]
    pub display_name: Option<String>,

    /// `null` in every capture to date. If it ever populates, it retires the
    /// family-token heuristic in rate-limit-segments.md §`rate_limit_7d_model`.
    #[serde(default)]
    pub id: Option<String>,
}

#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum LimitKind { Session, WeeklyAll, WeeklyScoped, #[serde(other)] Unknown }

#[derive(Default)]
#[non_exhaustive]
#[serde(rename_all = "snake_case")]
pub enum LimitSeverity { Normal, Warning, #[default] #[serde(other)] Unknown }

impl UsageLimit {
    /// The model family this limit is scoped to, or `None` for any limit
    /// that does not name one — wrong `kind`, absent `scope`, absent
    /// `model`, absent `display_name`. Those four cases are
    /// indistinguishable to every consumer, so the judgement lives here
    /// rather than being re-derived by the segment and again by each plugin.
    pub fn scoped_model_name(&self) -> Option<&str>;
}
```

`percent` takes the wire's name rather than `UsageBucket`'s `utilization`; they are the same quantity under two names, and matching the wire keeps the serde mapping direct.

`#[non_exhaustive]` sits on both enums as well as the structs: `#[serde(other)]` absorbs unknown values off the wire, but it does nothing for Rust SemVer, and a fifth `LimitKind` would otherwise break every downstream `match`.

Both enums carry `#[serde(other)]` so a new server-side `kind` or `severity`
degrades to `Unknown` instead of dropping the element — an unrecognized `kind`
simply never matches the `weekly_scoped` selection.

Three observed fields are deliberately absent. `group` (`session` | `weekly`)
is derivable from `kind`. `scope.surface` has only ever been `null`, so its
populated type is unknown; modelling it as `Option<String>` would drop the whole
element if it turns out to be an object, and nothing reads it. `is_active` is
omitted so that the "not a visibility signal" rule in
[rate-limit-segments.md](rate-limit-segments.md) §`rate_limit_7d_model` is
enforced by the type rather than by prose — it is a server-side judgement made
without knowledge of the local session's model.

`severity` is modelled while `is_active` is not, though core reads neither. The
rule is that an unread field is modelled by default, and omitted only when
exposing it would be actively misleading: `is_active` invites exactly the
misuse the segment spec forbids, whereas `severity` is merely unused.

Unmodelled fields here are unreachable, including from plugins: `unknown_buckets`
sits on the response root and does not extend into array elements. Adding one is
a struct change, which is the intended cost.

`limits` appears on all three of `UsageApiResponse`, `EndpointUsage`, and
`CachedData`; only the first is deserialized from the wire, so that is where the
tolerant deserializer lives. It reads `serde_json::Value` first, yields `None`
when that value is not an array, and otherwise deserializes each element
independently — warning and dropping the ones that fail. A single malformed entry
must not fail the response and drop the whole endpoint to the JSONL fallback.
A non-array value warns as well, with one exception: `null` yields `None`
silently, because null is this endpoint's own idiom for an absent bucket
(`seven_day_opus`, `iguana_necktie`, and others are null on every response) and
warning on it would fire on every render for accounts that have no limits.
`deserialize_line_entries` in
`crates/linesmith-core/src/config.rs` is the closest precedent, but it opens with
`Vec::<toml::Value>::deserialize(deserializer)?` and so still fails the parse on a
non-array; this one goes a step further. Per [ADR-0030](../adrs/0030-model-scoped-usage-arrives-in-a-limits-array.md).

The field stays `Option<Vec<UsageLimit>>` rather than `#[serde(default)] Vec<_>`.
The two render identically, but "the endpoint stopped sending `limits`" is the
exact drift that produced this ADR, and collapsing it into an empty vec would
leave `doctor` no way to say so.

`JsonlUsage` and the two window types expose `pub(crate)` fields plus smart constructors (`JsonlUsage::new`, `FiveHourWindow::new`, `SevenDayWindow::new`) so the aggregator owns the invariants. `#[non_exhaustive]` sits on `UsageData` (SemVer room for a future variant) and on `EndpointUsage` (upstream Anthropic can ship new bucket categories — `unknown_buckets` is evidence of prior churn); the JSONL-side structs don't need it because their fields are locked by the aggregator contract.

Only `UsageData::Endpoint(...)` is cached on disk. `UsageData::Jsonl(...)` is derived from `~/.claude/projects/**/*.jsonl` — themselves the primary on-disk record — so round-tripping JSONL through a second cache layer buys nothing. The cache schema below carries endpoint fields only; a future `schema_version` bump would be required to introduce a discriminator if that ever changes.

Three-tier cache stack per [ADR-0010](../adrs/0010-data-fetching-architecture.md):

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
    "seven_day_sonnet": null,
    "limits": [{ "kind": "weekly_scoped", "percent": 82.0,
                 "resets_at": "...", "severity": "warning",
                 "scope": { "model": { "display_name": "Fable", "id": null } } }],
    "extra_usage": { "is_enabled": false, ... },
    "unknown_buckets": { "omelette_promotional": null, ... }
  },
  "error": null
}
```

`data` and `error` are mutually exclusive (one or the other is non-null). Error cache uses a shorter TTL (30s default) so transient failures recover quickly.

The `limits` entries are what the writer produces, not raw wire JSON: `group`, `is_active`, and `scope.surface` are gone because `UsageLimit` does not model them, an unrecognized `kind` or `severity` has already become `"unknown"`, and `percent` round-trips through `Percent` as a float even though the wire sends an integer. For every field it validates, the cache is a lossy view of the response it came from.

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
6. Hit the endpoint with 2s timeout. On 200, build `UsageData::Endpoint(...)`, cache + return. On 429/timeout/network error with no stale cache, skip to JSONL fallback.
7. JSONL fallback: scan `~/.claude/projects/*/*.jsonl`, aggregate into 5h blocks + 7d window, and return `Ok(UsageData::Jsonl(...))` — NOT the masked-as-`Err(original)` path that v0.1.1 used.
8. If JSONL also empty or errors: surface the original error (`NoCredentials`, `Timeout`, `RateLimited`, etc.).

Credentials are resolved before the endpoint call (step 4 before step 6), preserving the ADR-0011 rule that `NoCredentials` is never masked as `Timeout`. This spec deliberately reorders the pure cache reads (steps 1-3) ahead of credentials compared to ADR-0011's step-2 placement, because cache hits and stale-lock serving have no need of a token — that avoids the Keychain subprocess on cache hits without weakening the `NoCredentials`-distinct-from-`Timeout` guarantee. When ADR-0011's cascade is re-read strictly in order, only the endpoint-fetch path ever reaches credentials anyway, so the behavior is equivalent even though the numbered ordering differs.

JSONL-derived values land in the dedicated [`UsageData::Jsonl`](#oauth-usage-cache-stack) variant, which carries raw `TokenCounts` rather than synthesized percentages. Segments switch their render shape on the variant ([rate-limit-segments.md §JSONL-fallback display](rate-limit-segments.md)); no tier ceiling is hardcoded. This intentionally diverges from ccstatusline (returns error-tagged widgets in JSONL mode) and CCometixLine (hides the segment); linesmith ships useful partial data in JSONL mode where both peers do not. Full rationale in [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md).

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
  - `limits` deserializer: integer `percent` (`82`) as well as float, since the wire sends an integer and the existing `utilization_clamps_*` tests cover only floats; one malformed element drops that element and keeps the rest; a non-array `limits` yields `None` and warns; an unrecognized `kind` / `severity` degrades to `Unknown` rather than failing the element
  - `limits` cache round-trip: an entry written and re-read is stable, and an entry whose `kind` was `Unknown` on the wire re-reads as `Unknown` (the lossiness is intended, but it should not compound)

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
- 2026-04-22: v0.1.2. Cascade step 7 returns `Ok(UsageData::Jsonl(...))`;
  flat struct + `UsageSource` tag replaced by
  `UsageData::{Endpoint, Jsonl}` enum carrying `TokenCounts` on the
  JSONL arm. Per [ADR-0013](../adrs/0013-jsonl-fallback-carries-token-counts.md);
  resolves jsonl-aggregation tier-ceiling open question.
