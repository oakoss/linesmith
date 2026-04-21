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

use super::cache::{CacheStore, CachedUsage, Lock, LockStore};
use super::credentials::Credentials;
use super::errors::{CredentialError, JsonlError, UsageError};
use super::fetcher::{self, UsageTransport};
use super::jsonl::JsonlAggregate;
use super::usage::{UsageApiResponse, UsageData, UsageSource};

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
/// Write failures against a real store `debug_assert!` in debug
/// builds (where they're likely a test-setup bug) and silently
/// fallthrough in release (where breaking persistence must not break
/// the statusline).
pub fn resolve_usage(
    cache: Option<&CacheStore>,
    lock: Option<&LockStore>,
    transport: &dyn UsageTransport,
    credentials: &dyn Fn() -> Arc<Result<Credentials, CredentialError>>,
    jsonl: &dyn Fn() -> Result<JsonlAggregate, JsonlError>,
    now: &dyn Fn() -> DateTime<Utc>,
    config: &UsageCascadeConfig,
) -> Result<UsageData, UsageError> {
    // v0.1 can't render JSONL-derived data (tier-aware utilization is
    // unavailable, see lsm-xhu), so we never invoke the closure. The
    // parameter stays on the signature as the lsm-xhu wiring point.
    let _ = jsonl;

    let cache_entry = read_cache(cache);
    let lock_entry = read_lock(lock);
    let now_ts = now();

    if let Some(entry) = &cache_entry {
        if is_fresh(entry, now_ts, config.cache_duration) {
            if let Some(data) = entry.data.clone() {
                return Ok(cached_to_usage_data(data, UsageSource::Endpoint));
            }
        }
    }

    let lock_active = lock_entry
        .as_ref()
        .is_some_and(|l| l.blocked_until > now_ts.timestamp());
    if lock_active {
        // Serve whatever we have without touching credentials: another
        // process is in backoff and we must honor it.
        if let Some(entry) = &cache_entry {
            if let Some(data) = entry.data.clone() {
                return Ok(cached_to_usage_data(data, UsageSource::Endpoint));
            }
            if let Some(cached) = &entry.error {
                return Err(usage_error_from_code(&cached.code));
            }
        }
        // No cache content at all: surface the lock's own error hint
        // (typically "RateLimited"). Crucially, we do NOT reach the
        // endpoint — that would defeat the cross-process spam guard
        // on cold-cache starts.
        return Err(lock_entry
            .as_ref()
            .and_then(|l| l.error.as_deref())
            .map(usage_error_from_code)
            .unwrap_or(UsageError::RateLimited { retry_after: None }));
    }

    let creds_arc = credentials();
    let creds = match &*creds_arc {
        Ok(c) => c.clone(),
        Err(CredentialError::NoCredentials) => return Err(UsageError::NoCredentials),
        Err(other) => {
            // Preserve the specific variant so `rate-limit-segments.md`
            // §Error message table can render `[Keychain error]` /
            // `[Credentials unreadable]` etc. The `Clone` impl on
            // `CredentialError` is lossy for io/serde inner errors but
            // keeps the variant tag (all segments key off) intact.
            return Err(UsageError::Credentials(other.clone()));
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
            Ok(response.into_usage_data(UsageSource::Endpoint))
        }
        // 401 is the sole failure-path exception to "serve stale on
        // error": the cached payload is tied to a no-longer-valid
        // token, so reusing it would mislead the user.
        Err(UsageError::Unauthorized) => {
            write_failure_lock(lock, now_ts, &UsageError::Unauthorized);
            Err(UsageError::Unauthorized)
        }
        Err(err) => {
            // Persist the backoff so concurrent processes honor it —
            // without this, every statusline invocation during a 429
            // or outage re-hits the endpoint.
            write_failure_lock(lock, now_ts, &err);
            if let Some(entry) = &cache_entry {
                if let Some(data) = entry.data.clone() {
                    return Ok(cached_to_usage_data(data, UsageSource::Endpoint));
                }
            }
            Err(err)
        }
    }
}

fn read_cache(cache: Option<&CacheStore>) -> Option<CachedUsage> {
    // Read-side errors (PermissionDenied, EIO on a corrupt filesystem,
    // parent-not-a-dir after a config mis-edit) collapse to "miss" so
    // the cascade can still serve the user. Next successful write
    // overwrites. The write-side helpers below `debug_assert!` because
    // that's where genuine data loss happens.
    // TODO(lsm-logger): record via structured logger once one exists.
    cache.and_then(|c| c.read().ok().flatten())
}

fn read_lock(lock: Option<&LockStore>) -> Option<Lock> {
    // Same rationale as `read_cache`: collapse to "no lock" on error.
    // TODO(lsm-logger): record via structured logger once one exists.
    lock.and_then(|l| l.read().ok().flatten())
}

fn write_cache(cache: Option<&CacheStore>, entry: CachedUsage) {
    if let Some(c) = cache {
        if let Err(e) = c.write(&entry) {
            debug_assert!(false, "cascade: cache write failed: {e}");
            // TODO(lsm-logger): forward to structured logger.
        }
    }
}

