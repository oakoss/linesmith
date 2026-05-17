//! JSONL transcript aggregator — terminal fallback for the rate-limit
//! data pipeline.
//!
//! Canonical spec: `docs/specs/jsonl-aggregation.md`. Ports the
//! billing-block math from [`ryoppippi/ccusage`](https://github.com/ryoppippi/ccusage)'s
//! `_session-blocks.ts` (MIT). Produces raw token counts and block
//! boundaries only; mapping to [`UsageBucket`](super::UsageBucket)
//! without tier detection is the orchestrator's problem.
//!
//! v0.1 exposes only the currently-active 5h block. Historical
//! blocks are deferred per spec §Open questions — extending
//! `JsonlAggregate` with `completed_blocks` is a non-breaking change
//! under `#[non_exhaustive]`.

use std::collections::HashSet;
use std::fs;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

/// Billing-block duration, matching ccusage's
/// `DEFAULT_SESSION_DURATION_HOURS` in `_session-blocks.ts`.
const BLOCK_DURATION_HOURS: i64 = 5;
/// Rolling-window width per spec §7-day window math.
const WINDOW_DAYS: i64 = 7;

// --- Public types -------------------------------------------------------

/// Output of the aggregator. `five_hour` is `None` when no entry
/// falls within the last [`BLOCK_DURATION_HOURS`] hours; `seven_day`
/// is always present (zero-valued on an empty transcript).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct JsonlAggregate {
    pub five_hour: Option<FiveHourBlock>,
    pub seven_day: SevenDayWindow,
    pub source_paths: Vec<PathBuf>,
}

/// Active 5-hour billing block. `start` is the UTC-floor-to-hour of
/// the block's first entry; `actual_last_activity` lets the caller
/// distinguish a block where the user stopped typing 10 seconds ago
/// from one where they stopped 4 hours ago. The block's end time is
/// a derivation from `start` — see [`Self::end`].
#[derive(Debug, Clone)]
pub struct FiveHourBlock {
    pub start: Timestamp,
    pub actual_last_activity: Timestamp,
    pub token_counts: TokenCounts,
    pub models: Vec<String>,
    /// `usageLimitResetTime` from the most recent entry that carried
    /// one. Verified absent across the surveyed Claude Code corpus
    /// (lsm-ghpj, 2026-05-16); the field is deserialized defensively
    /// but segments do not consume it — `rate_limit_5h_reset` uses
    /// `block.end()` per ADR-0013.
    pub usage_limit_reset: Option<Timestamp>,
}

impl FiveHourBlock {
    /// Nominal close of the block: `start + BLOCK_DURATION_HOURS`.
    /// Derived rather than stored so the invariant can't drift from
    /// `start` after construction.
    #[must_use]
    pub fn end(&self) -> Timestamp {
        self.start + SignedDuration::from_hours(BLOCK_DURATION_HOURS)
    }
}

/// Rolling 7-day window. `window_start` is `now - 7d` at the time
/// the aggregator ran.
#[derive(Debug, Clone)]
pub struct SevenDayWindow {
    pub window_start: Timestamp,
    pub token_counts: TokenCounts,
}

/// Per-category token counts aggregated from the transcript.
///
/// # Invariants
///
/// - All mutations go through [`Self::accumulate`] so additions
///   saturate at `u64::MAX` rather than wrapping. Fields are
///   `pub(crate)` so in-crate code can read them; writes are
///   funnelled through the one private path that preserves the
///   saturating discipline. External crates read via [`Self::total`]
///   / [`Self::input`] / [`Self::output`] / [`Self::cache_creation`]
///   / [`Self::cache_read`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenCounts {
    pub(crate) input: u64,
    pub(crate) output: u64,
    pub(crate) cache_creation: u64,
    pub(crate) cache_read: u64,
}

