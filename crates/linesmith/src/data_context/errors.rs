//! Error types for lazy [`DataContext`](super::DataContext) sources.
//!
//! Each source's real error variants land with its owning epic (e.g.
//! [`UsageError`] is real under the lsm-y6m epic; `GitError` lands
//! with lsm-8jl). Sources still in stub form return the
//! `NotImplemented` variant so the plugin runtime can expose a
//! uniform error surface to scripts.
//!
//! **`NotImplemented` is temporary.** When an epic lands real variants
//! for a given error enum, the `NotImplemented` variant is removed in
//! the same commit. Because each enum is `#[non_exhaustive]`, adding
//! new variants is non-breaking; *removing* `NotImplemented` is a
//! breaking change for any external code that matches on it. v0.1
//! treats that window as acceptable — no external consumers exist
//! yet, and the stub's whole purpose is to signal "not wired up."

macro_rules! stub_error {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq)]
        #[non_exhaustive]
        pub enum $name {
            /// Source not yet implemented. Real variants land with the
            /// epic that owns this source.
            NotImplemented,
        }

        impl $name {
            /// Short plugin-facing error tag per
            /// `docs/specs/plugin-api.md` §ctx shape exposed to rhai.
            /// Rendered in the rhai tagged-map mirror as `error:
            /// "<code>"`.
            #[must_use]
            pub fn code(&self) -> &'static str {
                match self {
                    Self::NotImplemented => "NotImplemented",
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.code())
            }
        }

        impl std::error::Error for $name {}
    };
}

stub_error!(
    SettingsError,
    "Errors from reading `~/.claude/settings.json` + overlays."
);
stub_error!(ClaudeJsonError, "Errors from reading `~/.claude.json`.");
stub_error!(JsonlError, "Errors from aggregating JSONL transcripts.");
stub_error!(SessionError, "Errors from the live sessions directory.");
stub_error!(GitError, "Errors from `gix` repo inspection.");

// `CredentialError` is the real type from `super::credentials` — re-exported
// at the data_context module root so `pub use errors::CredentialError` still
// resolves. When other error types graduate, follow the same pattern.
pub use super::credentials::CredentialError;

// --- UsageError (real, not stub) ---------------------------------------
//
// Variants mirror `docs/specs/rate-limit-segments.md` §Error message
// table; segment renderers map each variant (and the inner variants of
// `Credentials` + `Jsonl`) to the user-facing bracketed strings listed
// in that table. Adding variants is non-breaking (`#[non_exhaustive]`).

use std::time::Duration;

/// Errors from the OAuth `/api/oauth/usage` endpoint plus the cache
/// stack, credential, and JSONL-fallback layers that feed it. Rendered
/// to the user via the segment error table in
/// `docs/specs/rate-limit-segments.md`.
///
/// Neither `Clone` nor `PartialEq` are derived: inner types (`io::Error`
/// via `CredentialError`, `serde_json::Error`) don't support them.
/// `DataContext` memoizes this behind `Arc<Result<_, UsageError>>` so
/// cross-segment sharing clones the `Arc`, not the error.
#[derive(Debug)]
#[non_exhaustive]
pub enum UsageError {
    /// No OAuth token reachable from any cascade path. Rendered
    /// `[No credentials]`.
    NoCredentials,

    /// Credentials-layer failure. Segment code matches on the inner
    /// variant to render `[Keychain error]` / `[Credentials
    /// unreadable]` / `[No credentials]` per the error table.
    Credentials(CredentialError),

    /// Endpoint took longer than the configured timeout. Rendered
    /// `[Timeout]`.
    Timeout,

    /// Endpoint returned `429 Too Many Requests`. `retry_after` is the
    /// parsed `Retry-After` header (integer seconds or HTTP-date per
    /// ADR-0011 §Cache stack); `None` means the header was absent and
    /// the default 300s backoff applies. Rendered `[Rate limited]`.
    RateLimited { retry_after: Option<Duration> },

    /// Connection failed, DNS failure, TLS failure, or any other
    /// network-level error. Rendered `[Network error]`.
    NetworkError,

    /// Endpoint returned malformed JSON. Rendered `[Parse error]`.
    ParseError,

    /// Endpoint returned `401 Unauthorized`. Token is expired or
    /// revoked; Claude Code refreshes on its next request. Rendered
    /// `[Unauthorized]`.
    Unauthorized,

    /// JSONL-fallback-layer failure. Surfaces when the endpoint path
    /// recorded an error AND the JSONL aggregator also yielded
    /// nothing.
    Jsonl(JsonlError),
}

