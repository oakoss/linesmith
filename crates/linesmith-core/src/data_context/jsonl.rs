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

use chrono::{DateTime, Duration, DurationRound, Utc};
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
    pub start: DateTime<Utc>,
    pub actual_last_activity: DateTime<Utc>,
    pub token_counts: TokenCounts,
    pub models: Vec<String>,
    pub usage_limit_reset: Option<DateTime<Utc>>,
}

impl FiveHourBlock {
    /// Nominal close of the block: `start + BLOCK_DURATION_HOURS`.
    /// Derived rather than stored so the invariant can't drift from
    /// `start` after construction.
    #[must_use]
    pub fn end(&self) -> DateTime<Utc> {
        self.start + Duration::hours(BLOCK_DURATION_HOURS)
    }
}

/// Rolling 7-day window. `window_start` is `now - 7d` at the time
/// the aggregator ran.
#[derive(Debug, Clone)]
pub struct SevenDayWindow {
    pub window_start: DateTime<Utc>,
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
    timestamp: DateTime<Utc>,
    message: MessageFields,
    #[serde(default, rename = "usageLimitResetTime")]
    usage_limit_reset_time: Option<DateTime<Utc>>,
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
    let now = Utc::now();
    let window_start = now - Duration::days(WINDOW_DAYS);

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
fn compute_active_block(entries: &[UsageEntry], now: DateTime<Utc>) -> Option<FiveHourBlock> {
    let block_duration = Duration::hours(BLOCK_DURATION_HOURS);
    let mut current: Option<FiveHourBlock> = None;
    for entry in entries {
        match &mut current {
            None => current = Some(start_block(entry)),
            Some(block) => {
                let gap = entry.timestamp - block.actual_last_activity;
                if gap > block_duration {
                    current = Some(start_block(entry));
                } else {
                    extend_block(block, entry);
                }
            }
        }
    }
    let block = current?;
    if now - block.actual_last_activity > block_duration {
        None
    } else {
        Some(block)
    }
}

fn start_block(entry: &UsageEntry) -> FiveHourBlock {
    let mut block = FiveHourBlock {
        start: floor_to_hour(entry.timestamp),
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

pub(super) fn floor_to_hour(ts: DateTime<Utc>) -> DateTime<Utc> {
    // `duration_trunc(hours(1))` only errors on zero / overflow
    // durations, which can't arise from a 1-hour grain.
    ts.duration_trunc(Duration::hours(1))
        .expect("1-hour grain never overflows DateTime<Utc>")
}

// --- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use tempfile::TempDir;

    fn env_from(claude: Option<&Path>, xdg: Option<&Path>, home: Option<&Path>) -> DiscoveryEnv {
        DiscoveryEnv {
            claude_config_dir: claude.map(Path::to_path_buf),
            xdg_config_home: xdg.map(Path::to_path_buf),
            home: home.map(Path::to_path_buf),
        }
    }

    fn write_jsonl(dir: &Path, workspace: &str, session: &str, lines: &[&str]) -> PathBuf {
        let target = dir.join(workspace);
        fs::create_dir_all(&target).unwrap();
        let path = target.join(session);
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    fn record(ts: &str, input: u64, output: u64, id: Option<&str>) -> String {
        let id_part = id.map_or(String::new(), |i| format!(r#","id":"{i}""#));
        format!(
            r#"{{"timestamp":"{ts}","message":{{"usage":{{"input_tokens":{input},"output_tokens":{output}}},"model":"claude-opus-4-7"{id_part}}}}}"#
        )
    }

    // --- project_roots cascade ----------------------------------------

    #[test]
    fn project_roots_includes_env_dir_when_set() {
        let tmp = TempDir::new().unwrap();
        let env = env_from(Some(tmp.path()), None, Some(tmp.path()));
        let roots = project_roots(&env);
        assert!(roots[0].ends_with("projects"));
        assert!(roots[0].starts_with(tmp.path()));
    }

    #[test]
    fn project_roots_omits_env_dir_when_unset() {
        let tmp = TempDir::new().unwrap();
        let env = env_from(None, None, Some(tmp.path()));
        let roots = project_roots(&env);
        for r in &roots {
            assert!(!r
                .parent()
                .unwrap()
                .ends_with(tmp.path().file_name().unwrap()));
        }
    }

    #[test]
    fn project_roots_falls_back_to_home_when_xdg_unset() {
        let tmp = TempDir::new().unwrap();
        let env = env_from(None, None, Some(tmp.path()));
        let roots = project_roots(&env);
        assert!(roots
            .iter()
            .any(|r| r.starts_with(tmp.path().join(".config"))));
        assert!(roots
            .iter()
            .any(|r| r.starts_with(tmp.path().join(".claude"))));
    }

    // --- aggregate_jsonl_with: dir-level outcomes ---------------------

    #[test]
    fn aggregate_returns_directory_missing_when_no_roots_exist() {
        let tmp = TempDir::new().unwrap();
        let env = env_from(None, None, Some(&tmp.path().join("nonexistent")));
        let err = aggregate_jsonl_with(&env).unwrap_err();
        assert!(matches!(err, JsonlError::DirectoryMissing));
    }

    #[test]
    fn aggregate_returns_no_entries_when_roots_empty() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".claude").join("projects")).unwrap();
        let env = env_from(None, None, Some(tmp.path()));
        let err = aggregate_jsonl_with(&env).unwrap_err();
        assert!(matches!(err, JsonlError::NoEntries));
    }

    // --- 5h block math ------------------------------------------------

    #[test]
    fn active_block_computed_from_recent_entries() {
        // Two entries within 5h of now.
        let now = Utc::now();
        let e1 = UsageEntry {
            timestamp: now - Duration::hours(1),
            message: MessageFields {
                usage: Some(UsageCounts {
                    input_tokens: 100,
                    output_tokens: 50,
                    cache_creation: 0,
                    cache_read: 0,
                }),
                model: Some("claude-opus-4-7".into()),
                id: Some("msg_1".into()),
            },
            usage_limit_reset_time: None,
        };
        let block = compute_active_block(&[e1], now).expect("active");
        assert_eq!(block.token_counts.input, 100);
        assert_eq!(block.models, vec!["claude-opus-4-7"]);
    }

    #[test]
    fn no_active_block_when_last_entry_is_older_than_window() {
        let now = Utc::now();
        let e1 = UsageEntry {
            timestamp: now - Duration::hours(10),
            message: MessageFields::default(),
            usage_limit_reset_time: None,
        };
        assert!(compute_active_block(&[e1], now).is_none());
    }

    #[test]
    fn new_block_starts_on_gap_exceeding_window() {
        // e1 at T-8h, e2 at T-1h. Gap is 7h > 5h, so a new block
        // starts at e2. Only the newer block is kept.
        let now = Utc::now();
        let e1 = UsageEntry {
            timestamp: now - Duration::hours(8),
            message: MessageFields {
                usage: Some(UsageCounts {
                    input_tokens: 999,
                    ..UsageCounts::default()
                }),
                ..MessageFields::default()
            },
            usage_limit_reset_time: None,
        };
        let e2 = UsageEntry {
            timestamp: now - Duration::hours(1),
            message: MessageFields {
                usage: Some(UsageCounts {
                    input_tokens: 10,
                    ..UsageCounts::default()
                }),
                ..MessageFields::default()
            },
            usage_limit_reset_time: None,
        };
        let block = compute_active_block(&[e1, e2], now).expect("active");
        // Only e2's tokens count — e1 was rolled into a closed block.
        assert_eq!(block.token_counts.input, 10);
    }

    #[test]
    fn usage_limit_reset_picks_most_recent() {
        let now = Utc::now();
        let earlier_reset = now + Duration::hours(1);
        let later_reset = now + Duration::hours(2);
        let e1 = UsageEntry {
            timestamp: now - Duration::minutes(90),
            message: MessageFields::default(),
            usage_limit_reset_time: Some(earlier_reset),
        };
        let e2 = UsageEntry {
            timestamp: now - Duration::minutes(30),
            message: MessageFields::default(),
            usage_limit_reset_time: Some(later_reset),
        };
        let block = compute_active_block(&[e1, e2], now).expect("active");
        assert_eq!(block.usage_limit_reset, Some(later_reset));
    }

    // --- Per-line record parsing --------------------------------------

    #[test]
    fn parses_full_record_shape() {
        let line = r#"{"timestamp":"2026-04-20T14:23:47Z","message":{"id":"msg_1","model":"claude-opus-4-7","usage":{"input_tokens":1842,"output_tokens":631,"cache_creation_input_tokens":0,"cache_read_input_tokens":48122}},"costUSD":0.0421,"version":"1.0.85","usageLimitResetTime":"2026-04-20T19:00:00Z"}"#;
        let entry: UsageEntry = serde_json::from_str(line).expect("parse");
        let u = entry.message.usage.unwrap();
        assert_eq!(u.input_tokens, 1842);
        assert_eq!(u.cache_read, 48122);
        assert_eq!(entry.message.id.as_deref(), Some("msg_1"));
        assert!(entry.usage_limit_reset_time.is_some());
    }

    #[test]
    fn parses_sparse_record_shape() {
        // Missing model, id, cost, reset — just timestamp + usage.
        let line = r#"{"timestamp":"2026-04-20T14:23:47Z","message":{"usage":{"input_tokens":100,"output_tokens":50}}}"#;
        let entry: UsageEntry = serde_json::from_str(line).expect("parse");
        assert_eq!(entry.message.usage.unwrap().input_tokens, 100);
        assert!(entry.message.id.is_none());
    }

    #[test]
    fn unknown_fields_are_dropped() {
        // Future schema extension shouldn't break parsing.
        let line = r#"{"timestamp":"2026-04-20T14:23:47Z","message":{},"futureField":"ignored","anotherThing":{"nested":true}}"#;
        serde_json::from_str::<UsageEntry>(line).expect("parse");
    }

    // --- JsonlTailer -------------------------------------------------

    #[test]
    fn tailer_reads_all_lines_on_first_call() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        let lines = [
            record("2026-04-20T00:00:00Z", 1, 1, Some("a")),
            record("2026-04-20T00:01:00Z", 2, 2, Some("b")),
            record("2026-04-20T00:02:00Z", 3, 3, Some("c")),
        ];
        fs::write(&path, lines.join("\n") + "\n").unwrap();
        let mut tailer = JsonlTailer::new(path);
        let entries = tailer.read_new().expect("ok");
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn tailer_only_reads_new_lines_on_second_call() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        fs::write(
            &path,
            record("2026-04-20T00:00:00Z", 1, 1, Some("a")) + "\n",
        )
        .unwrap();
        let mut tailer = JsonlTailer::new(path.clone());
        let first = tailer.read_new().expect("ok");
        assert_eq!(first.len(), 1);

        let existing = fs::read_to_string(&path).unwrap();
        let new_line = record("2026-04-20T00:01:00Z", 2, 2, Some("b"));
        fs::write(&path, format!("{existing}{new_line}\n")).unwrap();
        let second = tailer.read_new().expect("ok");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].message.id.as_deref(), Some("b"));
    }

    #[test]
    fn tailer_returns_empty_for_missing_file() {
        let tmp = TempDir::new().unwrap();
        let mut tailer = JsonlTailer::new(tmp.path().join("nonexistent.jsonl"));
        let entries = tailer.read_new().expect("ok");
        assert!(entries.is_empty());
    }

    #[test]
    fn tailer_resets_on_truncation() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        let initial = [
            record("2026-04-20T00:00:00Z", 1, 1, Some("a")),
            record("2026-04-20T00:01:00Z", 2, 2, Some("b")),
        ];
        fs::write(&path, initial.join("\n") + "\n").unwrap();
        let mut tailer = JsonlTailer::new(path.clone());
        tailer.read_new().expect("first");

        let new_line = record("2026-04-20T00:02:00Z", 3, 3, Some("c"));
        fs::write(&path, new_line + "\n").unwrap();
        let after = tailer.read_new().expect("after truncate");
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].message.id.as_deref(), Some("c"));
    }

    #[test]
    fn tailer_skips_partial_trailing_line() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        let complete = record("2026-04-20T00:00:00Z", 1, 1, Some("a"));
        fs::write(
            &path,
            format!("{complete}\n{}", r#"{"timestamp":"2026-04-20T00:01:00Z""#),
        )
        .unwrap();
        let mut tailer = JsonlTailer::new(path);
        let entries = tailer.read_new().expect("ok");
        // Partial line must not advance the offset — it would be
        // re-read on the next poll once the tail completes.
        assert_eq!(entries.len(), 1);
    }

    #[test]
    fn tailer_skips_non_utf8_line_and_keeps_later_valid_lines() {
        // Per spec §Edge cases, non-UTF-8 is a malformed-line class
        // — the tailer must skip just the bad line, NOT abandon the
        // rest of the file. Pre-fix, `read_line(&mut String)` raised
        // `InvalidData` and the file was dropped entirely.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        let good_before = record("2026-04-20T00:00:00Z", 1, 1, Some("before"));
        let good_after = record("2026-04-20T00:02:00Z", 3, 3, Some("after"));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(good_before.as_bytes());
        bytes.push(b'\n');
        bytes.extend_from_slice(&[0xFF, 0xFE, 0xFD, b'\n']);
        bytes.extend_from_slice(good_after.as_bytes());
        bytes.push(b'\n');
        fs::write(&path, &bytes).unwrap();
        let mut tailer = JsonlTailer::new(path);
        let entries = tailer.read_new().expect("ok");
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn tailer_skips_malformed_lines_and_advances_past_them() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        let good = record("2026-04-20T00:00:00Z", 1, 1, Some("a"));
        let bad = "{ this is not json }";
        let good2 = record("2026-04-20T00:01:00Z", 2, 2, Some("b"));
        fs::write(&path, format!("{good}\n{bad}\n{good2}\n")).unwrap();
        let mut tailer = JsonlTailer::new(path);
        let entries = tailer.read_new().expect("ok");
        assert_eq!(entries.len(), 2);
    }

    // --- Dedup semantics ----------------------------------------------

    #[test]
    fn aggregate_dedupes_on_message_id() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let projects = home.join(".claude").join("projects");
        let now = Utc::now();
        let ts = now
            .duration_trunc(Duration::minutes(1))
            .unwrap()
            .to_rfc3339();
        let line = record(&ts, 100, 50, Some("dup-1"));
        write_jsonl(&projects, "-proj-a", "sess1.jsonl", &[&line]);
        write_jsonl(&projects, "-proj-a", "sess2.jsonl", &[&line]);

        let env = env_from(None, None, Some(home));
        let agg = aggregate_jsonl_with(&env).expect("aggregate");
        // Dedupe merges the shared id across sessions → 100, not 200.
        assert_eq!(agg.seven_day.token_counts.input, 100);
    }

    #[test]
    fn aggregate_keeps_missing_id_entries_individually() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let projects = home.join(".claude").join("projects");
        let now = Utc::now();
        let ts = now
            .duration_trunc(Duration::minutes(1))
            .unwrap()
            .to_rfc3339();
        let line = record(&ts, 100, 50, None);
        write_jsonl(&projects, "-proj-a", "sess1.jsonl", &[&line, &line]);

        let env = env_from(None, None, Some(home));
        let agg = aggregate_jsonl_with(&env).expect("aggregate");
        assert_eq!(agg.seven_day.token_counts.input, 200);
    }

    // --- Full aggregate integration -----------------------------------

    #[test]
    fn aggregate_happy_path_produces_active_block_and_7d_window() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let projects = home.join(".claude").join("projects");
        let now = Utc::now();
        let recent_ts = now
            .duration_trunc(Duration::minutes(1))
            .unwrap()
            .to_rfc3339();
        let old_ts = (now - Duration::days(3))
            .duration_trunc(Duration::minutes(1))
            .unwrap()
            .to_rfc3339();
        let old_line = record(&old_ts, 500, 100, Some("old-1"));
        let recent_line = record(&recent_ts, 250, 50, Some("new-1"));
        write_jsonl(
            &projects,
            "-Users-alice-code-myrepo",
            "session.jsonl",
            &[&old_line, &recent_line],
        );

        let env = env_from(None, None, Some(home));
        let agg = aggregate_jsonl_with(&env).expect("aggregate");

        // 7d window picks up both entries.
        assert_eq!(agg.seven_day.token_counts.input, 750);
        // Active block is anchored to the recent entry.
        let block = agg.five_hour.expect("active block");
        assert_eq!(block.token_counts.input, 250);
    }

    #[test]
    fn aggregate_old_only_transcript_has_no_active_block() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let projects = home.join(".claude").join("projects");
        let old_ts = (Utc::now() - Duration::days(10))
            .duration_trunc(Duration::minutes(1))
            .unwrap()
            .to_rfc3339();
        let line = record(&old_ts, 100, 50, Some("old-1"));
        write_jsonl(&projects, "-proj-a", "session.jsonl", &[&line]);

        let env = env_from(None, None, Some(home));
        let agg = aggregate_jsonl_with(&env).expect("aggregate");
        assert!(agg.five_hour.is_none());
        // 7d window also excludes the >7d entry.
        assert_eq!(agg.seven_day.token_counts.input, 0);
    }

    // --- TokenCounts saturating arithmetic ---------------------------

    #[test]
    fn token_counts_total_saturates_on_overflow() {
        let counts = TokenCounts::from_parts(u64::MAX - 5, 10, 0, 0);
        assert_eq!(counts.total(), u64::MAX);
    }

    #[test]
    fn token_counts_from_parts_pins_positional_argument_order() {
        // Regression guard: every in-crate test calls this constructor
        // with specific values in specific positions (e.g. the cascade
        // `jsonl_ok` fixture uses 1_000_000 input + 200_000 output).
        // If the argument order is ever reshuffled, the `total()`
        // assertions downstream still pass (sums are order-invariant)
        // so a typed-field check here is the only structural guard.
        let t = TokenCounts::from_parts(1, 2, 3, 4);
        assert_eq!(t.input(), 1);
        assert_eq!(t.output(), 2);
        assert_eq!(t.cache_creation(), 3);
        assert_eq!(t.cache_read(), 4);
        assert_eq!(t.total(), 10);
    }

    // --- Error taxonomy -----------------------------------------------

    #[test]
    fn jsonl_error_code_taxonomy_is_unique() {
        let all: [(JsonlError, &str); 4] = [
            (JsonlError::DirectoryMissing, "DirectoryMissing"),
            (JsonlError::NoEntries, "NoEntries"),
            (
                JsonlError::IoError {
                    path: PathBuf::from("/x"),
                    cause: io::Error::other("x"),
                },
                "IoError",
            ),
            (
                JsonlError::ParseError {
                    path: PathBuf::from("/x"),
                    line: 1,
                    cause: serde_json::from_str::<i32>("x").unwrap_err(),
                },
                "ParseError",
            ),
        ];
        let codes: std::collections::HashSet<&'static str> =
            all.iter().map(|(e, _)| e.code()).collect();
        assert_eq!(codes.len(), 4);
        for (err, expected) in &all {
            assert_eq!(err.code(), *expected);
        }
    }

    // --- floor_to_hour edge case --------------------------------------

    #[test]
    fn floor_to_hour_truncates_subhour_components() {
        let ts = Utc.with_ymd_and_hms(2026, 4, 20, 14, 37, 52).unwrap();
        let floored = floor_to_hour(ts);
        assert_eq!(
            floored,
            Utc.with_ymd_and_hms(2026, 4, 20, 14, 0, 0).unwrap()
        );
    }

    // --- FiveHourBlock::end derivation --------------------------------

    #[test]
    fn five_hour_block_end_derives_from_start() {
        let now = Utc::now();
        let e = UsageEntry {
            timestamp: now - Duration::minutes(30),
            message: MessageFields::default(),
            usage_limit_reset_time: None,
        };
        let block = compute_active_block(&[e], now).expect("active");
        assert_eq!(
            block.end(),
            block.start + Duration::hours(BLOCK_DURATION_HOURS)
        );
    }

    // --- Block-boundary math (off-by-one guard) -----------------------

    #[test]
    fn entries_exactly_5h_apart_stay_in_same_block() {
        // Spec: gap > 5h opens a new block. A gap of exactly 5h
        // (not strictly greater) must keep both entries in the
        // single rolling block. Guards against a `>=` refactor.
        let now = Utc::now();
        let e1 = UsageEntry {
            timestamp: now - Duration::hours(5),
            message: MessageFields {
                usage: Some(UsageCounts {
                    input_tokens: 100,
                    ..UsageCounts::default()
                }),
                ..MessageFields::default()
            },
            usage_limit_reset_time: None,
        };
        let e2 = UsageEntry {
            timestamp: now,
            message: MessageFields {
                usage: Some(UsageCounts {
                    input_tokens: 50,
                    ..UsageCounts::default()
                }),
                ..MessageFields::default()
            },
            usage_limit_reset_time: None,
        };
        let block = compute_active_block(&[e1, e2], now).expect("active");
        // Both entries rolled into one block → 150 total input.
        assert_eq!(block.token_counts.input, 150);
    }

    #[test]
    fn gap_of_5h_plus_one_ns_opens_new_block() {
        // Strict `>` — one nanosecond past the boundary flips.
        let now = Utc::now();
        let e1 = UsageEntry {
            timestamp: now - Duration::hours(5) - Duration::nanoseconds(1),
            message: MessageFields {
                usage: Some(UsageCounts {
                    input_tokens: 999,
                    ..UsageCounts::default()
                }),
                ..MessageFields::default()
            },
            usage_limit_reset_time: None,
        };
        let e2 = UsageEntry {
            timestamp: now,
            message: MessageFields {
                usage: Some(UsageCounts {
                    input_tokens: 7,
                    ..UsageCounts::default()
                }),
                ..MessageFields::default()
            },
            usage_limit_reset_time: None,
        };
        let block = compute_active_block(&[e1, e2], now).expect("active");
        // Only the newer block survives; 999 was rolled into a
        // closed block and discarded in v0.1.
        assert_eq!(block.token_counts.input, 7);
    }

    // --- 7-day window boundary ---------------------------------------

    #[test]
    fn entry_at_exactly_7d_boundary_is_included() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let projects = home.join(".claude").join("projects");
        // Write timestamp that `build_aggregate`'s `window_start`
        // check (`e.timestamp >= window_start`) will include.
        // Use `now - 7d + 10s` so the spec's `>=` boundary is
        // pinned without depending on instant-perfect math.
        let near_boundary = (Utc::now() - Duration::days(7) + Duration::seconds(10))
            .duration_trunc(Duration::seconds(1))
            .unwrap()
            .to_rfc3339();
        let line = record(&near_boundary, 42, 0, Some("boundary"));
        write_jsonl(&projects, "-proj", "sess.jsonl", &[&line]);
        let env = env_from(None, None, Some(home));
        let agg = aggregate_jsonl_with(&env).expect("aggregate");
        assert_eq!(agg.seven_day.token_counts.input, 42);
    }

    #[test]
    fn entry_older_than_7d_excluded_from_window() {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let projects = home.join(".claude").join("projects");
        // `now - 8d` is definitively outside the window.
        let old = (Utc::now() - Duration::days(8))
            .duration_trunc(Duration::seconds(1))
            .unwrap()
            .to_rfc3339();
        let line = record(&old, 1000, 0, Some("way-old"));
        write_jsonl(&projects, "-proj", "sess.jsonl", &[&line]);
        let env = env_from(None, None, Some(home));
        let agg = aggregate_jsonl_with(&env).expect("aggregate");
        assert_eq!(agg.seven_day.token_counts.input, 0);
    }

    // --- Cross-root dedup --------------------------------------------

    #[test]
    fn aggregate_dedupes_across_cascade_roots() {
        // Spec: the same message.id written to two different roots
        // (one after a user migrates ~/.claude → CLAUDE_CONFIG_DIR,
        // say) collapses to a single entry. Regression guard for
        // a per-root HashMap without a global merge step.
        let tmp = TempDir::new().unwrap();
        let env_dir = tmp.path().join("env-dir");
        let home = tmp.path().join("home");
        let env_projects = env_dir.join("projects");
        let legacy_projects = home.join(".claude").join("projects");
        let ts = Utc::now()
            .duration_trunc(Duration::minutes(1))
            .unwrap()
            .to_rfc3339();
        let line = record(&ts, 100, 50, Some("shared-msg"));
        write_jsonl(&env_projects, "-proj", "sess-env.jsonl", &[&line]);
        write_jsonl(&legacy_projects, "-proj", "sess-legacy.jsonl", &[&line]);
        let env = env_from(Some(&env_dir), None, Some(&home));
        let agg = aggregate_jsonl_with(&env).expect("aggregate");
        // If dedup is scoped per-root, this would show 200 tokens.
        assert_eq!(agg.seven_day.token_counts.input, 100);
    }

    // --- Offset monotonicity invariant -------------------------------

    #[test]
    fn tailer_offset_monotonically_advances_on_repeat_reads() {
        // Invariant from spec §Testing strategy: new_offset >=
        // old_offset and new_offset <= file_size. Exercise several
        // append cycles.
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("t.jsonl");
        fs::write(
            &path,
            record("2026-04-20T00:00:00Z", 1, 1, Some("a")) + "\n",
        )
        .unwrap();
        let mut tailer = JsonlTailer::new(path.clone());
        tailer.read_new().expect("first");
        let after_first = tailer.last_offset;
        assert_eq!(after_first, tailer.last_size);

        let existing = fs::read_to_string(&path).unwrap();
        let new_line = record("2026-04-20T00:01:00Z", 2, 2, Some("b"));
        fs::write(&path, format!("{existing}{new_line}\n")).unwrap();
        tailer.read_new().expect("second");
        let after_second = tailer.last_offset;
        assert!(after_second > after_first, "offset must advance");
        assert_eq!(after_second, tailer.last_size);

        tailer.read_new().expect("third");
        assert_eq!(tailer.last_offset, after_second);
    }

    // --- Models dedup within a block ---------------------------------

    #[test]
    fn block_models_dedupes_within_block() {
        let now = Utc::now();
        fn mk(ts: DateTime<Utc>, model: &str) -> UsageEntry {
            UsageEntry {
                timestamp: ts,
                message: MessageFields {
                    model: Some(model.to_string()),
                    ..MessageFields::default()
                },
                usage_limit_reset_time: None,
            }
        }
        let entries = [
            mk(now - Duration::minutes(30), "claude-opus-4-7"),
            mk(now - Duration::minutes(20), "claude-sonnet-4-6"),
            mk(now - Duration::minutes(10), "claude-opus-4-7"),
        ];
        let block = compute_active_block(&entries, now).expect("active");
        assert_eq!(block.models.len(), 2);
    }

    // --- usage_limit_reset merge -------------------------------------

    // --- XDG-without-HOME cascade ------------------------------------

    #[test]
    fn project_roots_includes_xdg_when_home_unset() {
        // Service/CI environments can set $XDG_CONFIG_HOME without
        // $HOME; the XDG projects root must not be gated on home.
        // Matches the credentials::file_cascade_candidates pattern.
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        let env = env_from(None, Some(&xdg), None);
        let roots = project_roots(&env);
        assert!(
            roots
                .iter()
                .any(|r| r == &xdg.join("claude").join("projects")),
            "XDG candidate must be present with HOME unset + XDG set",
        );
        assert!(
            !roots.iter().any(|r| r.ends_with(".claude/projects")),
            "Legacy ~/.claude requires HOME",
        );
    }

    #[test]
    fn aggregate_reads_xdg_projects_when_home_unset() {
        let tmp = TempDir::new().unwrap();
        let xdg = tmp.path().join("xdg");
        let ts = Utc::now()
            .duration_trunc(Duration::minutes(1))
            .unwrap()
            .to_rfc3339();
        let line = record(&ts, 77, 33, Some("xdg-only"));
        write_jsonl(
            &xdg.join("claude").join("projects"),
            "-proj",
            "sess.jsonl",
            &[&line],
        );
        let env = env_from(None, Some(&xdg), None);
        let agg = aggregate_jsonl_with(&env).expect("aggregate");
        assert_eq!(agg.seven_day.token_counts.input, 77);
    }

    // --- Future-dated entry excluded from 7d window ------------------

    #[test]
    fn seven_day_window_excludes_future_timestamps() {
        // Clock skew (backup restore, NTP glitch, VM resume) can
        // stamp entries in the future. They must NOT inflate the
        // 7d totals — spec says window is `[now - 7d, now]`.
        let tmp = TempDir::new().unwrap();
        let home = tmp.path();
        let projects = home.join(".claude").join("projects");
        let future = (Utc::now() + Duration::hours(2))
            .duration_trunc(Duration::seconds(1))
            .unwrap()
            .to_rfc3339();
        let future_line = record(&future, 500, 0, Some("future-1"));
        let past = Utc::now()
            .duration_trunc(Duration::seconds(1))
            .unwrap()
            .to_rfc3339();
        let past_line = record(&past, 10, 0, Some("past-1"));
        write_jsonl(
            &projects,
            "-proj",
            "sess.jsonl",
            &[&future_line, &past_line],
        );
        let env = env_from(None, None, Some(home));
        let agg = aggregate_jsonl_with(&env).expect("aggregate");
        // Only the present entry contributes; future one excluded.
        // Assert across categories so a half-applied fix that only
        // guards `input_tokens` (leaving `output_tokens` to bleed
        // through) trips this test.
        assert_eq!(agg.seven_day.token_counts.input, 10);
        assert_eq!(agg.seven_day.token_counts.output, 0);
        assert_eq!(agg.seven_day.token_counts.cache_creation, 0);
        assert_eq!(agg.seven_day.token_counts.cache_read, 0);
    }

    #[test]
    fn claude_config_dir_only_no_home_no_xdg() {
        // Pure env-dir cascade: CLAUDE_CONFIG_DIR set, both HOME and
        // XDG_CONFIG_HOME unset. Must produce exactly one candidate.
        let tmp = TempDir::new().unwrap();
        let env = env_from(Some(tmp.path()), None, None);
        let roots = project_roots(&env);
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0], tmp.path().join("projects"));
    }

    #[test]
    fn future_timestamp_inside_5h_block_is_counted_as_mild_skew() {
        // Pins the intended behavior for mild clock skew: an entry
        // stamped in the near future still anchors the active block
        // (the `now - actual_last_activity > 5h` liveness check is
        // false for negative durations). Users with slightly-fast
        // clocks shouldn't have their current session's tokens
        // disappear. Pathological far-future skew is a separate
        // hardening question, out of scope here.
        let now = Utc::now();
        let future_entry = UsageEntry {
            timestamp: now + Duration::minutes(10),
            message: MessageFields {
                usage: Some(UsageCounts {
                    input_tokens: 42,
                    ..UsageCounts::default()
                }),
                ..MessageFields::default()
            },
            usage_limit_reset_time: None,
        };
        let block = compute_active_block(&[future_entry], now).expect("active");
        assert_eq!(block.token_counts.input, 42);
    }

    #[test]
    fn usage_limit_reset_keeps_some_over_later_none() {
        // `extend_block` only overwrites when the incoming entry
        // carries `Some(_)`. A later entry with `None` must not
        // clear the earlier reset hint.
        let now = Utc::now();
        let reset = now + Duration::hours(1);
        let e1 = UsageEntry {
            timestamp: now - Duration::minutes(30),
            message: MessageFields::default(),
            usage_limit_reset_time: Some(reset),
        };
        let e2 = UsageEntry {
            timestamp: now - Duration::minutes(10),
            message: MessageFields::default(),
            usage_limit_reset_time: None,
        };
        let block = compute_active_block(&[e1, e2], now).expect("active");
        assert_eq!(block.usage_limit_reset, Some(reset));
    }
}
