//! Wire-level integration tests for `UreqTransport`.
//!
//! The `UsageTransport` trait hides headers behind a `Bearer token + URL + timeout`
//! façade, so the unit tests in `fetcher.rs` exercise only the trait surface —
//! header-construction inside `UreqTransport::get` is unreachable from them.
//! These tests stand up a real `mockito` HTTP server and assert on the bytes the
//! server actually receives.
//!
//! Contract pinned per ADR-0011 §Endpoint contract:
//!   - `Authorization: Bearer <token>`
//!   - `anthropic-beta: oauth-2025-04-20`
//!   - `User-Agent: linesmith/<CARGO_PKG_VERSION>`
//!   - `http_status_as_error(false)` — 4xx/5xx surface as `Ok(HttpResponse)`,
//!     not `Err`, so [`fetch_usage`] can classify 401 / 429 / 5xx distinctly
//!   - Timeouts map to `io::ErrorKind::TimedOut`
//!   - 3xx redirects to the same origin are followed transparently
//!
//! A ureq minor bump that changes header capitalization, redirect default, or
//! status-error mode will surface here, not in production telemetry.

use std::io;
use std::time::Duration;

use linesmith_core::data_context::fetcher::{
    default_user_agent, UreqTransport, UsageTransport, DEFAULT_TIMEOUT, OAUTH_USAGE_PATH,
};

/// Clear ambient proxy env so tests don't route through a developer's or CI
/// proxy. Runs unconditionally (not gated by `Once`) so any future
/// `UreqTransport::new()` in this binary gets isolation regardless of test
/// ordering.
fn new_isolated_transport() -> UreqTransport {
    for var in [
        "ALL_PROXY",
        "all_proxy",
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
    ] {
        std::env::remove_var(var);
    }
    std::env::set_var("NO_PROXY", "127.0.0.1,localhost");
    UreqTransport::new()
}

const TEST_TOKEN: &str = "test-token-abc123";
const TEST_BODY: &str = r#"{
    "five_hour": { "utilization": 22.0, "resets_at": "2026-04-19T05:00:00Z" },
    "seven_day": { "utilization": 33.0, "resets_at": "2026-04-23T19:00:00Z" }
}"#;

fn endpoint(server: &mockito::Server) -> String {
    format!("{}{}", server.url(), OAUTH_USAGE_PATH)
}

#[test]
fn sends_authorization_bearer_header() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", OAUTH_USAGE_PATH)
        .match_header("authorization", format!("Bearer {TEST_TOKEN}").as_str())
        .with_status(200)
        .with_body(TEST_BODY)
        .create();

    let transport = new_isolated_transport();
    let resp = transport
        .get(&endpoint(&server), TEST_TOKEN, DEFAULT_TIMEOUT)
        .expect("transport call");
    assert_eq!(resp.status, 200);
    mock.assert();
}

#[test]
fn sends_anthropic_beta_header() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", OAUTH_USAGE_PATH)
        .match_header("anthropic-beta", "oauth-2025-04-20")
        .with_status(200)
        .with_body(TEST_BODY)
        .create();

    let transport = new_isolated_transport();
    let resp = transport
        .get(&endpoint(&server), TEST_TOKEN, DEFAULT_TIMEOUT)
        .expect("transport call");
    assert_eq!(resp.status, 200);
    mock.assert();
}

#[test]
fn sends_user_agent_header_with_crate_name_and_version() {
    // `default_user_agent()` is unit-tested in `fetcher.rs`; this catches
    // a regression where the constructor builds the right string but
    // `.header("User-Agent", ...)` is dropped (e.g., ureq overriding it).
    let mut server = mockito::Server::new();
    let expected = default_user_agent();
    let mock = server
        .mock("GET", OAUTH_USAGE_PATH)
        .match_header("user-agent", expected.as_str())
        .with_status(200)
        .with_body(TEST_BODY)
        .create();

    let transport = new_isolated_transport();
    let resp = transport
        .get(&endpoint(&server), TEST_TOKEN, DEFAULT_TIMEOUT)
        .expect("transport call");
    assert_eq!(resp.status, 200);
    mock.assert();
}

#[test]
fn non_2xx_status_surfaces_as_ok_response() {
    // `http_status_as_error(false)` lets `fetch_usage` branch on 401/429/5xx
    // distinctly. Flipping that flag collapses them into a single `Err`,
    // breaking the rate-limit cache stack.
    for status in [401_u16, 429, 503] {
        let mut server = mockito::Server::new();
        let mock = server
            .mock("GET", OAUTH_USAGE_PATH)
            .with_status(status as usize)
            .with_body("")
            .create();

        let transport = new_isolated_transport();
        let resp = transport
            .get(&endpoint(&server), TEST_TOKEN, DEFAULT_TIMEOUT)
            .expect("non-2xx must not be Err");
        assert_eq!(resp.status, status, "status {status} round-trip");
        mock.assert();
    }
}