impl TokenCounts {
    /// Test / fixture constructor. Not exposed to runtime callers —
    /// production `TokenCounts` values come from the aggregator's
    /// `accumulate` loop, which preserves the saturating invariant.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn from_parts(
        input: u64,
        output: u64,
        cache_creation: u64,
        cache_read: u64,
    ) -> Self {
        Self {
            input,
            output,
            cache_creation,
            cache_read,
        }
    }

    #[must_use]
    pub fn input(&self) -> u64 {
        self.input
    }

    #[must_use]
    pub fn output(&self) -> u64 {
        self.output
    }

    #[must_use]
    pub fn cache_creation(&self) -> u64 {
        self.cache_creation
    }

    #[must_use]
    pub fn cache_read(&self) -> u64 {
        self.cache_read
    }

    /// Saturating sum across all four categories. Saturating to
    /// match the spec's open-question note: `u64` overflow is
    /// practically unreachable, but wrap-on-overflow is surprising.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.input
            .saturating_add(self.output)
            .saturating_add(self.cache_creation)
            .saturating_add(self.cache_read)
    }

    fn accumulate(&mut self, other: UsageCounts) {
        self.input = self.input.saturating_add(other.input_tokens);
        self.output = self.output.saturating_add(other.output_tokens);
        self.cache_creation = self.cache_creation.saturating_add(other.cache_creation);
        self.cache_read = self.cache_read.saturating_add(other.cache_read);
    }
}

// --- JsonlError ---------------------------------------------------------

#[derive(Debug)]
#[non_exhaustive]
pub enum JsonlError {
    /// No project-root directory exists in any cascade path.
    DirectoryMissing,
    /// Project roots exist but yielded zero parseable records.
    NoEntries,
    /// Filesystem error opening or traversing a path.
    IoError { path: PathBuf, cause: io::Error },
    /// Reserved for fail-fast callers. Production aggregation logs
    /// per-line parse failures and continues rather than surfacing
    /// this variant.
    ParseError {
        path: PathBuf,
        line: u64,
        cause: serde_json::Error,
    },
}

impl JsonlError {
    /// Short plugin-facing tag per `docs/specs/plugin-api.md` §ctx
    /// shape. `ctx.jsonl` is reserved (not plugin-accessible) in
    /// v0.1 but the tag stays useful for `UsageError::Jsonl`
    /// delegation.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::DirectoryMissing => "DirectoryMissing",
            Self::NoEntries => "NoEntries",
            Self::IoError { .. } => "IoError",
            Self::ParseError { .. } => "ParseError",
        }
    }
}

impl std::fmt::Display for JsonlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DirectoryMissing => f.write_str("no Claude Code project directory found"),
            Self::NoEntries => f.write_str("Claude Code project directory has no JSONL entries"),
            Self::IoError { path, cause } => write!(
                f,
                "failed to read JSONL path {}: {}",
                path.display(),
                cause.kind()
            ),
            Self::ParseError { path, line, cause } => write!(
                f,
                "JSONL parse failed in {} at line {}: {}",
                path.display(),
                line,
                cause
            ),
        }
    }
}

impl std::error::Error for JsonlError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::IoError { cause, .. } => Some(cause),
            Self::ParseError { cause, .. } => Some(cause),
            _ => None,
        }
    }
}

// --- Per-line record schema --------------------------------------------

/// Serde view over a single JSONL line. Only the fields the
/// aggregator consumes are named; unknown keys (including
/// `costUSD` and `version` until we need them) are dropped per
/// ADR-0009.
#[derive(Debug, Deserialize)]
pub(crate) struct UsageEntry {
    timestamp: Timestamp,
    message: MessageFields,
    #[serde(default, rename = "usageLimitResetTime")]
    usage_limit_reset_time: Option<Timestamp>,
}

#[derive(Debug, Deserialize, Default)]
struct MessageFields {
    #[serde(default)]
    usage: Option<UsageCounts>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    id: Option<String>,
}

#[derive(Debug, Deserialize, Default, Clone, Copy)]
struct UsageCounts {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default, rename = "cache_creation_input_tokens")]
    cache_creation: u64,
    #[serde(default, rename = "cache_read_input_tokens")]
    cache_read: u64,
}

// --- Project-root discovery --------------------------------------------

