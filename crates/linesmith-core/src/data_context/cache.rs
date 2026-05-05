//! Disk-backed OAuth usage cache + lock file.
//!
//! Two storage layers per `docs/specs/data-fetching.md` §OAuth usage
//! cache stack:
//!
//! - [`CacheStore`] — `usage.json` holds either the last endpoint
//!   response ([`CachedData`]) or a tag-only error record
//!   ([`CachedError`]), stamped with `schema_version` + `cached_at`.
//!   The orchestrator compares `cached_at` against
//!   `usage.cache_duration` to decide freshness.
//! - [`LockStore`] — `usage.lock` holds a `blocked_until` Unix
//!   timestamp that prevents concurrent linesmith processes from
//!   stampeding the endpoint under 429 backoff.
//!
//! Both writes go through [`atomic_write_json`]: write to a sibling
//! tempfile, then rename over the target. `tempfile::NamedTempFile::
//! persist` is rename-on-Unix and `MoveFileEx` with
//! `MOVEFILE_REPLACE_EXISTING` on Windows — atomic at the filesystem
//! level on both platforms.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::usage::{ExtraUsage, UsageApiResponse, UsageBucket};

/// Cache schema version. Bump when the on-disk shape changes in a
/// way that can't be read by an older linesmith — readers with a
/// mismatched version treat the file as a miss per
/// `docs/specs/data-fetching.md` §Schema versioning.
pub const CACHE_SCHEMA_VERSION: u32 = 1;

const USAGE_FILE: &str = "usage.json";
const LOCK_FILE: &str = "usage.lock";

/// Implicit lock TTL used when a legacy non-JSON `.lock` file is
/// encountered (`mtime + 30s`), per `docs/specs/data-fetching.md`
/// §Lock file shape.
const LEGACY_LOCK_TTL_SECS: i64 = 30;

/// Ceiling on `Lock.blocked_until - now`. A buggy or adversarial
/// writer persisting a wildly-future timestamp would otherwise park
/// the orchestrator in "rate-limited" state until the distant date.
/// Matches the `MAX_RETRY_AFTER` cap in `fetcher.rs` — 1h above any
/// realistic 429 backoff.
const MAX_LOCK_DURATION_SECS: i64 = 24 * 60 * 60;

// --- Errors -------------------------------------------------------------

/// Cache-layer I/O / parse failures. Distinct from [`UsageError`]
/// because cache-miss variants collapse to `Ok(None)` in [`CacheStore::read`];
/// `CacheError` carries only the cases that indicate a real problem
/// (filesystem error on write, inability to create the parent dir).
#[derive(Debug)]
#[non_exhaustive]
pub enum CacheError {
    /// Directory creation or file read/write failed.
    Io { path: PathBuf, cause: io::Error },
    /// Tempfile rename during atomic write failed.
    Persist { path: PathBuf, cause: io::Error },
}

impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, cause } => {
                write!(f, "cache I/O error on {}: {}", path.display(), cause.kind())
            }
            Self::Persist { path, cause } => write!(
                f,
                "atomic persist failed for {}: {}",
                path.display(),
                cause.kind()
            ),
        }
    }
}

impl std::error::Error for CacheError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { cause, .. } | Self::Persist { cause, .. } => Some(cause),
        }
    }
}

// --- On-disk types ------------------------------------------------------

/// Single cache-file entry. Writers produced via [`Self::with_data`]
/// or [`Self::with_error`] keep exactly one of `data`/`error` null
/// per `docs/specs/data-fetching.md` §OAuth usage cache stack.
/// Readers tolerate any combination (both null, both populated) and
/// treat anomalies as misses at the orchestrator layer rather than
/// erroring at parse time.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CachedUsage {
    pub schema_version: u32,
    pub cached_at: Timestamp,
    #[serde(default)]
    pub data: Option<CachedData>,
    #[serde(default)]
    pub error: Option<CachedError>,
}

impl CachedUsage {
    #[must_use]
    pub fn with_data(data: UsageApiResponse) -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            cached_at: Timestamp::now(),
            data: Some(CachedData::from(data)),
            error: None,
        }
    }

    #[must_use]
    pub fn with_error(code: &str) -> Self {
        Self {
            schema_version: CACHE_SCHEMA_VERSION,
            cached_at: Timestamp::now(),
            data: None,
            error: Some(CachedError {
                code: code.to_string(),
            }),
        }
    }
}

