//! OAuth `/api/oauth/usage` HTTP client.
//!
//! Lowest layer of the rate-limit data pipeline. Talks HTTP to the
//! Anthropic endpoint, classifies the response, and returns a parsed
//! [`UsageApiResponse`] on success. Retry / stale-serve / JSONL
//! fallback behavior lives in the orchestrator above this layer;
//! this module has no knowledge of the usage-data cache stack.
//! (The `ureq::Agent` held by [`UreqTransport`] maintains an internal
//! connection pool — that's a transport-level concern, separate from
//! the response cache.)
//!
//! Canonical spec: `docs/adrs/0011-rate-limit-data-source.md`
//! §Endpoint contract and §Cache stack (Retry-After rules).

use std::io;
use std::time::{Duration, SystemTime};

use super::credentials::Credentials;
use super::errors::UsageError;
use super::usage::UsageApiResponse;

/// Forward-compat header Anthropic currently requires for the OAuth
/// usage endpoint. When this rotates we bump the value here.
const ANTHROPIC_BETA_HEADER: &str = "anthropic-beta";
const ANTHROPIC_BETA_VALUE: &str = "oauth-2025-04-20";

/// Endpoint path appended to the configured `usage.api_base_url`.
pub const OAUTH_USAGE_PATH: &str = "/api/oauth/usage";

/// Default per-request timeout per ADR-0011 §Endpoint contract.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2);

/// Fallback backoff for 429 responses that omit the `Retry-After`
/// header. ADR-0011 §Cache stack specifies 300s.
const DEFAULT_RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(300);

/// Upper bound on parsed `Retry-After` values. A malformed or
/// pathological header like `Retry-After: 18446744073709551615`
/// would otherwise produce a Duration the orchestrator can't
/// meaningfully compare with a cache TTL.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// Cap on the response body we'll read. The live endpoint emits
/// roughly 500 bytes; 64 KiB is two orders of magnitude headroom
/// without letting a misbehaving (or MITM'd) server OOM us.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

// --- Transport trait ----------------------------------------------------

/// Raw HTTP response — enough for [`fetch_usage`] to classify the
/// outcome without leaking the HTTP crate to callers.
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    /// Verbatim `Retry-After` header value if present. Parsing lives
    /// in [`parse_retry_after`] so the transport stays dumb.
    pub retry_after: Option<String>,
}

/// Injected HTTP surface. [`UreqTransport`] is the default; tests use
/// a fake to exercise status-handling paths without real I/O. Errors
/// carry [`io::ErrorKind::TimedOut`] for timeouts; anything else is
/// treated as a generic network failure in [`fetch_usage`].
pub trait UsageTransport {
    fn get(&self, url: &str, token: &str, timeout: Duration) -> io::Result<HttpResponse>;
}

// --- Public entry point -------------------------------------------------

/// Fetch usage data for the given credentials. Maps transport errors
/// and HTTP status codes onto the [`UsageError`] taxonomy per
/// `docs/specs/rate-limit-segments.md` §Error message table.
pub fn fetch_usage(
    transport: &dyn UsageTransport,
    base_url: &str,
    creds: &Credentials,
    timeout: Duration,
) -> Result<UsageApiResponse, UsageError> {
    let url = build_url(base_url);
    match transport.get(&url, creds.token(), timeout) {
        Ok(resp) => interpret_status(resp),
        Err(e) if e.kind() == io::ErrorKind::TimedOut => Err(UsageError::Timeout),
        Err(_) => Err(UsageError::NetworkError),
    }
}

/// Strip a trailing slash from `base_url` before appending the fixed
/// path so both `"https://api.anthropic.com"` and
/// `"https://api.anthropic.com/"` yield a single-slash URL.
fn build_url(base_url: &str) -> String {
    let trimmed = base_url.trim_end_matches('/');
    format!("{trimmed}{OAUTH_USAGE_PATH}")
}

fn interpret_status(resp: HttpResponse) -> Result<UsageApiResponse, UsageError> {
    match resp.status {
        200..=299 => serde_json::from_slice(&resp.body).map_err(|_| UsageError::ParseError),
        401 => Err(UsageError::Unauthorized),
        429 => {
            let retry_after = resp
                .retry_after
                .as_deref()
                .and_then(parse_retry_after)
                .or(Some(DEFAULT_RATE_LIMIT_BACKOFF));
            Err(UsageError::RateLimited { retry_after })
        }
        // 3xx (when redirects are disabled or exhausted) and 5xx fall
        // here. ADR-0011 doesn't carve out a distinct variant for
        // server errors; `NetworkError` triggers the stale-cache
        // fallback path in the orchestrator, which is the intended
        // behavior for transient server failures.
        _ => Err(UsageError::NetworkError),
    }
}

