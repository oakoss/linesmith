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

pub mod cache;
pub mod cascade;
pub mod credentials;
pub mod deps;
pub mod errors;
pub mod fetcher;
pub mod jsonl;
pub mod usage;

use std::cell::OnceCell;
use std::sync::Arc;

use crate::input::StatusContext;

pub use credentials::{CredentialSource, Credentials};
pub use deps::DataDep;
pub use errors::{
    ClaudeJsonError, CredentialError, GitError, JsonlError, SessionError, SettingsError, UsageError,
};
pub use jsonl::{FiveHourBlock, JsonlAggregate, SevenDayWindow, TokenCounts};
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
    ///
    /// Invokes [`jsonl::aggregate_jsonl`] on first call and memoizes
    /// the `Arc<Result<...>>` for the process lifetime. Scans the
    /// project-root cascade once per process.
    #[must_use]
    pub fn jsonl(&self) -> Arc<Result<JsonlAggregate, JsonlError>> {
        self.jsonl
            .get_or_init(|| Arc::new(jsonl::aggregate_jsonl()))
            .clone()
    }

    /// OAuth usage endpoint data (shared across rate-limit segments).
    ///
    /// Runs the full fallback cascade on first call and memoizes the
    /// result for the process lifetime. See
    /// [`cascade::resolve_usage`] for the step order and
    /// `docs/specs/data-fetching.md` §OAuth fallback cascade for
    /// rationale. A fresh disk-cache hit short-circuits before any
    /// Keychain subprocess or HTTP call.
    #[must_use]
    pub fn usage(&self) -> Arc<Result<UsageData, UsageError>> {
        self.usage
            .get_or_init(|| Arc::new(self.resolve_usage_default()))
            .clone()
    }

    fn resolve_usage_default(&self) -> Result<UsageData, UsageError> {
        let root = cache::default_root();
        let cache_store = root.clone().map(cache::CacheStore::new);
        let lock_store = root.map(cache::LockStore::new);
        let transport = fetcher::UreqTransport::new();
        let config = cascade::UsageCascadeConfig::default();
        cascade::resolve_usage(
            cache_store.as_ref(),
            lock_store.as_ref(),
            &transport,
            &|| self.credentials(),
            // Cheap no-op closure: the cascade currently ignores JSONL
            // per lsm-xhu; this avoids triggering a full transcript
            // scan through `self.jsonl()` that would be discarded.
            &|| Err(JsonlError::NoEntries),
            &chrono::Utc::now,
            &config,
        )
    }

    /// macOS Keychain / `.credentials.json` OAuth credentials.
    ///
    /// Invokes [`credentials::resolve_credentials`] on first call and
    /// memoizes the `Arc<Result<...>>` for the process lifetime per
    /// `docs/specs/credentials.md` §Non-functional. On macOS this may
    /// trigger a `security` subprocess and a one-time Keychain access
    /// prompt; on Linux/Windows it's a file-cascade read.
    #[must_use]
    pub fn credentials(&self) -> Arc<Result<Credentials, CredentialError>> {
        self.credentials
            .get_or_init(|| Arc::new(credentials::resolve_credentials()))
            .clone()
    }

    /// Pre-populate the `usage` result so [`Self::usage`] returns it
    /// without running the fallback cascade (which would otherwise
    /// touch the real Keychain, network, and JSONL transcripts).
    /// Mirrors [`OnceCell::set`]'s semantics: returns `Err` with the
    /// already-stored value if the cell was already populated by a
    /// prior `ctx.usage()` read.
    pub fn preseed_usage(
        &self,
        result: Result<UsageData, UsageError>,
    ) -> Result<(), Arc<Result<UsageData, UsageError>>> {
        self.usage.set(Arc::new(result))
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