/// Disk-serializable mirror of [`UsageApiResponse`]. The wire shape
/// uses `#[serde(flatten)]` for `unknown_buckets` so codenamed keys
/// appear at the top level of the endpoint response. The cache nests
/// them under a named `unknown_buckets` key so the outer
/// [`CachedUsage`] wrapper's fields (`schema_version`, `cached_at`,
/// etc.) don't collide with endpoint-emitted keys like `five_hour`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[non_exhaustive]
pub struct CachedData {
    #[serde(default)]
    pub five_hour: Option<UsageBucket>,
    #[serde(default)]
    pub seven_day: Option<UsageBucket>,
    #[serde(default)]
    pub seven_day_opus: Option<UsageBucket>,
    #[serde(default)]
    pub seven_day_sonnet: Option<UsageBucket>,
    #[serde(default)]
    pub seven_day_oauth_apps: Option<UsageBucket>,
    #[serde(default)]
    pub extra_usage: Option<ExtraUsage>,
    #[serde(default)]
    pub unknown_buckets: HashMap<String, serde_json::Value>,
}

impl From<UsageApiResponse> for CachedData {
    fn from(r: UsageApiResponse) -> Self {
        Self {
            five_hour: r.five_hour,
            seven_day: r.seven_day,
            seven_day_opus: r.seven_day_opus,
            seven_day_sonnet: r.seven_day_sonnet,
            seven_day_oauth_apps: r.seven_day_oauth_apps,
            extra_usage: r.extra_usage,
            unknown_buckets: r.unknown_buckets,
        }
    }
}

impl From<CachedData> for UsageApiResponse {
    fn from(c: CachedData) -> Self {
        UsageApiResponse {
            five_hour: c.five_hour,
            seven_day: c.seven_day,
            seven_day_opus: c.seven_day_opus,
            seven_day_sonnet: c.seven_day_sonnet,
            seven_day_oauth_apps: c.seven_day_oauth_apps,
            extra_usage: c.extra_usage,
            unknown_buckets: c.unknown_buckets,
        }
    }
}

/// On-disk error record. Intentionally lossy — carries only the
/// [`UsageError::code`](super::UsageError::code) tag, not the full
/// Rust enum. Live errors from the current process take precedence
/// over cached ones; the cache is just for cross-invocation hints.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedError {
    pub code: String,
}

/// On-disk lock file shape. `blocked_until` is a signed Unix
/// timestamp in seconds — `i64` wide enough for any plausible Unix
/// time, signed so the `mtime + LEGACY_LOCK_TTL_SECS` arithmetic
/// used by the legacy path can't underflow.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Lock {
    pub blocked_until: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// --- Path resolution ----------------------------------------------------

/// Locate the linesmith cache root. Returns `None` in environments
/// that provide neither `$XDG_CACHE_HOME` nor `$HOME`. Delegates to
/// [`xdg::resolve_subdir`](super::xdg::resolve_subdir); the
/// `from_process_env` factory uses `var_os` so non-UTF-8 paths
/// (Unix byte-string paths) survive through to the cache reader.
#[must_use]
pub fn default_root() -> Option<PathBuf> {
    use super::xdg::{resolve_subdir, XdgEnv, XdgScope};
    resolve_subdir(&XdgEnv::from_process_env(), XdgScope::Cache, "")
}

// --- CacheStore ---------------------------------------------------------

/// Reader/writer for `usage.json`.
pub struct CacheStore {
    root: PathBuf,
}

impl CacheStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(USAGE_FILE)
    }

    /// Return the cached entry or `Ok(None)` for any condition that
    /// should degrade to a cache miss: file not present, non-UTF-8
    /// bytes, malformed JSON, `schema_version` mismatch, or
    /// `cached_at` in the future (clock skew). Only unexpected I/O
    /// errors (permission denied, etc.) surface as `Err`.
    pub fn read(&self) -> Result<Option<CachedUsage>, CacheError> {
        let path = self.path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(cause) => return Err(CacheError::Io { path, cause }),
        };
        // Non-UTF-8 is one flavor of corruption; collapse to miss so
        // the next write can overwrite. serde_json wouldn't accept
        // the bytes anyway.
        let Ok(text) = std::str::from_utf8(&bytes) else {
            return Ok(None);
        };
        match serde_json::from_str::<CachedUsage>(text) {
            Ok(entry)
                if entry.schema_version == CACHE_SCHEMA_VERSION
                    && entry.cached_at <= Timestamp::now() =>
            {
                Ok(Some(entry))
            }
            // The next write will overwrite.
            _ => Ok(None),
        }
    }

    /// Persist the entry via the atomic-rename helper. Creates the
    /// cache root on demand — no init step needed.
    pub fn write(&self, entry: &CachedUsage) -> Result<(), CacheError> {
        atomic_write_json(&self.path(), entry)
    }
}