impl UsageError {
    /// Short plugin-facing error tag per `docs/specs/plugin-api.md`
    /// §ctx shape exposed to rhai. Rendered in the rhai tagged-map
    /// mirror as `error: "<code>"`. Wrapping variants (`Credentials`,
    /// `Jsonl`) delegate to the inner error's `code()` so plugins see
    /// a flat taxonomy. Today those inner types are stubs that return
    /// `"NotImplemented"`; once the credential and JSONL layers land
    /// the delegation surfaces the full spec set (`"NoCredentials"`,
    /// `"SubprocessFailed"`, `"IoError"`, `"ParseError"`, etc.).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoCredentials => "NoCredentials",
            Self::Credentials(inner) => inner.code(),
            Self::Timeout => "Timeout",
            Self::RateLimited { .. } => "RateLimited",
            Self::NetworkError => "NetworkError",
            Self::ParseError => "ParseError",
            Self::Unauthorized => "Unauthorized",
            Self::Jsonl(inner) => inner.code(),
        }
    }
}

impl std::fmt::Display for UsageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoCredentials => f.write_str("no OAuth credentials found"),
            Self::Credentials(inner) => write!(f, "credentials error: {inner}"),
            Self::Timeout => f.write_str("endpoint request timed out"),
            Self::RateLimited {
                retry_after: Some(d),
            } => write!(f, "endpoint rate-limited; retry after {}s", d.as_secs()),
            Self::RateLimited { retry_after: None } => {
                f.write_str("endpoint rate-limited (no Retry-After)")
            }
            Self::NetworkError => f.write_str("network error"),
            Self::ParseError => f.write_str("endpoint response failed to parse"),
            Self::Unauthorized => f.write_str("endpoint returned 401 Unauthorized"),
            Self::Jsonl(inner) => write!(f, "JSONL fallback failed: {inner}"),
        }
    }
}

impl std::error::Error for UsageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Credentials(inner) => Some(inner),
            Self::Jsonl(inner) => Some(inner),
            _ => None,
        }
    }
}

#[cfg(test)]
mod usage_error_tests {
    use super::*;

    #[test]
    fn display_covers_every_variant() {
        let cases: &[(UsageError, &str)] = &[
            (UsageError::NoCredentials, "no OAuth credentials found"),
            (
                UsageError::Credentials(CredentialError::NoCredentials),
                "credentials error: no OAuth credentials found",
            ),
            (UsageError::Timeout, "endpoint request timed out"),
            (
                UsageError::RateLimited {
                    retry_after: Some(Duration::from_secs(42)),
                },
                "endpoint rate-limited; retry after 42s",
            ),
            (
                UsageError::RateLimited { retry_after: None },
                "endpoint rate-limited (no Retry-After)",
            ),
            (UsageError::NetworkError, "network error"),
            (UsageError::ParseError, "endpoint response failed to parse"),
            (
                UsageError::Unauthorized,
                "endpoint returned 401 Unauthorized",
            ),
            (
                UsageError::Jsonl(JsonlError::NotImplemented),
                "JSONL fallback failed: NotImplemented",
            ),
        ];
        for (err, expected) in cases {
            assert_eq!(err.to_string(), *expected);
        }
    }

    #[test]
    fn code_flattens_wrapping_variants_via_delegation() {
        // Plugin-facing contract: ctx.usage.error is a flat tag string.
        // Wrapping variants delegate to inner code() so a rhai switch
        // on "NoCredentials" | "Timeout" | ... works uniformly.
        assert_eq!(UsageError::NoCredentials.code(), "NoCredentials");
        assert_eq!(UsageError::Timeout.code(), "Timeout");
        assert_eq!(
            UsageError::RateLimited { retry_after: None }.code(),
            "RateLimited",
        );
        assert_eq!(UsageError::NetworkError.code(), "NetworkError");
        assert_eq!(UsageError::ParseError.code(), "ParseError");
        assert_eq!(UsageError::Unauthorized.code(), "Unauthorized");

        // Credentials delegation surfaces real CredentialError codes.
        assert_eq!(
            UsageError::Credentials(CredentialError::NoCredentials).code(),
            "NoCredentials",
        );
        // Jsonl still delegates to a stub until lsm-26y lands.
        assert_eq!(
            UsageError::Jsonl(JsonlError::NotImplemented).code(),
            "NotImplemented",
        );
    }

    #[test]
    fn source_chains_through_wrapping_variants() {
        use std::error::Error;

        // Wrapping variants always expose the inner as their source,
        // regardless of whether that inner itself wraps an io::Error.
        // The full chain terminates at the leaf variant.
        let wrapped = UsageError::Credentials(CredentialError::IoError {
            path: std::path::PathBuf::from("/x"),
            cause: std::io::Error::other("boom"),
        });
        let source = wrapped.source().unwrap();
        // wrapped → CredentialError::IoError → io::Error
        assert!(source.source().is_some());

        // `Credentials(NoCredentials)` wraps a leaf — chain has 1 step.
        let credless = UsageError::Credentials(CredentialError::NoCredentials);
        let source = credless.source().unwrap();
        assert!(source.source().is_none());

        let wrapped_jsonl = UsageError::Jsonl(JsonlError::NotImplemented);
        assert!(wrapped_jsonl.source().is_some());

        let bare = UsageError::NoCredentials;
        assert!(bare.source().is_none());
    }
}
