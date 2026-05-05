use super::*;
use std::cell::{Cell, RefCell};
use std::io;

use jiff::SignedDuration as ChronoDuration;
use tempfile::TempDir;

use crate::data_context::cache::{CacheStore, CachedUsage, Lock, LockStore};
use crate::data_context::credentials::Credentials;
use crate::data_context::error::CredentialError;
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

fn now_fn() -> impl Fn() -> Timestamp {
    let ts = Timestamp::now();
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
            window_start: Timestamp::now() - ChronoDuration::from_hours(7 * 24),
            token_counts: TokenCounts::from_parts(1_000_000, 200_000, 0, 0),
        },
        source_paths: Vec::new(),
    })
}

/// JSONL aggregate with an active 5h block. Start is `now - 1h` so
/// the block's `end()` (= start + 5h) lies ~4h in the future, a
/// realistic reset-timer window for the 5h-reset segment tests.
fn jsonl_ok_with_active_block() -> Result<JsonlAggregate, JsonlError> {
    let now = Timestamp::now();
    let start = now - ChronoDuration::from_hours(1);
    Ok(JsonlAggregate {
        five_hour: Some(FiveHourBlock {
            start,
            actual_last_activity: now,
            token_counts: TokenCounts::from_parts(400_000, 20_000, 0, 0),
            models: vec!["claude-opus-4-7".into()],
            usage_limit_reset: None,
        }),
        seven_day: JsonlSevenDayWindow {
            window_start: now - ChronoDuration::from_hours(7 * 24),
            token_counts: TokenCounts::from_parts(1_000_000, 200_000, 0, 0),
        },
        source_paths: Vec::new(),
    })
}

fn stale_cache_entry(age: ChronoDuration) -> CachedUsage {
    let mut entry = CachedUsage::with_data(sample_response());
    entry.cached_at = Timestamp::now() - age;
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
        .write(&stale_cache_entry(ChronoDuration::from_mins(10)))
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
    let age = Timestamp::now().duration_since(refreshed.cached_at);
    assert!(age.as_secs() < 5, "cache must be re-stamped on success");
}