/// Environmental inputs for `project_roots`. Injected by tests so
/// they don't have to mutate the (thread-unsafe) process env — same
/// pattern as `credentials::FileCascadeEnv`.
#[derive(Debug, Clone, Default)]
struct DiscoveryEnv {
    claude_config_dir: Option<PathBuf>,
    xdg_config_home: Option<PathBuf>,
    home: Option<PathBuf>,
}

impl DiscoveryEnv {
    fn from_process_env() -> Self {
        fn non_empty(key: &str) -> Option<PathBuf> {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        }
        Self {
            claude_config_dir: non_empty("CLAUDE_CONFIG_DIR"),
            xdg_config_home: non_empty("XDG_CONFIG_HOME"),
            home: non_empty("HOME"),
        }
    }
}

fn project_roots(env: &DiscoveryEnv) -> Vec<PathBuf> {
    let mut out = Vec::with_capacity(3);
    if let Some(dir) = &env.claude_config_dir {
        out.push(dir.join("projects"));
    }
    // XDG candidate is emitted whenever an XDG root is derivable —
    // either `$XDG_CONFIG_HOME` directly or `$HOME/.config`. A
    // HOME-less CI/service environment with only `$XDG_CONFIG_HOME`
    // set still gets its XDG path probed. Same pattern as
    // `credentials::file_cascade_candidates`.
    let xdg_root = env
        .xdg_config_home
        .clone()
        .or_else(|| env.home.as_ref().map(|h| h.join(".config")));
    if let Some(xdg_root) = xdg_root {
        out.push(xdg_root.join("claude").join("projects"));
    }
    // Legacy `~/.claude/projects/` requires `$HOME`.
    if let Some(home) = &env.home {
        out.push(home.join(".claude").join("projects"));
    }
    out
}

// --- JsonlTailer --------------------------------------------------------

/// Byte-offset incremental reader for a single JSONL file. Opens +
/// reads + closes per call; does NOT hold a file handle across
/// invocations. Detects truncation via `size < last_size` and
/// resets the offset when that happens.
pub(crate) struct JsonlTailer {
    path: PathBuf,
    last_offset: u64,
    last_size: u64,
}

impl JsonlTailer {
    #[must_use]
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            last_offset: 0,
            last_size: 0,
        }
    }

    /// Read any new complete lines since the last call. Malformed
    /// lines are silently skipped (the offset advances past them so
    /// repeat invocations don't re-encounter). Returns `Ok(vec![])`
    /// when the file doesn't exist yet — a fresh install scenario.
    pub(crate) fn read_new(&mut self) -> Result<Vec<UsageEntry>, JsonlError> {
        let metadata = match fs::metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(cause) => {
                return Err(JsonlError::IoError {
                    path: self.path.clone(),
                    cause,
                })
            }
        };

        let size = metadata.len();
        if size < self.last_size {
            self.last_offset = 0;
        }
        self.last_size = size;

        if self.last_offset >= size {
            return Ok(Vec::new());
        }

        let mut file = fs::File::open(&self.path).map_err(|cause| JsonlError::IoError {
            path: self.path.clone(),
            cause,
        })?;
        file.seek(SeekFrom::Start(self.last_offset))
            .map_err(|cause| JsonlError::IoError {
                path: self.path.clone(),
                cause,
            })?;

        let mut reader = BufReader::new(file);
        let mut entries = Vec::new();
        let mut buf: Vec<u8> = Vec::new();
        loop {
            buf.clear();
            // Byte-level read: a non-UTF-8 line becomes a per-line
            // skip (lossy convert + serde reject), not a whole-file
            // abort the way `read_line(&mut String)` would be.
            let read = reader
                .read_until(b'\n', &mut buf)
                .map_err(|cause| JsonlError::IoError {
                    path: self.path.clone(),
                    cause,
                })?;
            if read == 0 {
                break;
            }
            if buf.last() != Some(&b'\n') {
                // Partial trailing line: don't advance past it.
                break;
            }
            self.last_offset += read as u64;
            let line = match buf.strip_suffix(b"\n") {
                Some(rest) => rest.strip_suffix(b"\r").unwrap_or(rest),
                None => &buf[..],
            };
            let text = String::from_utf8_lossy(line);
            if let Ok(entry) = serde_json::from_str::<UsageEntry>(&text) {
                entries.push(entry);
            }
        }

        Ok(entries)
    }
}

