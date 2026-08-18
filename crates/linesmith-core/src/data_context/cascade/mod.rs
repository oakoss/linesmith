//! OAuth usage fallback cascade.
//!
//! Glues the slice modules (`cache`, `credentials`, `fetcher`, `jsonl`,
//! `usage`) into the full cascade from `docs/specs/data-fetching.md`
//! §OAuth fallback cascade. The orchestrator is a pure function keyed
//! on injected dependencies so every branch is exercised without real
//! I/O, network, or Keychain access.
//!
//! The lock-active short-circuit runs before the credentials read so
//! a process observing another's backoff window can answer from disk
//! (or the JSONL fallback) without paying the Keychain subprocess.
//! `NoCredentials`-vs-`Timeout` masking is preserved because
//! credentials are still resolved before any endpoint call that could
//! time out.

use std::sync::Arc;
use std::time::Duration;

use jiff::Timestamp;

use super::cache::{CacheError, CacheStore, CachedUsage, Lock, LockStore};
use super::credentials::Credentials;
use super::error::{CredentialError, JsonlError, UsageError};
use super::fetcher::{self, UsageTransport};
use super::jsonl::{self, JsonlAggregate};
use super::usage::{FiveHourWindow, JsonlUsage, SevenDayWindow, UsageApiResponse, UsageData};

/// Default cache freshness window per
/// `docs/specs/data-fetching.md` §OAuth usage cache stack.
pub const DEFAULT_CACHE_DURATION: Duration = Duration::from_secs(180);

/// Shorter TTL applied to error responses and lock-backoff windows
/// for non-429 failures, per `docs/specs/data-fetching.md` §OAuth
/// usage cache stack ("Error cache uses a shorter TTL (30s default)").
pub const DEFAULT_ERROR_TTL: Duration = Duration::from_secs(30);

/// Fallback backoff when a `429` arrives without a parseable
/// `Retry-After`. Matches `DEFAULT_RATE_LIMIT_BACKOFF` in `fetcher.rs`
/// (300s per ADR-0011 §Cache stack).
pub const DEFAULT_RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(300);

/// Backoff for auth failures a token refresh can't resolve. Re-probing
/// only succeeds once the user signs in again, so this trades recovery
/// latency for endpoint load: a user who re-authenticates early waits
/// out the rest of the window, because `lock_active` short-circuits
/// before credentials are re-read. The JSONL fallback renders through
/// that window, so the cost is estimated-instead-of-exact, not blank.
pub const DEFAULT_AUTH_BACKOFF: Duration = Duration::from_secs(300);

/// Default endpoint base URL per ADR-0011 §Endpoint contract.
pub const DEFAULT_API_BASE_URL: &str = "https://api.anthropic.com";

/// Tunables threaded into [`resolve_usage`]. Out-of-box defaults match
/// `docs/specs/data-fetching.md` §OAuth usage cache stack.
#[derive(Debug, Clone)]
pub struct UsageCascadeConfig {
    pub api_base_url: String,
    pub timeout: Duration,
    pub cache_duration: Duration,
}

impl Default for UsageCascadeConfig {
    fn default() -> Self {
        Self {
            api_base_url: DEFAULT_API_BASE_URL.into(),
            timeout: fetcher::DEFAULT_TIMEOUT,
            cache_duration: DEFAULT_CACHE_DURATION,
        }
    }
}