#[test]
fn stale_cache_with_active_lock_serves_stale_without_credentials() {
    let tmp = TempDir::new().unwrap();
    let cache = CacheStore::new(tmp.path().to_path_buf());
    cache
        .write(&stale_cache_entry(ChronoDuration::from_mins(10)))
        .unwrap();
    let lock = LockStore::new(tmp.path().to_path_buf());
    lock.write(&Lock {
        blocked_until: Timestamp::now().as_second() + 60,
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
        Timestamp::now().as_second() + config().cache_duration.as_secs() as i64;
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
    let now = Timestamp::now();
    // Build a skewed block at +2h so `block.start` starts in the
    // future and `ends_at = start + 5h` would land ~7h out.
    let skewed_start = now + ChronoDuration::from_hours(2);
    let skewed: Result<JsonlAggregate, JsonlError> = Ok(JsonlAggregate {
        five_hour: Some(FiveHourBlock {
            start: skewed_start,
            actual_last_activity: now + ChronoDuration::from_mins(30),
            token_counts: TokenCounts::from_parts(100, 0, 0, 0),
            models: vec!["claude-opus-4-7".into()],
            usage_limit_reset: None,
        }),
        seven_day: JsonlSevenDayWindow {
            window_start: now - ChronoDuration::from_hours(7 * 24),
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
        window.ends_at() <= now + ChronoDuration::from_hours(5),
        "ends_at={:?} must be clamped at/before now + 5h ({:?})",
        window.ends_at(),
        now + ChronoDuration::from_hours(5),
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
    let expected_ends_at = Timestamp::now() + ChronoDuration::from_hours(4);
    let drift = window
        .ends_at()
        .duration_since(expected_ends_at)
        .as_secs()
        .abs();
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
        .write(&stale_cache_entry(ChronoDuration::from_mins(10)))
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
        .write(&stale_cache_entry(ChronoDuration::from_mins(10)))
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
        .write(&stale_cache_entry(ChronoDuration::from_mins(10)))
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
        .write(&stale_cache_entry(ChronoDuration::from_mins(10)))
        .unwrap();
    let lock = LockStore::new(tmp.path().to_path_buf());
    lock.write(&Lock {
        blocked_until: Timestamp::now().as_second() + 30,
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
    let expected = Timestamp::now().as_second() + 120;
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
    let expected = Timestamp::now().as_second() + DEFAULT_ERROR_TTL.as_secs() as i64;
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
        .write(&stale_cache_entry(ChronoDuration::from_mins(10)))
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
        .write(&stale_cache_entry(ChronoDuration::from_mins(10)))
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
        .write(&stale_cache_entry(ChronoDuration::from_mins(10)))
        .unwrap();
    let lock = LockStore::new(tmp.path().to_path_buf());
    lock.write(&Lock {
        blocked_until: Timestamp::now().as_second() - 60,
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
        blocked_until: Timestamp::now().as_second() + 60,
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
        blocked_until: Timestamp::now().as_second() + 60,
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
        blocked_until: Timestamp::now().as_second() + 60,
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
        blocked_until: Timestamp::now().as_second() + 60,
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
    entry.cached_at = Timestamp::now() + ChronoDuration::from_hours(1);
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

// --- classify_persist_error contract ---
//
// log_persist_error routes via macros (lsm_debug! / lsm_error!) which
// write to stderr. classify_persist_error is the pure half. A refactor
// that silently dropped EITHER emission path would still pass the
// surviving cache_write_failure_does_not_block_* happy-path test
// (which only asserts the cascade returns endpoint data); these
// tests fail loud on the route + message format so the regression
// can't sneak through.

fn make_io_error(kind: io::ErrorKind) -> CacheError {
    CacheError::Io {
        path: std::path::PathBuf::from("/test/path"),
        cause: io::Error::new(kind, "test"),
    }
}

fn make_persist_error(kind: io::ErrorKind) -> CacheError {
    CacheError::Persist {
        path: std::path::PathBuf::from("/test/path"),
        cause: io::Error::new(kind, "test"),
    }
}

#[test]
fn classify_persist_error_routes_io_failure_to_error() {
    let (class, msg) = classify_persist_error("cache", &make_io_error(io::ErrorKind::NotFound));
    assert_eq!(class, PersistLogClass::Error);
    assert!(
        msg.contains("cascade: cache write failed:"),
        "expected loud-signal prefix, got {msg:?}"
    );
}

#[test]
fn classify_persist_error_routes_lock_kind_into_message() {
    let (class, msg) =
        classify_persist_error("lock", &make_persist_error(io::ErrorKind::OutOfMemory));
    assert_eq!(class, PersistLogClass::Error);
    assert!(
        msg.contains("cascade: lock write failed:"),
        "kind label must thread through, got {msg:?}"
    );
}

#[cfg(unix)]
#[test]
fn classify_persist_error_routes_permission_denied_to_error_on_unix() {
    // PermissionDenied on unix is a real perm bug (EACCES), not a
    // transient race — `is_transient_persist_race` returns false
    // on cfg(not(windows)) so this stays loud.
    let (class, msg) = classify_persist_error(
        "cache",
        &make_persist_error(io::ErrorKind::PermissionDenied),
    );
    assert_eq!(class, PersistLogClass::Error);
    assert!(msg.contains("cascade: cache write failed:"));
}

#[cfg(windows)]
#[test]
fn classify_persist_error_routes_persist_permission_denied_to_debug_on_windows() {
    // The documented MoveFileEx race-loser signature: Persist
    // variant + PermissionDenied cause. Routes to Debug so multi-
    // terminal Windows users don't see stderr noise.
    let (class, msg) = classify_persist_error(
        "cache",
        &make_persist_error(io::ErrorKind::PermissionDenied),
    );
    assert_eq!(class, PersistLogClass::Debug);
    assert!(
        msg.contains("race-loser") && msg.contains("Windows MoveFileEx"),
        "expected race-loser framing, got {msg:?}"
    );
}

#[cfg(windows)]
#[test]
fn classify_persist_error_routes_io_permission_denied_to_error_on_windows() {
    // Even on Windows, PermissionDenied via the Io variant (not
    // Persist) is a real bug — only the Persist+PermissionDenied
    // combination is the MoveFileEx race signature.
    let (class, _msg) =
        classify_persist_error("cache", &make_io_error(io::ErrorKind::PermissionDenied));
    assert_eq!(class, PersistLogClass::Error);
}

// Production `log_persist_error` and these tests share the SAME
// `route_persist_error` match block, so a future arm-swap (Debug
// routing to Error or vice versa) fails loud here.

#[test]
fn route_persist_error_dispatches_debug_class_to_debug_closure_only() {
    let mut debug_calls = 0;
    let mut error_calls = 0;
    route_persist_error(
        PersistLogClass::Debug,
        "msg",
        |_| debug_calls += 1,
        |_| error_calls += 1,
    );
    assert_eq!((debug_calls, error_calls), (1, 0));
}

#[test]
fn route_persist_error_dispatches_error_class_to_error_closure_only() {
    let mut debug_calls = 0;
    let mut error_calls = 0;
    route_persist_error(
        PersistLogClass::Error,
        "msg",
        |_| debug_calls += 1,
        |_| error_calls += 1,
    );
    assert_eq!((debug_calls, error_calls), (0, 1));
}

#[test]
fn route_persist_error_passes_msg_through_unchanged() {
    let mut received: Option<String> = None;
    route_persist_error(
        PersistLogClass::Error,
        "cascade: cache write failed: disk full",
        |_| {},
        |s| received = Some(s.to_string()),
    );
    assert_eq!(
        received.as_deref(),
        Some("cascade: cache write failed: disk full")
    );
}