/// Parse a `Retry-After` header value. Accepts the two RFC 9110
/// §10.2.3 forms (RFC 7231 §7.1.3 superseded): integer seconds
/// (`"120"`) and HTTP-date (`"Fri, 31 Dec 2026 23:59:59 GMT"`).
/// Past-dated values and values the `httpdate` crate rejects fall
/// back to `None`; the caller then applies
/// [`DEFAULT_RATE_LIMIT_BACKOFF`]. Values above [`MAX_RETRY_AFTER`]
/// are capped so a malformed header can't produce a Duration the
/// orchestrator treats as "never retry."
fn parse_retry_after(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    let parsed = if let Ok(secs) = raw.parse::<u64>() {
        Some(Duration::from_secs(secs))
    } else {
        let when = httpdate::parse_http_date(raw).ok()?;
        when.duration_since(SystemTime::now()).ok()
    };
    parsed.map(|d| d.min(MAX_RETRY_AFTER))
}

// --- ureq transport -----------------------------------------------------

/// `ureq::Agent`-backed [`UsageTransport`]. One agent is shared across
/// all `fetch_usage` calls so connection pooling / keepalive applies.
pub struct UreqTransport {
    agent: ureq::Agent,
    user_agent: String,
}

impl UreqTransport {
    #[must_use]
    pub fn new() -> Self {
        // `http_status_as_error(false)` so 4xx/5xx surface as
        // `Ok(Response)` with a status code rather than `Err` — we
        // need to inspect 401 and 429 specifically.
        let config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            user_agent: default_user_agent(),
        }
    }
}

