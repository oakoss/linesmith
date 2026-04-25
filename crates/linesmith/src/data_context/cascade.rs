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

use chrono::{DateTime, Utc};

use super::cache::{CacheError, CacheStore, CachedUsage, Lock, LockStore};
use super::credentials::Credentials;
use super::errors::{CredentialError, JsonlError, UsageError};
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
    now: &dyn Fn() -> DateTime<Utc>,
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
        .is_some_and(|l| l.blocked_until > now_ts.timestamp());
    if lock_active {
        // Serve whatever we have without touching credentials: another
        // process is in backoff and we must honor it.
        let lock_error = lock_entry.as_ref().and_then(|l| l.error.as_deref());
        let lock_from_401 = lock_error == Some("Unauthorized");
        if let Some(entry) = &cache_entry {
            // A lock from a 401 means the cached `data` was fetched
            // with a now-revoked token; skip the stale-serve so
            // invocation B (after A's 401) doesn't return the pre-401
            // payload through this branch. Other lock errors (429,
            // timeout) still serve stale — those are transient and
            // the cached data was legitimately valid when fetched.
            if !lock_from_401 {
                if let Some(data) = entry.data.clone() {
                    return Ok(cached_to_usage_data(data));
                }
            }
            if let Some(cached) = &entry.error {
                return jsonl_or(jsonl, now_ts, usage_error_from_code(&cached.code));
            }
        }
        // No cache content (or a 401-lock bypassed the data entry):
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
                    blocked_until: add_secs(now_ts.timestamp(), config.cache_duration),
                    error: None,
                },
            );
            Ok(UsageData::Endpoint(response.into_endpoint_usage()))
        }
        // 401 is the sole failure-path exception to "serve stale on
        // error": the cached payload is tied to a no-longer-valid
        // token, so reusing it would mislead the user. JSONL, however,
        // is independent of token validity — fall through to it before
        // surfacing the error so a user with a revoked token still
        // sees their local transcript totals. The next invocation's
        // lock-active branch refuses the stale data via the
        // `lock_from_401` guard; we deliberately do NOT write an
        // "Unauthorized" error into the cache here, because the
        // cached error would then outlive the lock and mask a
        // subsequent unrelated lock (e.g. a 429 after token refresh).
        Err(UsageError::Unauthorized) => {
            write_failure_lock(lock, now_ts, &UsageError::Unauthorized);
            jsonl_or(jsonl, now_ts, UsageError::Unauthorized)
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
    now: DateTime<Utc>,
    fallback: UsageError,
) -> Result<UsageData, UsageError> {
    match build_jsonl_usage(jsonl(), now) {
        Some(data) => Ok(UsageData::Jsonl(data)),
        None => Err(fallback),
    }
}