/// Resolve OAuth usage data using the full fallback cascade.
///
/// `credentials` and `jsonl` are lazily evaluated: the cascade does
/// NOT invoke either on a fresh-cache or stale-lock-serve path,
/// preserving the "no Keychain subprocess on cache hits" guarantee.
/// `cache` and `lock` being `None` is equivalent to pointing at paths
/// that don't exist: reads degrade to "miss" and writes are skipped.
/// Write failures fall into two classes. Real bugs (disk full,
/// missing parent dir, EACCES) log via `lsm_error!` (bypasses the
/// level gate). The documented Windows MoveFileEx race-loser case
/// logs via `lsm_debug!` (suppressible) so multi-terminal Windows
/// users don't get persistent stderr noise on healthy runs. Either
/// way the cascade still returns fetched data.
pub fn resolve_usage(
    cache: Option<&CacheStore>,
    lock: Option<&LockStore>,
    transport: &dyn UsageTransport,
    credentials: &dyn Fn() -> Arc<Result<Credentials, CredentialError>>,
    jsonl: &dyn Fn() -> Result<JsonlAggregate, JsonlError>,
    now: &dyn Fn() -> Timestamp,
    config: &UsageCascadeConfig,
) -> Result<UsageData, UsageError> {
    let cache_entry = read_cache(cache);
    let lock_entry = read_lock(lock);
    let now_ts = now();

    if let Some(entry) = &cache_entry {
        if is_fresh(entry, now_ts, config.cache_duration) {
            if let Some(data) = entry.data.clone() {
                return Ok(cached_to_usage_data(data));
            }
        }
    }

    let lock_active = lock_entry
        .as_ref()
        .is_some_and(|l| l.blocked_until > now_ts.as_second());
    if lock_active {
        // Serve whatever we have without touching credentials: another
        // process is in backoff and we must honor it.
        let lock_error = lock_entry.as_ref().and_then(|l| l.error.as_deref());
        let lock_from_auth = matches!(lock_error, Some("Unauthorized" | "Forbidden"));
        if let Some(entry) = &cache_entry {
            // Auth failures have no bounded recovery time, and
            // stale-serve has no age ceiling — usage would keep
            // accruing behind numbers that never move. 429 and timeout
            // still serve stale: those clear on their own.
            if !lock_from_auth {
                if let Some(data) = entry.data.clone() {
                    return Ok(cached_to_usage_data(data));
                }
            }
            if let Some(cached) = &entry.error {
                return jsonl_or(jsonl, now_ts, usage_error_from_code(&cached.code));
            }
        }
        // No cache content (or an auth lock bypassed the data entry):
        // try JSONL before surfacing the lock's own error hint.
        // Crucially, we still do NOT reach the endpoint — that would
        // defeat the cross-process spam guard on cold-cache starts.
        let lock_err = lock_error
            .map(usage_error_from_code)
            .unwrap_or(UsageError::RateLimited { retry_after: None });
        return jsonl_or(jsonl, now_ts, lock_err);
    }

    let creds_arc = credentials();
    let creds = match &*creds_arc {
        Ok(c) => c.clone(),
        // INVARIANT: credential failures never write a failure-lock.
        // They're not network transients — the same error will recur
        // on every invocation until the user fixes their creds file /
        // Keychain ACL, so a lock would just replay the error and
        // delay recovery. If a future CredentialError variant becomes
        // genuinely retry-stable, add a matching `write_failure_lock`
        // here and update the test suite accordingly.
        Err(CredentialError::NoCredentials) => {
            return jsonl_or(jsonl, now_ts, UsageError::NoCredentials)
        }
        Err(other) => {
            // Preserve the specific variant so `rate-limit-segments.md`
            // §Error message table can render `[Keychain error]` /
            // `[Credentials unreadable]` etc. The `Clone` impl on
            // `CredentialError` is lossy for io/serde inner errors but
            // keeps the variant tag (all segments key off) intact.
            return jsonl_or(jsonl, now_ts, UsageError::Credentials(other.clone()));
        }
    };

    match fetcher::fetch_usage(transport, &config.api_base_url, &creds, config.timeout) {
        Ok(response) => {
            write_cache(cache, CachedUsage::with_data(response.clone()));
            write_lock(
                lock,
                Lock {
                    blocked_until: add_secs(now_ts.as_second(), config.cache_duration),
                    error: None,
                },
            );
            Ok(UsageData::Endpoint(response.into_endpoint_usage()))
        }
        // Auth failures are the failure-path exception to "serve stale
        // on error": recovery time is unbounded from here (401 may
        // clear on Claude Code's next refresh, 403 needs a re-login)
        // and stale-serve has no age ceiling, so the cached payload
        // would outlive its accuracy with nothing marking it. JSONL,
        // however, is independent of token validity — fall through to
        // it before surfacing the error so a user with broken auth
        // still sees their local transcript totals. We deliberately
        // do NOT write the error
        // into the cache here, because the cached error would then
        // outlive the lock and mask a subsequent unrelated lock (e.g.
        // a 429 after token refresh). Instead, clear the cache file so
        // a peer invocation arriving after this one (and before the
        // lock expires) reads no cache entry, can't short-circuit on
        // the fresh-cache check, and falls through to the
        // `lock_from_auth` guard.
        //
        // This only changes behavior after an invocation has reached
        // the endpoint and seen the failure. When access is lost while
        // the cache is still fresh, every render returns at the
        // top-of-cascade `is_fresh` short-circuit until the entry
        // expires; that single-process first-invocation window is
        // fundamental at this layer. Closing it would require
        // out-of-band revalidation (background probe, credentials-
        // file watch) and is tracked separately.
        Err(err @ (UsageError::Unauthorized | UsageError::Forbidden)) => {
            if let Some(c) = cache {
                if let Err(e) = c.clear() {
                    // `lsm_error!` (bypasses `LINESMITH_LOG=off`)
                    // matches the severity this module already uses
                    // for cache-write failures: a clear failure
                    // leaves peers serving unrefreshable data
                    // through the fresh-cache short-circuit until
                    // the entry's TTL expires.
                    crate::lsm_error!(
                        "cascade: cache clear after {err} failed; peers may serve unrefreshable data until TTL expires: {e}"
                    );
                }
            }
            write_failure_lock(lock, now_ts, &err);
            jsonl_or(jsonl, now_ts, err)
        }
        Err(err) => {
            // Persist the backoff so concurrent processes honor it —
            // without this, every statusline invocation during a 429
            // or outage re-hits the endpoint.
            write_failure_lock(lock, now_ts, &err);
            if let Some(entry) = &cache_entry {
                if let Some(data) = entry.data.clone() {
                    return Ok(cached_to_usage_data(data));
                }
            }
            jsonl_or(jsonl, now_ts, err)
        }
    }
}

