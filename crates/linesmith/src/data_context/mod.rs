//! Shared data-access context threaded through every segment's
//! [`render`](crate::segments::Segment::render) call.
//!
//! [`DataContext`] owns the eagerly-parsed stdin payload
//! ([`StatusContext`](crate::input::StatusContext) at `ctx.status`) plus
//! lazy [`OnceCell`](std::cell::OnceCell) accessors for every other
//! source (settings, `~/.claude.json`, JSONL transcripts, OAuth usage,
//! credentials, live sessions, git).
//!
//! This module ships the v0.1 skeleton: the struct shape, the accessor
//! surface, and stub [`NotImplemented`](errors::SettingsError::NotImplemented)
//! errors. Real source implementations arrive with their owning epics
//! (lsm-y6m for usage, lsm-8jl for git, etc.). Plugin scripts see a
//! uniform `{ kind: "error", error: "NotImplemented" }` shape until
//! those land.
//!
//! Canonical definition: `docs/specs/data-fetching.md` §DataContext.

pub mod deps;
pub mod errors;
pub mod usage;

use std::cell::OnceCell;
use std::sync::Arc;

use crate::input::StatusContext;

pub use deps::DataDep;
pub use errors::{
    ClaudeJsonError, CredentialError, GitError, JsonlError, SessionError, SettingsError, UsageError,
};
pub use usage::{ExtraUsage, UsageApiResponse, UsageBucket, UsageData, UsageSource};

// --- Stub source types ---------------------------------------------------
//
// Each gets real fields when its epic lands. Defined here as opaque
// `#[non_exhaustive]` marker structs so `Arc<Result<T, E>>` types
// compile today. Braced-empty (`{}`) form is deliberate: unit structs
// would force a breaking `Foo` → `Foo { ... }` migration at every
// construction site when fields land. `Default` is intentionally NOT
// derived — we want real construction sites to surface in review
// when each epic populates its fields; a `UsageData::default()` that
// silently returns a zero-token record would render a misleading
// statusline.

/// Parsed `~/.claude/settings.json` + overlays. Stub.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Settings {}

/// Parsed `~/.claude.json` per-user state. Stub.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ClaudeJson {}

/// Aggregated JSONL transcript state. Stub placeholder; concrete shape
/// is deferred until the dedicated `jsonl-aggregation` spec lands.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct JsonlAggregate {}

/// macOS Keychain / file-backed OAuth credentials. Stub.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Credentials {}

/// Snapshot of `~/.claude/sessions/{pid}.json` entries. Stub.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LiveSessions {}

/// Git repo inspection state. Stub placeholder; concrete shape lands
/// with lsm-8jl.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct GitContext {}

// --- DataContext ---------------------------------------------------------

/// Bundle of every source a segment may read during a single render
/// invocation. `status` is populated eagerly from the stdin payload;
/// all other sources lazy-init on first access and cache their
/// `Result` (including errors) for the lifetime of this context.
///
/// Accessors return `Arc<Result<T, E>>` so segments can hold the data
/// across calls without tying lifetimes to `&self`. The `Result` shape
/// preserves failure info for the plugin runtime's tagged-map mirror
/// (`#{ kind: "ok"|"error", ... }`) per `plugin-api.md` §ctx shape.
pub struct DataContext {
    /// Eagerly-parsed stdin payload.
    pub status: StatusContext,

    settings: OnceCell<Arc<Result<Settings, SettingsError>>>,
    claude_json: OnceCell<Arc<Result<ClaudeJson, ClaudeJsonError>>>,
    jsonl: OnceCell<Arc<Result<JsonlAggregate, JsonlError>>>,
    usage: OnceCell<Arc<Result<UsageData, UsageError>>>,
    credentials: OnceCell<Arc<Result<Credentials, CredentialError>>>,
    sessions: OnceCell<Arc<Result<LiveSessions, SessionError>>>,
    git: OnceCell<Arc<Result<Option<GitContext>, GitError>>>,
}

impl DataContext {
    /// Wrap a parsed [`StatusContext`] with lazy accessors for every
    /// other data source.
    #[must_use]
    pub fn new(status: StatusContext) -> Self {
        Self {
            status,
            settings: OnceCell::new(),
            claude_json: OnceCell::new(),
            jsonl: OnceCell::new(),
            usage: OnceCell::new(),
            credentials: OnceCell::new(),
            sessions: OnceCell::new(),
            git: OnceCell::new(),
        }
    }

    /// `~/.claude/settings.json` + overlays.
    #[must_use]
    pub fn settings(&self) -> Arc<Result<Settings, SettingsError>> {
        self.settings
            .get_or_init(|| Arc::new(Err(SettingsError::NotImplemented)))
            .clone()
    }

    /// `~/.claude.json` per-user state.
    #[must_use]
    pub fn claude_json(&self) -> Arc<Result<ClaudeJson, ClaudeJsonError>> {
        self.claude_json
            .get_or_init(|| Arc::new(Err(ClaudeJsonError::NotImplemented)))
            .clone()
    }

    /// Aggregated JSONL transcript state.
    #[must_use]
    pub fn jsonl(&self) -> Arc<Result<JsonlAggregate, JsonlError>> {
        self.jsonl
            .get_or_init(|| Arc::new(Err(JsonlError::NotImplemented)))
            .clone()
    }

    /// OAuth usage endpoint data (shared across rate-limit segments).
    ///
    /// Returns a sentinel error until the real fallback cascade is
    /// wired. The sentinel wraps [`JsonlError::NotImplemented`] so the
    /// mirror's `.code()` delegation produces the same
    /// `"NotImplemented"` short-tag as the other stub sources — NOT
    /// the shape a live cascade will produce. ADR-0011 §Fallback
    /// cascade unwraps inner errors on real failures, so callers
    /// should not treat the wrap as semantically meaningful.
    #[must_use]
    pub fn usage(&self) -> Arc<Result<UsageData, UsageError>> {
        self.usage
            .get_or_init(|| Arc::new(Err(UsageError::Jsonl(JsonlError::NotImplemented))))
            .clone()
    }

    /// macOS Keychain / `.credentials.json` OAuth credentials.
    #[must_use]
    pub fn credentials(&self) -> Arc<Result<Credentials, CredentialError>> {
        self.credentials
            .get_or_init(|| Arc::new(Err(CredentialError::NotImplemented)))
            .clone()
    }

    /// `~/.claude/sessions/{pid}.json` live process snapshot.
    #[must_use]
    pub fn sessions(&self) -> Arc<Result<LiveSessions, SessionError>> {
        self.sessions
            .get_or_init(|| Arc::new(Err(SessionError::NotImplemented)))
            .clone()
    }

    /// Git repo inspection via `gix`. `Ok(None)` means cwd is not
    /// inside a git repo; `Ok(Some(_))` covers main checkouts, linked
    /// worktrees, and bare repos; `Err` is a gix failure.
    #[must_use]
    pub fn git(&self) -> Arc<Result<Option<GitContext>, GitError>> {
        self.git
            .get_or_init(|| Arc::new(Err(GitError::NotImplemented)))
            .clone()
    }
}