fn build_jsonl_usage(
    result: Result<JsonlAggregate, JsonlError>,
    now: DateTime<Utc>,
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
    // Clamp `block.start` to `floor_to_hour(now)` so a future-dated
    // entry (clock skew) can't produce an `ends_at` further out than
    // the current window's nominal close. The aggregator deliberately
    // keeps token counts intact under mild skew so users don't lose
    // their current session; this clamp normalizes the reset-timer
    // surface without corrupting those totals.
    let now_floor = jsonl::floor_to_hour(now);
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

/// Real bugs (disk full, missing parent dir, EACCES) route through
/// `lsm_error!`, which bypasses the level gate so a user with
/// `LINESMITH_LOG=off` still sees the "statusline hammers the API"
/// class of defect. The documented Windows MoveFileEx race-loser case
/// (concurrent processes both calling `atomic_write_json`, the loser
/// gets `PermissionDenied`) is expected per the cache.rs contract;
/// route it through `lsm_debug!` so multi-terminal Windows users
/// don't get persistent stderr noise on otherwise-healthy runs.
fn log_persist_error(kind: &str, err: &CacheError) {
    if is_transient_persist_race(err) {
        crate::lsm_debug!("cascade: {kind} write race-loser (Windows MoveFileEx): {err}");
    } else {
        crate::lsm_error!("cascade: {kind} write failed: {err}");
    }
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

fn write_failure_lock(lock: Option<&LockStore>, now_ts: DateTime<Utc>, err: &UsageError) {
    let backoff = backoff_for_error(err);
    write_lock(
        lock,
        Lock {
            blocked_until: add_secs(now_ts.timestamp(), backoff),
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
        "ParseError" => UsageError::ParseError,
        _ => UsageError::NetworkError,
    }
}

fn is_fresh(entry: &CachedUsage, now: DateTime<Utc>, ttl: Duration) -> bool {
    // `cached_at > now` (clock skew) is filtered out by
    // `CacheStore::read`, so a normal `age < ttl` check is enough.
    match now.signed_duration_since(entry.cached_at).to_std() {
        Ok(elapsed) => elapsed < ttl,
        Err(_) => false,
    }
}

fn cached_to_usage_data(data: super::cache::CachedData) -> UsageData {
    let response: UsageApiResponse = data.into();
    UsageData::Endpoint(response.into_endpoint_usage())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::io;

    use chrono::Duration as ChronoDuration;
    use tempfile::TempDir;

    use crate::data_context::cache::{CacheStore, CachedUsage, Lock, LockStore};
    use crate::data_context::credentials::Credentials;
    use crate::data_context::errors::CredentialError;
    use crate::data_context::fetcher::{HttpResponse, UsageTransport};
    use crate::data_context::jsonl::{
        FiveHourBlock, JsonlAggregate, SevenDayWindow as JsonlSevenDayWindow, TokenCounts,
    };

    struct FakeTransport {
        response: RefCell<io::Result<HttpResponse>>,
        calls: Cell<u32>,
    }

    impl FakeTransport {
        fn ok(status: u16, body: &str, retry_after: Option<&str>) -> Self {
            Self {
                response: RefCell::new(Ok(HttpResponse {
                    status,
                    body: body.as_bytes().to_vec(),
                    retry_after: retry_after.map(String::from),
                })),
                calls: Cell::new(0),
            }
        }

        fn err(kind: io::ErrorKind) -> Self {
            Self {
                response: RefCell::new(Err(io::Error::new(kind, "fake"))),
                calls: Cell::new(0),
            }
        }
    }

    impl UsageTransport for FakeTransport {
        fn get(&self, _url: &str, _token: &str, _timeout: Duration) -> io::Result<HttpResponse> {
            self.calls.set(self.calls.get() + 1);
            match &*self.response.borrow() {
                Ok(r) => Ok(HttpResponse {
                    status: r.status,
                    body: r.body.clone(),
                    retry_after: r.retry_after.clone(),
                }),
                Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
            }
        }
    }

    const SAMPLE_BODY: &str = r#"{
        "five_hour":  { "utilization": 42.0, "resets_at": "2026-04-19T05:00:00Z" },
        "seven_day":  { "utilization": 33.0, "resets_at": "2026-04-23T19:00:00Z" }
    }"#;

    fn sample_response() -> UsageApiResponse {
        serde_json::from_str(SAMPLE_BODY).unwrap()
    }

    fn config() -> UsageCascadeConfig {
        UsageCascadeConfig::default()
    }

    fn now_fn() -> impl Fn() -> DateTime<Utc> {
        let ts = Utc::now();
        move || ts
    }

    fn ok_creds() -> Arc<Result<Credentials, CredentialError>> {
        Arc::new(Ok(Credentials::for_testing("test-token")))
    }

    fn no_creds() -> Arc<Result<Credentials, CredentialError>> {
        Arc::new(Err(CredentialError::NoCredentials))
    }

    fn jsonl_empty() -> Result<JsonlAggregate, JsonlError> {
        Err(JsonlError::NoEntries)
    }

    /// 7d-only JSONL aggregate. Exercises the case where the cascade
    /// falls back to JSONL and the 7d window is populated but no 5h
    /// block is active (e.g. the user hasn't coded in the last 5h).
    fn jsonl_ok() -> Result<JsonlAggregate, JsonlError> {
        Ok(JsonlAggregate {
            five_hour: None,
            seven_day: JsonlSevenDayWindow {
                window_start: Utc::now() - ChronoDuration::days(7),
                token_counts: TokenCounts::from_parts(1_000_000, 200_000, 0, 0),
            },
            source_paths: Vec::new(),
        })
    }

    /// JSONL aggregate with an active 5h block. Start is `now - 1h` so
    /// the block's `end()` (= start + 5h) lies ~4h in the future, a
    /// realistic reset-timer window for the 5h-reset segment tests.
    fn jsonl_ok_with_active_block() -> Result<JsonlAggregate, JsonlError> {
        let now = Utc::now();
        let start = now - ChronoDuration::hours(1);
        Ok(JsonlAggregate {
            five_hour: Some(FiveHourBlock {
                start,
                actual_last_activity: now,
                token_counts: TokenCounts::from_parts(400_000, 20_000, 0, 0),
                models: vec!["claude-opus-4-7".into()],
                usage_limit_reset: None,
            }),
            seven_day: JsonlSevenDayWindow {
                window_start: now - ChronoDuration::days(7),
                token_counts: TokenCounts::from_parts(1_000_000, 200_000, 0, 0),
            },
            source_paths: Vec::new(),
        })
    }

    fn stale_cache_entry(age: ChronoDuration) -> CachedUsage {
        let mut entry = CachedUsage::with_data(sample_response());
        entry.cached_at = Utc::now() - age;
        entry
    }

    /// Assert that `data` is the `Jsonl` variant built from the
    /// [`jsonl_ok`] fixture (no active 5h block, 7d window
    /// populated with `1_000_000 + 200_000` tokens).
    ///
    /// Fallthrough tests use this instead of `matches!(data, UsageData::Jsonl(_))`
    /// so that a cascade bug serving `SevenDayWindow::default()` or
    /// dropping the window entirely gets caught.
    fn assert_jsonl_matches_ok_fixture(data: &UsageData) {
        let UsageData::Jsonl(j) = data else {
            panic!("expected UsageData::Jsonl, got {data:?}");
        };
        assert!(
            j.five_hour.is_none(),
            "jsonl_ok fixture has no active 5h block",
        );
        assert_eq!(
            j.seven_day.tokens.total(),
            1_200_000,
            "7d total must match jsonl_ok fixture (1M input + 200k output)",
        );
    }

    #[test]
    fn fresh_disk_cache_short_circuits_without_reading_credentials() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&CachedUsage::with_data(sample_response()))
            .unwrap();

        let cred_calls = Cell::new(0u32);
        let jsonl_calls = Cell::new(0u32);
        let credentials = || {
            cred_calls.set(cred_calls.get() + 1);
            ok_creds()
        };
        let jsonl = || {
            jsonl_calls.set(jsonl_calls.get() + 1);
            jsonl_empty()
        };
        let transport = FakeTransport::ok(200, "", None);

        let data = resolve_usage(
            Some(&cache),
            None,
            &transport,
            &credentials,
            &jsonl,
            &now_fn(),
            &config(),
        )
        .expect("ok");

        let UsageData::Endpoint(endpoint) = &data else {
            panic!("expected endpoint variant, got {data:?}");
        };
        assert_eq!(endpoint.five_hour.unwrap().utilization.value(), 42.0);
        assert_eq!(cred_calls.get(), 0, "credentials must not be called");
        assert_eq!(jsonl_calls.get(), 0, "jsonl must not be called");
        assert_eq!(transport.calls.get(), 0, "no HTTP on cache hit");
    }

    #[test]
    fn stale_cache_without_lock_triggers_fetch_and_overwrites() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&stale_cache_entry(ChronoDuration::minutes(10)))
            .unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());

        let transport = FakeTransport::ok(200, SAMPLE_BODY, None);
        let data = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");

        assert!(matches!(data, UsageData::Endpoint(_)));
        assert_eq!(transport.calls.get(), 1);
        let refreshed = cache.read().unwrap().unwrap();
        let age = Utc::now().signed_duration_since(refreshed.cached_at);
        assert!(age.num_seconds() < 5, "cache must be re-stamped on success");
    }

    #[test]
    fn stale_cache_with_active_lock_serves_stale_without_credentials() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&stale_cache_entry(ChronoDuration::minutes(10)))
            .unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());
        lock.write(&Lock {
            blocked_until: Utc::now().timestamp() + 60,
            error: Some("rate-limited".into()),
        })
        .unwrap();

        let cred_calls = Cell::new(0u32);
        let credentials = || {
            cred_calls.set(cred_calls.get() + 1);
            ok_creds()
        };
        let transport = FakeTransport::ok(200, "", None);

        let data = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport,
            &credentials,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");

        assert!(matches!(data, UsageData::Endpoint(_)));
        assert_eq!(
            cred_calls.get(),
            0,
            "active lock must short-circuit before credentials read",
        );
        assert_eq!(transport.calls.get(), 0, "no HTTP when lock + stale cache");
    }

    #[test]
    fn no_credentials_surfaces_nocredentials_not_timeout() {
        let transport = FakeTransport::err(io::ErrorKind::TimedOut);
        let err = resolve_usage(
            None,
            None,
            &transport,
            &no_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::NoCredentials));
        assert_eq!(transport.calls.get(), 0, "no HTTP when credentials missing",);
    }

    #[test]
    fn no_credentials_falls_through_to_jsonl_when_available() {
        // ADR-0013: JSONL aggregation is the terminal fallback. A
        // user with no OAuth credentials who still has Claude Code
        // transcript history should see their local token totals
        // rather than `[No credentials]`.
        let data = resolve_usage(
            None,
            None,
            &FakeTransport::ok(200, "", None),
            &no_creds,
            &jsonl_ok,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert_jsonl_matches_ok_fixture(&data);
    }

    #[test]
    fn no_credentials_with_empty_jsonl_still_surfaces_nocredentials() {
        // JSONL unavailable → original endpoint-path error wins so
        // users on a clean machine see the actionable `[No credentials]`
        // rather than a silent hide.
        let err = resolve_usage(
            None,
            None,
            &FakeTransport::ok(200, "", None),
            &no_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::NoCredentials));
    }

    #[test]
    fn endpoint_200_writes_cache_and_lock() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        let lock = LockStore::new(tmp.path().to_path_buf());
        let transport = FakeTransport::ok(200, SAMPLE_BODY, None);

        let data = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");

        assert!(matches!(data, UsageData::Endpoint(_)));
        assert!(cache.read().unwrap().is_some(), "cache must be populated");
        let persisted_lock = lock.read().unwrap().unwrap();
        let expected_blocked_until =
            Utc::now().timestamp() + config().cache_duration.as_secs() as i64;
        assert!(
            (persisted_lock.blocked_until - expected_blocked_until).abs() < 5,
            "lock blocked_until = {}, expected near {}",
            persisted_lock.blocked_until,
            expected_blocked_until,
        );
    }

    #[test]
    fn endpoint_401_falls_through_to_jsonl_when_available() {
        // ADR-0013: a revoked/expired token invalidates the endpoint
        // response but not the local transcript. JSONL has to kick in
        // on 401 too, otherwise a user who rotates their token but
        // hasn't re-auth'd sees `[Unauthorized]` instead of real data
        // they could otherwise surface locally.
        let transport = FakeTransport::ok(401, "", None);
        let data = resolve_usage(
            None,
            None,
            &transport,
            &ok_creds,
            &jsonl_ok,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert_jsonl_matches_ok_fixture(&data);
        // Endpoint is still hit first — JSONL is a fallback, not a
        // short-circuit. Regression guard against a future refactor
        // that inverts the ordering.
        assert_eq!(transport.calls.get(), 1);
    }

    #[test]
    fn jsonl_fallback_clamps_future_dated_block_start_to_now() {
        // Clock-skew regression (Codex P2, 2026-04-22): a future-dated
        // entry makes `block.start = floor_to_hour(future_timestamp)`,
        // which lies beyond `now`. Without clamping, `FiveHourWindow`
        // would derive an `ends_at` further in the future than 5h,
        // inflating the reset countdown and distorting `rate_limit_5h`
        // tokens. The aggregator keeps the skewed block so mild-skew
        // users don't lose their session; the cascade clamps
        // `block.start` to `floor_to_hour(now)` before surfacing.
        let now = Utc::now();
        // Build a skewed block at +2h so `block.start` starts in the
        // future and `ends_at = start + 5h` would land ~7h out.
        let skewed_start = now + ChronoDuration::hours(2);
        let skewed: Result<JsonlAggregate, JsonlError> = Ok(JsonlAggregate {
            five_hour: Some(FiveHourBlock {
                start: skewed_start,
                actual_last_activity: now + ChronoDuration::minutes(30),
                token_counts: TokenCounts::from_parts(100, 0, 0, 0),
                models: vec!["claude-opus-4-7".into()],
                usage_limit_reset: None,
            }),
            seven_day: JsonlSevenDayWindow {
                window_start: now - ChronoDuration::days(7),
                token_counts: TokenCounts::from_parts(100, 0, 0, 0),
            },
            source_paths: Vec::new(),
        });
        let skewed_closure = || match &skewed {
            Ok(agg) => Ok(agg.clone()),
            Err(_) => Err(JsonlError::NoEntries),
        };
        let now_clock = move || now;
        let data = resolve_usage(
            None,
            None,
            &FakeTransport::err(io::ErrorKind::TimedOut),
            &ok_creds,
            &skewed_closure,
            &now_clock,
            &config(),
        )
        .expect("ok");
        let UsageData::Jsonl(j) = &data else {
            panic!("expected jsonl variant, got {data:?}");
        };
        let window = j
            .five_hour
            .as_ref()
            .expect("active block should populate five_hour window");
        // Clamped: start cannot exceed floor_to_hour(now), so
        // ends_at <= floor_to_hour(now) + 5h <= now + 5h.
        assert!(
            window.ends_at() <= now + ChronoDuration::hours(5),
            "ends_at={:?} must be clamped at/before now + 5h ({:?})",
            window.ends_at(),
            now + ChronoDuration::hours(5),
        );
    }

    #[test]
    fn jsonl_fallback_surfaces_five_hour_window_with_ends_at() {
        // End-to-end: under endpoint failure + active JSONL block, the
        // cascade wraps `block.end()` as `FiveHourWindow.ends_at` so
        // `rate_limit_5h_reset` can derive its countdown without a
        // tier-aware `resets_at`.
        let data = resolve_usage(
            None,
            None,
            &FakeTransport::err(io::ErrorKind::TimedOut),
            &ok_creds,
            &jsonl_ok_with_active_block,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        let UsageData::Jsonl(j) = &data else {
            panic!("expected jsonl variant, got {data:?}");
        };
        let window = j
            .five_hour
            .as_ref()
            .expect("active block should populate five_hour window");
        let expected_ends_at = Utc::now() + ChronoDuration::hours(4);
        let drift = (window.ends_at() - expected_ends_at).num_seconds().abs();
        assert!(
            drift < 5,
            "ends_at={:?} drifted {drift}s from expected",
            window.ends_at(),
        );
        // Total from the active-block fixture (400_000 + 20_000 input+output).
        assert_eq!(window.tokens.total(), 420_000);
    }

    #[test]
    fn endpoint_401_with_empty_jsonl_surfaces_unauthorized() {
        let err = resolve_usage(
            None,
            None,
            &FakeTransport::ok(401, "", None),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::Unauthorized));
    }

    #[test]
    fn endpoint_401_does_not_serve_stale_cache() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&stale_cache_entry(ChronoDuration::minutes(10)))
            .unwrap();
        let err = resolve_usage(
            Some(&cache),
            None,
            &FakeTransport::ok(401, "", None),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::Unauthorized));
    }

    #[test]
    fn invocation_after_401_does_not_serve_stale_cache_via_lock_active() {
        // A→B sequence: invocation A gets a 401 that wrote a failure-
        // lock with error="Unauthorized". Invocation B within the
        // lock TTL must NOT serve the pre-401 cached `data: Some(...)`
        // through the lock-active branch — the `lock_from_401` guard
        // catches it. Same "401 does not serve stale" contract as
        // endpoint_401_does_not_serve_stale_cache, but via the A→B
        // code path that test doesn't exercise.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&stale_cache_entry(ChronoDuration::minutes(10)))
            .unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());

        // Invocation A: 401.
        let transport_a = FakeTransport::ok(401, "", None);
        let err_a = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport_a,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err_a, UsageError::Unauthorized));

        // Invocation B: lock active, cache still holds pre-401 data.
        // Transport returns fresh 200 data if hit; we assert it isn't.
        let transport_b = FakeTransport::ok(200, SAMPLE_BODY, None);
        let err_b = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport_b,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err_b, UsageError::Unauthorized));
        assert_eq!(
            transport_b.calls.get(),
            0,
            "active lock must still gate the endpoint on invocation B",
        );
    }

    #[test]
    fn invocation_after_401_falls_through_to_jsonl_when_available() {
        // ADR-0013 parity for the A→B sequence: when invocation A
        // 401'd and invocation B has JSONL data, B gets local
        // transcript totals instead of either the stale cache or the
        // Unauthorized error.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&stale_cache_entry(ChronoDuration::minutes(10)))
            .unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());

        let data_a = resolve_usage(
            Some(&cache),
            Some(&lock),
            &FakeTransport::ok(401, "", None),
            &ok_creds,
            &jsonl_ok,
            &now_fn(),
            &config(),
        )
        .expect("A falls through to JSONL with jsonl_ok");
        assert_jsonl_matches_ok_fixture(&data_a);

        let transport_b = FakeTransport::ok(200, SAMPLE_BODY, None);
        let data_b = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport_b,
            &ok_creds,
            &jsonl_ok,
            &now_fn(),
            &config(),
        )
        .expect("B returns JSONL on lock-active path");
        assert_jsonl_matches_ok_fixture(&data_b);
        assert_eq!(transport_b.calls.get(), 0);
    }

    #[test]
    fn active_unauthorized_lock_rejects_stale_cached_data() {
        // Isolates the `lock_from_401` guard: seeds `cache.data =
        // Some(stale)` + `lock.error = Some("Unauthorized")` directly,
        // bypassing the A→B integration path. The seeded state is
        // realistic because a different process could have run the
        // 401 (writing the lock) while leaving our cache untouched.
        // Verifies the guard refuses to serve the stale data without
        // depending on the 401 handler's own write ordering.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&stale_cache_entry(ChronoDuration::minutes(10)))
            .unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());
        lock.write(&Lock {
            blocked_until: Utc::now().timestamp() + 30,
            error: Some("Unauthorized".into()),
        })
        .unwrap();

        let transport = FakeTransport::ok(200, SAMPLE_BODY, None);
        let err = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::Unauthorized));
        assert_eq!(transport.calls.get(), 0);
    }

    #[test]
    fn endpoint_429_writes_lock_with_retry_after_backoff() {
        // Codex P1: without this, every concurrent process re-hits
        // the endpoint during a rate-limit window.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        let lock = LockStore::new(tmp.path().to_path_buf());

        let _ = resolve_usage(
            Some(&cache),
            Some(&lock),
            &FakeTransport::ok(429, "", Some("120")),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        );

        let persisted = lock.read().unwrap().expect("lock must be written");
        let expected = Utc::now().timestamp() + 120;
        assert!(
            (persisted.blocked_until - expected).abs() < 5,
            "blocked_until={}, expected near {}",
            persisted.blocked_until,
            expected,
        );
        assert_eq!(persisted.error.as_deref(), Some("RateLimited"));
    }

    #[test]
    fn endpoint_timeout_writes_lock_with_error_ttl() {
        let tmp = TempDir::new().unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());

        let _ = resolve_usage(
            None,
            Some(&lock),
            &FakeTransport::err(io::ErrorKind::TimedOut),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        );

        let persisted = lock.read().unwrap().expect("lock must be written");
        let expected = Utc::now().timestamp() + DEFAULT_ERROR_TTL.as_secs() as i64;
        assert!(
            (persisted.blocked_until - expected).abs() < 5,
            "blocked_until={}, expected near {}",
            persisted.blocked_until,
            expected,
        );
        assert_eq!(persisted.error.as_deref(), Some("Timeout"));
    }

    #[test]
    fn lock_written_on_429_blocks_next_process_from_hitting_endpoint() {
        // End-to-end P1a+P1b: process A gets a 429 and writes the
        // lock; process B observes the lock and skips the endpoint.
        // Without either half of the fix, B stampedes the rate-limited
        // endpoint.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        let lock = LockStore::new(tmp.path().to_path_buf());

        let transport_a = FakeTransport::ok(429, "", Some("120"));
        let _ = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport_a,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        );

        let transport_b = FakeTransport::ok(200, SAMPLE_BODY, None);
        let result_b = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport_b,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        );
        assert!(matches!(result_b, Err(UsageError::RateLimited { .. })));
        assert_eq!(
            transport_b.calls.get(),
            0,
            "process B must not hit endpoint"
        );
    }

    #[test]
    fn endpoint_401_writes_lock_so_peers_skip_the_stale_token() {
        let tmp = TempDir::new().unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());

        let _ = resolve_usage(
            None,
            Some(&lock),
            &FakeTransport::ok(401, "", None),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        );

        let persisted = lock.read().unwrap().expect("lock must be written");
        assert_eq!(persisted.error.as_deref(), Some("Unauthorized"));
    }

    #[test]
    fn endpoint_429_with_stale_cache_serves_stale() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&stale_cache_entry(ChronoDuration::minutes(10)))
            .unwrap();
        let data = resolve_usage(
            Some(&cache),
            None,
            &FakeTransport::ok(429, "", Some("120")),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        let UsageData::Endpoint(endpoint) = &data else {
            panic!("expected endpoint variant, got {data:?}");
        };
        assert_eq!(endpoint.five_hour.unwrap().utilization.value(), 42.0);
    }

    #[test]
    fn endpoint_429_with_empty_jsonl_surfaces_ratelimited() {
        // Endpoint + JSONL both empty → original rate-limit error wins
        // so the user sees `[Rate limited]` rather than a silent hide.
        let err = resolve_usage(
            None,
            None,
            &FakeTransport::ok(429, "", None),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::RateLimited { .. }));
    }

    #[test]
    fn endpoint_429_falls_through_to_jsonl_when_available() {
        // ADR-0013: rate-limited users with a local transcript see
        // `~5h: ...` / `~7d: ...` rather than `[Rate limited]`.
        let transport = FakeTransport::ok(429, "", None);
        let data = resolve_usage(
            None,
            None,
            &transport,
            &ok_creds,
            &jsonl_ok,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert_jsonl_matches_ok_fixture(&data);
        assert_eq!(transport.calls.get(), 1);
    }

    #[test]
    fn endpoint_timeout_with_stale_cache_serves_stale() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&stale_cache_entry(ChronoDuration::minutes(10)))
            .unwrap();
        let data = resolve_usage(
            Some(&cache),
            None,
            &FakeTransport::err(io::ErrorKind::TimedOut),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert!(matches!(data, UsageData::Endpoint(_)));
    }

    #[test]
    fn endpoint_timeout_without_stale_falls_through_to_jsonl() {
        // ADR-0013: Timeout / NetworkError falls through to JSONL so
        // an offline user still sees their local token totals.
        let transport = FakeTransport::err(io::ErrorKind::TimedOut);
        let data = resolve_usage(
            None,
            None,
            &transport,
            &ok_creds,
            &jsonl_ok,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert_jsonl_matches_ok_fixture(&data);
        assert_eq!(
            transport.calls.get(),
            1,
            "endpoint must be attempted before JSONL fallback",
        );
    }

    #[test]
    fn endpoint_timeout_without_stale_or_jsonl_surfaces_original_error() {
        let err = resolve_usage(
            None,
            None,
            &FakeTransport::err(io::ErrorKind::TimedOut),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::Timeout));
    }

    #[test]
    fn endpoint_network_error_falls_through_same_as_timeout() {
        let err = resolve_usage(
            None,
            None,
            &FakeTransport::err(io::ErrorKind::ConnectionRefused),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::NetworkError));
    }

    #[test]
    fn endpoint_malformed_response_falls_through_to_jsonl() {
        let err = resolve_usage(
            None,
            None,
            &FakeTransport::ok(200, "{ not valid", None),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::ParseError));
    }

    #[test]
    fn cascade_tolerates_missing_cache_and_lock_stores() {
        // Mirrors the no-cache-root branch (HOME and XDG both unset):
        // cascade must still reach credentials + endpoint instead of
        // hard-erroring on cache I/O.
        let data = resolve_usage(
            None,
            None,
            &FakeTransport::ok(200, SAMPLE_BODY, None),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert!(matches!(data, UsageData::Endpoint(_)));
    }

    #[test]
    fn expired_lock_does_not_gate_fetch() {
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&stale_cache_entry(ChronoDuration::minutes(10)))
            .unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());
        lock.write(&Lock {
            blocked_until: Utc::now().timestamp() - 60,
            error: None,
        })
        .unwrap();

        let transport = FakeTransport::ok(200, SAMPLE_BODY, None);
        let _ = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert_eq!(
            transport.calls.get(),
            1,
            "expired lock must not block fetch"
        );
    }

    #[test]
    fn active_lock_with_no_cached_data_does_not_hit_endpoint() {
        // Cold-cache start during another process's backoff window:
        // the lock must block the fetch even without stale data to
        // serve, else every concurrent statusline invocation stampedes
        // `/api/oauth/usage`. Flagged P1 by Codex.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        let lock = LockStore::new(tmp.path().to_path_buf());
        lock.write(&Lock {
            blocked_until: Utc::now().timestamp() + 60,
            error: Some("RateLimited".into()),
        })
        .unwrap();

        let cred_calls = Cell::new(0u32);
        let credentials = || {
            cred_calls.set(cred_calls.get() + 1);
            ok_creds()
        };
        let transport = FakeTransport::ok(200, SAMPLE_BODY, None);
        let err = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport,
            &credentials,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::RateLimited { .. }));
        assert_eq!(cred_calls.get(), 0, "must not resolve credentials");
        assert_eq!(transport.calls.get(), 0, "must not hit endpoint");
    }

    #[test]
    fn active_lock_falls_through_to_jsonl_when_available() {
        // ADR-0013: even when gated by another process's backoff lock,
        // a populated JSONL aggregate wins over the lock-hint error so
        // rate-limited users with local transcripts see `~5h: ...`.
        // The lock still gates the endpoint — no HTTP call may happen.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        let lock = LockStore::new(tmp.path().to_path_buf());
        lock.write(&Lock {
            blocked_until: Utc::now().timestamp() + 60,
            error: Some("RateLimited".into()),
        })
        .unwrap();

        let transport = FakeTransport::ok(200, SAMPLE_BODY, None);
        let data = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport,
            &ok_creds,
            &jsonl_ok,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert_jsonl_matches_ok_fixture(&data);
        assert_eq!(
            transport.calls.get(),
            0,
            "active lock must still gate the endpoint even with JSONL data"
        );
    }

    #[test]
    fn active_lock_serves_cached_error_without_hitting_endpoint() {
        // When the cache carries a specific error tag (e.g. Unauthorized
        // from a prior 401), the lock-active path must surface that
        // code — not the generic lock-hint — so plugins/segments see
        // the real reason.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&CachedUsage::with_error("Unauthorized"))
            .unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());
        lock.write(&Lock {
            blocked_until: Utc::now().timestamp() + 60,
            error: Some("RateLimited".into()),
        })
        .unwrap();

        let transport = FakeTransport::ok(200, "", None);
        let err = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(matches!(err, UsageError::Unauthorized));
        assert_eq!(transport.calls.get(), 0);
    }

    #[test]
    fn active_lock_with_cached_error_falls_through_to_jsonl_when_available() {
        // ADR-0013 + silent-failure review: when the cache carries a
        // specific error code AND the lock is active AND JSONL has
        // data, the JSONL fallback wins. Otherwise users with a
        // cached `Unauthorized` plus a valid transcript would see
        // `[Unauthorized]` instead of their local totals — the exact
        // failure mode the ADR rejects.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&CachedUsage::with_error("Unauthorized"))
            .unwrap();
        let lock = LockStore::new(tmp.path().to_path_buf());
        lock.write(&Lock {
            blocked_until: Utc::now().timestamp() + 60,
            error: Some("RateLimited".into()),
        })
        .unwrap();

        let transport = FakeTransport::ok(200, "", None);
        let data = resolve_usage(
            Some(&cache),
            Some(&lock),
            &transport,
            &ok_creds,
            &jsonl_ok,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert_jsonl_matches_ok_fixture(&data);
        assert_eq!(transport.calls.get(), 0);
    }

    #[test]
    fn credential_failure_other_than_missing_preserves_variant_tag() {
        // `rate-limit-segments.md` §Error message table distinguishes
        // `[Keychain error]` from `[No credentials]`, so the cascade
        // must preserve the specific CredentialError flavor. Only
        // `NoCredentials` maps to the flat `UsageError::NoCredentials`;
        // everything else wraps.
        let creds_err: Arc<Result<Credentials, CredentialError>> =
            Arc::new(Err(CredentialError::MissingField {
                path: std::path::PathBuf::from("/x"),
            }));
        let credentials = || creds_err.clone();
        let err = resolve_usage(
            None,
            None,
            &FakeTransport::err(io::ErrorKind::TimedOut),
            &credentials,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                UsageError::Credentials(CredentialError::MissingField { .. })
            ),
            "expected Credentials(MissingField), got {err:?}",
        );
        assert_eq!(err.code(), "MissingField", "variant tag must round-trip");
    }

    #[test]
    fn subprocess_failed_cred_preserves_subprocess_tag() {
        // `SubprocessFailed` carries a non-Clone `io::Error`; the
        // lossy Clone impl on CredentialError must still preserve the
        // variant so segments can render `[Keychain error]`.
        let creds_err: Arc<Result<Credentials, CredentialError>> = Arc::new(Err(
            CredentialError::SubprocessFailed(io::Error::new(io::ErrorKind::PermissionDenied, "x")),
        ));
        let credentials = || creds_err.clone();
        let err = resolve_usage(
            None,
            None,
            &FakeTransport::err(io::ErrorKind::TimedOut),
            &credentials,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .unwrap_err();
        assert_eq!(err.code(), "SubprocessFailed");
    }

    #[test]
    fn credential_variant_falls_through_to_jsonl_when_available() {
        // ADR-0013: non-`NoCredentials` cred failures (broken Keychain,
        // malformed credentials.json) still fall through to JSONL when
        // the transcript is readable, rather than hard-returning the
        // cred error variant. Common degraded-environment scenario.
        let creds_err: Arc<Result<Credentials, CredentialError>> = Arc::new(Err(
            CredentialError::SubprocessFailed(io::Error::new(io::ErrorKind::PermissionDenied, "x")),
        ));
        let credentials = || creds_err.clone();
        let data = resolve_usage(
            None,
            None,
            &FakeTransport::err(io::ErrorKind::TimedOut),
            &credentials,
            &jsonl_ok,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert_jsonl_matches_ok_fixture(&data);
    }

    // Cascade must still return fetched data when persistence breaks.
    // The write-side helpers log via `lsm_error!` and continue (the
    // cache.rs contract permits per-call failures), so this contract
    // is observable in both debug and release builds.
    #[test]
    fn cache_write_failure_does_not_block_returned_data() {
        let tmp = TempDir::new().unwrap();
        let blocking_file = tmp.path().join("blocked");
        std::fs::write(&blocking_file, "x").unwrap();
        let cache = CacheStore::new(blocking_file.join("nested"));

        let data = resolve_usage(
            Some(&cache),
            None,
            &FakeTransport::ok(200, SAMPLE_BODY, None),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert!(matches!(data, UsageData::Endpoint(_)));
    }

    #[test]
    fn fresh_cache_is_source_endpoint_not_jsonl() {
        // Regression guard: it would be tempting to tag cached data
        // as `Jsonl` to signal "stale" — but the cache stores the
        // original endpoint payload, so `Endpoint` is correct.
        // Segments decide staleness via TTL, not via the tag.
        let tmp = TempDir::new().unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());
        cache
            .write(&CachedUsage::with_data(sample_response()))
            .unwrap();

        let data = resolve_usage(
            Some(&cache),
            None,
            &FakeTransport::ok(200, "", None),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert!(matches!(data, UsageData::Endpoint(_)));
    }

    #[test]
    fn clock_skew_future_cached_at_treats_entry_as_stale() {
        // `CacheStore::read` already drops entries with `cached_at >
        // now`, so the cascade sees no entry and falls through to the
        // endpoint. Pin the behavior here so a future relaxation of
        // `CacheStore::read` doesn't silently let the cascade serve
        // future-stamped junk.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("usage.json");
        let mut entry = CachedUsage::with_data(sample_response());
        entry.cached_at = Utc::now() + ChronoDuration::hours(1);
        std::fs::write(&path, serde_json::to_string(&entry).unwrap()).unwrap();
        let cache = CacheStore::new(tmp.path().to_path_buf());

        let transport = FakeTransport::ok(200, SAMPLE_BODY, None);
        let _ = resolve_usage(
            Some(&cache),
            None,
            &transport,
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        )
        .expect("ok");
        assert_eq!(transport.calls.get(), 1);
    }
}