/// Build a [`UsageData::Jsonl`] from the aggregator if it produced any
/// data; otherwise surface `fallback` unchanged. Callers pass the
/// endpoint-path error they would have returned, so a JSONL miss
/// preserves the original failure reason the user sees. `now` is
/// threaded through so the mapping can clamp future-dated block
/// starts (clock skew) to a sane bound — see [`build_jsonl_usage`].
fn jsonl_or(
    jsonl: &dyn Fn() -> Result<JsonlAggregate, JsonlError>,
    now: Timestamp,
    fallback: UsageError,
) -> Result<UsageData, UsageError> {
    match build_jsonl_usage(jsonl(), now) {
        Some(data) => Ok(UsageData::Jsonl(data)),
        None => Err(fallback),
    }
}

fn build_jsonl_usage(
    result: Result<JsonlAggregate, JsonlError>,
    now: Timestamp,
) -> Option<JsonlUsage> {
    let agg = match result {
        Ok(agg) => agg,
        Err(JsonlError::NoEntries | JsonlError::DirectoryMissing) => return None,
        Err(other) => {
            // `DataContext::resolve_usage_default` already collapses
            // IoError / ParseError to NoEntries with a warn trace, so
            // this arm is only reachable from direct test callers. Warn
            // anyway so any future cascade caller that threads the real
            // aggregator error through leaves a stderr breadcrumb.
            crate::lsm_warn!(
                "cascade: JSONL fallback unavailable ({other}); surfacing endpoint error"
            );
            return None;
        }
    };
    // Clamp `block.start` to `floor_to_grain(now, 3600)` so a future-dated
    // entry (clock skew) can't produce an `ends_at` further out than
    // the current window's nominal close. The aggregator deliberately
    // keeps token counts intact under mild skew so users don't lose
    // their current session; this clamp normalizes the reset-timer
    // surface without corrupting those totals.
    let now_floor = jsonl::floor_to_grain(now, 3600);
    let five_hour = agg.five_hour.as_ref().map(|block| {
        let start = block.start.min(now_floor);
        FiveHourWindow::new(block.token_counts, start)
    });
    let seven_day = SevenDayWindow::new(agg.seven_day.token_counts);
    // Reaching here implies the aggregator returned `Ok(...)`; any
    // aggregator failure (including the mod.rs-collapsed variants)
    // kept us out of this branch. Token counts may still be zero —
    // a parseable record can lie outside the 7d window or outside
    // any active 5h block — so `five_hour: None` and/or a
    // zero-valued `seven_day` are valid post-conditions here.
    Some(JsonlUsage::new(five_hour, seven_day))
}