// --- Aggregation entry point -------------------------------------------

/// Discover project roots, scan every `*.jsonl` under them, dedupe,
/// aggregate. Memoization is the caller's responsibility; each call
/// re-scans from offset zero.
pub fn aggregate_jsonl() -> Result<JsonlAggregate, JsonlError> {
    aggregate_jsonl_with(&DiscoveryEnv::from_process_env())
}

fn aggregate_jsonl_with(env: &DiscoveryEnv) -> Result<JsonlAggregate, JsonlError> {
    let candidate_roots = project_roots(env);
    let existing_roots: Vec<PathBuf> = candidate_roots.into_iter().filter(|r| r.exists()).collect();
    if existing_roots.is_empty() {
        return Err(JsonlError::DirectoryMissing);
    }

    let mut all_entries: Vec<UsageEntry> = Vec::new();
    let mut source_paths: Vec<PathBuf> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for root in &existing_roots {
        collect_from_root(root, &mut all_entries, &mut source_paths, &mut seen_ids)?;
    }

    if all_entries.is_empty() {
        return Err(JsonlError::NoEntries);
    }

    all_entries.sort_by_key(|e| e.timestamp);
    Ok(build_aggregate(&all_entries, source_paths))
}

/// Recurse one level into each `projects/{workspace}/` subdir and
/// pick up `*.jsonl` files. Dedup on `message.id`; missing-id
/// entries are always kept.
fn collect_from_root(
    root: &Path,
    entries: &mut Vec<UsageEntry>,
    source_paths: &mut Vec<PathBuf>,
    seen_ids: &mut HashSet<String>,
) -> Result<(), JsonlError> {
    let top = match fs::read_dir(root) {
        Ok(iter) => iter,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(cause) => {
            return Err(JsonlError::IoError {
                path: root.to_path_buf(),
                cause,
            })
        }
    };
    for project in top {
        let project = match project {
            Ok(entry) => entry,
            Err(cause) => {
                crate::lsm_warn!(
                    "jsonl: dirent iteration under {} failed: {} ({cause}); skipping",
                    root.display(),
                    cause.kind(),
                );
                continue;
            }
        };
        let project_path = project.path();
        if !project_path.is_dir() {
            continue;
        }
        let session_iter = match fs::read_dir(&project_path) {
            Ok(iter) => iter,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(cause) => {
                // EACCES / EIO on a specific workspace dir — the
                // top-level project-root fix in resolve_usage_default
                // only catches root-level failures. Without this warn,
                // a stale/unreadable workspace silently poisons the
                // JSONL fallback and users see the endpoint-path error
                // with no diagnostic trail.
                crate::lsm_warn!(
                    "jsonl: read_dir {} failed: {} ({cause}); skipping workspace",
                    project_path.display(),
                    cause.kind(),
                );
                continue;
            }
        };
        for session in session_iter {
            let session = match session {
                Ok(entry) => entry,
                Err(cause) => {
                    crate::lsm_warn!(
                        "jsonl: dirent iteration under {} failed: {} ({cause}); skipping",
                        project_path.display(),
                        cause.kind(),
                    );
                    continue;
                }
            };
            let session_path = session.path();
            if session_path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let mut tailer = JsonlTailer::new(session_path.clone());
            let file_entries = match tailer.read_new() {
                Ok(entries) => entries,
                Err(JsonlError::IoError { path, cause }) => {
                    crate::lsm_warn!(
                        "jsonl: tailer read {} failed: {} ({cause}); skipping file",
                        path.display(),
                        cause.kind(),
                    );
                    continue;
                }
                Err(other) => {
                    crate::lsm_warn!(
                        "jsonl: tailer read {} failed: {other}; skipping file",
                        session_path.display(),
                    );
                    continue;
                }
            };
            source_paths.push(session_path);
            for entry in file_entries {
                if let Some(id) = &entry.message.id {
                    if !seen_ids.insert(id.clone()) {
                        continue;
                    }
                }
                entries.push(entry);
            }
        }
    }
    Ok(())
}