#[test]
fn captures_retry_after_header_on_429() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", OAUTH_USAGE_PATH)
        .with_status(429)
        .with_header("retry-after", "120")
        .with_body("")
        .create();

    let transport = new_isolated_transport();
    let resp = transport
        .get(&endpoint(&server), TEST_TOKEN, DEFAULT_TIMEOUT)
        .expect("transport call");
    assert_eq!(resp.status, 429);
    assert_eq!(resp.retry_after.as_deref(), Some("120"));
    mock.assert();
}

#[test]
fn timeout_surfaces_as_io_errorkind_timedout() {
    // mockito's `with_chunked_body` lets us hold the response open. The
    // server starts writing, ureq's global timeout fires mid-read, and
    // the transport must surface `ErrorKind::TimedOut` so `fetch_usage`
    // can map it to `UsageError::Timeout` (distinct from generic
    // `NetworkError`, which would suppress the user-visible "request
    // timed out" message).
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", OAUTH_USAGE_PATH)
        .with_status(200)
        .with_chunked_body(|_w| {
            // Returning Ok() would close the stream cleanly; sleep past
            // the client's timeout to force a mid-read hang.
            std::thread::sleep(Duration::from_secs(5));
            Ok(())
        })
        .create();

    let transport = new_isolated_transport();
    let short_timeout = Duration::from_millis(150);
    let started = std::time::Instant::now();
    let result = transport.get(&endpoint(&server), TEST_TOKEN, short_timeout);
    let elapsed = started.elapsed();
    let err = match result {
        Ok(_) => panic!("expected timeout, got Ok response"),
        Err(e) => e,
    };
    assert_eq!(
        err.kind(),
        io::ErrorKind::TimedOut,
        "expected TimedOut, got {:?}: {err}",
        err.kind()
    );
    // Pin both the duration argument and the server-interaction:
    // - `elapsed < 2s` proves the 150ms `short_timeout` fired (a regression
    //   hard-coding `timeout_global(5s)` would let the server sleep run to
    //   completion).
    // - `mock.assert()` proves the request reached the server, ruling out a
    //   connect-phase failure (`ConnectionRefused`, DNS) that also maps to
    //   `ErrorKind::TimedOut` on some platforms.
    assert!(
        elapsed < Duration::from_secs(2),
        "timeout fired too late ({elapsed:?}); 150ms argument not honored"
    );
    mock.assert();
}

#[test]
fn follows_same_origin_redirect_and_returns_final_body() {
    // The CDN in front of the Anthropic endpoint may 3xx during incident
    // routing. ADR-0011 has no distinct error variant for redirect failures,
    // so following transparently is observable behavior; a ureq bump that
    // changes the default (e.g., requires opting in via `max_redirects`)
    // must fail here.
    let mut server = mockito::Server::new();
    let final_path = "/api/oauth/usage/final";
    let redirect = server
        .mock("GET", OAUTH_USAGE_PATH)
        .with_status(302)
        .with_header("location", final_path)
        .with_body("")
        .create();
    let final_mock = server
        .mock("GET", final_path)
        .with_status(200)
        .with_body(TEST_BODY)
        .create();

    let transport = new_isolated_transport();
    let resp = transport
        .get(&endpoint(&server), TEST_TOKEN, DEFAULT_TIMEOUT)
        .expect("transport call");
    assert_eq!(resp.status, 200, "redirect should have been followed");
    assert_eq!(resp.body, TEST_BODY.as_bytes());
    redirect.assert();
    final_mock.assert();
}

#[test]
fn returns_empty_body_for_204() {
    // The transport must not error on a zero-byte body — that's the
    // ParseError branch's responsibility, not the transport's.
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", OAUTH_USAGE_PATH)
        .with_status(204)
        .with_body("")
        .create();

    let transport = new_isolated_transport();
    let resp = transport
        .get(&endpoint(&server), TEST_TOKEN, DEFAULT_TIMEOUT)
        .expect("transport call");
    assert_eq!(resp.status, 204);
    assert!(
        resp.body.is_empty(),
        "expected empty body, got {:?}",
        resp.body
    );
    mock.assert();
}

#[test]
fn passes_response_body_through_verbatim() {
    let mut server = mockito::Server::new();
    let mock = server
        .mock("GET", OAUTH_USAGE_PATH)
        .with_status(200)
        .with_body(TEST_BODY)
        .create();

    let transport = new_isolated_transport();
    let resp = transport
        .get(&endpoint(&server), TEST_TOKEN, DEFAULT_TIMEOUT)
        .expect("transport call");
    assert_eq!(resp.status, 200);
    assert_eq!(resp.body, TEST_BODY.as_bytes());
    mock.assert();
}