fn read_cache(cache: Option<&CacheStore>) -> Option<CachedUsage> {
    cache.and_then(|c| match c.read() {
        Ok(hit) => hit,
        Err(e) => {
            log_cache_read_failure("cache", &e);
            None
        }
    })
}

fn read_lock(lock: Option<&LockStore>) -> Option<Lock> {
    lock.and_then(|l| match l.read() {
        Ok(hit) => hit,
        Err(e) => {
            log_cache_read_failure("lock", &e);
            None
        }
    })
}

/// A cache/lock read error always collapses to "miss" so the cascade
/// keeps serving the user, but not every error is equivalent. Ephemeral
/// kinds (`NotFound`, truncated-read) are normal cold-start / partial-
/// write symptoms and stay at debug. Persistent kinds (permission,
/// ENOSPC, corrupt payload) are config defects that won't self-heal and
/// silently force every invocation back onto the endpoint — escalate
/// those so a user chasing "why does my statusline hammer the API"
/// finds the cause without `LINESMITH_LOG=debug`.
fn log_cache_read_failure(kind: &str, err: &super::cache::CacheError) {
    use std::io::ErrorKind;
    let io_kind = match err {
        super::cache::CacheError::Io { cause, .. }
        | super::cache::CacheError::Persist { cause, .. } => cause.kind(),
    };
    match io_kind {
        ErrorKind::NotFound | ErrorKind::UnexpectedEof => {
            crate::lsm_debug!("cascade: {kind} read failed: {err}; treating as miss");
        }
        _ => {
            crate::lsm_warn!("cascade: {kind} read failed: {err}");
        }
    }
}

fn write_cache(cache: Option<&CacheStore>, entry: CachedUsage) {
    if let Some(c) = cache {
        if let Err(e) = c.write(&entry) {
            log_persist_error("cache", &e);
        }
    }
}

fn write_lock(lock: Option<&LockStore>, entry: Lock) {
    if let Some(l) = lock {
        if let Err(e) = l.write(&entry) {
            log_persist_error("lock", &e);
        }
    }
}

/// Routing tag for [`classify_persist_error`]. `Error` bypasses the
/// log level gate (real bugs surface even with `LINESMITH_LOG=off`);
/// `Debug` is gated (Windows MoveFileEx race losers are expected and
/// shouldn't pollute stderr on healthy multi-terminal runs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistLogClass {
    Error,
    Debug,
}

/// Pure classification of a persistence failure. Returns the routing
/// class plus the formatted message. Split from [`log_persist_error`]
/// so tests can lock in the contract (route + message format) without
/// touching global log state or capturing process stderr.
fn classify_persist_error(kind: &str, err: &CacheError) -> (PersistLogClass, String) {
    if is_transient_persist_race(err) {
        (
            PersistLogClass::Debug,
            format!("cascade: {kind} write race-loser (Windows MoveFileEx): {err}"),
        )
    } else {
        (
            PersistLogClass::Error,
            format!("cascade: {kind} write failed: {err}"),
        )
    }
}

/// Dispatch a classified persist error to the right severity sink.
/// Generic over the emit closures so production callers pass the
/// `lsm_debug!`/`lsm_error!` macros while tests pass capturing
/// closures — the match arms themselves are shared, so a future
/// arm-swap regression fails loud in the routing tests below.
fn route_persist_error<D, E>(class: PersistLogClass, msg: &str, on_debug: D, on_error: E)
where
    D: FnOnce(&str),
    E: FnOnce(&str),
{
    match class {
        PersistLogClass::Debug => on_debug(msg),
        PersistLogClass::Error => on_error(msg),
    }
}

/// Real bugs (disk full, missing parent dir, EACCES) route through
/// `lsm_error!`, which bypasses the level gate so a user with
/// `LINESMITH_LOG=off` still sees the "statusline hammers the API"
/// class of defect. The documented Windows MoveFileEx race-loser case
/// (concurrent processes both calling `atomic_write_json`, the loser
/// gets `PermissionDenied`) is expected per the cache.rs contract;
/// route it through `lsm_debug!` so multi-terminal Windows users
/// don't get persistent stderr noise on otherwise-healthy runs.
fn log_persist_error(kind: &str, err: &CacheError) {
    let (class, msg) = classify_persist_error(kind, err);
    route_persist_error(
        class,
        &msg,
        |s| crate::lsm_debug!("{s}"),
        |s| crate::lsm_error!("{s}"),
    );
}