/// Build the `User-Agent` header value from the compile-time package
/// version. Exposed for regression tests that pin the format contract.
#[must_use]
pub fn default_user_agent() -> String {
    format!("linesmith/{}", env!("CARGO_PKG_VERSION"))
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UsageTransport for UreqTransport {
    fn get(&self, url: &str, token: &str, timeout: Duration) -> io::Result<HttpResponse> {
        let auth = format!("Bearer {token}");
        let mut response = self
            .agent
            .get(url)
            .config()
            .timeout_global(Some(timeout))
            .build()
            .header("Authorization", &auth)
            .header(ANTHROPIC_BETA_HEADER, ANTHROPIC_BETA_VALUE)
            .header("User-Agent", &self.user_agent)
            .call()
            .map_err(ureq_err_to_io)?;

        let status: u16 = response.status().as_u16();
        let retry_after = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .map(String::from);

        // Cap the body read so a misbehaving / MITM'd server can't
        // OOM us. Oversized responses surface as ParseError in
        // `interpret_status` (the serde parse will fail on the
        // truncated prefix).
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(ureq_err_to_io)?;

        Ok(HttpResponse {
            status,
            body,
            retry_after,
        })
    }
}

/// Collapse `ureq::Error` into `io::Error` for the [`UsageTransport`]
/// boundary. Timeouts keep [`io::ErrorKind::TimedOut`] so
/// [`fetch_usage`] can differentiate; everything else is treated as
/// a generic network failure.
fn ureq_err_to_io(e: ureq::Error) -> io::Error {
    match e {
        ureq::Error::Timeout(_) => io::Error::new(io::ErrorKind::TimedOut, "request timed out"),
        ureq::Error::Io(inner) => inner,
        other => io::Error::other(other.to_string()),
    }
}

// --- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn creds() -> Credentials {
        Credentials::for_testing("test-token-xyz")
    }

    /// Single pre-programmed response returned on every `get()`. Tests
    /// verify the call arguments (URL, token, timeout) separately via
    /// `captured`.
    struct FakeTransport {
        response: io::Result<HttpResponse>,
        captured: RefCell<Option<FakeCall>>,
    }

    #[derive(Debug, Clone)]
    struct FakeCall {
        url: String,
        token: String,
        timeout: Duration,
    }

    impl UsageTransport for FakeTransport {
        fn get(&self, url: &str, token: &str, timeout: Duration) -> io::Result<HttpResponse> {
            *self.captured.borrow_mut() = Some(FakeCall {
                url: url.to_string(),
                token: token.to_string(),
                timeout,
            });
            match &self.response {
                Ok(r) => Ok(HttpResponse {
                    status: r.status,
                    body: r.body.clone(),
                    retry_after: r.retry_after.clone(),
                }),
                Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
            }
        }
    }

    fn ok_transport(status: u16, body: &str, retry_after: Option<&str>) -> FakeTransport {
        FakeTransport {
            response: Ok(HttpResponse {
                status,
                body: body.as_bytes().to_vec(),
                retry_after: retry_after.map(String::from),
            }),
            captured: RefCell::new(None),
        }
    }

    fn err_transport(kind: io::ErrorKind) -> FakeTransport {
        FakeTransport {
            response: Err(io::Error::new(kind, "fake")),
            captured: RefCell::new(None),
        }
    }

    const SAMPLE_OK_BODY: &str = r#"{
        "five_hour": { "utilization": 22.0, "resets_at": "2026-04-19T05:00:00Z" },
        "seven_day": { "utilization": 33.0, "resets_at": "2026-04-23T19:00:00Z" }
    }"#;

    #[test]
    fn fetch_happy_path_parses_live_shape() {
        let transport = ok_transport(200, SAMPLE_OK_BODY, None);
        let resp = fetch_usage(
            &transport,
            "https://api.anthropic.com",
            &creds(),
            DEFAULT_TIMEOUT,
        )
        .expect("ok");
        assert_eq!(resp.five_hour.unwrap().utilization.value(), 22.0);
    }

    #[test]
    fn fetch_builds_url_without_double_slash() {
        let transport = ok_transport(200, SAMPLE_OK_BODY, None);
        let _ = fetch_usage(
            &transport,
            "https://api.anthropic.com/",
            &creds(),
            DEFAULT_TIMEOUT,
        );
        let captured = transport.captured.borrow().clone().unwrap();
        assert_eq!(captured.url, "https://api.anthropic.com/api/oauth/usage");
    }

    #[test]
    fn fetch_passes_token_through_to_transport() {
        let transport = ok_transport(200, SAMPLE_OK_BODY, None);
        let _ = fetch_usage(
            &transport,
            "https://example.test",
            &Credentials::for_testing("unique-token-42"),
            DEFAULT_TIMEOUT,
        );
        let captured = transport.captured.borrow().clone().unwrap();
        assert_eq!(captured.token, "unique-token-42");
    }

    #[test]
    fn fetch_passes_timeout_through_to_transport() {
        let transport = ok_transport(200, SAMPLE_OK_BODY, None);
        let custom_timeout = Duration::from_millis(750);
        let _ = fetch_usage(&transport, "https://x", &creds(), custom_timeout);
        let captured = transport.captured.borrow().clone().unwrap();
        assert_eq!(captured.timeout, custom_timeout);
    }

    #[test]
    fn fetch_maps_401_to_unauthorized() {
        let transport = ok_transport(401, "", None);
        let err = fetch_usage(&transport, "https://x", &creds(), DEFAULT_TIMEOUT).unwrap_err();
        assert!(matches!(err, UsageError::Unauthorized));
    }

    #[test]
    fn fetch_maps_429_with_integer_retry_after() {
        let transport = ok_transport(429, "", Some("120"));
        let err = fetch_usage(&transport, "https://x", &creds(), DEFAULT_TIMEOUT).unwrap_err();
        match err {
            UsageError::RateLimited {
                retry_after: Some(d),
            } => assert_eq!(d.as_secs(), 120),
            other => panic!("expected RateLimited(Some(120s)), got {other:?}"),
        }
    }

    #[test]
    fn fetch_maps_429_with_http_date_retry_after() {
        // `httpdate` validates the day-name-vs-date consistency, so
        // build the header value via the crate's own formatter rather
        // than hand-rolling a hopefully-correct fixed date.
        let future = SystemTime::now() + Duration::from_secs(3600);
        let header_value = httpdate::fmt_http_date(future);
        let transport = ok_transport(429, "", Some(&header_value));
        let err = fetch_usage(&transport, "https://x", &creds(), DEFAULT_TIMEOUT).unwrap_err();
        let UsageError::RateLimited {
            retry_after: Some(d),
        } = err
        else {
            panic!("expected RateLimited with Some duration, got {err:?}");
        };
        assert!(d.as_secs() > 0, "expected positive duration, got {d:?}");
    }

    #[test]
    fn fetch_maps_429_without_retry_after_to_default_backoff() {
        let transport = ok_transport(429, "", None);
        let err = fetch_usage(&transport, "https://x", &creds(), DEFAULT_TIMEOUT).unwrap_err();
        match err {
            UsageError::RateLimited {
                retry_after: Some(d),
            } => assert_eq!(d, DEFAULT_RATE_LIMIT_BACKOFF),
            other => panic!("expected RateLimited with default backoff, got {other:?}"),
        }
    }

    #[test]
    fn fetch_maps_5xx_to_network_error() {
        let transport = ok_transport(503, "", None);
        let err = fetch_usage(&transport, "https://x", &creds(), DEFAULT_TIMEOUT).unwrap_err();
        assert!(matches!(err, UsageError::NetworkError));
    }

    #[test]
    fn fetch_maps_malformed_json_to_parse_error() {
        let transport = ok_transport(200, "{ not valid json ", None);
        let err = fetch_usage(&transport, "https://x", &creds(), DEFAULT_TIMEOUT).unwrap_err();
        assert!(matches!(err, UsageError::ParseError));
    }

    #[test]
    fn fetch_maps_timeout_to_usage_timeout() {
        let transport = err_transport(io::ErrorKind::TimedOut);
        let err = fetch_usage(&transport, "https://x", &creds(), DEFAULT_TIMEOUT).unwrap_err();
        assert!(matches!(err, UsageError::Timeout));
    }

    #[test]
    fn fetch_maps_connection_refused_to_network_error() {
        let transport = err_transport(io::ErrorKind::ConnectionRefused);
        let err = fetch_usage(&transport, "https://x", &creds(), DEFAULT_TIMEOUT).unwrap_err();
        assert!(matches!(err, UsageError::NetworkError));
    }

    #[test]
    fn fetch_401_display_does_not_leak_token() {
        // Regression guard for credentials.md §Non-functional:
        // token bytes must not appear in error output for a request
        // that carried them, even when the server rejects auth.
        let transport = ok_transport(401, "", None);
        let err = fetch_usage(
            &transport,
            "https://x",
            &Credentials::for_testing("super-secret-token-abc123"),
            DEFAULT_TIMEOUT,
        )
        .unwrap_err();
        let display = format!("{err}");
        let debug = format!("{err:?}");
        assert!(
            !display.contains("super-secret-token-abc123"),
            "display leaked: {display}"
        );
        assert!(
            !debug.contains("super-secret-token-abc123"),
            "debug leaked: {debug}"
        );
    }

    #[test]
    fn parse_retry_after_integer_seconds() {
        assert_eq!(parse_retry_after("60"), Some(Duration::from_secs(60)));
        assert_eq!(parse_retry_after("  60  "), Some(Duration::from_secs(60)));
    }

    #[test]
    fn parse_retry_after_zero() {
        assert_eq!(parse_retry_after("0"), Some(Duration::from_secs(0)));
    }

    #[test]
    fn parse_retry_after_http_date_future() {
        // Roundtrip via `httpdate::fmt_http_date` so we feed parse the
        // same format the library emits — eliminates day-name drift.
        let future = SystemTime::now() + Duration::from_secs(3600);
        let raw = httpdate::fmt_http_date(future);
        let parsed = parse_retry_after(&raw);
        assert!(parsed.is_some_and(|d| d.as_secs() > 0));
    }

    #[test]
    fn parse_retry_after_http_date_past_returns_none() {
        // A past date can't yield a positive duration-since-now.
        assert_eq!(parse_retry_after("Thu, 01 Jan 1970 00:00:00 GMT"), None);
    }

    #[test]
    fn parse_retry_after_garbage_returns_none() {
        assert_eq!(parse_retry_after("not a date"), None);
        assert_eq!(parse_retry_after(""), None);
        assert_eq!(parse_retry_after("-1"), None);
    }

    #[test]
    fn parse_retry_after_caps_pathological_values() {
        // u64::MAX seconds would otherwise produce a Duration the
        // orchestrator can't meaningfully compare to a cache TTL.
        let parsed = parse_retry_after(&u64::MAX.to_string()).unwrap();
        assert_eq!(parsed, MAX_RETRY_AFTER);
    }

    #[test]
    fn default_user_agent_includes_version_and_crate_name() {
        // Regression guard for the ADR-0011 §Endpoint contract
        // User-Agent requirement. Pins the format so a future
        // refactor can't silently drop the version suffix.
        let ua = default_user_agent();
        assert!(ua.starts_with("linesmith/"), "ua = {ua}");
        assert!(
            ua.ends_with(env!("CARGO_PKG_VERSION")),
            "ua = {ua}; version = {}",
            env!("CARGO_PKG_VERSION"),
        );
    }

    #[test]
    fn fetch_204_empty_body_surfaces_parse_error() {
        // 2xx non-200 responses with empty bodies collapse to
        // ParseError via `serde_json::from_slice(b"")`. Lock the
        // behavior down so a future change to 204 handling is
        // intentional.
        let transport = ok_transport(204, "", None);
        let err = fetch_usage(&transport, "https://x", &creds(), DEFAULT_TIMEOUT).unwrap_err();
        assert!(matches!(err, UsageError::ParseError));
    }
}