// --- LockStore ----------------------------------------------------------

/// Reader/writer for `usage.lock`.
pub struct LockStore {
    root: PathBuf,
}

impl LockStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.root.join(LOCK_FILE)
    }

    /// Return the current lock, the legacy-mtime fallback, or
    /// `Ok(None)` for absence. `blocked_until` is capped at
    /// `now + MAX_LOCK_DURATION_SECS` so a pathological on-disk
    /// value can't park the orchestrator indefinitely. Non-UTF-8 or
    /// non-JSON contents route through the legacy mtime fallback
    /// per `docs/specs/data-fetching.md` §Lock file shape.
    /// Unexpected I/O errors surface as `Err`.
    pub fn read(&self) -> Result<Option<Lock>, CacheError> {
        let path = self.path();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(cause) => return Err(CacheError::Io { path, cause }),
        };
        // Only try JSON when the bytes are valid UTF-8; anything
        // else falls through to the legacy-mtime path.
        if let Ok(text) = std::str::from_utf8(&bytes) {
            if let Ok(mut lock) = serde_json::from_str::<Lock>(text) {
                cap_blocked_until(&mut lock.blocked_until);
                return Ok(Some(lock));
            }
        }
        // Legacy non-JSON (or non-UTF-8) lock: derive `blocked_until`
        // from mtime per `docs/specs/data-fetching.md` §Lock file
        // shape.
        let meta = fs::metadata(&path).map_err(|cause| CacheError::Io {
            path: path.clone(),
            cause,
        })?;
        let mtime = meta.modified().map_err(|cause| CacheError::Io {
            path: path.clone(),
            cause,
        })?;
        let mtime_unix: i64 = match mtime.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs() as i64,
            Err(_) => {
                // mtime before UNIX_EPOCH — extreme clock
                // misconfiguration or restored-from-backup weirdness.
                // Fall back to 0 so the legacy lock is effectively
                // expired; in debug builds the assertion loud-fails.
                debug_assert!(false, "lock file mtime before UNIX_EPOCH");
                0
            }
        };
        let mut blocked_until = mtime_unix + LEGACY_LOCK_TTL_SECS;
        cap_blocked_until(&mut blocked_until);
        Ok(Some(Lock {
            blocked_until,
            error: None,
        }))
    }

    pub fn write(&self, lock: &Lock) -> Result<(), CacheError> {
        atomic_write_json(&self.path(), lock)
    }
}

fn cap_blocked_until(blocked_until: &mut i64) {
    let max = Timestamp::now().as_second() + MAX_LOCK_DURATION_SECS;
    if *blocked_until > max {
        *blocked_until = max;
    }
}

// --- Atomic write helper ------------------------------------------------

/// Write a JSON-serializable value to `path` atomically: serialize
/// into a sibling tempfile, then `persist` (rename on Unix,
/// `MoveFileEx` on Windows). The parent directory is created on
/// demand; a concurrent writer will always see either the old file
/// or the new one, never a torn write.
pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), CacheError> {
    let parent = path.parent().ok_or_else(|| CacheError::Io {
        path: path.to_path_buf(),
        cause: io::Error::new(io::ErrorKind::InvalidInput, "path has no parent"),
    })?;
    fs::create_dir_all(parent).map_err(|cause| CacheError::Io {
        path: parent.to_path_buf(),
        cause,
    })?;
    let tmp = tempfile::NamedTempFile::new_in(parent).map_err(|cause| CacheError::Io {
        path: parent.to_path_buf(),
        cause,
    })?;
    serde_json::to_writer_pretty(&tmp, value).map_err(|e| CacheError::Io {
        path: path.to_path_buf(),
        cause: io::Error::other(e),
    })?;
    tmp.as_file().sync_all().map_err(|cause| CacheError::Io {
        path: path.to_path_buf(),
        cause,
    })?;
    tmp.persist(path).map_err(|e| CacheError::Persist {
        path: path.to_path_buf(),
        cause: e.error,
    })?;
    Ok(())
}