#[cfg(windows)]
fn is_transient_persist_race(err: &CacheError) -> bool {
    matches!(
        err,
        CacheError::Persist { cause, .. }
            if cause.kind() == std::io::ErrorKind::PermissionDenied
    )
}

#[cfg(not(windows))]
fn is_transient_persist_race(_err: &CacheError) -> bool {
    // Unix `rename(2)` doesn't expose this race; PermissionDenied on
    // Unix is always a real perm bug and stays loud.
    false
}

fn write_failure_lock(lock: Option<&LockStore>, now_ts: Timestamp, err: &UsageError) {
    let backoff = backoff_for_error(err);
    write_lock(
        lock,
        Lock {
            blocked_until: add_secs(now_ts.as_second(), backoff),
            error: Some(err.code().to_string()),
        },
    );
}

fn backoff_for_error(err: &UsageError) -> Duration {
    match err {
        UsageError::RateLimited {
            retry_after: Some(d),
        } => *d,
        UsageError::RateLimited { retry_after: None } => DEFAULT_RATE_LIMIT_BACKOFF,
        // A 403 won't clear until the user re-authenticates, so the
        // 30s transient TTL would re-probe a dead endpoint ~2900x/day.
        UsageError::Forbidden => DEFAULT_AUTH_BACKOFF,
        _ => DEFAULT_ERROR_TTL,
    }
}

fn add_secs(base_ts: i64, secs: Duration) -> i64 {
    // `LockStore::read` caps the read side of this (MAX_LOCK_DURATION
    // ceiling in cache.rs), so saturating to i64::MAX here is safe —
    // any pathological config gets sanitized on the next read.
    let offset = i64::try_from(secs.as_secs()).unwrap_or(i64::MAX);
    base_ts.saturating_add(offset)
}

/// Reconstruct a `UsageError` from a cached `.code()` tag. Used when
/// an active lock or error-cached entry tells us "another process
/// just saw X" and we want to honor that semantic downstream without
/// having the full error payload. Unknown codes fall back to
/// `NetworkError` — the most generic transient failure.
///
/// INVARIANT: credential-layer codes (`SubprocessFailed`, `MissingField`,
/// `EmptyToken`, `IoError`) and JSONL-layer codes (`NoEntries`,
/// `DirectoryMissing`) are intentionally NOT matched here and collapse
/// to `NetworkError`. They're unreachable today because the credential
/// arm at `resolve_usage` returns before any `write_failure_lock` call
/// (see the matching "credential failures never write a failure-lock"
/// invariant in `resolve_usage`), and JSONL errors never enter the
/// cache's error-code path. If a future change persists one of those
/// codes to the cache or lock, extend this match — the lsm-50fs bead
/// tracks the structural fix.
fn usage_error_from_code(code: &str) -> UsageError {
    match code {
        "NoCredentials" => UsageError::NoCredentials,
        "Timeout" => UsageError::Timeout,
        "RateLimited" => UsageError::RateLimited { retry_after: None },
        "Unauthorized" => UsageError::Unauthorized,
        "Forbidden" => UsageError::Forbidden,
        "ParseError" => UsageError::ParseError,
        _ => UsageError::NetworkError,
    }
}

fn is_fresh(entry: &CachedUsage, now: Timestamp, ttl: Duration) -> bool {
    // `cached_at > now` (clock skew) is filtered out by
    // `CacheStore::read`, so a normal `age < ttl` check is enough.
    match Duration::try_from(now.duration_since(entry.cached_at)) {
        Ok(elapsed) => elapsed < ttl,
        Err(_) => false,
    }
}

fn cached_to_usage_data(data: super::cache::CachedData) -> UsageData {
    let response: UsageApiResponse = data.into();
    UsageData::Endpoint(response.into_endpoint_usage())
}

#[cfg(test)]
mod tests;