fn build_aggregate(entries: &[UsageEntry], source_paths: Vec<PathBuf>) -> JsonlAggregate {
    let now = Timestamp::now();
    let window_start = now - SignedDuration::from_hours(WINDOW_DAYS * 24);

    let five_hour = compute_active_block(entries, now);

    let mut seven_day_counts = TokenCounts::default();
    for entry in entries {
        // Spec §7-day window math: `[now - 7d, now]`. Clock skew
        // can produce future-dated entries — exclude them so a
        // misconfigured machine can't inflate the 7d totals until
        // wall-clock catches up.
        if entry.timestamp >= window_start && entry.timestamp <= now {
            if let Some(usage) = entry.message.usage {
                seven_day_counts.accumulate(usage);
            }
        }
    }

    JsonlAggregate {
        five_hour,
        seven_day: SevenDayWindow {
            window_start,
            token_counts: seven_day_counts,
        },
        source_paths,
    }
}

/// Walk entries chronologically, rolling into blocks whenever the
/// gap from the previous entry exceeds `BLOCK_DURATION_HOURS`.
/// Returns the latest block only if it's still active (last activity
/// within `BLOCK_DURATION_HOURS` of `now`).
///
/// Future-dated entries (clock skew) are deliberately NOT filtered
/// here — their tokens still count so a user with a slightly-fast
/// clock doesn't lose their current session under JSONL fallback.
/// The cascade's [`build_jsonl_usage`](super::cascade::build_jsonl_usage)
/// clamps `block.start` to `now`'s hour-floor before surfacing the
/// window to segments, which neutralizes skewed `ends_at` without
/// corrupting the token totals.
fn compute_active_block(entries: &[UsageEntry], now: Timestamp) -> Option<FiveHourBlock> {
    let block_duration = SignedDuration::from_hours(BLOCK_DURATION_HOURS);
    let mut current: Option<FiveHourBlock> = None;
    for entry in entries {
        match &mut current {
            None => current = Some(start_block(entry)),
            Some(block) => {
                let gap = entry.timestamp.duration_since(block.actual_last_activity);
                if gap > block_duration {
                    current = Some(start_block(entry));
                } else {
                    extend_block(block, entry);
                }
            }
        }
    }
    let block = current?;
    if now.duration_since(block.actual_last_activity) > block_duration {
        None
    } else {
        Some(block)
    }
}

fn start_block(entry: &UsageEntry) -> FiveHourBlock {
    let mut block = FiveHourBlock {
        start: floor_to_grain(entry.timestamp, 3600),
        actual_last_activity: entry.timestamp,
        token_counts: TokenCounts::default(),
        models: Vec::new(),
        usage_limit_reset: None,
    };
    extend_block(&mut block, entry);
    block
}

fn extend_block(block: &mut FiveHourBlock, entry: &UsageEntry) {
    if let Some(usage) = entry.message.usage {
        block.token_counts.accumulate(usage);
    }
    if let Some(model) = &entry.message.model {
        if !block.models.iter().any(|m| m == model) {
            block.models.push(model.clone());
        }
    }
    if let Some(reset) = entry.usage_limit_reset_time {
        block.usage_limit_reset = Some(reset);
    }
    block.actual_last_activity = entry.timestamp;
}

/// Floor a timestamp to a whole multiple of `grain_secs` seconds (UTC).
/// Falls back to the input on overflow: `rem_euclid` always returns a
/// non-negative remainder, so subtracting it pushes a near-`MIN`
/// timestamp out of jiff's range. A crafted JSONL line with a
/// `-009999-01-02T01:59:59Z` timestamp (= `Timestamp::MIN`) round-trips
/// through serde, so an unconditional `expect` would panic on the
/// aggregator hot path.
pub(super) fn floor_to_grain(ts: Timestamp, grain_secs: i64) -> Timestamp {
    let secs = ts.as_second();
    let floored = secs - secs.rem_euclid(grain_secs);
    Timestamp::from_second(floored).unwrap_or(ts)
}

#[cfg(test)]
mod tests;