fn write_lock(lock: Option<&LockStore>, entry: Lock) {
    if let Some(l) = lock {
        if let Err(e) = l.write(&entry) {
            debug_assert!(false, "cascade: lock write failed: {e}");
            // TODO(lsm-logger): forward to structured logger.
        }
    }
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

fn cached_to_usage_data(data: super::cache::CachedData, source: UsageSource) -> UsageData {
    let response: UsageApiResponse = data.into();
    response.into_usage_data(source)
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
    use crate::data_context::jsonl::{JsonlAggregate, SevenDayWindow, TokenCounts};

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

    fn jsonl_ok() -> Result<JsonlAggregate, JsonlError> {
        Ok(JsonlAggregate {
            five_hour: None,
            seven_day: SevenDayWindow {
                window_start: Utc::now() - ChronoDuration::days(7),
                token_counts: TokenCounts::default(),
            },
            source_paths: Vec::new(),
        })
    }

    fn stale_cache_entry(age: ChronoDuration) -> CachedUsage {
        let mut entry = CachedUsage::with_data(sample_response());
        entry.cached_at = Utc::now() - age;
        entry
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

        assert_eq!(data.source, UsageSource::Endpoint);
        assert_eq!(data.five_hour.unwrap().utilization.value(), 42.0);
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

        assert_eq!(data.source, UsageSource::Endpoint);
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

        assert_eq!(data.source, UsageSource::Endpoint);
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
    fn no_credentials_surfaces_original_error_even_when_jsonl_has_data() {
        // v0.1: JSONL aggregator data doesn't populate `UsageBucket`
        // because tier-aware utilization is unavailable (lsm-xhu),
        // so the cascade surfaces the endpoint-path error regardless
        // of JSONL result.
        let err = resolve_usage(
            None,
            None,
            &FakeTransport::ok(200, "", None),
            &no_creds,
            &jsonl_ok,
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

        assert_eq!(data.source, UsageSource::Endpoint);
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
    fn endpoint_401_surfaces_unauthorized_regardless_of_jsonl() {
        // With JSONL aggregation unusable in v0.1 (lsm-xhu), both
        // jsonl-empty and jsonl-ok paths must surface the original
        // `Unauthorized` error rather than hiding rate-limit segments.
        for jsonl in [
            &jsonl_empty as &dyn Fn() -> Result<JsonlAggregate, JsonlError>,
            &jsonl_ok,
        ] {
            let err = resolve_usage(
                None,
                None,
                &FakeTransport::ok(401, "", None),
                &ok_creds,
                jsonl,
                &now_fn(),
                &config(),
            )
            .unwrap_err();
            assert!(matches!(err, UsageError::Unauthorized));
        }
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
        assert_eq!(data.source, UsageSource::Endpoint);
        assert_eq!(data.five_hour.unwrap().utilization.value(), 42.0);
    }

    #[test]
    fn endpoint_429_without_stale_falls_through_to_jsonl() {
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
        assert_eq!(data.source, UsageSource::Endpoint);
    }

    #[test]
    fn endpoint_timeout_without_stale_surfaces_timeout_regardless_of_jsonl() {
        // lsm-xhu: JSONL aggregator data can't populate UsageBucket
        // in v0.1, so jsonl-ok and jsonl-empty both surface Timeout.
        for jsonl in [
            &jsonl_empty as &dyn Fn() -> Result<JsonlAggregate, JsonlError>,
            &jsonl_ok,
        ] {
            let err = resolve_usage(
                None,
                None,
                &FakeTransport::err(io::ErrorKind::TimedOut),
                &ok_creds,
                jsonl,
                &now_fn(),
                &config(),
            )
            .unwrap_err();
            assert!(matches!(err, UsageError::Timeout));
        }
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
        assert_eq!(data.source, UsageSource::Endpoint);
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

    // Release-only: cascade must still return fetched data when
    // persistence breaks. In debug builds the write-side helpers
    // `debug_assert!` on the error (intentional, per silent-failure
    // review), so the graceful-fallthrough contract is only observable
    // in release mode. Without this split, `cargo test` (debug) would
    // panic on the exact anchor sites that silent-failure review asked
    // us to make loud.
    #[cfg(not(debug_assertions))]
    #[test]
    fn cache_write_failure_does_not_block_returned_data_in_release() {
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
        assert_eq!(data.source, UsageSource::Endpoint);
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "cache write failed")]
    fn cache_write_failure_panics_in_debug() {
        let tmp = TempDir::new().unwrap();
        let blocking_file = tmp.path().join("blocked");
        std::fs::write(&blocking_file, "x").unwrap();
        let cache = CacheStore::new(blocking_file.join("nested"));

        let _ = resolve_usage(
            Some(&cache),
            None,
            &FakeTransport::ok(200, SAMPLE_BODY, None),
            &ok_creds,
            &jsonl_empty,
            &now_fn(),
            &config(),
        );
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
        assert_eq!(data.source, UsageSource::Endpoint);
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