// --- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::SignedDuration;
    use tempfile::TempDir;

    fn sample_response() -> UsageApiResponse {
        let json = r#"{
            "five_hour": { "utilization": 22.0, "resets_at": "2026-04-19T05:00:00Z" },
            "seven_day": { "utilization": 33.0, "resets_at": "2026-04-23T19:00:00Z" }
        }"#;
        serde_json::from_str(json).expect("parse")
    }

    // Path-resolution tests live with the XDG cascade in
    // `data_context/xdg.rs`; `default_root` is a thin wrapper that
    // reads process env into `XdgEnv` and delegates.

    // --- CacheStore round-trip -----------------------------------------

    #[test]
    fn cache_round_trip_preserves_data_entry() {
        let tmp = TempDir::new().unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        let entry = CachedUsage::with_data(sample_response());
        store.write(&entry).expect("write");
        let read_back = store.read().expect("read").expect("some");
        assert_eq!(read_back, entry);
    }

    #[test]
    fn cache_round_trip_preserves_error_entry() {
        let tmp = TempDir::new().unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        let entry = CachedUsage::with_error("Timeout");
        store.write(&entry).expect("write");
        let read_back = store.read().expect("read").expect("some");
        assert_eq!(read_back.error.unwrap().code, "Timeout");
        assert!(read_back.data.is_none());
    }

    #[test]
    fn cache_read_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        assert!(store.read().expect("read").is_none());
    }

    #[test]
    fn cache_reads_rfc3339_z_suffix_serde_format() {
        // Pin the on-disk RFC 3339 (`Z` suffix) timestamp shape so a
        // future datetime-library bump that changes the default serde
        // format fails loudly here, and existing cache files don't
        // start silently failing to parse. Bump CACHE_SCHEMA_VERSION
        // when the wire format intentionally changes.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(USAGE_FILE);
        let payload = r#"{
            "schema_version": 1,
            "cached_at": "2026-04-19T12:00:00.000Z",
            "data": {
                "five_hour": { "utilization": 42.0, "resets_at": "2026-04-19T17:00:00.000Z" },
                "seven_day": null,
                "seven_day_opus": null,
                "seven_day_sonnet": null,
                "seven_day_oauth_apps": null,
                "extra_usage": null,
                "unknown_buckets": {}
            },
            "error": null
        }"#;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, payload).unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        let read_back = store.read().expect("read").expect("some");
        assert_eq!(read_back.cached_at.to_string(), "2026-04-19T12:00:00Z");
        let bucket = read_back.data.as_ref().unwrap().five_hour.as_ref().unwrap();
        assert_eq!(bucket.utilization.value(), 42.0);
        assert_eq!(
            bucket.resets_at.unwrap().to_string(),
            "2026-04-19T17:00:00Z",
        );
    }

    #[test]
    fn cache_read_returns_none_for_schema_mismatch() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(USAGE_FILE);
        fs::create_dir_all(tmp.path()).unwrap();
        fs::write(
            &path,
            r#"{ "schema_version": 9999, "cached_at": "2026-04-20T12:00:00Z", "data": null, "error": null }"#,
        )
        .unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        assert!(store.read().expect("read").is_none());
    }

    #[test]
    fn cache_read_returns_none_for_clock_skew() {
        // cached_at is 10 minutes in the future → treated as a miss.
        let tmp = TempDir::new().unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        let mut entry = CachedUsage::with_data(sample_response());
        entry.cached_at = Timestamp::now() + SignedDuration::from_mins(10);
        store.write(&entry).expect("write");
        assert!(store.read().expect("read").is_none());
    }

    #[test]
    fn cache_read_returns_none_for_corrupt_json() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(USAGE_FILE), "{ not valid json ").unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        assert!(store.read().expect("read").is_none());
    }

    #[test]
    fn cache_read_returns_none_for_zero_byte_file() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(USAGE_FILE), "").unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        assert!(store.read().expect("read").is_none());
    }

    #[test]
    fn cache_read_returns_none_for_non_utf8_bytes() {
        // `fs::read_to_string` would raise `InvalidData` on these
        // bytes, turning a corrupt file into a hard error that
        // blocks the fallback cascade. `read` must collapse this to
        // a miss so the next successful fetch can overwrite.
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join(USAGE_FILE), [0xFF, 0xFE, 0xFD]).unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        assert!(store.read().expect("read").is_none());
    }

    #[test]
    fn cache_write_creates_missing_parent_directory() {
        // Root points at a not-yet-existing subdir; write should
        // create it rather than erroring.
        let tmp = TempDir::new().unwrap();
        let nested = tmp.path().join("nested").join("linesmith");
        let store = CacheStore::new(nested.clone());
        store
            .write(&CachedUsage::with_data(sample_response()))
            .expect("write");
        assert!(nested.join(USAGE_FILE).exists());
    }

    #[test]
    fn cache_round_trip_preserves_unknown_buckets() {
        // Forward-compat: codenamed buckets the endpoint emits (but
        // we don't recognize by name) must round-trip through the
        // cache.
        let tmp = TempDir::new().unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        let json = r#"{
            "five_hour": { "utilization": 10.0, "resets_at": "2026-04-19T05:00:00Z" },
            "quokka_experimental": { "utilization": 99.0, "resets_at": null }
        }"#;
        let response: UsageApiResponse = serde_json::from_str(json).unwrap();
        store
            .write(&CachedUsage::with_data(response))
            .expect("write");
        let read_back = store.read().expect("read").expect("some");
        let data = read_back.data.unwrap();
        assert!(data.unknown_buckets.contains_key("quokka_experimental"));
    }

    // --- Concurrent write smoke ---------------------------------------

    #[test]
    fn concurrent_writes_produce_intact_file() {
        use std::sync::Arc;
        use std::thread;

        let tmp = TempDir::new().unwrap();
        let store = Arc::new(CacheStore::new(tmp.path().to_path_buf()));

        let store_a = Arc::clone(&store);
        let handle_a = thread::spawn(move || {
            let mut succeeded = 0;
            for _ in 0..10 {
                if store_a.write(&CachedUsage::with_error("Timeout")).is_ok() {
                    succeeded += 1;
                }
            }
            succeeded
        });
        let store_b = Arc::clone(&store);
        let handle_b = thread::spawn(move || {
            let mut succeeded = 0;
            for _ in 0..10 {
                if store_b
                    .write(&CachedUsage::with_data(sample_response()))
                    .is_ok()
                {
                    succeeded += 1;
                }
            }
            succeeded
        });
        let succeeded = handle_a.join().unwrap() + handle_b.join().unwrap();

        // Documented contract is final-state integrity, not per-call
        // success. POSIX rename(2) never fails on concurrent renames,
        // so on Unix every write must succeed — a regression that
        // introduced spurious failures should fail loud here. Windows
        // MoveFileEx returns ERROR_ACCESS_DENIED to the racing loser
        // (surfaces as PermissionDenied), so on Windows we only require
        // at least one writer to win (otherwise the final-state
        // assertion below is meaningless).
        #[cfg(unix)]
        assert_eq!(succeeded, 20, "POSIX rename(2) should never fail");
        #[cfg(not(unix))]
        assert!(succeeded > 0, "at least one concurrent write must win");

        // Final state is one of the two writers — never an interleaved
        // torn write. Parse must succeed.
        let read_back = store.read().expect("read").expect("some");
        assert_eq!(read_back.schema_version, CACHE_SCHEMA_VERSION);
        assert!(read_back.data.is_some() ^ read_back.error.is_some());
    }

    // --- LockStore ----------------------------------------------------

    #[test]
    fn lock_round_trip() {
        let tmp = TempDir::new().unwrap();
        let store = LockStore::new(tmp.path().to_path_buf());
        // A recent timestamp that falls well within the cap window.
        let now = Timestamp::now().as_second();
        let lock = Lock {
            blocked_until: now + 60,
            error: Some("rate-limited".into()),
        };
        store.write(&lock).expect("write");
        let read_back = store.read().expect("read").expect("some");
        assert_eq!(read_back, lock);
    }

    #[test]
    fn lock_read_returns_none_when_missing() {
        let tmp = TempDir::new().unwrap();
        let store = LockStore::new(tmp.path().to_path_buf());
        assert!(store.read().expect("read").is_none());
    }

    #[test]
    fn lock_read_non_utf8_routes_through_legacy_fallback() {
        // Partially-written binary or otherwise-corrupt lock must
        // fall through to the mtime+30s path per
        // `docs/specs/data-fetching.md` §Lock file shape, not
        // hard-error the cache layer.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(LOCK_FILE);
        fs::write(&path, [0xFF, 0xFE, 0x00, 0xFD]).unwrap();
        let mtime = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let store = LockStore::new(tmp.path().to_path_buf());
        let lock = store.read().expect("read").expect("some");
        assert_eq!(lock.blocked_until, mtime + LEGACY_LOCK_TTL_SECS);
        assert!(lock.error.is_none());
    }

    #[test]
    fn lock_read_legacy_non_json_uses_mtime_plus_30s() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(LOCK_FILE);
        fs::write(&path, "# legacy lock from older linesmith").unwrap();
        let mtime = fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let store = LockStore::new(tmp.path().to_path_buf());
        let lock = store.read().expect("read").expect("some");
        assert_eq!(lock.blocked_until, mtime + LEGACY_LOCK_TTL_SECS);
        assert!(lock.error.is_none());
    }

    #[test]
    fn lock_read_caps_pathological_blocked_until() {
        // A writer persisting a wildly-future `blocked_until` must
        // not let the orchestrator park forever. Same risk class as
        // `Retry-After: u64::MAX` fixed in fetcher.rs.
        let tmp = TempDir::new().unwrap();
        let store = LockStore::new(tmp.path().to_path_buf());
        let malicious = Lock {
            blocked_until: i64::MAX,
            error: None,
        };
        store.write(&malicious).expect("write");
        let read_back = store.read().expect("read").expect("some");
        let ceiling = Timestamp::now().as_second() + MAX_LOCK_DURATION_SECS;
        // Cap may drift by a second during the test; allow a small
        // window but reject the raw i64::MAX that was persisted.
        assert!(
            read_back.blocked_until <= ceiling + 1 && read_back.blocked_until >= ceiling - 1,
            "blocked_until = {}, expected near {}",
            read_back.blocked_until,
            ceiling
        );
    }

    #[test]
    fn lock_error_omitted_from_serialized_form_when_none() {
        // `Option<String>` with `skip_serializing_if` keeps the JSON
        // clean on legacy-fallback writes that have no error text.
        let tmp = TempDir::new().unwrap();
        let store = LockStore::new(tmp.path().to_path_buf());
        store
            .write(&Lock {
                blocked_until: Timestamp::now().as_second() + 60,
                error: None,
            })
            .expect("write");
        let raw = fs::read_to_string(store.path()).unwrap();
        assert!(!raw.contains("\"error\""), "unexpected error key: {raw}");
    }

    // --- atomic_write_json failure paths ------------------------------
    //
    // Serialization failure isn't covered by a dedicated test: our
    // cache types (jiff::Timestamp, Option<String>, HashMap<String, Value>)
    // are JSON-safe end to end, and `serde_json` turns pathological
    // floats into `null` rather than erroring. The branch remains
    // for defensive correctness if a future type introduces a failing
    // Serialize impl.

    #[test]
    fn atomic_write_json_rejects_path_without_parent() {
        // The root path `/` has no parent — `parent()` returns None.
        let err = atomic_write_json(
            Path::new("/"),
            &Lock {
                blocked_until: 0,
                error: None,
            },
        )
        .unwrap_err();
        match err {
            CacheError::Io { cause, .. } => {
                assert_eq!(cause.kind(), io::ErrorKind::InvalidInput);
            }
            other => panic!("expected Io(InvalidInput), got {other:?}"),
        }
    }

    // --- CacheStore I/O error branch ----------------------------------

    #[cfg(unix)]
    #[test]
    fn cache_read_surfaces_permission_denied() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(USAGE_FILE);
        fs::write(&path, "{}").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let err = CacheStore::new(tmp.path().to_path_buf())
            .read()
            .unwrap_err();
        assert!(matches!(err, CacheError::Io { .. }));
        // Restore perms so TempDir cleanup doesn't fail.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    // --- CachedUsage tolerant-reader invariant ------------------------

    #[test]
    fn cache_read_tolerates_entry_with_both_data_and_error() {
        // The doc says constructors keep one null, but readers are
        // tolerant — pin that contract so a future "helpful" fix
        // doesn't regress silently.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join(USAGE_FILE);
        fs::write(
            &path,
            r#"{
                "schema_version": 1,
                "cached_at": "2026-04-20T12:00:00Z",
                "data": {
                    "five_hour": { "utilization": 0.0, "resets_at": null }
                },
                "error": { "code": "Timeout" }
            }"#,
        )
        .unwrap();
        let store = CacheStore::new(tmp.path().to_path_buf());
        let entry = store.read().expect("read").expect("some");
        assert!(entry.data.is_some());
        assert!(entry.error.is_some());
    }
}
