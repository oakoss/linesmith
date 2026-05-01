//! `linesmith doctor` — diagnostic subcommand. Renders a categorized
//! health report with PASS / WARN / FAIL / SKIP severities, then exits
//! with a contract-defined code (any FAIL → 1; otherwise 0). Spec:
//! `docs/specs/doctor.md`.
//!
//! Encapsulation note: `CheckResult` hides its fields behind
//! constructors because severity and hint must agree (PASS forbids
//! hints; non-PASS requires them). `Category` and `Report` have no
//! cross-field invariants, so their fields stay public — same shape
//! as `std::process::Output`. If `Category` grows a non-empty-name
//! invariant or `Report` gains check-id-uniqueness, both gain
//! constructors and seal their fields.

use std::collections::BTreeSet;
use std::io::{IsTerminal, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use crate::config::{Config, ConfigPath};

mod snapshot;
use snapshot::*;

/// One of four outcomes a check can report. See `docs/specs/doctor.md`
/// §Severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Severity {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Severity {
    /// Unicode glyph (used unless `--plain`).
    #[must_use]
    pub fn unicode_glyph(self) -> &'static str {
        match self {
            Self::Pass => "✓",
            Self::Warn => "⚠",
            Self::Fail => "✗",
            Self::Skip => "·",
        }
    }

    /// ASCII glyph (used under `--plain`).
    #[must_use]
    pub fn ascii_glyph(self) -> &'static str {
        match self {
            Self::Pass => "OK",
            Self::Warn => "!!",
            Self::Fail => "XX",
            Self::Skip => "--",
        }
    }
}

/// One check's outcome. Construct via [`CheckResult::pass`],
/// [`CheckResult::warn`], [`CheckResult::fail`], or
/// [`CheckResult::skip`] — direct construction is not allowed so the
/// "PASS-with-hint" anti-state is unrepresentable. `id` is the stable
/// machine-readable key documented in `docs/specs/doctor.md` §JSON
/// output; reserved for v0.2 `--json` consumers but populated now so
/// adding a serializer later is purely additive.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CheckResult {
    pub(crate) severity: Severity,
    pub(crate) id: &'static str,
    pub(crate) label: String,
    /// Renders as an indented second line. PASS constructors don't
    /// accept this; non-PASS constructors require it.
    pub(crate) hint: Option<String>,
}

impl CheckResult {
    /// PASS check. Hints are not accepted — there's nothing to remediate.
    #[must_use]
    pub fn pass(id: &'static str, label: impl Into<String>) -> Self {
        Self {
            severity: Severity::Pass,
            id,
            label: label.into(),
            hint: None,
        }
    }

    /// WARN check. `hint` is required so the user has something
    /// actionable to read on the indented second line.
    #[must_use]
    pub fn warn(id: &'static str, label: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            id,
            label: label.into(),
            hint: Some(hint.into()),
        }
    }

    /// FAIL check. `hint` is required.
    #[must_use]
    pub fn fail(id: &'static str, label: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            severity: Severity::Fail,
            id,
            label: label.into(),
            hint: Some(hint.into()),
        }
    }

    /// SKIP check. `reason` explains why the check didn't run (e.g.
    /// "config not loaded", "no plugins configured"); rendered on the
    /// indented second line.
    #[must_use]
    pub fn skip(id: &'static str, label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            severity: Severity::Skip,
            id,
            label: label.into(),
            hint: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// Stable machine-readable identifier (e.g. `"env.stdout_tty"`).
    /// Reserved for v0.2 `--json` consumers and structured logging;
    /// not surfaced in the human renderer.
    #[must_use]
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// Human-readable label rendered next to the severity glyph.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Remediation hint (for WARN / FAIL) or skip reason (for SKIP),
    /// or `None` for PASS. Rendered as an indented second line.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
}

/// One named group of checks, e.g. `"Environment"`, `"Config"`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Category {
    pub name: &'static str,
    pub checks: Vec<CheckResult>,
}

impl Category {
    #[must_use]
    pub fn new(name: &'static str, checks: Vec<CheckResult>) -> Self {
        Self { name, checks }
    }
}

/// Aggregated severity histogram, one count per [`Severity`] variant.
/// Named fields prevent positional-destructure mistakes when the
/// renderer / JSON serializer reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SummaryCounts {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub skip: usize,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Report {
    pub linesmith_version: &'static str,
    pub categories: Vec<Category>,
}

impl Report {
    /// Build a `Report`. The struct is `#[non_exhaustive]` so
    /// struct-literal construction is blocked downstream; integration
    /// tests and any future out-of-crate consumers go through this so
    /// new fields land in one place.
    #[must_use]
    pub fn new(linesmith_version: &'static str, categories: Vec<Category>) -> Self {
        Self {
            linesmith_version,
            categories,
        }
    }

    /// Any FAIL → 1, otherwise 0. Usage errors (bad flags) are handled
    /// by the parser and never reach this function — don't add a `2`
    /// branch here.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        if self
            .categories
            .iter()
            .flat_map(|c| &c.checks)
            .any(|c| c.severity == Severity::Fail)
        {
            1
        } else {
            0
        }
    }

    #[must_use]
    pub fn summary_counts(&self) -> SummaryCounts {
        let mut counts = SummaryCounts::default();
        for c in self.categories.iter().flat_map(|c| &c.checks) {
            match c.severity {
                Severity::Pass => counts.pass += 1,
                Severity::Warn => counts.warn += 1,
                Severity::Fail => counts.fail += 1,
                Severity::Skip => counts.skip += 1,
            }
        }
        counts
    }
}

/// Render mode for the report. `Plain` swaps Unicode glyphs for ASCII
/// and uses an ASCII summary separator.
///
/// **Plain-mode caveat:** the renderer guarantees no Unicode bytes in
/// the strings *it* emits (glyphs, separators, fixed labels). User-
/// supplied `label` and `hint` strings (paths like `~/café/config`,
/// gix branch names, parser errors) pass through verbatim. CI scripts
/// that need byte-clean ASCII should ASCII-fold their environment, not
/// rely on `--plain` to do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderMode {
    Default,
    Plain,
}

/// Render `report` to `out`. Tree-style: header + version, then each
/// category as a name line followed by indented checks. Non-PASS
/// checks emit an indented hint/reason line.
///
/// On I/O error, partial output may already have been flushed —
/// including a missing `Exit:` line. Callers that parse doctor output
/// must treat a truncated report (no `Exit:` line) as "I/O failed
/// mid-render," not as a successful run.
///
/// # Errors
///
/// Returns the first `io::Error` from a `writeln!` to `out`.
pub fn render(out: &mut dyn Write, report: &Report, mode: RenderMode) -> std::io::Result<()> {
    writeln!(out, "linesmith doctor (v{})", report.linesmith_version)?;
    for category in &report.categories {
        writeln!(out)?;
        writeln!(out, "{}", category.name)?;
        for check in &category.checks {
            let glyph = match mode {
                RenderMode::Default => check.severity.unicode_glyph(),
                RenderMode::Plain => check.severity.ascii_glyph(),
            };
            writeln!(out, "  {glyph} {}", check.label)?;
            if let Some(hint) = &check.hint {
                writeln!(out, "    -> {hint}")?;
            }
        }
    }
    writeln!(out)?;
    let counts = report.summary_counts();
    let sep = match mode {
        RenderMode::Default => "·",
        RenderMode::Plain => "/",
    };
    writeln!(
        out,
        "Summary: {} PASS {sep} {} WARN {sep} {} FAIL {sep} {} SKIP",
        counts.pass, counts.warn, counts.fail, counts.skip,
    )?;
    writeln!(out, "Exit: {}", report.exit_code())?;
    Ok(())
}

/// One environment variable's read state. Distinguishes the three
/// outcomes a check has to remediate differently: not present (set
/// it), set-to-empty (also set it), set-but-non-UTF-8 (the hint
/// "set $X" would be wrong — `$X` *is* set, it's just unreadable).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EnvVarState {
    Unset,
    Set(String),
    /// Variable is set but the value contains bytes that aren't valid
    /// UTF-8. Carries a lossy preview so the hint can quote the
    /// actual offending value rather than just say "missing".
    NonUtf8(String),
}

impl EnvVarState {
    fn snapshot(name: &str) -> Self {
        match std::env::var(name) {
            Ok(s) => Self::Set(s),
            Err(std::env::VarError::NotPresent) => Self::Unset,
            Err(std::env::VarError::NotUnicode(raw)) => {
                Self::NonUtf8(raw.to_string_lossy().into_owned())
            }
        }
    }

    /// Convenience accessor: `Some(s)` only when the variable is set
    /// AND non-empty AND valid UTF-8. Centralizes the
    /// `Some(s) if !s.is_empty()` predicate that every consumer would
    /// otherwise duplicate.
    #[must_use]
    pub fn nonempty(&self) -> Option<&str> {
        match self {
            Self::Set(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
}

/// State of `~/.claude/` per spec §Claude Code "directory" row.
#[derive(Debug)]
#[non_exhaustive]
pub enum ClaudeDirState {
    Ok,
    /// `metadata()` failed with `PermissionDenied`.
    PermissionDenied {
        message: String,
    },
    NotADirectory,
    Missing,
    /// Unexpected I/O error from `metadata()`. `EINTR` (transient)
    /// lands here too; doctor renders WARN so a re-run can clear.
    OtherIo {
        message: String,
    },
}

/// State of `~/.claude.json` per spec §Claude Code "parses" row.
#[derive(Debug)]
#[non_exhaustive]
pub enum ClaudeJsonState {
    /// `oauthAccount` key is present. Per spec wording "block
    /// present", any value (including `null`) counts — the WARN
    /// row triggers on the *key* being absent, not the value.
    Ok,
    NoOauthAccount,
    /// File exceeds the spec's 2 MB read cap (see
    /// `docs/specs/doctor.md` §Edge cases). Spec rules a 500 MB
    /// `~/.claude.json` is pathological / corrupt and the doctor
    /// must FAIL fast rather than spend memory + time parsing it.
    /// `actual_bytes` is the on-disk size for the diagnostic hint.
    TooLarge {
        actual_bytes: u64,
    },
    /// `serde_json::Error::Display` already includes the `at line N
    /// column M` suffix; surface the message verbatim so the spec
    /// hint stays actionable.
    ParseError {
        message: String,
    },
    Missing,
    IoError {
        message: String,
    },
}

/// State of `~/.claude/sessions/` per spec §Claude Code "Recent
/// sessions recorded" row.
#[derive(Debug)]
#[non_exhaustive]
pub enum ClaudeSessionsState {
    HasFiles { count: usize },
    Empty,
    Missing,
    NotADirectory,
    IoError { message: String },
}

/// Filesystem-side state of the Claude Code integration. Wrapped in
/// `Option` on the snapshot because the three filesystem checks all
/// depend on a resolvable `$HOME`; the binary-presence check
/// (`which claude`) walks `$PATH` and stays meaningful regardless.
#[derive(Debug)]
#[non_exhaustive]
pub struct ClaudeHomeState {
    pub dir: ClaudeDirState,
    pub claude_json: ClaudeJsonState,
    pub sessions: ClaudeSessionsState,
}

/// Snapshot of every input the Claude Code category needs. Built
/// once at the call boundary so checks stay pure.
#[derive(Debug)]
#[non_exhaustive]
pub struct DoctorClaudeCodeSnapshot {
    /// First entry in `$PATH` containing an executable named
    /// `claude` (or `claude.exe` on Windows). `None` when the
    /// binary isn't on `$PATH`.
    pub binary_path: Option<PathBuf>,
    /// Whether `$PATH` itself was readable. Carried separately so
    /// the binary check can distinguish "no claude on PATH" from
    /// "couldn't read PATH at all" — the user-facing remediation
    /// is the same, but the diagnostic is different.
    pub path_env: EnvVarState,
    /// Filesystem-side state. `None` when `$HOME` is unresolved;
    /// the dir / json / sessions checks then render SKIP.
    pub home_state: Option<ClaudeHomeState>,
}

/// Lossy summary of `CredentialError` for the doctor snapshot. The
/// raw `CredentialError` carries `serde_json::Error` whose Display
/// impl can include token bytes (parser context shows neighboring
/// JSON); doctor snapshots store only the variant tag + path so a
/// future renderer change can't accidentally leak.
#[derive(Debug)]
#[non_exhaustive]
pub enum CredentialErrorSummary {
    NoCredentials,
    SubprocessFailed { message: String },
    IoError { path: PathBuf, message: String },
    ParseError { path: PathBuf },
    MissingField { path: PathBuf },
    EmptyToken { path: PathBuf },
}

/// Resolved credentials projected to the fields doctor needs. The
/// raw token NEVER reaches this struct — only the cascade source +
/// scope list.
#[derive(Debug)]
#[non_exhaustive]
pub struct CredentialsSummary {
    pub source: crate::data_context::credentials::CredentialSource,
    pub scopes: Vec<String>,
}

/// Snapshot of the credentials cascade. Wraps the result of
/// `resolve_credentials()` minus the bearer token.
#[derive(Debug)]
#[non_exhaustive]
pub enum DoctorCredentialsSnapshot {
    /// Credentials resolved cleanly; checks render PASS.
    Resolved(CredentialsSummary),
    /// Cascade failed. The variant determines which downstream
    /// Credentials checks render FAIL vs SKIP.
    Failed(CredentialErrorSummary),
    /// `$HOME` unresolved AND macOS keychain unavailable —
    /// cascade can't even be attempted. Distinct from `Failed`
    /// because the spec §Cross-category short-circuits says all
    /// file-based Credentials checks SKIP, not FAIL, when $HOME
    /// is missing.
    Unresolvable,
}

/// State of the cache root directory. Determined by read-only stat
/// only: doctor does NOT probe-write. POSIX permission bits and
/// existence checks let us distinguish the "first-run, runtime will
/// create it" case (PASS) from the "absent under a permanently
/// non-creatable path" case (WARN), without the read-only-contract
/// violation an actual probe-write would cause.
#[derive(Debug)]
#[non_exhaustive]
pub enum CacheRootState {
    /// Directory exists and is readable.
    Exists,
    /// Directory doesn't exist and the first existing ancestor is
    /// writable. Expected first-run state — PASS with a "will be
    /// created on first fetch" hint.
    Absent,
    /// Directory doesn't exist AND the first existing ancestor is
    /// read-only (e.g. `XDG_CACHE_HOME` points into a read-only mount
    /// like `/proc`, or the user pointed it at a path under an
    /// ancestor with the user-write bit cleared). WARN: the runtime's
    /// `create_dir_all` will fail on every fetch; without this signal
    /// doctor would report all-green forever for an unfixable setup.
    /// `parent` is the read-only ancestor for the hint.
    AbsentParentReadOnly { parent: PathBuf },
    /// Path exists but as a regular file. (`std::fs::metadata` follows
    /// symlinks, so a symlink resolving to a non-directory lands here
    /// too; broken symlinks resolve to `Absent` via the NotFound arm.)
    /// Real misconfiguration — WARN so the user knows to remove it
    /// before runtime tries to use the path.
    NotADirectory,
    /// `metadata()` failed for a reason other than NotFound (e.g.
    /// permission denied on an ancestor). `message` carries the
    /// rendered `io::Error` so the hint can name the cause.
    Unreadable { message: String },
    /// `$HOME` and `$XDG_CACHE_HOME` both unresolvable; no cache
    /// path could be derived.
    Unresolved,
}

/// State of `usage.json` per spec §Cache "shape current" row.
#[derive(Debug)]
#[non_exhaustive]
pub enum UsageJsonState {
    /// File missing — first-run state, treated as PASS (the next
    /// fetch creates it). Distinct from the spec's PASS row but
    /// matches the spirit ("safe to ignore; next fetch rewrites").
    Missing,
    /// File parses as `CachedUsage`, schema matches, and
    /// `cached_at <= now`. Matches the runtime cache reader's
    /// `Some(entry)` path.
    Current { schema_version: u32 },
    /// File present with a different `schema_version` than the
    /// current code expects.
    Stale { schema_version: u32 },
    /// Parsed cleanly but `cached_at` is in the future. The
    /// runtime reader treats this as a cache miss (clock skew);
    /// doctor surfaces it so the user can fix NTP rather than
    /// puzzle over a stale-data segment.
    FutureTimestamp,
    /// Read or parse failed in a way that doesn't surface to the
    /// runtime cache reader (which silently treats these as a miss).
    /// Doctor still reports them so the user knows about latent
    /// corruption.
    Unreadable { message: String },
}

/// State of `usage.lock` per spec §Cache "Lock file is fresh" row.
#[derive(Debug)]
#[non_exhaustive]
pub enum LockState {
    /// File absent. No rate-limit backoff is currently active.
    Absent,
    /// Lock present and `blocked_until > now`.
    Active { blocked_until_secs: i64 },
    /// Lock present but `blocked_until` is in the past — the user
    /// can `rm` to clear it.
    Stale { blocked_until_secs: i64 },
    /// Read failed for a reason other than absence.
    Unreadable { message: String },
}

/// Snapshot of cache state — root dir + usage.json + lock.
#[derive(Debug)]
#[non_exhaustive]
pub struct DoctorCacheSnapshot {
    /// Resolved cache root path, or `None` when the cascade
    /// (`$XDG_CACHE_HOME`, `$HOME/.cache`) yielded nothing.
    pub root_path: Option<PathBuf>,
    pub root: CacheRootState,
    pub usage_json: UsageJsonState,
    pub lock: LockState,
}

/// Outcome of one rate-limit-endpoint probe. Doctor snapshots a
/// single dry-run probe (no cache mutation) at `from_process` time.
#[derive(Debug)]
#[non_exhaustive]
pub enum EndpointProbeOutcome {
    /// `200 OK` and the body deserialized into the expected shape
    /// (with no unexpected forward-compat keys).
    Ok,
    /// `200 OK` and shape OK, but `elapsed >= 2s`.
    Slow,
    /// Body parsed into `UsageApiResponse` but had codenamed /
    /// unknown top-level keys (forward-compat keys Anthropic
    /// added without notice).
    UnexpectedShape { extra_keys: Vec<String> },
    /// `200 OK` but body didn't deserialize — endpoint shape changed.
    ParseError,
    /// Transport-level failure: DNS, TLS, connect/read timeout,
    /// captive portal, proxy refusal.
    TransportError,
    /// `4xx` other than `429` (e.g. `401` revoked token, `403`
    /// scope missing).
    BadStatus { status: u16 },
    /// `429 Too Many Requests`. `retry_after_secs` is the parsed
    /// `Retry-After` header value, or `None` when the header was
    /// absent.
    RateLimited { retry_after_secs: Option<u64> },
}

/// Snapshot of the rate-limit endpoint probe. `probe = None`
/// when the probe couldn't even be attempted (no token, etc.).
/// `credentials_vanished = true` flags the rare race where the
/// snapshot's first `resolve_credentials()` call succeeded but
/// the probe's second call failed (keychain locked between, file
/// removed, login expired); the orchestrator renders a distinct
/// SKIP reason so the user doesn't read "Resolved" alongside
/// "endpoint probe not attempted".
#[derive(Debug)]
#[non_exhaustive]
pub struct DoctorEndpointSnapshot {
    pub probe: Option<EndpointProbe>,
    pub credentials_vanished: bool,
}

#[derive(Debug)]
#[non_exhaustive]
pub struct EndpointProbe {
    pub elapsed_ms: u128,
    pub outcome: EndpointProbeOutcome,
}

/// Snapshot of plugin discovery for the Plugins category.
///
/// Built by delegating to the runtime predicate
/// (`PluginRegistry::load_with_xdg`) — the same helper the live
/// segment loader uses — so the doctor can never report PASS for
/// a plugin set the runtime would silently reject. See
/// the doctor-runtime parity rule.
#[derive(Debug)]
#[non_exhaustive]
pub enum DoctorPluginsSnapshot {
    /// No plugin sources to scan. Either no `plugin_dirs` in the
    /// loaded config AND no `$XDG_CONFIG_HOME/linesmith/segments/`
    /// on disk, OR `$HOME` is unresolved and no XDG override
    /// exists. Per spec §Edge cases this SKIPs all 4 plugin checks.
    NoSources,
    /// Discovery ran. May have zero plugins (vacuous PASS) or load
    /// errors classified at render time per `PluginError` variant.
    Discovered(PluginsRegistrySummary),
}

/// Result of one `PluginRegistry::load_with_xdg` pass — the
/// runtime's predicate, lifted into a snapshot for the Plugins
/// checks.
#[derive(Debug)]
#[non_exhaustive]
pub struct PluginsRegistrySummary {
    /// Number of plugins that compiled successfully (one render
    /// each is what the runtime would do).
    pub compiled_count: usize,
    /// Load-time errors (`Compile`, `MalformedDataDeps`,
    /// `UnknownDataDep`, `IdCollision`). Render-time errors
    /// (Runtime / Timeout / ResourceExceeded / MalformedReturn)
    /// don't surface here — the doctor doesn't render plugins.
    pub errors: Vec<crate::plugins::errors::PluginError>,
}

/// Snapshot of `gix::discover(cwd)` outcome for the Git category.
///
/// Built by delegating to `data_context::git::resolve_repo` — the
/// same predicate the runtime's `git_*` segments consume. Every
/// state the runtime distinguishes (NotInRepo / Repo / corrupt)
/// surfaces here verbatim.
#[derive(Debug)]
#[non_exhaustive]
pub enum DoctorGitSnapshot {
    /// `gix::discover(cwd)` returned `Ok(None)` — the user's cwd
    /// is not inside a repo. Spec gates the severity on whether
    /// any `git_*` segment is enabled in the config.
    NotInRepo,
    /// Repo resolved cleanly; HEAD + RepoKind are populated.
    Repo(GitContextSummary),
    /// `gix::discover` or `resolve_head` failed (corrupt repo,
    /// permission error, ceiling-dir misconfig).
    Failed { message: String },
}

/// Subset of `data_context::git::GitContext` the doctor renders.
/// Drops the lazy `dirty` / `upstream` cells (doctor doesn't need
/// to walk the full status) and the underlying `gix::Repository`
/// handle (not Send + Sync, would prevent the snapshot from being
/// freely shared).
#[derive(Debug)]
#[non_exhaustive]
pub struct GitContextSummary {
    pub repo_path: PathBuf,
    pub repo_kind: crate::data_context::git::RepoKind,
    pub head: crate::data_context::git::Head,
}

/// Outcome of probing the GitHub releases API for the latest
/// linesmith release. Per spec §Self the severity mapping is
/// `Latest` → PASS and every other variant → WARN; never FAIL, so an
/// offline doctor run still exits 0. The four-variant split lets the
/// renderer pick a hint targeted at the actual failure mode rather
/// than a generic "couldn't check upstream".
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum DoctorUpdateProbe {
    /// Running on the latest release.
    Latest,
    /// A newer release is available upstream. `latest` is the upstream
    /// `tag_name` (sanitized: control bytes stripped, length capped at
    /// 64 chars) so the renderer can surface what GitHub shipped
    /// without re-canonicalizing.
    Newer { latest: String },
    /// Transport-level failure: DNS, connect, read timeout, proxy
    /// refusal, captive portal, non-2xx HTTP status, or response body
    /// exceeding `UPDATE_PROBE_MAX_BYTES`. `message` is clamped to one
    /// line + 200 chars to bound diagnostic strings (which can echo
    /// body fragments via `io::Error` / `ureq::Error`).
    TransportError { message: String },
    /// 2xx response whose body didn't carry the expected `tag_name`
    /// shape (GitHub API change, MITM proxy, mixed-parseability where
    /// one side parses as semver and the other doesn't). Distinct
    /// from `TransportError` so the WARN hint can point at "GitHub
    /// API shape changed" rather than "no network". `message` is
    /// clamped the same way as `TransportError`.
    ParseError { message: String },
}

/// Outcome of attempting to read + parse the resolved config file.
/// Snapshot-shaped: every variant carries enough state for the Config
/// checks to render PASS/WARN/FAIL/SKIP without re-touching the
/// filesystem or re-parsing TOML.
#[derive(Debug)]
#[non_exhaustive]
pub enum ConfigReadOutcome {
    /// No source resolved a config path. Distinct from `NotFound`
    /// because the cascade itself failed (e.g. `$HOME` unset and no
    /// `--config` / `XDG_CONFIG_HOME` / `LINESMITH_CONFIG`); a
    /// `NotFound` means the cascade *did* land somewhere but nothing
    /// was on disk.
    Unresolved,
    /// Resolved path doesn't exist. `explicit` is `true` when the
    /// path came from `--config` or `LINESMITH_CONFIG` (user named
    /// it directly) so the discovery check can hint differently than
    /// for the implicit XDG fallback.
    NotFound { path: PathBuf, explicit: bool },
    /// `fs::read_to_string` failed for a reason other than NotFound
    /// (e.g. permission denied). `message` carries the rendered
    /// `io::Error` so the hint can surface the actual cause —
    /// "permission denied" vs "is a directory" need different fixes.
    IoError { path: PathBuf, message: String },
    /// TOML parse failed. `message` is the rendered `toml::de::Error`
    /// which already includes line/col + the offending token. The
    /// renderer emits it verbatim; callers don't re-extract spans.
    ParseError { path: PathBuf, message: String },
    /// Read + parsed successfully. `Config` is boxed because it
    /// dwarfs the other variants (~260 bytes vs. ~48); inlining it
    /// would bloat every snapshot, including the common-case
    /// `NotFound`/`Unresolved` variants the first-run user hits.
    Loaded { path: PathBuf, config: Box<Config> },
}

/// Stat result for one entry in `config.plugin_dirs`. Captured at
/// snapshot time so the plugin-dirs check is pure: it iterates the
/// vec and renders, never touching the filesystem itself.
#[derive(Debug)]
#[non_exhaustive]
pub struct PluginDirStatus {
    pub path: PathBuf,
    pub state: PluginDirState,
}

/// Disposition of one plugin directory. `Missing` and `NotADirectory`
/// are WARN territory ("user typo'd a path"); `PermissionDenied` and
/// `OtherIo` are FAIL territory ("the user can't fix this from
/// config alone"). The split keeps the spec's WARN-vs-FAIL contract
/// explicit at the data layer instead of in the renderer.
#[derive(Debug)]
#[non_exhaustive]
pub enum PluginDirState {
    Ok,
    Missing,
    NotADirectory,
    PermissionDenied { message: String },
    OtherIo { message: String },
}

/// Snapshot of the config-resolution + read + plugin-dir-stat
/// pipeline. Lives on [`DoctorEnv`] so a test fixture can express
/// "config loaded fine" vs "parse error" vs "permission denied" by
/// constructing the variant directly, without writing files to a
/// tempdir.
#[derive(Debug)]
#[non_exhaustive]
pub struct DoctorConfigSnapshot {
    /// The CLI-supplied `--config` path, if any. Carried alongside
    /// `resolved` so a snapshot is self-describing for tests and so
    /// future checks can distinguish `--config` from `LINESMITH_CONFIG`
    /// when both could plausibly land on the same `resolved.path`.
    pub cli_override: Option<PathBuf>,
    /// Resolved config path per the cascade in
    /// `crate::config::resolve_config_path`, or `None` when no
    /// source resolved.
    pub resolved: Option<ConfigPath>,
    /// Outcome of attempting to read + parse `resolved`.
    pub read: ConfigReadOutcome,
    /// One entry per `[plugin_dirs]` element in the loaded config.
    /// Empty when no config loaded or `plugin_dirs` is empty.
    pub plugin_dirs: Vec<PluginDirStatus>,
    /// Every segment id known to the runtime: built-ins plus the
    /// `const ID` of every successfully-compiled plugin `.rhai`.
    /// Snapshotting prevents the doctor from WARNing on plugin
    /// segments that load fine — the spec hint "install the plugin
    /// that provides it" is only correct when no plugin is providing
    /// it. Built-ins are always present; plugin ids are added when
    /// `read = Loaded` and discovery succeeds.
    pub known_segment_ids: BTreeSet<String>,
    /// Every theme name known to the runtime: built-ins plus every
    /// `*.toml` loaded from the user themes directory. Same reason
    /// as `known_segment_ids` — `theme = "neon"` from a valid user
    /// theme should not WARN.
    pub known_theme_names: BTreeSet<String>,
}

impl DoctorConfigSnapshot {
    /// Built-in segment ids as a fresh `BTreeSet`. Both the
    /// production snapshot and test fixtures seed `known_segment_ids`
    /// from this so the two views can't drift.
    #[must_use]
    pub fn built_in_segment_ids() -> BTreeSet<String> {
        crate::segments::BUILT_IN_SEGMENT_IDS
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    /// Built-in theme names as a fresh `BTreeSet`.
    #[must_use]
    pub fn built_in_theme_names() -> BTreeSet<String> {
        crate::theme::builtin_names().map(str::to_string).collect()
    }

    /// Baseline "config loaded fine" fixture. `cfg(test)` keeps it
    /// out of the public API.
    #[cfg(test)]
    fn healthy() -> Self {
        Self {
            cli_override: None,
            resolved: Some(ConfigPath {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                explicit: false,
            }),
            read: ConfigReadOutcome::Loaded {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                config: Box::new(Config::default()),
            },
            plugin_dirs: Vec::new(),
            known_segment_ids: Self::built_in_segment_ids(),
            known_theme_names: Self::built_in_theme_names(),
        }
    }
}

/// Snapshot of the process state the doctor inspects, taken once at
/// the call boundary and handed to [`build_report`]. Snapshotting
/// keeps checks pure and tests hermetic — no test races the live env
/// or sees mutations from a parallel test.
///
/// Deliberately does NOT derive `Clone`: `current_exe` carries an
/// `io::Error` which isn't `Clone`, and rebuilding the snapshot
/// (test fixture or `from_process()`) is the right pattern when a
/// caller wants a fresh one.
#[derive(Debug)]
#[non_exhaustive]
pub struct DoctorEnv {
    /// Raw `$HOME` env var. NOT the `dirs::home_dir()` resolved path
    /// — Unix `dirs` falls back to passwd entries which this snapshot
    /// does not capture. Slices that need the resolved home directory
    /// (e.g. Config) compute it themselves.
    pub home_env: EnvVarState,
    pub xdg_config_home: EnvVarState,
    pub xdg_cache_home: EnvVarState,
    /// `$LINESMITH_CONFIG` override, second in the config-path
    /// cascade after `--config`. See `crate::config::resolve_config_path`.
    pub linesmith_config: EnvVarState,
    pub term: EnvVarState,
    pub colorterm: EnvVarState,
    pub no_color: bool,
    /// `Ok(path)` when `std::env::current_exe()` succeeds; the error
    /// is preserved (rather than collapsed to `None`) so the binary-
    /// path check can render the actual cause — "permission denied"
    /// vs "broken symlink" vs "/proc unavailable" all need different
    /// remediation hints.
    pub current_exe: Result<PathBuf, std::io::Error>,
    pub stdout_is_terminal: bool,
    pub terminal_width_cells: Option<u16>,
    /// Snapshot of the config-resolution pipeline. Read + parse +
    /// plugin-dir stats happen at snapshot time so the Config-category
    /// checks render purely from this struct.
    pub config: DoctorConfigSnapshot,
    /// Snapshot of the Claude Code integration state — `which
    /// claude`, `~/.claude/`, `~/.claude.json`, and the sessions
    /// directory.
    pub claude_code: DoctorClaudeCodeSnapshot,
    /// Snapshot of the OAuth credentials cascade.
    pub credentials: DoctorCredentialsSnapshot,
    /// Snapshot of the OAuth-usage cache (root dir, `usage.json`,
    /// `usage.lock`).
    pub cache: DoctorCacheSnapshot,
    /// Snapshot of one dry-run probe against the rate-limit
    /// endpoint. `None` when the probe was skipped (no token,
    /// `$HOME` unresolved, etc.).
    pub endpoint: DoctorEndpointSnapshot,
    /// Snapshot of the plugin discovery + compile pass that the
    /// runtime would otherwise run on segment load. Built once
    /// alongside `config.known_segment_ids` so the doctor compiles
    /// each plugin only once.
    pub plugins: DoctorPluginsSnapshot,
    /// Snapshot of `gix::discover(cwd)` for the Git category.
    pub git: DoctorGitSnapshot,
    /// Snapshot of one GitHub releases API probe for the Self
    /// category's "update available" check. Always populated by
    /// `from_process` (no skip path — the spec says always-run);
    /// transport / parse failures land as `TransportError` /
    /// `ParseError` variants per spec WARN routing.
    pub update_probe: DoctorUpdateProbe,
    /// Build-time SHA captured via
    /// `option_env!("LINESMITH_BUILD_SHA")`. `None` in dev / `cargo
    /// build` flows where no release pipeline set the env var;
    /// `Some(sha)` when cargo-dist or a CI script provided it. The
    /// `self.binary_integrity` check PASSes on `Some` (binary traces
    /// to a known commit), WARNs on `None` (built from a non-canonical
    /// source — possibly fine, possibly tampered).
    pub binary_build_sha: Option<&'static str>,
}

impl DoctorEnv {
    /// Snapshot the live process env, including the config-resolution
    /// pipeline. Only the binary entry should call this; tests and any
    /// non-binary caller construct `DoctorEnv` manually to stay
    /// hermetic.
    ///
    /// `cli_config_override` is the `--config <PATH>` value (top of
    /// the config-path cascade). Threading it through the snapshot —
    /// rather than the action layer applying it after — keeps the
    /// Config checks pure: every input they need is on `DoctorEnv`.
    #[must_use]
    pub fn from_process(cli_config_override: Option<PathBuf>) -> Self {
        let home_env = EnvVarState::snapshot("HOME");
        let xdg_config_home = EnvVarState::snapshot("XDG_CONFIG_HOME");
        let linesmith_config = EnvVarState::snapshot("LINESMITH_CONFIG");
        let resolved = crate::config::resolve_config_path(
            cli_config_override.clone(),
            linesmith_config.nonempty(),
            xdg_config_home.nonempty(),
            home_env.nonempty(),
        );
        let read = resolved
            .as_ref()
            .map_or(ConfigReadOutcome::Unresolved, read_config_at);
        let plugin_dirs = match &read {
            ConfigReadOutcome::Loaded { config, .. } => stat_plugin_dirs(&config.plugin_dirs),
            _ => Vec::new(),
        };
        // Build the runtime XdgEnv once and reuse it across every
        // runtime predicate doctor calls — eliminates drift if
        // XdgEnv ever picks up new fields (e.g. XDG_DATA_HOME).
        //
        // Both doctor and cli land in `XdgEnv::from_os_options`,
        // which is the single empty-string filter. Non-UTF-8 was
        // already dropped upstream by both paths, but for different
        // reasons: cli via `std::env::var().ok()` (UTF-8-strict),
        // doctor via `EnvVarState::nonempty()` (which collapses
        // `NonUtf8 → None`). Net effect is parity today; if either
        // path ever needs to preserve non-UTF-8 bytes, both have
        // to change together.
        let runtime_xdg_env = crate::data_context::xdg::XdgEnv::from_os_options(
            None,
            xdg_config_home.nonempty().map(std::ffi::OsString::from),
            home_env.nonempty().map(std::ffi::OsString::from),
        );
        let (known_segment_ids, plugins) = snapshot_plugins(&read, &runtime_xdg_env);
        let known_theme_names = collect_known_theme_names(&runtime_xdg_env);
        let config = DoctorConfigSnapshot {
            cli_override: cli_config_override,
            resolved,
            read,
            plugin_dirs,
            known_segment_ids,
            known_theme_names,
        };
        let path_env = EnvVarState::snapshot("PATH");
        let claude_code = DoctorClaudeCodeSnapshot {
            binary_path: find_claude_binary(path_env.nonempty()),
            home_state: home_env
                .nonempty()
                .map(|h| snapshot_claude_home(Path::new(h))),
            path_env,
        };
        let xdg_cache_home = EnvVarState::snapshot("XDG_CACHE_HOME");
        // Build the cascade env via `var_os` (preserves non-UTF-8
        // bytes; Unix paths are byte-strings) rather than threading
        // the EnvVarState lossy preview through. Without this, doctor
        // would falsely report `Unresolvable` for any user with a
        // non-UTF-8 $HOME / $XDG_CONFIG_HOME / $CLAUDE_CONFIG_DIR
        // even though the runtime cascade resolves them correctly.
        let credentials = snapshot_credentials(
            &crate::data_context::credentials::FileCascadeEnv::from_process_env(),
        );
        let cache = snapshot_cache(&xdg_cache_home, &home_env);
        // Per spec §Cross-category short-circuits: OAuth token
        // missing → endpoint SKIP. The probe runs only when
        // credentials resolved cleanly. Production timeout is 2s
        // per spec §Timing.
        let endpoint = match &credentials {
            DoctorCredentialsSnapshot::Resolved(_) => probe_endpoint_via_ureq(),
            _ => DoctorEndpointSnapshot {
                probe: None,
                credentials_vanished: false,
            },
        };
        let git = snapshot_git();
        let update_probe = snapshot_update_probe();
        Self {
            home_env,
            xdg_config_home,
            xdg_cache_home,
            linesmith_config,
            term: EnvVarState::snapshot("TERM"),
            colorterm: EnvVarState::snapshot("COLORTERM"),
            no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            current_exe: std::env::current_exe(),
            stdout_is_terminal: std::io::stdout().is_terminal(),
            terminal_width_cells: terminal_size::terminal_size()
                .map(|(terminal_size::Width(w), _)| w),
            config,
            claude_code,
            credentials,
            cache,
            endpoint,
            plugins,
            git,
            update_probe,
            // `option_env!` is compile-time; resolves to the value
            // baked at build, or `None` when the env var was unset
            // for that build. cargo-dist / release CI sets it; local
            // `cargo build` does not.
            binary_build_sha: option_env!("LINESMITH_BUILD_SHA"),
        }
    }

    /// Baseline "everything healthy" fixture for tests. Mutate
    /// individual fields to exercise specific check branches.
    /// `cfg(test)` keeps it out of the public API entirely —
    /// external embedders can't fabricate a snapshot that lies
    /// about a real environment.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn healthy() -> Self {
        Self {
            home_env: EnvVarState::Set("/home/user".to_string()),
            xdg_config_home: EnvVarState::Unset,
            xdg_cache_home: EnvVarState::Unset,
            linesmith_config: EnvVarState::Unset,
            term: EnvVarState::Set("xterm-256color".to_string()),
            colorterm: EnvVarState::Set("truecolor".to_string()),
            no_color: false,
            current_exe: Ok(PathBuf::from("/usr/local/bin/linesmith")),
            stdout_is_terminal: true,
            terminal_width_cells: Some(120),
            config: DoctorConfigSnapshot::healthy(),
            claude_code: DoctorClaudeCodeSnapshot {
                binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
                path_env: EnvVarState::Set("/usr/local/bin:/usr/bin".to_string()),
                home_state: Some(ClaudeHomeState {
                    dir: ClaudeDirState::Ok,
                    claude_json: ClaudeJsonState::Ok,
                    sessions: ClaudeSessionsState::HasFiles { count: 3 },
                }),
            },
            credentials: DoctorCredentialsSnapshot::Resolved(CredentialsSummary {
                source: crate::data_context::credentials::CredentialSource::ClaudeLegacy {
                    path: PathBuf::from("/home/user/.claude/.credentials.json"),
                },
                scopes: vec!["user:inference".to_string()],
            }),
            cache: DoctorCacheSnapshot {
                root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
                root: CacheRootState::Exists,
                usage_json: UsageJsonState::Current { schema_version: 1 },
                lock: LockState::Absent,
            },
            endpoint: DoctorEndpointSnapshot {
                probe: Some(EndpointProbe {
                    elapsed_ms: 250,
                    outcome: EndpointProbeOutcome::Ok,
                }),
                credentials_vanished: false,
            },
            // Discovered with zero plugins: vacuous PASS for every
            // Plugins check (no compile failures, no collisions).
            plugins: DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
                compiled_count: 0,
                errors: Vec::new(),
            }),
            git: DoctorGitSnapshot::Repo(GitContextSummary {
                repo_path: PathBuf::from("/home/user/code/project/.git"),
                repo_kind: crate::data_context::git::RepoKind::Main,
                head: crate::data_context::git::Head::Branch("main".to_string()),
            }),
            update_probe: DoctorUpdateProbe::Latest,
            binary_build_sha: Some("abc1234"),
        }
    }
}

/// Build the diagnostic report. Catalog scope is tracked in
/// `docs/specs/doctor.md` §Check catalog.
#[must_use]
pub fn build_report(env: &DoctorEnv) -> Report {
    Report {
        linesmith_version: env!("CARGO_PKG_VERSION"),
        categories: vec![
            environment_category(env),
            config_category(env),
            claude_code_category(env),
            credentials_category(env),
            cache_category(env),
            endpoint_category(env),
            plugins_category(env),
            git_category(env),
            self_category(env),
        ],
    }
}

fn environment_category(env: &DoctorEnv) -> Category {
    Category::new(
        "Environment",
        vec![
            check_stdout_tty(env),
            check_terminal_width(env),
            check_term(env),
            check_no_color(env),
            check_home(env),
        ],
    )
}

fn check_stdout_tty(env: &DoctorEnv) -> CheckResult {
    if env.stdout_is_terminal {
        CheckResult::pass("env.stdout_tty", "Terminal is a tty (stdout fd 1)")
    } else {
        CheckResult::warn(
            "env.stdout_tty",
            "Stdout is not a tty (piped or redirected)",
            "use --plain for CI or log capture",
        )
    }
}

/// Single source for the env.terminal_width and env.term hint
/// strings — the WARN branches share the same remediation, so any
/// future wording change touches one place.
const TERMINAL_WIDTH_HINT: &str = "set $COLUMNS or use --plain; narrow widths may wrap output";
const TERM_HINT: &str = "set TERM=xterm-256color, or accept plain-mode fallback";

fn check_terminal_width(env: &DoctorEnv) -> CheckResult {
    match env.terminal_width_cells {
        Some(0) => CheckResult::warn(
            "env.terminal_width",
            "Terminal reported 0 cells (likely driver or terminfo bug)",
            "set $COLUMNS to override, or report the issue to your terminal emulator",
        ),
        Some(w) if w >= 40 => CheckResult::pass(
            "env.terminal_width",
            format!("Terminal width detected: {w} cells"),
        ),
        Some(w) => CheckResult::warn(
            "env.terminal_width",
            format!("Terminal width is {w} cells (narrow)"),
            TERMINAL_WIDTH_HINT,
        ),
        None => CheckResult::warn(
            "env.terminal_width",
            "Terminal width could not be detected",
            TERMINAL_WIDTH_HINT,
        ),
    }
}

fn check_term(env: &DoctorEnv) -> CheckResult {
    match &env.term {
        EnvVarState::Set(t) if !t.is_empty() && t != "dumb" => {
            CheckResult::pass("env.term", format!("$TERM={t}"))
        }
        EnvVarState::Set(t) if t == "dumb" => {
            CheckResult::warn("env.term", "$TERM=dumb", TERM_HINT)
        }
        EnvVarState::NonUtf8(raw) => CheckResult::warn(
            "env.term",
            format!("$TERM is set but not valid UTF-8 (lossy: {raw:?})"),
            "rewrite $TERM with a UTF-8 value (e.g. xterm-256color)",
        ),
        // Unset OR Set-to-empty.
        _ => CheckResult::warn("env.term", "$TERM is unset", TERM_HINT),
    }
}

fn check_no_color(env: &DoctorEnv) -> CheckResult {
    if env.no_color {
        CheckResult::pass(
            "env.no_color",
            "NO_COLOR is set — colors disabled per user preference",
        )
    } else {
        CheckResult::pass("env.no_color", "NO_COLOR is unset")
    }
}

fn check_home(env: &DoctorEnv) -> CheckResult {
    match &env.home_env {
        EnvVarState::Set(h) if !h.is_empty() => CheckResult::pass("env.home", format!("$HOME={h}")),
        EnvVarState::NonUtf8(raw) => CheckResult::fail(
            "env.home",
            format!("$HOME is set but not valid UTF-8 (lossy: {raw:?})"),
            "rewrite $HOME with a UTF-8 path",
        ),
        // Unset OR Set-to-empty: same remediation either way.
        _ => CheckResult::fail(
            "env.home",
            "$HOME is unset",
            "set $HOME to your user directory",
        ),
    }
}

const CONFIG_DISCOVERED_ID: &str = "config.discovered";
const CONFIG_PARSES_ID: &str = "config.parses";
const CONFIG_SEGMENTS_ID: &str = "config.segments_resolvable";
const CONFIG_THEME_ID: &str = "config.theme_installed";
const CONFIG_PLUGIN_DIRS_ID: &str = "config.plugin_dirs_readable";

fn config_category(env: &DoctorEnv) -> Category {
    // Spec §Cross-category short-circuits has a Config carve-out:
    // when `--config` / `$LINESMITH_CONFIG` / `$XDG_CONFIG_HOME`
    // resolves a path, run the checks against it even if `$HOME` is
    // unresolved. Gating on `$HOME` would silently SKIP a config the
    // user explicitly named. The `Unresolved` arm of the discovery
    // check covers the genuine "no path source" case as a WARN, with
    // a hint pointing at every cascade source.
    let snapshot = &env.config;
    let discovered = check_config_discovered(snapshot);
    let parses = check_config_parses(snapshot);

    // Spec §Cross-category short-circuits: parse failure (or "no
    // config to parse") downgrades the schema-level checks to SKIP.
    // The discovery check itself is independent — a missing file is
    // a WARN, not a propagating failure.
    let downstream_runs = matches!(snapshot.read, ConfigReadOutcome::Loaded { .. });
    let (segments_check, theme_check, plugin_dirs_check) = if downstream_runs {
        (
            check_config_segments(snapshot),
            check_config_theme(snapshot),
            check_config_plugin_dirs(snapshot),
        )
    } else {
        let reason = "config not loaded";
        (
            CheckResult::skip(CONFIG_SEGMENTS_ID, "All referenced segments exist", reason),
            CheckResult::skip(CONFIG_THEME_ID, "Theme is installed", reason),
            CheckResult::skip(CONFIG_PLUGIN_DIRS_ID, "Plugin dirs are readable", reason),
        )
    };

    Category::new(
        "Config",
        vec![
            discovered,
            parses,
            segments_check,
            theme_check,
            plugin_dirs_check,
        ],
    )
}

fn check_config_discovered(snapshot: &DoctorConfigSnapshot) -> CheckResult {
    match &snapshot.read {
        ConfigReadOutcome::Loaded { path, .. } => CheckResult::pass(
            CONFIG_DISCOVERED_ID,
            format!("Config file: {}", path.display()),
        ),
        // Parse failure is the next check's signal; double-reporting
        // it here would clutter the report with a WARN beside the FAIL.
        ConfigReadOutcome::ParseError { path, .. } => CheckResult::pass(
            CONFIG_DISCOVERED_ID,
            format!("Config file: {}", path.display()),
        ),
        ConfigReadOutcome::NotFound { path, explicit } => {
            // Explicit path users named themselves get a FAIL: they
            // asked us to use *this file*, and it isn't there. Falling
            // back to defaults silently would mask a typo. Implicit
            // XDG-cascade misses are the first-run case and stay WARN
            // per spec ("none found, using built-in defaults").
            if *explicit {
                CheckResult::fail(
                    CONFIG_DISCOVERED_ID,
                    format!("Config file not found: {}", path.display()),
                    "create the file, or remove --config / unset $LINESMITH_CONFIG",
                )
            } else {
                CheckResult::warn(
                    CONFIG_DISCOVERED_ID,
                    format!(
                        "No config file at {} (using built-in defaults)",
                        path.display()
                    ),
                    "run `linesmith init` to create a config",
                )
            }
        }
        ConfigReadOutcome::IoError { path, message } => CheckResult::fail(
            CONFIG_DISCOVERED_ID,
            format!("Config file unreadable: {} ({message})", path.display()),
            "check filesystem permissions on the config path",
        ),
        ConfigReadOutcome::Unresolved => CheckResult::warn(
            CONFIG_DISCOVERED_ID,
            "No config path resolved (using built-in defaults)",
            "set $XDG_CONFIG_HOME or $HOME, or pass --config <PATH>",
        ),
    }
}

fn check_config_parses(snapshot: &DoctorConfigSnapshot) -> CheckResult {
    match &snapshot.read {
        ConfigReadOutcome::Loaded { .. } => CheckResult::pass(CONFIG_PARSES_ID, "Config parses"),
        ConfigReadOutcome::ParseError { path, message } => CheckResult::fail(
            CONFIG_PARSES_ID,
            format!("TOML parse error in {}", path.display()),
            // toml::de::Error.to_string() already includes line/col;
            // surfacing it verbatim keeps the spec's "see line/column
            // in the error" hint actionable.
            message.clone(),
        ),
        // No file → no parse to do. The discovery check carries the
        // remediation for these cases; rendering SKIP here keeps the
        // category shape stable (5 checks, every time).
        ConfigReadOutcome::NotFound { .. }
        | ConfigReadOutcome::IoError { .. }
        | ConfigReadOutcome::Unresolved => {
            CheckResult::skip(CONFIG_PARSES_ID, "Config parses", "no config file to parse")
        }
    }
}

fn check_config_segments(snapshot: &DoctorConfigSnapshot) -> CheckResult {
    let ConfigReadOutcome::Loaded { config, .. } = &snapshot.read else {
        // Belt-and-braces: config_category already gates on Loaded.
        // SKIP keeps the user-facing contract stable if a future
        // refactor decouples the gate from this check.
        return CheckResult::skip(
            CONFIG_SEGMENTS_ID,
            "All referenced segments exist",
            "config not loaded",
        );
    };

    let known = &snapshot.known_segment_ids;
    let mut unknown: Vec<String> = Vec::new();
    let mut malformed_lines: Vec<String> = Vec::new();
    if let Some(line) = &config.line {
        for id in &line.segments {
            if !known.contains(id) {
                unknown.push(id.clone());
            }
        }
        // `[line.N]` numbered tables: each value should be a table
        // with a `segments` array of strings. The runtime builder
        // warns on malformed shapes and exits 0; doctor surfaces
        // them as a visible WARN at health-check time so a user
        // fixing config doesn't have to spelunk through a debug log.
        for (key, value) in &line.numbered {
            // `$schema` is the JSON-schema sentinel, not a line index.
            if key == "$schema" {
                continue;
            }
            // `NonZeroU32` rather than `u32` so `[line.0]` lands in
            // the malformed bucket — the hint reads "must be a
            // positive integer" and `0` would otherwise sneak past.
            if key.parse::<NonZeroU32>().is_err() {
                malformed_lines.push(format!("[line.{key}] (key must be a positive integer)"));
                continue;
            }
            let Some(table) = value.as_table() else {
                malformed_lines.push(format!("[line.{key}] (must be a table)"));
                continue;
            };
            match table.get("segments") {
                Some(toml::Value::Array(arr)) => {
                    for item in arr {
                        match item.as_str() {
                            Some(id) if !known.contains(id) => unknown.push(id.to_string()),
                            None => malformed_lines
                                .push(format!("[line.{key}].segments contains a non-string entry")),
                            _ => {}
                        }
                    }
                }
                Some(_) => malformed_lines.push(format!(
                    "[line.{key}].segments (must be an array of strings)"
                )),
                None => malformed_lines.push(format!("[line.{key}] missing `segments` array")),
            }
        }
    }

    if unknown.is_empty() && malformed_lines.is_empty() {
        CheckResult::pass(CONFIG_SEGMENTS_ID, "All referenced segments exist")
    } else {
        let mut details: Vec<String> = Vec::new();
        if !unknown.is_empty() {
            details.push(format!("unknown ids: {}", unknown.join(", ")));
        }
        if !malformed_lines.is_empty() {
            details.push(format!("malformed: {}", malformed_lines.join("; ")));
        }
        CheckResult::warn(
            CONFIG_SEGMENTS_ID,
            format!("Segment references have issues ({})", details.join(" / ")),
            "remove the unknown id, or install the plugin that provides it",
        )
    }
}

fn check_config_theme(snapshot: &DoctorConfigSnapshot) -> CheckResult {
    let ConfigReadOutcome::Loaded { config, .. } = &snapshot.read else {
        return CheckResult::skip(CONFIG_THEME_ID, "Theme is installed", "config not loaded");
    };

    match config.theme.as_deref() {
        // Omitted theme → built-in default applies, which is healthy.
        None => CheckResult::pass(CONFIG_THEME_ID, "Theme: (default)"),
        Some(name) if snapshot.known_theme_names.contains(name) => {
            CheckResult::pass(CONFIG_THEME_ID, format!("Theme: {name}"))
        }
        Some(name) => CheckResult::warn(
            CONFIG_THEME_ID,
            format!("Theme `{name}` is unknown; falling back to default"),
            "run `linesmith themes list` to see available names",
        ),
    }
}

fn check_config_plugin_dirs(snapshot: &DoctorConfigSnapshot) -> CheckResult {
    if snapshot.plugin_dirs.is_empty() {
        return CheckResult::pass(CONFIG_PLUGIN_DIRS_ID, "Plugin dirs: (none configured)");
    }

    let mut perm_errors: Vec<String> = Vec::new();
    let mut other_errors: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut not_dirs: Vec<String> = Vec::new();
    for status in &snapshot.plugin_dirs {
        let path_str = status.path.display().to_string();
        match &status.state {
            PluginDirState::Ok => {}
            PluginDirState::Missing => missing.push(path_str),
            PluginDirState::NotADirectory => not_dirs.push(path_str),
            PluginDirState::PermissionDenied { .. } => perm_errors.push(path_str),
            PluginDirState::OtherIo { .. } => other_errors.push(path_str),
        }
    }

    // FAIL beats WARN: if any dir is genuinely unusable (permission
    // or unexpected I/O), surface that — the user can't ignore it.
    if !perm_errors.is_empty() || !other_errors.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if !perm_errors.is_empty() {
            parts.push(format!("permission denied: {}", perm_errors.join(", ")));
        }
        if !other_errors.is_empty() {
            parts.push(format!("io error: {}", other_errors.join(", ")));
        }
        return CheckResult::fail(
            CONFIG_PLUGIN_DIRS_ID,
            format!("Plugin dirs unreadable ({})", parts.join("; ")),
            "fix permissions, or remove the entry from config.toml plugin_dirs",
        );
    }

    if !missing.is_empty() || !not_dirs.is_empty() {
        let mut parts: Vec<String> = Vec::new();
        if !missing.is_empty() {
            parts.push(format!("missing: {}", missing.join(", ")));
        }
        if !not_dirs.is_empty() {
            parts.push(format!("not a directory: {}", not_dirs.join(", ")));
        }
        return CheckResult::warn(
            CONFIG_PLUGIN_DIRS_ID,
            format!("Plugin dirs have issues ({})", parts.join("; ")),
            "mkdir -p <path> or remove the entry from config.toml plugin_dirs",
        );
    }

    CheckResult::pass(
        CONFIG_PLUGIN_DIRS_ID,
        format!(
            "Plugin dirs: {} configured, all readable",
            snapshot.plugin_dirs.len(),
        ),
    )
}

const CLAUDE_BINARY_ID: &str = "claude.binary_found";
const CLAUDE_DIR_ID: &str = "claude.dir";
const CLAUDE_JSON_ID: &str = "claude.json_parses";
const CLAUDE_SESSIONS_ID: &str = "claude.sessions_recorded";

fn claude_code_category(env: &DoctorEnv) -> Category {
    let snapshot = &env.claude_code;
    let binary = check_claude_binary(snapshot);
    let (dir, claude_json, sessions) = match &snapshot.home_state {
        Some(home) => (
            check_claude_dir(home),
            check_claude_json(home),
            // Spec §Cross-category short-circuits: `~/.claude/`
            // missing → sessions SKIP. Widened beyond Missing to
            // cover every dir state where `read_dir(.../sessions/)`
            // can't produce a meaningful answer; the dir check
            // already carries the right remediation for those.
            match sessions_skip_reason(&home.dir) {
                Some(reason) => {
                    CheckResult::skip(CLAUDE_SESSIONS_ID, "Recent sessions recorded", reason)
                }
                None => check_claude_sessions(home),
            },
        ),
        None => {
            let reason = "$HOME unresolved";
            (
                CheckResult::skip(CLAUDE_DIR_ID, "`~/.claude/` directory", reason),
                CheckResult::skip(CLAUDE_JSON_ID, "`~/.claude.json` parses", reason),
                CheckResult::skip(CLAUDE_SESSIONS_ID, "Recent sessions recorded", reason),
            )
        }
    };
    Category::new("Claude Code", vec![binary, dir, claude_json, sessions])
}

/// SKIP reason for the sessions check when `~/.claude/` itself is
/// in a state that prevents `read_dir(.../sessions/)` from being
/// meaningful, or `None` when the sessions check should run.
fn sessions_skip_reason(dir: &ClaudeDirState) -> Option<&'static str> {
    match dir {
        ClaudeDirState::Missing => Some("`~/.claude/` missing"),
        ClaudeDirState::PermissionDenied { .. } => Some("`~/.claude/` unreadable"),
        ClaudeDirState::NotADirectory => Some("`~/.claude/` is not a directory"),
        ClaudeDirState::OtherIo { .. } => Some("`~/.claude/` stat failed"),
        ClaudeDirState::Ok => None,
    }
}

fn check_claude_binary(snapshot: &DoctorClaudeCodeSnapshot) -> CheckResult {
    if let Some(path) = &snapshot.binary_path {
        return CheckResult::pass(
            CLAUDE_BINARY_ID,
            format!("`claude` binary: {}", path.display()),
        );
    }
    // `$PATH` itself broken (unset / empty / non-UTF-8) is a
    // different remediation from "claude not installed". Without
    // this branch the user reads "install Claude Code" when their
    // shell init wiped PATH.
    if let Some(hint) = path_env_problem_hint(&snapshot.path_env) {
        return CheckResult::fail(
            CLAUDE_BINARY_ID,
            format!(
                "`claude` binary not located ({})",
                path_env_problem(&snapshot.path_env)
            ),
            hint,
        );
    }
    // Spec §Claude Code "binary found": WARN when binary is absent
    // but `~/.claude/` is *present in any form* — Claude Code was
    // installed once and the launcher fell off PATH or the dir got
    // corrupted (the dir's own check renders the right hint for
    // PermissionDenied / NotADirectory / OtherIo). FAIL is reserved
    // for the genuinely-never-installed case.
    let dir_present = snapshot
        .home_state
        .as_ref()
        .is_some_and(|h| !matches!(h.dir, ClaudeDirState::Missing));
    if dir_present {
        CheckResult::warn(
            CLAUDE_BINARY_ID,
            "`claude` binary not on $PATH (but ~/.claude/ exists)",
            "reinstall Claude Code from https://claude.ai/code, or check $PATH",
        )
    } else {
        CheckResult::fail(
            CLAUDE_BINARY_ID,
            "`claude` binary not found and ~/.claude/ missing",
            "install Claude Code from https://claude.ai/code",
        )
    }
}

/// Human-readable `$PATH` problem class for the binary-check label.
fn path_env_problem(path_env: &EnvVarState) -> &'static str {
    match path_env {
        EnvVarState::Unset => "$PATH is unset",
        EnvVarState::Set(s) if s.is_empty() => "$PATH is empty",
        EnvVarState::NonUtf8(_) => "$PATH is not valid UTF-8",
        EnvVarState::Set(_) => "$PATH searched", // reachable only if `nonempty()` is Some
    }
}

/// Hint for the `$PATH`-broken case, or `None` when `$PATH` is
/// usable (binary check should fall through to the install/dir
/// remediation).
fn path_env_problem_hint(path_env: &EnvVarState) -> Option<&'static str> {
    match path_env {
        EnvVarState::Unset => Some("set $PATH to include the directory holding `claude`"),
        EnvVarState::Set(s) if s.is_empty() => {
            Some("set $PATH to include the directory holding `claude`")
        }
        EnvVarState::NonUtf8(_) => {
            Some("rewrite $PATH with valid UTF-8 (check shell init for stray bytes)")
        }
        EnvVarState::Set(_) => None,
    }
}

fn check_claude_dir(home: &ClaudeHomeState) -> CheckResult {
    match &home.dir {
        ClaudeDirState::Ok => CheckResult::pass(CLAUDE_DIR_ID, "`~/.claude/` exists"),
        ClaudeDirState::PermissionDenied { message } => CheckResult::warn(
            CLAUDE_DIR_ID,
            format!("`~/.claude/` exists but is unreadable ({message})"),
            "fix filesystem permissions on ~/.claude/",
        ),
        ClaudeDirState::NotADirectory => CheckResult::warn(
            CLAUDE_DIR_ID,
            "`~/.claude/` exists but is not a directory",
            "remove the file at ~/.claude and let Claude Code recreate the directory",
        ),
        ClaudeDirState::OtherIo { message } => CheckResult::warn(
            CLAUDE_DIR_ID,
            format!("`~/.claude/` stat failed: {message}"),
            "check ~/.claude/ manually; doctor couldn't classify the failure",
        ),
        ClaudeDirState::Missing => CheckResult::fail(
            CLAUDE_DIR_ID,
            "`~/.claude/` directory missing",
            "launch Claude Code at least once to create it",
        ),
    }
}

fn check_claude_json(home: &ClaudeHomeState) -> CheckResult {
    match &home.claude_json {
        ClaudeJsonState::Ok => CheckResult::pass(
            CLAUDE_JSON_ID,
            "`~/.claude.json` parses (oauthAccount present)",
        ),
        ClaudeJsonState::NoOauthAccount => CheckResult::warn(
            CLAUDE_JSON_ID,
            "`~/.claude.json` parses but `oauthAccount` is missing",
            "run `claude` to log in and regenerate the oauthAccount block",
        ),
        ClaudeJsonState::ParseError { message } => CheckResult::fail(
            CLAUDE_JSON_ID,
            "`~/.claude.json` parse error".to_string(),
            // serde_json's Display impl already names line/col;
            // surfacing it verbatim keeps the spec hint actionable.
            message.clone(),
        ),
        ClaudeJsonState::Missing => CheckResult::fail(
            CLAUDE_JSON_ID,
            "`~/.claude.json` missing",
            "run `claude` to log in and regenerate the file",
        ),
        ClaudeJsonState::IoError { message } => CheckResult::fail(
            CLAUDE_JSON_ID,
            format!("`~/.claude.json` unreadable: {message}"),
            "check filesystem permissions on ~/.claude.json",
        ),
        ClaudeJsonState::TooLarge { actual_bytes } => CheckResult::fail(
            CLAUDE_JSON_ID,
            format!(
                "`~/.claude.json` too large to parse ({actual_bytes} bytes; cap is 2 MB)"
            ),
            "file is likely corrupt; back up and remove ~/.claude.json, then re-run `claude` to regenerate",
        ),
    }
}

fn check_claude_sessions(home: &ClaudeHomeState) -> CheckResult {
    match &home.sessions {
        ClaudeSessionsState::HasFiles { count } => CheckResult::pass(
            CLAUDE_SESSIONS_ID,
            format!("{count} recent session(s) in `~/.claude/sessions/`"),
        ),
        ClaudeSessionsState::Empty => CheckResult::warn(
            CLAUDE_SESSIONS_ID,
            "`~/.claude/sessions/` is empty",
            "open a new Claude Code session to populate",
        ),
        ClaudeSessionsState::Missing => CheckResult::fail(
            CLAUDE_SESSIONS_ID,
            "`~/.claude/sessions/` directory missing",
            "open a new Claude Code session to create the directory",
        ),
        ClaudeSessionsState::NotADirectory => CheckResult::warn(
            CLAUDE_SESSIONS_ID,
            "`~/.claude/sessions` exists but is not a directory",
            "remove the file at ~/.claude/sessions and let Claude Code recreate it",
        ),
        ClaudeSessionsState::IoError { message } => CheckResult::warn(
            CLAUDE_SESSIONS_ID,
            format!("`~/.claude/sessions/` read failed: {message}"),
            "check filesystem permissions on ~/.claude/sessions/",
        ),
    }
}

// --- Credentials ---

const CREDS_TOKEN_RESOLVABLE_ID: &str = "creds.token_resolvable";
const CREDS_SOURCE_ATTESTED_ID: &str = "creds.source_attested";
const CREDS_TOKEN_SHAPE_ID: &str = "creds.token_shape_valid";
const CREDS_SCOPES_ID: &str = "creds.scopes_present";

fn credentials_category(env: &DoctorEnv) -> Category {
    let snapshot = &env.credentials;
    Category::new(
        "Credentials",
        vec![
            check_creds_token_resolvable(snapshot),
            check_creds_source_attested(snapshot),
            check_creds_token_shape(snapshot),
            check_creds_scopes(snapshot),
        ],
    )
}

fn check_creds_token_resolvable(snapshot: &DoctorCredentialsSnapshot) -> CheckResult {
    match snapshot {
        DoctorCredentialsSnapshot::Resolved(_) => CheckResult::pass(
            CREDS_TOKEN_RESOLVABLE_ID,
            "OAuth token resolved via cascade",
        ),
        // Spec §Cross-category short-circuits: SKIP all four when
        // the cascade has nothing to attempt — no `$HOME`, no
        // `$XDG_CONFIG_HOME`, no `$CLAUDE_CONFIG_DIR`, AND we're
        // not on macOS (where the keychain is a path-independent
        // source).
        DoctorCredentialsSnapshot::Unresolvable => CheckResult::skip(
            CREDS_TOKEN_RESOLVABLE_ID,
            "OAuth token resolvable",
            "no usable credentials cascade source",
        ),
        DoctorCredentialsSnapshot::Failed(err) => CheckResult::fail(
            CREDS_TOKEN_RESOLVABLE_ID,
            format!("OAuth token cascade failed ({})", creds_error_label(err)),
            "log in to Claude Code to provision a fresh token",
        ),
    }
}

fn check_creds_source_attested(snapshot: &DoctorCredentialsSnapshot) -> CheckResult {
    match snapshot {
        DoctorCredentialsSnapshot::Resolved(s) => CheckResult::pass(
            CREDS_SOURCE_ATTESTED_ID,
            format!("Source: {}", source_label(&s.source)),
        ),
        DoctorCredentialsSnapshot::Unresolvable => CheckResult::skip(
            CREDS_SOURCE_ATTESTED_ID,
            "Token source attested",
            "no usable credentials cascade source",
        ),
        // Path-bearing failure variants (IoError / ParseError /
        // MissingField / EmptyToken) all carry the cascade-winner
        // path. The source IS identifiable even though the file
        // is unreadable / malformed; the resolvable / shape rows
        // own the FAIL signal. Only the no-path variants
        // (NoCredentials, SubprocessFailed) leave the source
        // indeterminate.
        DoctorCredentialsSnapshot::Failed(
            CredentialErrorSummary::IoError { path, .. }
            | CredentialErrorSummary::ParseError { path }
            | CredentialErrorSummary::MissingField { path }
            | CredentialErrorSummary::EmptyToken { path },
        ) => CheckResult::pass(
            CREDS_SOURCE_ATTESTED_ID,
            format!("Source: {} (cascade reached this file)", path.display()),
        ),
        DoctorCredentialsSnapshot::Failed(_) => CheckResult::fail(
            CREDS_SOURCE_ATTESTED_ID,
            "Token source indeterminate",
            "rm any stale credentials file and log in again",
        ),
    }
}

fn check_creds_token_shape(snapshot: &DoctorCredentialsSnapshot) -> CheckResult {
    match snapshot {
        DoctorCredentialsSnapshot::Resolved(_) => {
            CheckResult::pass(CREDS_TOKEN_SHAPE_ID, "Token shape valid")
        }
        DoctorCredentialsSnapshot::Unresolvable => CheckResult::skip(
            CREDS_TOKEN_SHAPE_ID,
            "Token shape valid",
            "no usable credentials cascade source",
        ),
        DoctorCredentialsSnapshot::Failed(
            err @ (CredentialErrorSummary::ParseError { .. }
            | CredentialErrorSummary::MissingField { .. }
            | CredentialErrorSummary::EmptyToken { .. }),
        ) => CheckResult::fail(
            CREDS_TOKEN_SHAPE_ID,
            format!("Token shape invalid: {}", creds_error_label(err)),
            "rerun `claude` to rewrite the credentials file",
        ),
        // No usable file to evaluate; the resolvable check carries
        // the actionable hint.
        DoctorCredentialsSnapshot::Failed(_) => CheckResult::skip(
            CREDS_TOKEN_SHAPE_ID,
            "Token shape valid",
            "no credentials file to inspect",
        ),
    }
}

fn check_creds_scopes(snapshot: &DoctorCredentialsSnapshot) -> CheckResult {
    let scopes = match snapshot {
        DoctorCredentialsSnapshot::Resolved(s) => &s.scopes,
        DoctorCredentialsSnapshot::Unresolvable => {
            return CheckResult::skip(
                CREDS_SCOPES_ID,
                "Required scopes present",
                "no usable credentials cascade source",
            );
        }
        DoctorCredentialsSnapshot::Failed(_) => {
            return CheckResult::skip(
                CREDS_SCOPES_ID,
                "Required scopes present",
                "credentials not loaded",
            );
        }
    };
    if scopes.iter().any(|s| s == "user:inference") {
        CheckResult::pass(
            CREDS_SCOPES_ID,
            format!("Scopes: {} ({} total)", "user:inference", scopes.len()),
        )
    } else {
        CheckResult::fail(
            CREDS_SCOPES_ID,
            "Required scope `user:inference` absent",
            "log in again to refresh scopes",
        )
    }
}

fn creds_error_label(err: &CredentialErrorSummary) -> &'static str {
    match err {
        CredentialErrorSummary::NoCredentials => "no credentials found",
        CredentialErrorSummary::SubprocessFailed { .. } => "keychain subprocess failed",
        CredentialErrorSummary::IoError { .. } => "file unreadable",
        CredentialErrorSummary::ParseError { .. } => "JSON parse error",
        CredentialErrorSummary::MissingField { .. } => "claudeAiOauth block missing",
        CredentialErrorSummary::EmptyToken { .. } => "accessToken empty",
    }
}

fn source_label(source: &crate::data_context::credentials::CredentialSource) -> String {
    use crate::data_context::credentials::CredentialSource;
    match source {
        CredentialSource::MacosKeychainPrimary => "macOS Keychain (primary)".to_string(),
        CredentialSource::MacosKeychainMultiAccount { service, .. } => {
            format!("macOS Keychain (multi-account: {service})")
        }
        CredentialSource::EnvDir { path } => {
            format!("$CLAUDE_CONFIG_DIR file ({})", path.display())
        }
        CredentialSource::XdgConfig { path } => format!("XDG file ({})", path.display()),
        CredentialSource::ClaudeLegacy { path } => format!("legacy file ({})", path.display()),
        // CredentialSource is `#[non_exhaustive]` per ADR-0018.
        // Future variants render generically until cli wires an
        // explicit label.
        _ => "unknown credential source".to_string(),
    }
}

// --- Cache ---

const CACHE_DIR_ID: &str = "cache.dir_writable";
const CACHE_USAGE_ID: &str = "cache.usage_json_shape";
const CACHE_LOCK_ID: &str = "cache.lock_fresh";

fn cache_category(env: &DoctorEnv) -> Category {
    let snapshot = &env.cache;
    let dir = check_cache_dir(snapshot);
    // Spec §Cross-category short-circuits: when the cache root
    // can't be derived from `$XDG_CACHE_HOME` or `$HOME`, the two
    // downstream checks SKIP — there's no path to inspect.
    let (usage, lock) = if matches!(snapshot.root, CacheRootState::Unresolved) {
        let reason = "cache root unresolved";
        (
            CheckResult::skip(CACHE_USAGE_ID, "`usage.json` shape current", reason),
            CheckResult::skip(CACHE_LOCK_ID, "Lock file is fresh", reason),
        )
    } else {
        (check_cache_usage_json(snapshot), check_cache_lock(snapshot))
    };
    Category::new("Cache", vec![dir, usage, lock])
}

fn check_cache_dir(snapshot: &DoctorCacheSnapshot) -> CheckResult {
    match (&snapshot.root, &snapshot.root_path) {
        (CacheRootState::Exists, Some(path)) => {
            CheckResult::pass(CACHE_DIR_ID, format!("Cache dir: {}", path.display()))
        }
        // Absent + writable parent: expected first-run state. PASS
        // with a hint that the runtime will create the directory.
        (CacheRootState::Absent, Some(path)) => CheckResult::pass(
            CACHE_DIR_ID,
            format!(
                "Cache dir: {} (will be created on first fetch)",
                path.display()
            ),
        ),
        // Absent + read-only ancestor: runtime's `create_dir_all`
        // will fail every time. Without this WARN doctor would PASS
        // forever for setups like `XDG_CACHE_HOME=/proc/cache/...`
        // where the cache can never be created — the next-run
        // recovery path doesn't apply because the failed fetch
        // leaves the path Absent.
        (CacheRootState::AbsentParentReadOnly { parent }, Some(path)) => CheckResult::warn(
            CACHE_DIR_ID,
            format!(
                "Cache dir cannot be created: {} (read-only ancestor: {})",
                path.display(),
                parent.display(),
            ),
            "point $XDG_CACHE_HOME (or $HOME) at a writable location",
        ),
        (CacheRootState::NotADirectory, Some(path)) => CheckResult::warn(
            CACHE_DIR_ID,
            format!(
                "Cache path exists but is not a directory: {}",
                path.display()
            ),
            "remove or rename the file at the cache path so linesmith can create the cache dir",
        ),
        (CacheRootState::Unreadable { message }, Some(path)) => CheckResult::warn(
            CACHE_DIR_ID,
            format!("Cache dir unreadable: {} ({message})", path.display()),
            "check filesystem permissions on the cache path or its parents",
        ),
        (CacheRootState::Unresolved, _) | (_, None) => CheckResult::skip(
            CACHE_DIR_ID,
            "Cache dir exists or creatable",
            "cache root unresolved",
        ),
    }
}

fn check_cache_usage_json(snapshot: &DoctorCacheSnapshot) -> CheckResult {
    match &snapshot.usage_json {
        UsageJsonState::Missing => CheckResult::pass(
            CACHE_USAGE_ID,
            "`usage.json` not yet written (next fetch will create it)",
        ),
        UsageJsonState::Current { schema_version } => CheckResult::pass(
            CACHE_USAGE_ID,
            format!("`usage.json` schema_version={schema_version}"),
        ),
        UsageJsonState::Stale { schema_version } => CheckResult::warn(
            CACHE_USAGE_ID,
            format!("`usage.json` is stale (schema_version={schema_version})"),
            "safe to ignore; next fetch rewrites",
        ),
        UsageJsonState::FutureTimestamp => CheckResult::warn(
            CACHE_USAGE_ID,
            "`usage.json` has a `cached_at` in the future (clock skew)",
            "fix the system clock (sudo sntp -sS time.apple.com or equivalent); the runtime treats this as a cache miss",
        ),
        // Distinct hint from Stale: a corrupt usage.json may
        // signal real FS damage rather than a routine
        // schema-version drift. Don't tell the user to ignore it.
        UsageJsonState::Unreadable { message } => CheckResult::warn(
            CACHE_USAGE_ID,
            format!("`usage.json` unreadable: {message}"),
            "investigate filesystem corruption; rm the file if it's safe to discard",
        ),
    }
}

fn check_cache_lock(snapshot: &DoctorCacheSnapshot) -> CheckResult {
    match &snapshot.lock {
        LockState::Absent => CheckResult::pass(CACHE_LOCK_ID, "No lock file (no active backoff)"),
        LockState::Active { blocked_until_secs } => CheckResult::pass(
            CACHE_LOCK_ID,
            format!("Lock active (blocked_until={blocked_until_secs})"),
        ),
        LockState::Stale { blocked_until_secs } => CheckResult::warn(
            CACHE_LOCK_ID,
            format!("Stale lock (blocked_until={blocked_until_secs} in the past)"),
            "rm ~/.cache/linesmith/usage.lock to clear",
        ),
        LockState::Unreadable { message } => CheckResult::warn(
            CACHE_LOCK_ID,
            format!("Lock file unreadable: {message}"),
            "rm ~/.cache/linesmith/usage.lock to clear",
        ),
    }
}

// --- Rate-limit endpoint ---

const ENDPOINT_REACHABLE_ID: &str = "endpoint.reachable";
const ENDPOINT_SHAPE_ID: &str = "endpoint.shape_current";
const ENDPOINT_HEADERS_ID: &str = "endpoint.headers_sane";

fn endpoint_category(env: &DoctorEnv) -> Category {
    let Some(probe) = &env.endpoint.probe else {
        // Spec §Cross-category: OAuth token missing → SKIP all 3.
        // The race-window case (snapshot's first call resolved
        // creds, probe's second call failed) gets a distinct reason
        // so the user doesn't read "Resolved" alongside an opaque
        // "endpoint probe not attempted".
        let reason = if env.endpoint.credentials_vanished {
            "credentials became unavailable mid-probe (race)"
        } else {
            match &env.credentials {
                DoctorCredentialsSnapshot::Resolved(_) => "endpoint probe not attempted",
                DoctorCredentialsSnapshot::Unresolvable => "credentials cascade has no source",
                DoctorCredentialsSnapshot::Failed(_) => "no token to probe with",
            }
        };
        return Category::new(
            "Rate-limit endpoint",
            vec![
                CheckResult::skip(ENDPOINT_REACHABLE_ID, "Endpoint reachable", reason),
                CheckResult::skip(ENDPOINT_SHAPE_ID, "Endpoint returns expected shape", reason),
                CheckResult::skip(ENDPOINT_HEADERS_ID, "Rate-limit headers sane", reason),
            ],
        );
    };
    Category::new(
        "Rate-limit endpoint",
        vec![
            check_endpoint_reachable(probe),
            check_endpoint_shape(probe),
            check_endpoint_headers(probe),
        ],
    )
}

fn check_endpoint_reachable(probe: &EndpointProbe) -> CheckResult {
    match &probe.outcome {
        EndpointProbeOutcome::Ok => CheckResult::pass(
            ENDPOINT_REACHABLE_ID,
            format!("`/api/oauth/usage` 200 OK ({}ms)", probe.elapsed_ms),
        ),
        EndpointProbeOutcome::Slow => CheckResult::warn(
            ENDPOINT_REACHABLE_ID,
            format!("Endpoint responded slowly ({}ms)", probe.elapsed_ms),
            "check internet, or Anthropic status page",
        ),
        EndpointProbeOutcome::TransportError => CheckResult::warn(
            ENDPOINT_REACHABLE_ID,
            "Endpoint unreachable (DNS / connect / read timeout / proxy)",
            "check internet, or Anthropic status page",
        ),
        EndpointProbeOutcome::BadStatus { status } => CheckResult::fail(
            ENDPOINT_REACHABLE_ID,
            format!("Endpoint returned HTTP {status}"),
            match status {
                401 => "log in again to refresh credentials",
                403 => "verify scopes; log in again",
                _ => "report a linesmith issue with the status code",
            },
        ),
        // Shape / headers are reported by their own check rows.
        EndpointProbeOutcome::ParseError | EndpointProbeOutcome::UnexpectedShape { .. } => {
            CheckResult::pass(
                ENDPOINT_REACHABLE_ID,
                format!("`/api/oauth/usage` reachable ({}ms)", probe.elapsed_ms),
            )
        }
        EndpointProbeOutcome::RateLimited { .. } => CheckResult::pass(
            ENDPOINT_REACHABLE_ID,
            format!("`/api/oauth/usage` reachable ({}ms)", probe.elapsed_ms),
        ),
    }
}

fn check_endpoint_shape(probe: &EndpointProbe) -> CheckResult {
    match &probe.outcome {
        EndpointProbeOutcome::Ok | EndpointProbeOutcome::Slow => {
            CheckResult::pass(ENDPOINT_SHAPE_ID, "Response shape current")
        }
        EndpointProbeOutcome::UnexpectedShape { extra_keys } => CheckResult::warn(
            ENDPOINT_SHAPE_ID,
            format!(
                "Response has forward-compat keys: {}",
                extra_keys.join(", ")
            ),
            "no action needed; linesmith ignores unknown buckets",
        ),
        EndpointProbeOutcome::ParseError => CheckResult::fail(
            ENDPOINT_SHAPE_ID,
            "Response did not deserialize into `UsageApiResponse`",
            "report a linesmith issue; Anthropic changed the API",
        ),
        // The reachability check carries the FAIL/WARN; this row
        // SKIPs because there's no body to inspect. Listing every
        // variant explicitly (no `_`) so a future
        // `#[non_exhaustive]` addition fails to compile until it
        // picks a severity rather than silently SKIPing.
        EndpointProbeOutcome::TransportError
        | EndpointProbeOutcome::BadStatus { .. }
        | EndpointProbeOutcome::RateLimited { .. } => CheckResult::skip(
            ENDPOINT_SHAPE_ID,
            "Endpoint returns expected shape",
            "endpoint not reachable",
        ),
    }
}

fn check_endpoint_headers(probe: &EndpointProbe) -> CheckResult {
    match &probe.outcome {
        EndpointProbeOutcome::RateLimited { retry_after_secs } => match retry_after_secs {
            Some(secs) if *secs > 3600 => CheckResult::fail(
                ENDPOINT_HEADERS_ID,
                format!("429 with abusive Retry-After: {secs}s"),
                "slow down: you're hitting the rate limit hard",
            ),
            Some(secs) => CheckResult::warn(
                ENDPOINT_HEADERS_ID,
                format!("429 with Retry-After: {secs}s"),
                "slow down: you're hitting the rate limit",
            ),
            None => CheckResult::warn(
                ENDPOINT_HEADERS_ID,
                "429 without Retry-After header",
                "slow down: you're hitting the rate limit",
            ),
        },
        EndpointProbeOutcome::Ok
        | EndpointProbeOutcome::Slow
        | EndpointProbeOutcome::UnexpectedShape { .. }
        | EndpointProbeOutcome::ParseError => {
            CheckResult::pass(ENDPOINT_HEADERS_ID, "No 429 returned")
        }
        // Explicit list (no `_`) so a future `#[non_exhaustive]`
        // variant fails to compile until it picks a severity.
        EndpointProbeOutcome::TransportError | EndpointProbeOutcome::BadStatus { .. } => {
            CheckResult::skip(
                ENDPOINT_HEADERS_ID,
                "Rate-limit headers sane",
                "endpoint not reachable",
            )
        }
    }
}

// --- Plugins ---

const PLUGINS_COMPILE_ID: &str = "plugins.compile";
const PLUGINS_DEPS_ID: &str = "plugins.deps_valid";
const PLUGINS_ID_COLLISIONS_ID: &str = "plugins.no_id_collisions";
const PLUGINS_BUILTIN_COLLISIONS_ID: &str = "plugins.no_builtin_collisions";

fn plugins_category(env: &DoctorEnv) -> Category {
    use crate::plugins::errors::{CollisionWinner, PluginError};
    let summary = match &env.plugins {
        DoctorPluginsSnapshot::NoSources => {
            // Spec §Edge cases: "Default plugin directory doesn't
            // exist (no `plugin_dirs` configured) → SKIP the
            // Plugins category with reason 'no plugins
            // configured'; not a failure."
            let reason = "no plugins configured";
            return Category::new(
                "Plugins",
                vec![
                    CheckResult::skip(PLUGINS_COMPILE_ID, "All plugins compile", reason),
                    CheckResult::skip(PLUGINS_DEPS_ID, "All `@data_deps` valid", reason),
                    CheckResult::skip(PLUGINS_ID_COLLISIONS_ID, "No id collisions", reason),
                    CheckResult::skip(
                        PLUGINS_BUILTIN_COLLISIONS_ID,
                        "No built-in collisions",
                        reason,
                    ),
                ],
            );
        }
        DoctorPluginsSnapshot::Discovered(summary) => summary,
    };

    let mut compile_errors: Vec<&PluginError> = Vec::new();
    let mut dep_errors: Vec<&PluginError> = Vec::new();
    let mut id_collisions: Vec<&PluginError> = Vec::new();
    let mut builtin_collisions: Vec<&PluginError> = Vec::new();
    let mut unexpected: Vec<&PluginError> = Vec::new();
    for err in &summary.errors {
        match err {
            PluginError::Compile { .. } => compile_errors.push(err),
            PluginError::MalformedDataDeps { .. } | PluginError::UnknownDataDep { .. } => {
                dep_errors.push(err);
            }
            PluginError::IdCollision {
                winner: CollisionWinner::BuiltIn,
                ..
            } => builtin_collisions.push(err),
            PluginError::IdCollision {
                winner: CollisionWinner::Plugin(_),
                ..
            } => id_collisions.push(err),
            // Render-time variants are constructed by `render(ctx)`
            // and shouldn't reach `load_errors()`. If one shows up
            // here a future plugin lifecycle hook (init, schema
            // validate, etc.) started running at compile time and
            // the doctor needs to pick a category. Surfacing it as
            // a FAIL on the compile row keeps the user informed
            // until the variant gets explicit routing.
            PluginError::Runtime { .. }
            | PluginError::ResourceExceeded { .. }
            | PluginError::Timeout { .. }
            | PluginError::MalformedReturn { .. } => {
                debug_assert!(
                    false,
                    "render-time PluginError reached the doctor's load-error classifier: {err:?}",
                );
                unexpected.push(err);
            }
            // PluginError is `#[non_exhaustive]` per ADR-0018.
            // Future variants from linesmith-core route to
            // `unexpected` so the doctor surfaces them on a
            // "compile" row until cli adds explicit categorization.
            _ => unexpected.push(err),
        }
    }

    Category::new(
        "Plugins",
        vec![
            check_plugins_compile(summary.compiled_count, &compile_errors, &unexpected),
            check_plugins_deps_valid(&dep_errors),
            check_plugins_id_collisions(&id_collisions),
            check_plugins_builtin_collisions(&builtin_collisions),
        ],
    )
}

fn check_plugins_compile(
    compiled_count: usize,
    errors: &[&crate::plugins::errors::PluginError],
    unexpected: &[&crate::plugins::errors::PluginError],
) -> CheckResult {
    use crate::plugins::errors::PluginError;
    // `unexpected` carries render-time variants that shouldn't be
    // in `load_errors()`. Surface them as FAIL on the compile row
    // (the closest fit) with a "report this as a doctor bug" hint
    // so the user knows to file an issue rather than chase a
    // phantom config problem. Render via `kind()` rather than
    // `Display`/`Debug` because Runtime / MalformedReturn variants
    // carry plugin-author-controlled `message` strings that could
    // include secrets from a hostile `throw("...")`.
    if !unexpected.is_empty() {
        let detail = unexpected
            .iter()
            .map(|e| format!("unexpected variant: {}", e.kind()))
            .collect::<Vec<_>>()
            .join("; ");
        return CheckResult::fail(
            PLUGINS_COMPILE_ID,
            format!(
                "{} unexpected plugin error(s) at load time",
                unexpected.len()
            ),
            format!("file a linesmith bug; doctor saw: {detail}"),
        );
    }
    if errors.is_empty() {
        return CheckResult::pass(
            PLUGINS_COMPILE_ID,
            format!("{compiled_count} plugin(s) compiled cleanly"),
        );
    }
    let detail = errors
        .iter()
        .filter_map(|e| match e {
            PluginError::Compile { path, message } => {
                Some(format!("{}: {}", path.display(), message))
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ");
    CheckResult::fail(
        PLUGINS_COMPILE_ID,
        format!("{} plugin(s) failed to compile", errors.len()),
        // serde_rhai's Display already includes line/col for compile
        // errors; surface the path + message verbatim.
        detail,
    )
}

fn check_plugins_deps_valid(errors: &[&crate::plugins::errors::PluginError]) -> CheckResult {
    use crate::plugins::errors::PluginError;
    if errors.is_empty() {
        return CheckResult::pass(PLUGINS_DEPS_ID, "All `@data_deps` valid");
    }
    let detail = errors
        .iter()
        .map(|e| match e {
            PluginError::UnknownDataDep { path, name } => {
                format!("{} declares unknown dep `{name}`", path.display())
            }
            PluginError::MalformedDataDeps { path, message } => {
                format!("{}: {message}", path.display())
            }
            // The classifier never routes other variants here.
            other => format!("{other}"),
        })
        .collect::<Vec<_>>()
        .join("; ");
    CheckResult::fail(
        PLUGINS_DEPS_ID,
        format!("{} plugin(s) declared invalid `@data_deps`", errors.len()),
        detail,
    )
}

fn check_plugins_id_collisions(errors: &[&crate::plugins::errors::PluginError]) -> CheckResult {
    use crate::plugins::errors::PluginError;
    if errors.is_empty() {
        return CheckResult::pass(PLUGINS_ID_COLLISIONS_ID, "No id collisions");
    }
    let detail = errors
        .iter()
        .filter_map(|e| match e {
            PluginError::IdCollision {
                id,
                winner,
                loser_path,
            } => Some(format!(
                "id `{id}` claimed by both {winner} and {} (loser dropped)",
                loser_path.display()
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ");
    CheckResult::fail(
        PLUGINS_ID_COLLISIONS_ID,
        format!("{} plugin id collision(s)", errors.len()),
        detail,
    )
}

fn check_plugins_builtin_collisions(
    errors: &[&crate::plugins::errors::PluginError],
) -> CheckResult {
    use crate::plugins::errors::PluginError;
    if errors.is_empty() {
        return CheckResult::pass(PLUGINS_BUILTIN_COLLISIONS_ID, "No built-in collisions");
    }
    let detail = errors
        .iter()
        .filter_map(|e| match e {
            PluginError::IdCollision { id, loser_path, .. } => Some(format!(
                "plugin id `{id}` shadows a built-in segment ({})",
                loser_path.display()
            )),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("; ");
    CheckResult::fail(
        PLUGINS_BUILTIN_COLLISIONS_ID,
        format!("{} plugin(s) shadow a built-in", errors.len()),
        detail,
    )
}

// --- Git ---

const GIT_REPO_DETECTED_ID: &str = "git.repo_detected";
const GIT_HEAD_RESOLVES_ID: &str = "git.head_resolves";
const GIT_REPO_KIND_ID: &str = "git.repo_kind";

fn git_category(env: &DoctorEnv) -> Category {
    Category::new(
        "Git",
        vec![
            check_git_repo_detected(env),
            check_git_head_resolves(&env.git),
            check_git_repo_kind(&env.git),
        ],
    )
}

fn check_git_repo_detected(env: &DoctorEnv) -> CheckResult {
    match &env.git {
        DoctorGitSnapshot::Repo(ctx) => CheckResult::pass(
            GIT_REPO_DETECTED_ID,
            format!("Repo detected: {}", ctx.repo_path.display()),
        ),
        DoctorGitSnapshot::Failed { message } => CheckResult::fail(
            GIT_REPO_DETECTED_ID,
            format!("`gix::discover` failed: {message}"),
            "repair the repo, or move out of this directory",
        ),
        DoctorGitSnapshot::NotInRepo => {
            // Spec §Git "cwd git status" carve-out: WARN by
            // default, SKIP when no `git_*` segment is enabled
            // in the user's config (the segment would silently
            // hide; not a doctor concern).
            if any_git_segment_enabled(&env.config.read) {
                CheckResult::warn(
                    GIT_REPO_DETECTED_ID,
                    "Not inside a git repo (a git segment is configured)",
                    "cd into a repo, or remove `git_*` from `[line.segments]`",
                )
            } else {
                CheckResult::skip(
                    GIT_REPO_DETECTED_ID,
                    "cwd is in a git repo",
                    "no `git_*` segment configured",
                )
            }
        }
    }
}

fn check_git_head_resolves(snapshot: &DoctorGitSnapshot) -> CheckResult {
    match snapshot {
        DoctorGitSnapshot::Repo(ctx) => {
            // Every `Head` variant the runtime constructs is, by
            // definition, resolvable — `resolve_head` collapses
            // all unresolvable cases into the `Failed` arm above.
            // The spec's PASS condition ("Branch / Detached /
            // Unborn") plus the live `OtherRef` variant all PASS;
            // the FAIL row is unreachable from this snapshot.
            //
            // Exception: `OtherRef.full_name` comes from gix's
            // lossy-UTF-8 path (`as_bstr().to_string()` substitutes
            // U+FFFD for invalid bytes). A name carrying a
            // replacement character means the underlying refname is
            // corrupt; PASS would silently hide it.
            if let crate::data_context::git::Head::OtherRef { full_name } = &ctx.head {
                if full_name.contains('\u{FFFD}') {
                    return CheckResult::warn(
                        GIT_HEAD_RESOLVES_ID,
                        format!("HEAD points at a ref with non-UTF-8 bytes (lossy: {full_name})"),
                        "repair the refname (`git update-ref`); the ref is unreadable as UTF-8",
                    );
                }
            }
            let label = match &ctx.head {
                crate::data_context::git::Head::Branch(name) => format!("HEAD -> {name}"),
                crate::data_context::git::Head::Detached(oid) => {
                    format!("HEAD detached at {oid}")
                }
                crate::data_context::git::Head::Unborn { symbolic_ref } => {
                    format!("HEAD unborn (would point at {symbolic_ref})")
                }
                crate::data_context::git::Head::OtherRef { full_name } => {
                    format!("HEAD -> {full_name}")
                }
                // Head is `#[non_exhaustive]` per ADR-0018. A future
                // variant in linesmith-core renders generically
                // until cli adds an explicit label.
                _ => "HEAD (unrecognized)".to_string(),
            };
            CheckResult::pass(GIT_HEAD_RESOLVES_ID, label)
        }
        DoctorGitSnapshot::Failed { .. } => CheckResult::skip(
            GIT_HEAD_RESOLVES_ID,
            "HEAD resolves",
            "repo discovery failed",
        ),
        DoctorGitSnapshot::NotInRepo => {
            CheckResult::skip(GIT_HEAD_RESOLVES_ID, "HEAD resolves", "not in a git repo")
        }
    }
}

fn check_git_repo_kind(snapshot: &DoctorGitSnapshot) -> CheckResult {
    use crate::data_context::git::RepoKind;
    match snapshot {
        DoctorGitSnapshot::Repo(ctx) => {
            let label = match &ctx.repo_kind {
                RepoKind::Main => "RepoKind: main",
                RepoKind::Bare => "RepoKind: bare",
                RepoKind::Submodule => "RepoKind: submodule",
                RepoKind::LinkedWorktree { .. } => "RepoKind: linked worktree",
                // RepoKind is `#[non_exhaustive]` per ADR-0018.
                // Future variants render generically.
                _ => "RepoKind: unrecognized",
            };
            // Worktree carries a name field; surface it in the
            // label so a user can tell which worktree.
            let label = if let RepoKind::LinkedWorktree { name } = &ctx.repo_kind {
                format!("{label} ({name})")
            } else {
                label.to_string()
            };
            CheckResult::pass(GIT_REPO_KIND_ID, label)
        }
        DoctorGitSnapshot::Failed { .. } => CheckResult::skip(
            GIT_REPO_KIND_ID,
            "RepoKind detected",
            "repo discovery failed",
        ),
        DoctorGitSnapshot::NotInRepo => {
            CheckResult::skip(GIT_REPO_KIND_ID, "RepoKind detected", "not in a git repo")
        }
    }
}

/// Whether any built-in `git_*` segment id appears in the loaded
/// config's segment lists. Used by the git-not-in-repo
/// SKIP-vs-WARN gate.
///
/// Gates against `BUILT_IN_SEGMENT_IDS` filtered by the `git_`
/// prefix rather than a raw `starts_with("git_")` lens — a user
/// plugin id like `git_typo` shouldn't trigger the runtime's
/// "you're not in a repo" WARN, since the plugin is the one
/// rendering and it has its own error path. Forward-compat: a
/// future built-in `git_status` / `git_stash` automatically picks
/// up the carve-out without doctor edits, since they'd land in
/// `BUILT_IN_SEGMENT_IDS`.
fn any_git_segment_enabled(read: &ConfigReadOutcome) -> bool {
    let ConfigReadOutcome::Loaded { config, .. } = read else {
        return false;
    };
    let Some(line) = &config.line else {
        return false;
    };
    let is_builtin_git = |id: &str| {
        crate::segments::BUILT_IN_SEGMENT_IDS
            .iter()
            .any(|b| *b == id && b.starts_with("git_"))
    };
    if line.segments.iter().any(|id| is_builtin_git(id)) {
        return true;
    }
    line.numbered.values().any(|v| {
        v.as_table()
            .and_then(|t| t.get("segments"))
            .and_then(|s| s.as_array())
            .is_some_and(|arr| {
                arr.iter()
                    .any(|item| item.as_str().is_some_and(is_builtin_git))
            })
    })
}

fn self_category(env: &DoctorEnv) -> Category {
    Category::new(
        "Self",
        vec![
            check_self_version(),
            check_binary_path(env),
            check_self_update_available(&env.update_probe),
            check_self_binary_integrity(env.binary_build_sha),
        ],
    )
}

fn check_self_version() -> CheckResult {
    CheckResult::pass(
        "self.version",
        format!("linesmith {}", env!("CARGO_PKG_VERSION")),
    )
}

fn check_binary_path(env: &DoctorEnv) -> CheckResult {
    match &env.current_exe {
        Ok(p) => CheckResult::pass("self.binary_path", format!("Binary: {}", p.display())),
        // Preserve the underlying error so the user sees whether it
        // was a missing /proc, a deleted exe, a permission issue,
        // etc. Generic "reinstall" advice is only right for some of
        // those.
        Err(err) => CheckResult::warn(
            "self.binary_path",
            format!("Could not resolve binary path: {err}"),
            "std::env::current_exe failed (unusual; check sandbox / permissions or reinstall)",
        ),
    }
}

fn check_self_binary_integrity(build_sha: Option<&'static str>) -> CheckResult {
    // Trim before checking emptiness. CI pipelines that pipe `git
    // rev-parse HEAD` through a buggy substitution can produce
    // whitespace-only SHAs (`" "`, `"\n"`, `"\t"`); these are NOT
    // traceability and rendering them as PASS produces a tell-tale
    // double-space "Built from   (linesmith ...)" line that's worse
    // than the WARN — it falsely claims provenance.
    let trimmed = build_sha.map(str::trim).filter(|s| !s.is_empty());
    match trimmed {
        // Build-time SHA present and non-blank → traceable back to a
        // known commit through whatever pipeline (cargo-dist, GitHub
        // Actions, etc.) set the env var. Surface a 7-char prefix in
        // the label so the user can copy it into bug reports without
        // a follow-up round trip.
        Some(sha) => {
            let short = sha.get(..sha.len().min(7)).unwrap_or(sha);
            CheckResult::pass(
                "self.binary_integrity",
                format!(
                    "Built from {short} (linesmith {})",
                    env!("CARGO_PKG_VERSION")
                ),
            )
        }
        // No build metadata, or a blank value → almost always a local
        // `cargo build` (a dev environment doesn't set
        // `LINESMITH_BUILD_SHA`); rarely, a binary patched after the
        // fact. Spec WARN, not FAIL — most users hit this by building
        // from source rather than by being attacked.
        None => CheckResult::warn(
            "self.binary_integrity",
            "Build metadata missing (LINESMITH_BUILD_SHA not set at compile time)",
            "reinstall from a canonical source (cargo-dist release / package manager) to verify provenance",
        ),
    }
}

fn check_self_update_available(probe: &DoctorUpdateProbe) -> CheckResult {
    match probe {
        DoctorUpdateProbe::Latest => CheckResult::pass(
            "self.update_available",
            format!("Running on latest release (v{})", env!("CARGO_PKG_VERSION")),
        ),
        DoctorUpdateProbe::Newer { latest } => CheckResult::warn(
            "self.update_available",
            format!(
                "New release available: {latest} (running v{})",
                env!("CARGO_PKG_VERSION")
            ),
            "run `brew upgrade linesmith` (or your equivalent install path)",
        ),
        // Per spec §Self: transport errors WARN rather than FAIL —
        // air-gapped CI / corporate proxy / GitHub outage shouldn't
        // gate exit-1 on a setup-verification command.
        DoctorUpdateProbe::TransportError { message } => CheckResult::warn(
            "self.update_available",
            format!("Could not reach GitHub releases API: {message}"),
            "no network or GitHub unreachable; re-run when online to verify version",
        ),
        DoctorUpdateProbe::ParseError { message } => CheckResult::warn(
            "self.update_available",
            format!("GitHub releases response unrecognized: {message}"),
            "GitHub releases API may have changed shape; file a linesmith bug if this persists",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_within_a_mode_are_pairwise_distinct() {
        // Reader must distinguish PASS from FAIL at a glance — the
        // user-facing contract is intra-mode distinctness, not
        // cross-mode (which would still allow collisions like
        // PASS_unicode == SKIP_unicode).
        let unicode: Vec<_> = [
            Severity::Pass,
            Severity::Warn,
            Severity::Fail,
            Severity::Skip,
        ]
        .iter()
        .map(|s| s.unicode_glyph())
        .collect();
        let ascii: Vec<_> = [
            Severity::Pass,
            Severity::Warn,
            Severity::Fail,
            Severity::Skip,
        ]
        .iter()
        .map(|s| s.ascii_glyph())
        .collect();
        for (i, a) in unicode.iter().enumerate() {
            for (j, b) in unicode.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "unicode glyph collision: {a} == {b}");
                }
            }
        }
        for (i, a) in ascii.iter().enumerate() {
            for (j, b) in ascii.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "ascii glyph collision: {a} == {b}");
                }
            }
        }
    }

    #[test]
    fn check_result_constructors_round_trip_id_and_severity() {
        let p = CheckResult::pass("p.id", "label");
        assert_eq!(p.id(), "p.id");
        assert_eq!(p.severity(), Severity::Pass);
        assert!(p.hint.is_none(), "PASS must not carry a hint");

        let w = CheckResult::warn("w.id", "label", "do thing");
        assert_eq!(w.id(), "w.id");
        assert_eq!(w.severity(), Severity::Warn);
        assert_eq!(w.hint.as_deref(), Some("do thing"));

        let f = CheckResult::fail("f.id", "label", "fix");
        assert_eq!(f.severity(), Severity::Fail);
        assert_eq!(f.hint.as_deref(), Some("fix"));

        let s = CheckResult::skip("s.id", "label", "no $HOME");
        assert_eq!(s.severity(), Severity::Skip);
        assert_eq!(s.hint.as_deref(), Some("no $HOME"));
    }

    #[test]
    fn ascii_glyphs_contain_no_unicode() {
        for s in [
            Severity::Pass,
            Severity::Warn,
            Severity::Fail,
            Severity::Skip,
        ] {
            assert!(
                s.ascii_glyph().is_ascii(),
                "ascii glyph for {s:?} contains non-ASCII bytes",
            );
        }
    }

    fn fail_only_report() -> Report {
        Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "Self",
                vec![CheckResult::fail("self.broken", "broken", "fix it")],
            )],
        }
    }

    #[test]
    fn exit_code_is_one_on_any_fail() {
        assert_eq!(fail_only_report().exit_code(), 1);
    }

    #[test]
    fn exit_code_is_zero_on_warn_only() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "Self",
                vec![CheckResult::warn("self.warn", "degraded", "do thing")],
            )],
        };
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn exit_code_is_zero_on_all_pass() {
        assert_eq!(build_report(&DoctorEnv::healthy()).exit_code(), 0);
    }

    #[test]
    fn exit_code_skip_does_not_fail() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "Self",
                vec![CheckResult::skip("self.na", "n/a", "not applicable")],
            )],
        };
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn exit_code_is_one_when_fail_mixed_with_other_severities() {
        // Defends against `.any() → .all()` typo: every other exit-
        // code test uses a homogeneous report, so the `.any()` could
        // be silently swapped without detection.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![
                Category::new(
                    "A",
                    vec![
                        CheckResult::pass("a.ok", "ok"),
                        CheckResult::warn("a.warn", "degraded", "do thing"),
                    ],
                ),
                Category::new(
                    "B",
                    vec![
                        CheckResult::skip("b.na", "n/a", "skipped"),
                        CheckResult::fail("b.broken", "broken", "fix"),
                    ],
                ),
            ],
        };
        assert_eq!(r.exit_code(), 1);
    }

    #[test]
    fn exit_code_is_zero_when_no_fail_in_mixed_report() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![
                Category::new(
                    "A",
                    vec![
                        CheckResult::pass("a.ok", "ok"),
                        CheckResult::warn("a.warn", "degraded", "do thing"),
                    ],
                ),
                Category::new("B", vec![CheckResult::skip("b.na", "n/a", "skipped")]),
            ],
        };
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn summary_counts_aggregate_across_categories() {
        // Distinct counts per severity (2/3/1/4) so any positional
        // swap in the renderer's format string surfaces — equal
        // counts would let a `{} WARN` ↔ `{} FAIL` swap slip through.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![
                Category::new(
                    "A",
                    vec![
                        CheckResult::pass("a.1", "ok"),
                        CheckResult::pass("a.2", "ok"),
                        CheckResult::warn("a.3", "deg", "hint"),
                        CheckResult::warn("a.4", "deg", "hint"),
                        CheckResult::warn("a.5", "deg", "hint"),
                    ],
                ),
                Category::new(
                    "B",
                    vec![
                        CheckResult::fail("b.1", "broken", "fix"),
                        CheckResult::skip("b.2", "na", "reason"),
                        CheckResult::skip("b.3", "na", "reason"),
                        CheckResult::skip("b.4", "na", "reason"),
                        CheckResult::skip("b.5", "na", "reason"),
                    ],
                ),
            ],
        };
        let counts = r.summary_counts();
        assert_eq!(counts.pass, 2);
        assert_eq!(counts.warn, 3);
        assert_eq!(counts.fail, 1);
        assert_eq!(counts.skip, 4);

        // Pin the field-to-position mapping in the format string —
        // catches typos like `{} WARN` interpolating counts.fail.
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("2 PASS"), "wrong PASS count rendered:\n{s}");
        assert!(s.contains("3 WARN"), "wrong WARN count rendered:\n{s}");
        assert!(s.contains("1 FAIL"), "wrong FAIL count rendered:\n{s}");
        assert!(s.contains("4 SKIP"), "wrong SKIP count rendered:\n{s}");
    }

    #[test]
    fn plain_mode_emits_no_unicode() {
        // Walk every renderer-emitted label across every check
        // through `RenderMode::Plain` and assert ASCII for each.
        // Each `DoctorEnv` snapshot holds one state per slot, so
        // covering the full label surface requires N rendered
        // reports — `plain_mode_coverage_envs()` enumerates them.
        // The previous version of this test rendered only one
        // synthetic fixture covering nothing real, then a
        // healthy-baseline (PASS-only) walk; FAIL/WARN labels
        // could ship with Unicode and slip the tripwire.
        for (name, env) in plain_mode_coverage_envs() {
            let mut out = Vec::new();
            render(&mut out, &build_report(&env), RenderMode::Plain).expect("render ok");
            let s = String::from_utf8(out).expect("utf8");
            assert!(
                s.is_ascii(),
                "plain output contains non-ASCII bytes for `{name}`:\n{s}",
            );
        }
    }

    /// Enumerate `DoctorEnv` fixtures that, between them, exercise
    /// every label-producing branch in every check. Returns
    /// `(name, env)` pairs so the failing assertion can name the
    /// scenario rather than dumping a wall of text. Each branch
    /// needs at least one fixture; some fixtures exercise several
    /// where the snapshot shape allows it.
    fn plain_mode_coverage_envs() -> Vec<(&'static str, DoctorEnv)> {
        use crate::data_context::credentials::CredentialSource;
        use crate::data_context::git::{Head, RepoKind};
        use crate::plugins::errors::{CollisionWinner, PluginError};

        let mut envs: Vec<(&'static str, DoctorEnv)> = Vec::new();

        // Healthy baseline covers every PASS-class label.
        envs.push(("healthy", DoctorEnv::healthy()));

        // --- Environment FAIL/WARN labels ---
        let mut env = DoctorEnv::healthy();
        // ASCII-only payload — `EnvVarState::NonUtf8` fires the
        // FAIL branch on the variant tag, not the value bytes,
        // and any Unicode in the value would correctly pass
        // through per the renderer's user-content caveat.
        env.home_env = EnvVarState::NonUtf8("nonutf8-placeholder".to_string());
        envs.push(("env.home_non_utf8", env));

        let mut env = DoctorEnv::healthy();
        env.term = EnvVarState::Set(String::new());
        envs.push(("env.term_empty", env));

        let mut env = DoctorEnv::healthy();
        env.terminal_width_cells = Some(0);
        envs.push(("env.term_width_zero", env));

        let mut env = DoctorEnv::healthy();
        env.current_exe = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "denied",
        ));
        envs.push(("self.binary_path_err", env));

        // --- Config FAIL/WARN labels ---
        let mut env = DoctorEnv::healthy();
        env.config.read = ConfigReadOutcome::ParseError {
            path: PathBuf::from("/etc/linesmith/config.toml"),
            message: "TOML parse error at line 3, column 7".to_string(),
        };
        envs.push(("config.parse_error", env));

        let mut env = DoctorEnv::healthy();
        env.config.read = ConfigReadOutcome::IoError {
            path: PathBuf::from("/etc/linesmith/config.toml"),
            message: "Permission denied".to_string(),
        };
        envs.push(("config.io_error", env));

        let mut env = DoctorEnv::healthy();
        env.config.plugin_dirs = vec![PluginDirStatus {
            path: PathBuf::from("/etc/missing"),
            state: PluginDirState::Missing,
        }];
        envs.push(("config.plugin_dirs_missing", env));

        // --- Claude Code FAIL/WARN labels ---
        let mut env = DoctorEnv::healthy();
        env.claude_code.binary_path = None;
        envs.push(("claude.binary_missing", env));

        let mut env = DoctorEnv::healthy();
        env.claude_code.path_env = EnvVarState::NonUtf8("nonutf8-placeholder".to_string());
        env.claude_code.binary_path = None;
        envs.push(("claude.path_env_non_utf8", env));

        let mut env = DoctorEnv::healthy();
        if let Some(home) = &mut env.claude_code.home_state {
            home.dir = ClaudeDirState::PermissionDenied {
                message: "EACCES".to_string(),
            };
            home.claude_json = ClaudeJsonState::ParseError {
                message: "expected `:` at line 4 column 12".to_string(),
            };
            home.sessions = ClaudeSessionsState::Empty;
        }
        envs.push(("claude.permission_denied_and_parse_error", env));

        // --- Credentials FAIL/WARN labels ---
        let mut env = DoctorEnv::healthy();
        env.credentials = DoctorCredentialsSnapshot::Failed(CredentialErrorSummary::IoError {
            path: PathBuf::from("/home/user/.claude/.credentials.json"),
            message: "Permission denied".to_string(),
        });
        envs.push(("creds.io_error", env));

        let mut env = DoctorEnv::healthy();
        env.credentials = DoctorCredentialsSnapshot::Failed(CredentialErrorSummary::ParseError {
            path: PathBuf::from("/home/user/.claude/.credentials.json"),
        });
        envs.push(("creds.parse_error", env));

        let mut env = DoctorEnv::healthy();
        env.credentials =
            DoctorCredentialsSnapshot::Failed(CredentialErrorSummary::SubprocessFailed {
                message: "security: locked".to_string(),
            });
        envs.push(("creds.subprocess_failed", env));

        let mut env = DoctorEnv::healthy();
        env.credentials = DoctorCredentialsSnapshot::Resolved(CredentialsSummary {
            source: CredentialSource::MacosKeychainMultiAccount {
                service: "Claude Code-credentials-2".to_string(),
                mdat: Some("12345".to_string()),
            },
            scopes: vec!["user:profile".to_string()], // missing user:inference
        });
        envs.push(("creds.scopes_missing", env));

        // --- Cache FAIL/WARN labels ---
        let mut env = DoctorEnv::healthy();
        env.cache.usage_json = UsageJsonState::Stale { schema_version: 0 };
        envs.push(("cache.usage_stale", env));

        let mut env = DoctorEnv::healthy();
        env.cache.usage_json = UsageJsonState::FutureTimestamp;
        envs.push(("cache.usage_future_timestamp", env));

        let mut env = DoctorEnv::healthy();
        env.cache.usage_json = UsageJsonState::Unreadable {
            message: "filesystem corruption".to_string(),
        };
        env.cache.lock = LockState::Stale {
            blocked_until_secs: 1,
        };
        envs.push(("cache.unreadable_and_stale_lock", env));

        let mut env = DoctorEnv::healthy();
        env.cache.root = CacheRootState::NotADirectory;
        envs.push(("cache.not_a_directory", env));

        let mut env = DoctorEnv::healthy();
        env.cache.root = CacheRootState::Unreadable {
            message: "Permission denied (os error 13)".to_string(),
        };
        envs.push(("cache.unreadable", env));

        let mut env = DoctorEnv::healthy();
        env.cache.root = CacheRootState::Absent;
        envs.push(("cache.absent_first_run", env));

        let mut env = DoctorEnv::healthy();
        env.cache.root = CacheRootState::AbsentParentReadOnly {
            parent: PathBuf::from("/proc"),
        };
        envs.push(("cache.absent_parent_read_only", env));

        // --- Endpoint FAIL/WARN labels ---
        let mut env = DoctorEnv::healthy();
        env.endpoint.probe = Some(EndpointProbe {
            elapsed_ms: 2500,
            outcome: EndpointProbeOutcome::Slow,
        });
        envs.push(("endpoint.slow", env));

        let mut env = DoctorEnv::healthy();
        env.endpoint.probe = Some(EndpointProbe {
            elapsed_ms: 150,
            outcome: EndpointProbeOutcome::BadStatus { status: 401 },
        });
        envs.push(("endpoint.bad_status", env));

        let mut env = DoctorEnv::healthy();
        env.endpoint.probe = Some(EndpointProbe {
            elapsed_ms: 100,
            outcome: EndpointProbeOutcome::TransportError,
        });
        envs.push(("endpoint.transport_error", env));

        let mut env = DoctorEnv::healthy();
        env.endpoint.probe = Some(EndpointProbe {
            elapsed_ms: 200,
            outcome: EndpointProbeOutcome::UnexpectedShape {
                extra_keys: vec!["omelette_5h".to_string(), "iguana_7d".to_string()],
            },
        });
        envs.push(("endpoint.unexpected_shape", env));

        let mut env = DoctorEnv::healthy();
        env.endpoint.probe = Some(EndpointProbe {
            elapsed_ms: 300,
            outcome: EndpointProbeOutcome::ParseError,
        });
        envs.push(("endpoint.parse_error", env));

        let mut env = DoctorEnv::healthy();
        env.endpoint.probe = Some(EndpointProbe {
            elapsed_ms: 250,
            outcome: EndpointProbeOutcome::RateLimited {
                retry_after_secs: Some(60),
            },
        });
        envs.push(("endpoint.rate_limited_warn", env));

        let mut env = DoctorEnv::healthy();
        env.endpoint.probe = Some(EndpointProbe {
            elapsed_ms: 250,
            outcome: EndpointProbeOutcome::RateLimited {
                retry_after_secs: Some(7200),
            },
        });
        envs.push(("endpoint.rate_limited_fail", env));

        // --- Plugins FAIL labels (every load-time PluginError variant) ---
        let mut env = DoctorEnv::healthy();
        env.plugins = DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
            compiled_count: 0,
            errors: vec![
                PluginError::Compile {
                    path: PathBuf::from("/p/compile.rhai"),
                    message: "syntax error".to_string(),
                },
                PluginError::UnknownDataDep {
                    path: PathBuf::from("/p/dep.rhai"),
                    name: "credentialz".to_string(),
                },
                PluginError::MalformedDataDeps {
                    path: PathBuf::from("/p/header.rhai"),
                    message: "expected array".to_string(),
                },
                PluginError::IdCollision {
                    id: "shared".to_string(),
                    winner: CollisionWinner::Plugin(PathBuf::from("/p/first.rhai")),
                    loser_path: PathBuf::from("/p/second.rhai"),
                },
                PluginError::IdCollision {
                    id: "model".to_string(),
                    winner: CollisionWinner::BuiltIn,
                    loser_path: PathBuf::from("/p/shadow.rhai"),
                },
            ],
        });
        envs.push(("plugins.all_load_errors", env));

        let env = with_plugins(DoctorPluginsSnapshot::NoSources);
        envs.push(("plugins.no_sources", env));

        // --- Git FAIL/WARN labels ---
        let env = with_git(DoctorGitSnapshot::Failed {
            message: "InvalidFormat: expected git_dir".to_string(),
        });
        envs.push(("git.failed", env));

        let env = with_git(DoctorGitSnapshot::Repo(GitContextSummary {
            repo_path: PathBuf::from("/repo/.git"),
            repo_kind: RepoKind::LinkedWorktree {
                name: "feature-x".to_string(),
            },
            head: Head::Detached(gix::ObjectId::null(gix::hash::Kind::Sha1)),
        }));
        envs.push(("git.detached_worktree", env));

        let env = with_git(DoctorGitSnapshot::Repo(GitContextSummary {
            repo_path: PathBuf::from("/repo/.git"),
            repo_kind: RepoKind::Bare,
            head: Head::Unborn {
                symbolic_ref: "main".to_string(),
            },
        }));
        envs.push(("git.bare_unborn", env));

        let env = with_git(DoctorGitSnapshot::Repo(GitContextSummary {
            repo_path: PathBuf::from("/repo/.git"),
            repo_kind: RepoKind::Submodule,
            head: Head::OtherRef {
                full_name: "refs/tags/v1.0".to_string(),
            },
        }));
        envs.push(("git.submodule_otherref", env));

        // --- Self/update_available WARN labels ---
        let mut env = DoctorEnv::healthy();
        env.update_probe = DoctorUpdateProbe::Newer {
            latest: "v99.0.0".to_string(),
        };
        envs.push(("self.update_newer", env));

        let mut env = DoctorEnv::healthy();
        env.update_probe = DoctorUpdateProbe::TransportError {
            message: "dns lookup failed".to_string(),
        };
        envs.push(("self.update_transport_error", env));

        let mut env = DoctorEnv::healthy();
        env.update_probe = DoctorUpdateProbe::ParseError {
            message: "missing tag_name".to_string(),
        };
        envs.push(("self.update_parse_error", env));

        // --- Self/binary_integrity WARN label ---
        let mut env = DoctorEnv::healthy();
        env.binary_build_sha = None;
        envs.push(("self.binary_integrity_missing", env));

        // --- Claude Code/json_parses TooLarge FAIL label ---
        let mut env = DoctorEnv::healthy();
        if let Some(home) = &mut env.claude_code.home_state {
            home.claude_json = ClaudeJsonState::TooLarge {
                actual_bytes: 500 * 1024 * 1024,
            };
        }
        envs.push(("claude.json_too_large", env));

        envs
    }

    #[test]
    fn plain_mode_summary_separator_is_ascii() {
        let mut out = Vec::new();
        render(
            &mut out,
            &build_report(&DoctorEnv::healthy()),
            RenderMode::Plain,
        )
        .expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("PASS / "),
            "plain summary should use '/' separator:\n{s}"
        );
    }

    #[test]
    fn default_mode_emits_unicode_glyphs() {
        let mut out = Vec::new();
        render(&mut out, &fail_only_report(), RenderMode::Default).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains('✗'), "expected ✗ glyph in default render:\n{s}");
    }

    #[test]
    fn default_mode_summary_separator_is_middle_dot() {
        let mut out = Vec::new();
        render(
            &mut out,
            &build_report(&DoctorEnv::healthy()),
            RenderMode::Default,
        )
        .expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("PASS · "),
            "default summary should use '·' separator:\n{s}"
        );
    }

    #[test]
    fn render_includes_summary_and_exit_lines() {
        let mut out = Vec::new();
        render(
            &mut out,
            &build_report(&DoctorEnv::healthy()),
            RenderMode::Plain,
        )
        .expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("Summary:"), "missing Summary line:\n{s}");
        assert!(s.contains("Exit: 0"), "missing Exit line:\n{s}");
    }

    #[test]
    fn fail_check_renders_hint_indented_after_fail_line() {
        let mut out = Vec::new();
        render(&mut out, &fail_only_report(), RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        // Intent: hint appears indented after the failure line.
        // Exact arrow / spacing is presentation detail covered by
        // snapshot tests in the integration suite.
        assert!(s.contains("fix it"), "hint text missing:\n{s}");
        let fail_idx = s.find("XX broken").expect("FAIL line missing");
        let hint_idx = s.find("fix it").expect("hint missing");
        assert!(hint_idx > fail_idx, "hint must follow the FAIL line");
    }

    #[test]
    fn warn_and_skip_checks_render_their_hint() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "X",
                vec![
                    CheckResult::warn("x.w", "deg", "warn-hint"),
                    CheckResult::skip("x.s", "na", "skip-reason"),
                ],
            )],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("warn-hint"), "WARN hint missing:\n{s}");
        assert!(s.contains("skip-reason"), "SKIP reason missing:\n{s}");
    }

    #[test]
    fn render_includes_category_header() {
        let mut out = Vec::new();
        render(
            &mut out,
            &build_report(&DoctorEnv::healthy()),
            RenderMode::Plain,
        )
        .expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("\nSelf\n"), "missing category header:\n{s}");
    }

    #[test]
    fn render_emits_blank_line_between_categories() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![
                Category::new("A", vec![CheckResult::pass("a.1", "a-line")]),
                Category::new("B", vec![CheckResult::pass("b.1", "b-line")]),
            ],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("a-line\n\nB\n"),
            "expected blank line separating categories:\n{s}"
        );
    }

    #[test]
    fn plain_mode_passes_user_supplied_unicode_through_verbatim() {
        // Contract pin: per docs/specs/doctor.md §plain caveat, --plain
        // guarantees no Unicode in *renderer-emitted* strings only.
        // User-supplied label/hint (paths like ~/café/config) pass
        // through verbatim. A future "fix" that ASCII-folds user
        // content would be a contract change, not a bug fix.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "X",
                vec![CheckResult::warn("x.cfg", "config at ~/café", "edit ☃")],
            )],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("~/café"), "user label must pass through:\n{s}");
        assert!(s.contains('☃'), "user hint must pass through:\n{s}");
    }

    #[test]
    fn empty_report_renders_summary_and_exits_zero() {
        // Guards against a future regression: code that asserts
        // non-empty categories would slip through every existing test
        // since none currently exercise the empty case.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("Summary: 0 PASS"), "missing summary:\n{s}");
        assert!(s.contains("Exit: 0"), "missing exit line:\n{s}");
        assert_eq!(r.exit_code(), 0);
        assert_eq!(r.summary_counts(), SummaryCounts::default());
    }

    #[test]
    fn empty_category_renders_header_with_no_checks() {
        // E.g. Plugins category when no plugins are configured, or
        // Git category outside a repo. Header must still render so
        // the user can see the category was considered.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new("Plugins", vec![])],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("\nPlugins\n"),
            "missing empty-category header:\n{s}"
        );
    }

    #[test]
    fn label_and_hint_accessors_return_constructor_inputs() {
        let p = CheckResult::pass("p.id", "label-p");
        assert_eq!(p.label(), "label-p");
        assert_eq!(p.hint(), None);

        let w = CheckResult::warn("w.id", "label-w", "warn-hint");
        assert_eq!(w.label(), "label-w");
        assert_eq!(w.hint(), Some("warn-hint"));

        let f = CheckResult::fail("f.id", "label-f", "fail-hint");
        assert_eq!(f.label(), "label-f");
        assert_eq!(f.hint(), Some("fail-hint"));

        let s = CheckResult::skip("s.id", "label-s", "skip-reason");
        assert_eq!(s.label(), "label-s");
        assert_eq!(s.hint(), Some("skip-reason"));
    }

    // --- Environment / Self check categories ---

    fn find_check<'a>(report: &'a Report, id: &str) -> &'a CheckResult {
        report
            .categories
            .iter()
            .flat_map(|c| &c.checks)
            .find(|c| c.id() == id)
            .unwrap_or_else(|| panic!("check {id} not present in report"))
    }

    #[test]
    fn healthy_env_produces_only_pass_checks() {
        let r = build_report(&DoctorEnv::healthy());
        for check in r.categories.iter().flat_map(|c| &c.checks) {
            assert_eq!(
                check.severity(),
                Severity::Pass,
                "check {} should be PASS in healthy env, got {:?}",
                check.id(),
                check.severity(),
            );
        }
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn report_categories_run_in_spec_order() {
        let r = build_report(&DoctorEnv::healthy());
        let names: Vec<_> = r.categories.iter().map(|c| c.name).collect();
        // Spec §Check ordering pins Environment → Config → Claude
        // Code → … → Self. As each category lands, slot it here so a
        // refactor that swaps order trips this test rather than
        // surfacing as a confusing report layout.
        assert_eq!(
            names,
            vec![
                "Environment",
                "Config",
                "Claude Code",
                "Credentials",
                "Cache",
                "Rate-limit endpoint",
                "Plugins",
                "Git",
                "Self",
            ],
        );
    }

    #[test]
    fn home_unset_fails_and_promotes_exit_code() {
        let mut env = DoctorEnv::healthy();
        env.home_env = EnvVarState::Unset;
        let r = build_report(&env);
        let home = find_check(&r, "env.home");
        assert_eq!(home.severity(), Severity::Fail);
        assert!(home.hint().unwrap().contains("$HOME"));
        assert_eq!(r.exit_code(), 1);
    }

    #[test]
    fn home_empty_string_fails() {
        // Empty $HOME is the same shape as missing — `dirs::home_dir`
        // returns None on Unix when $HOME is empty, and the check
        // mirrors that.
        let mut env = DoctorEnv::healthy();
        env.home_env = EnvVarState::Set(String::new());
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.home").severity(), Severity::Fail);
    }

    #[test]
    fn home_non_utf8_fails_with_distinct_hint() {
        // Critical: the user-facing hint must NOT say "$HOME is unset"
        // when $HOME is in fact set but unreadable. Misleading
        // remediation makes the user fight a phantom problem.
        let mut env = DoctorEnv::healthy();
        env.home_env = EnvVarState::NonUtf8("/home/\u{FFFD}".to_string());
        let r = build_report(&env);
        let home = find_check(&r, "env.home");
        assert_eq!(home.severity(), Severity::Fail);
        assert!(
            home.label().contains("UTF-8"),
            "label should mention UTF-8: {}",
            home.label()
        );
        assert!(
            home.hint().unwrap().contains("UTF-8") || home.hint().unwrap().contains("rewrite"),
            "hint should point at the real fix: {:?}",
            home.hint()
        );
    }

    #[test]
    fn no_color_set_or_unset_both_pass() {
        for no_color in [true, false] {
            let mut env = DoctorEnv::healthy();
            env.no_color = no_color;
            let r = build_report(&env);
            assert_eq!(find_check(&r, "env.no_color").severity(), Severity::Pass);
        }
    }

    #[test]
    fn term_dumb_warns_not_fails() {
        let mut env = DoctorEnv::healthy();
        env.term = EnvVarState::Set("dumb".to_string());
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.term").severity(), Severity::Warn);
    }

    #[test]
    fn term_unset_warns() {
        let mut env = DoctorEnv::healthy();
        env.term = EnvVarState::Unset;
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.term").severity(), Severity::Warn);
    }

    #[test]
    fn term_empty_warns() {
        let mut env = DoctorEnv::healthy();
        env.term = EnvVarState::Set(String::new());
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.term").severity(), Severity::Warn);
    }

    #[test]
    fn term_non_utf8_warns_with_distinct_hint() {
        let mut env = DoctorEnv::healthy();
        env.term = EnvVarState::NonUtf8("xterm-\u{FFFD}".to_string());
        let r = build_report(&env);
        let term = find_check(&r, "env.term");
        assert_eq!(term.severity(), Severity::Warn);
        assert!(term.label().contains("UTF-8"));
    }

    #[test]
    fn stdout_not_a_tty_warns_not_fails() {
        // Critical contract: piped/CI stdout is WARN, never FAIL, so
        // `linesmith doctor --plain | tee log.txt` in CI doesn't gate
        // exit-1. Per spec §Cross-category short-circuits.
        let mut env = DoctorEnv::healthy();
        env.stdout_is_terminal = false;
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.stdout_tty").severity(), Severity::Warn);
        assert_eq!(r.exit_code(), 0, "non-tty must not promote exit code");
    }

    #[test]
    fn terminal_width_unknown_warns() {
        let mut env = DoctorEnv::healthy();
        env.terminal_width_cells = None;
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "env.terminal_width").severity(),
            Severity::Warn
        );
    }

    #[test]
    fn terminal_width_under_threshold_warns() {
        // Spec §Environment: Some((W, _)) with W < 40 → WARN.
        let mut env = DoctorEnv::healthy();
        env.terminal_width_cells = Some(39);
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "env.terminal_width").severity(),
            Severity::Warn
        );
    }

    #[test]
    fn terminal_width_at_threshold_passes() {
        let mut env = DoctorEnv::healthy();
        env.terminal_width_cells = Some(40);
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "env.terminal_width").severity(),
            Severity::Pass
        );
    }

    #[test]
    fn terminal_width_zero_warns_with_distinct_hint() {
        // 0 cells is qualitatively different from "narrow" — it's a
        // driver / terminfo bug. The hint must point at the terminal
        // emulator, not "set $COLUMNS".
        let mut env = DoctorEnv::healthy();
        env.terminal_width_cells = Some(0);
        let r = build_report(&env);
        let w = find_check(&r, "env.terminal_width");
        assert_eq!(w.severity(), Severity::Warn);
        let hint = w.hint().unwrap();
        assert!(
            hint.contains("terminal emulator") || hint.contains("driver"),
            "hint should distinguish driver bug from narrow width: {hint}"
        );
    }

    #[test]
    fn binary_path_resolves_passes() {
        let env = DoctorEnv::healthy();
        let r = build_report(&env);
        let bin = find_check(&r, "self.binary_path");
        assert_eq!(bin.severity(), Severity::Pass);
        assert!(bin.label().contains("Binary"));
    }

    #[test]
    fn binary_path_failure_preserves_io_error_in_label() {
        // Generic "current_exe failed" hides the cause. The label
        // must include the underlying io::Error so a user sees
        // whether it's permission-denied vs broken-symlink etc.
        let mut env = DoctorEnv::healthy();
        env.current_exe = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "no access to /proc/self/exe",
        ));
        let r = build_report(&env);
        let bin = find_check(&r, "self.binary_path");
        assert_eq!(bin.severity(), Severity::Warn);
        assert!(
            bin.label().contains("no access to /proc/self/exe"),
            "io::Error message must surface in label: {}",
            bin.label()
        );
    }

    #[test]
    fn self_version_check_includes_crate_version() {
        let r = build_report(&DoctorEnv::healthy());
        let v = find_check(&r, "self.version");
        assert_eq!(v.severity(), Severity::Pass);
        assert!(v.label().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn self_update_available_pass_when_probe_is_latest() {
        let r = build_report(&DoctorEnv::healthy());
        let u = find_check(&r, "self.update_available");
        assert_eq!(u.severity(), Severity::Pass);
        assert!(
            u.label().contains("latest"),
            "label should mention 'latest': {}",
            u.label()
        );
    }

    #[test]
    fn self_update_available_warns_when_newer_release_exists() {
        let mut env = DoctorEnv::healthy();
        env.update_probe = DoctorUpdateProbe::Newer {
            latest: "v99.0.0".to_string(),
        };
        let r = build_report(&env);
        let u = find_check(&r, "self.update_available");
        assert_eq!(u.severity(), Severity::Warn);
        assert!(u.label().contains("v99.0.0"), "label: {}", u.label());
        assert!(
            u.hint().is_some_and(|h| h.contains("brew upgrade")),
            "hint should reference upgrade path: {:?}",
            u.hint()
        );
        assert_eq!(
            r.exit_code(),
            0,
            "newer-release WARN must not promote exit code"
        );
    }

    #[test]
    fn self_update_available_warns_on_transport_error() {
        let mut env = DoctorEnv::healthy();
        env.update_probe = DoctorUpdateProbe::TransportError {
            message: "dns failure".to_string(),
        };
        let r = build_report(&env);
        let u = find_check(&r, "self.update_available");
        assert_eq!(u.severity(), Severity::Warn);
        assert!(u.label().contains("dns failure"), "label: {}", u.label());
        assert_eq!(
            r.exit_code(),
            0,
            "offline / transport error must not gate exit-1"
        );
    }

    #[test]
    fn self_update_available_warns_on_parse_error() {
        let mut env = DoctorEnv::healthy();
        env.update_probe = DoctorUpdateProbe::ParseError {
            message: "missing tag_name".to_string(),
        };
        let r = build_report(&env);
        let u = find_check(&r, "self.update_available");
        assert_eq!(u.severity(), Severity::Warn);
        assert!(
            u.hint().is_some_and(|h| h.contains("file a linesmith bug")),
            "parse-error hint should point at filing a bug: {:?}",
            u.hint()
        );
    }

    #[test]
    fn self_binary_integrity_passes_when_build_sha_present() {
        let r = build_report(&DoctorEnv::healthy());
        let bi = find_check(&r, "self.binary_integrity");
        assert_eq!(bi.severity(), Severity::Pass);
        // Healthy fixture sets sha = "abc1234"; label should surface
        // a short prefix so users can copy it into bug reports.
        assert!(
            bi.label().contains("abc1234"),
            "label should include the SHA: {}",
            bi.label()
        );
    }

    #[test]
    fn self_binary_integrity_warns_when_build_sha_missing() {
        let mut env = DoctorEnv::healthy();
        env.binary_build_sha = None;
        let r = build_report(&env);
        let bi = find_check(&r, "self.binary_integrity");
        assert_eq!(bi.severity(), Severity::Warn);
        assert!(
            bi.hint().is_some_and(|h| h.contains("reinstall")),
            "hint should suggest reinstalling: {:?}",
            bi.hint()
        );
        assert_eq!(
            r.exit_code(),
            0,
            "missing build metadata is WARN, must not gate exit-1"
        );
    }

    #[test]
    fn self_binary_integrity_warns_on_empty_sha_same_as_missing() {
        // `LINESMITH_BUILD_SHA=""` (a CI mistake — env var exported
        // but never set) is NOT traceability. Without this guard the
        // PASS line would render as "Built from  (linesmith …)" with
        // a tell-tale double space and no commit. Fold into the same
        // WARN as `None` so the WARN-vs-PASS boundary tracks the
        // spec's "matches build metadata" contract instead of
        // `Option::is_some`.
        let mut env = DoctorEnv::healthy();
        env.binary_build_sha = Some("");
        let r = build_report(&env);
        let bi = find_check(&r, "self.binary_integrity");
        assert_eq!(
            bi.severity(),
            Severity::Warn,
            "empty SHA must WARN, not PASS; got label {:?}",
            bi.label()
        );
    }

    #[test]
    fn self_binary_integrity_warns_on_whitespace_only_sha() {
        // CI pipelines that do `LINESMITH_BUILD_SHA=$(some-cmd)`
        // where some-cmd outputs only whitespace (newline-only
        // git output, broken trim, etc.) must NOT render as PASS
        // with a "Built from   " double-space line. Same WARN class
        // as the empty-string fix.
        for blank in [" ", "\t", "\n", "  \t\n  "] {
            let mut env = DoctorEnv::healthy();
            env.binary_build_sha = Some(blank);
            let r = build_report(&env);
            let bi = find_check(&r, "self.binary_integrity");
            assert_eq!(
                bi.severity(),
                Severity::Warn,
                "whitespace-only SHA {blank:?} must WARN; got label {:?}",
                bi.label()
            );
        }
    }

    #[test]
    fn self_binary_integrity_handles_multibyte_sha_without_panicking() {
        // Defensive UTF-8 boundary check: `sha.get(..n)` returns
        // `None` (rather than panicking) when `n` lands inside a
        // codepoint, and the `unwrap_or(sha)` arm catches that case.
        // Pin the contract so a future "simplification" to
        // `&sha[..7]` (which DOES panic on a codepoint boundary)
        // gets caught.
        let mut env = DoctorEnv::healthy();
        // "é" is 2 bytes; "é12" has len() = 4. `..min(7)` = `..4`
        // happens to fit, but `..3` would land inside "é".
        env.binary_build_sha = Some("é12");
        let r = build_report(&env);
        let bi = find_check(&r, "self.binary_integrity");
        assert_eq!(bi.severity(), Severity::Pass);
    }

    #[test]
    fn self_binary_integrity_truncates_long_sha_to_seven_chars() {
        // Real git SHAs are 40 hex chars; the renderer should show
        // only the conventional 7-char prefix so the label stays
        // tight at the standard tree width.
        let mut env = DoctorEnv::healthy();
        env.binary_build_sha = Some("0123456789abcdef0123456789abcdef01234567");
        let r = build_report(&env);
        let bi = find_check(&r, "self.binary_integrity");
        // Pin the boundary exactly: the rendered prefix is "0123456 "
        // (7 chars then a space), not 6 ("012345 ") or 8 ("01234567").
        assert!(
            bi.label().contains("0123456 (linesmith"),
            "expected exactly 7-char prefix followed by ' (linesmith': {}",
            bi.label()
        );
        assert!(
            !bi.label().contains("01234567"),
            "label must stop at 7 chars, not include the 8th: {}",
            bi.label()
        );
    }

    #[test]
    fn self_binary_integrity_handles_short_sha_without_panicking_and_passes() {
        // Defensive: if a build script supplies a SHA shorter than
        // 7 chars (some test pipelines do), the truncation slice
        // must not panic AND the check must still PASS — short SHAs
        // are still traceability, just less of it.
        let mut env = DoctorEnv::healthy();
        env.binary_build_sha = Some("abc");
        let r = build_report(&env);
        let bi = find_check(&r, "self.binary_integrity");
        assert_eq!(bi.severity(), Severity::Pass, "short SHA must still PASS");
        assert!(bi.label().contains("abc"));
    }

    #[test]
    fn doctor_env_from_process_does_not_panic() {
        // Smoke: the production env-snapshot path must not panic
        // regardless of the host's env state. (The actual values are
        // host-dependent so we don't assert on them.) Pass `None`
        // since we don't want to read a real --config off disk.
        let _ = DoctorEnv::from_process(None);
    }

    #[test]
    fn env_var_state_nonempty_filters_unset_empty_and_nonutf8() {
        assert_eq!(EnvVarState::Unset.nonempty(), None);
        assert_eq!(EnvVarState::Set(String::new()).nonempty(), None);
        assert_eq!(
            EnvVarState::NonUtf8("garbage".into()).nonempty(),
            None,
            "non-UTF-8 must not surface as Some — caller would treat the lossy preview as the real value"
        );
        assert_eq!(EnvVarState::Set("x".into()).nonempty(), Some("x"));
    }

    #[test]
    fn check_ids_follow_namespacing_convention() {
        // Spec §JSON output: ids are <category>.<check_name> in
        // snake_case. Extend the prefix allowlist as new categories
        // ship per spec §Check catalog — this test is a tripwire,
        // not a free pass.
        let r = build_report(&DoctorEnv::healthy());
        for check in r.categories.iter().flat_map(|c| &c.checks) {
            let id = check.id();
            assert!(id.contains('.'), "id `{id}` missing dotted namespace",);
            let prefix = id.split('.').next().unwrap();
            assert!(
                matches!(
                    prefix,
                    "env"
                        | "config"
                        | "claude"
                        | "creds"
                        | "cache"
                        | "endpoint"
                        | "plugins"
                        | "git"
                        | "self"
                ),
                "id `{id}` has unknown category prefix `{prefix}`",
            );
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c == '.'),
                "id `{id}` not snake_case",
            );
        }
    }

    // --- Config category ---

    use crate::config::{Config, ConfigPath, LineConfig};

    fn config_snapshot_loaded(config: Config) -> DoctorConfigSnapshot {
        let path = PathBuf::from("/home/user/.config/linesmith/config.toml");
        DoctorConfigSnapshot {
            cli_override: None,
            resolved: Some(ConfigPath {
                path: path.clone(),
                explicit: false,
            }),
            read: ConfigReadOutcome::Loaded {
                path,
                config: Box::new(config),
            },
            plugin_dirs: Vec::new(),
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        }
    }

    #[test]
    fn config_category_with_default_loaded_config_passes_every_check() {
        let r = build_report(&DoctorEnv::healthy());
        let category = r
            .categories
            .iter()
            .find(|c| c.name == "Config")
            .expect("Config category present");
        for check in &category.checks {
            assert_eq!(
                check.severity(),
                Severity::Pass,
                "{} should be PASS in healthy env, got {:?}",
                check.id(),
                check.severity(),
            );
        }
    }

    #[test]
    fn config_category_emits_five_checks_in_spec_order() {
        // Spec §Config table: Config file discovered → Config parses
        // → All referenced segments exist → Theme is installed →
        // Plugin dirs are readable. Order matters because the parse
        // short-circuit propagates SKIPs downstream.
        let r = build_report(&DoctorEnv::healthy());
        let ids: Vec<_> = r
            .categories
            .iter()
            .find(|c| c.name == "Config")
            .expect("Config category present")
            .checks
            .iter()
            .map(|c| c.id())
            .collect();
        assert_eq!(
            ids,
            vec![
                "config.discovered",
                "config.parses",
                "config.segments_resolvable",
                "config.theme_installed",
                "config.plugin_dirs_readable",
            ]
        );
    }

    #[test]
    fn config_runs_when_home_unset_but_explicit_path_loaded() {
        // Spec §Cross-category short-circuits Config carve-out: when
        // `--config` / `$LINESMITH_CONFIG` / `$XDG_CONFIG_HOME` has
        // already resolved a path, run the checks against it even if
        // `$HOME` is unresolved. Gating on `$HOME` would silently
        // SKIP a config the user explicitly named — exactly the
        // false-negative the carve-out exists to prevent.
        let mut env = DoctorEnv::healthy();
        env.home_env = EnvVarState::Unset;
        // healthy()'s config snapshot is Loaded, simulating a
        // successful `--config /tmp/foo.toml` resolution.
        let r = build_report(&env);
        let category = r.categories.iter().find(|c| c.name == "Config").unwrap();
        for check in &category.checks {
            assert_eq!(
                check.severity(),
                Severity::Pass,
                "{} should PASS; $HOME unset must not gate explicit overrides",
                check.id(),
            );
        }
    }

    #[test]
    fn config_unresolved_warns_and_skips_downstream() {
        // The genuine "no config path source" case — discovery
        // WARNs (using built-in defaults is a normal first-run
        // state, not a failure); the four downstream checks SKIP
        // because there's nothing to load. This test pins the
        // Config-category-only behavior; `env.home_env` stays Set
        // so the Environment category's exit code doesn't bleed
        // into the assertion.
        let mut env = DoctorEnv::healthy();
        env.config = DoctorConfigSnapshot {
            cli_override: None,
            resolved: None,
            read: ConfigReadOutcome::Unresolved,
            plugin_dirs: Vec::new(),
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        };
        let r = build_report(&env);
        let discovered = find_check(&r, "config.discovered");
        assert_eq!(discovered.severity(), Severity::Warn);
        assert!(
            discovered.label().contains("No config path resolved"),
            "label should describe the unresolved cascade: {}",
            discovered.label(),
        );
        for id in [
            "config.parses",
            "config.segments_resolvable",
            "config.theme_installed",
            "config.plugin_dirs_readable",
        ] {
            assert_eq!(
                find_check(&r, id).severity(),
                Severity::Skip,
                "{id} should SKIP when no config is loaded",
            );
        }
        assert_eq!(r.exit_code(), 0, "no config is WARN, not FAIL");
    }

    #[test]
    fn config_segments_pass_for_plugin_discovered_id() {
        // Without `known_segment_ids` carrying plugin ids, doctor
        // WARNs on every legitimate plugin segment — a real
        // false-negative for any plugin user. Snapshot-as-source-
        // of-truth means a plugin that compiles registers its id
        // and the check passes.
        let config = Config {
            line: Some(LineConfig {
                segments: vec!["my_plugin".into(), "model".into()],
                ..LineConfig::default()
            }),
            ..Config::default()
        };
        let mut snapshot = config_snapshot_loaded(config);
        snapshot.known_segment_ids.insert("my_plugin".to_string());
        let mut env = DoctorEnv::healthy();
        env.config = snapshot;
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "config.segments_resolvable").severity(),
            Severity::Pass,
        );
    }

    #[test]
    fn config_theme_pass_for_user_theme_in_registry() {
        // User-installed themes (loaded by `ThemeRegistry::with_user_themes`)
        // must not WARN. Snapshot the name, the check resolves it.
        let config = Config {
            theme: Some("neon".to_string()),
            ..Config::default()
        };
        let mut snapshot = config_snapshot_loaded(config);
        snapshot.known_theme_names.insert("neon".to_string());
        let mut env = DoctorEnv::healthy();
        env.config = snapshot;
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "config.theme_installed").severity(),
            Severity::Pass,
        );
    }

    #[test]
    fn config_segments_warn_on_line_zero_index() {
        // `[line.0]` is the boundary case `key.parse::<u32>()` would
        // accept but the hint ("must be a positive integer") rejects.
        // Use `NonZeroU32` so the zero key lands in the malformed
        // bucket, matching what the user reads.
        let raw = r#"
            [line]
            segments = ["model"]

            [line.0]
            segments = ["workspace"]
        "#;
        let parsed: Config = raw.parse().expect("parse ok");
        let mut env = DoctorEnv::healthy();
        env.config = config_snapshot_loaded(parsed);
        let r = build_report(&env);
        let check = find_check(&r, "config.segments_resolvable");
        assert_eq!(
            check.severity(),
            Severity::Warn,
            "[line.0] must be flagged as malformed",
        );
        assert!(
            check.label().contains("[line.0]") && check.label().contains("positive integer"),
            "label must name the offending key and the constraint: {}",
            check.label(),
        );
    }

    #[test]
    fn config_discovered_warns_for_implicit_missing() {
        // First-run users have no config; spec §Config table maps
        // this to WARN ("none found, using built-in defaults"). FAIL
        // would gate `linesmith doctor` exit-1 behind an entirely
        // normal first-run state.
        let mut env = DoctorEnv::healthy();
        env.config = DoctorConfigSnapshot {
            cli_override: None,
            resolved: Some(ConfigPath {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                explicit: false,
            }),
            read: ConfigReadOutcome::NotFound {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                explicit: false,
            },
            plugin_dirs: Vec::new(),
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        };
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "config.discovered").severity(),
            Severity::Warn,
        );
        assert_eq!(r.exit_code(), 0, "first-run WARN must not gate exit-1");
    }

    #[test]
    fn config_discovered_fails_for_explicit_missing() {
        // The user named --config /path/to/file. Falling back to
        // defaults silently — same WARN as the implicit case —
        // would mask the typo. FAIL with a hint pointed at the
        // exact path keeps the user pointed at the real fix.
        let mut env = DoctorEnv::healthy();
        env.config = DoctorConfigSnapshot {
            cli_override: Some(PathBuf::from("/etc/linesmith/typo.toml")),
            resolved: Some(ConfigPath {
                path: PathBuf::from("/etc/linesmith/typo.toml"),
                explicit: true,
            }),
            read: ConfigReadOutcome::NotFound {
                path: PathBuf::from("/etc/linesmith/typo.toml"),
                explicit: true,
            },
            plugin_dirs: Vec::new(),
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        };
        let r = build_report(&env);
        let discovered = find_check(&r, "config.discovered");
        assert_eq!(discovered.severity(), Severity::Fail);
        assert!(
            discovered.label().contains("/etc/linesmith/typo.toml"),
            "label must name the missing path: {}",
            discovered.label(),
        );
        assert_eq!(r.exit_code(), 1);
    }

    #[test]
    fn config_discovered_fails_on_io_error() {
        // Permission denied / is-a-directory / etc. is a real
        // problem — fall back to defaults silently and the user
        // never learns why their config doesn't apply.
        let mut env = DoctorEnv::healthy();
        env.config = DoctorConfigSnapshot {
            cli_override: None,
            resolved: Some(ConfigPath {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                explicit: false,
            }),
            read: ConfigReadOutcome::IoError {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                message: "Permission denied (os error 13)".to_string(),
            },
            plugin_dirs: Vec::new(),
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        };
        let r = build_report(&env);
        let discovered = find_check(&r, "config.discovered");
        assert_eq!(discovered.severity(), Severity::Fail);
        assert!(
            discovered.label().contains("Permission denied"),
            "io::Error message must surface in label: {}",
            discovered.label(),
        );
    }

    #[test]
    fn config_parse_error_fails_with_line_col() {
        // Spec hint is "see line/column in the error for the invalid
        // key". toml::de::Error.to_string() already names line/col, so
        // surfacing it verbatim covers the contract; a reformat that
        // strips the message would silently break it.
        let mut env = DoctorEnv::healthy();
        env.config = DoctorConfigSnapshot {
            cli_override: None,
            resolved: Some(ConfigPath {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                explicit: false,
            }),
            read: ConfigReadOutcome::ParseError {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                message: "TOML parse error at line 3, column 7".to_string(),
            },
            plugin_dirs: Vec::new(),
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        };
        let r = build_report(&env);
        let parses = find_check(&r, "config.parses");
        assert_eq!(parses.severity(), Severity::Fail);
        assert!(
            parses.hint().unwrap().contains("line 3, column 7"),
            "hint should carry the parser's line/col: {:?}",
            parses.hint(),
        );
    }

    #[test]
    fn config_parse_error_short_circuits_downstream_checks_to_skip() {
        // Spec §Cross-category short-circuits: parse fail → skip
        // segment / theme / plugin-dirs. Without this, doctor would
        // try to interpret a half-parsed config and surface
        // distracting / wrong WARNs alongside the real FAIL.
        let mut env = DoctorEnv::healthy();
        env.config = DoctorConfigSnapshot {
            cli_override: None,
            resolved: Some(ConfigPath {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                explicit: false,
            }),
            read: ConfigReadOutcome::ParseError {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                message: "broken".to_string(),
            },
            plugin_dirs: Vec::new(),
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        };
        let r = build_report(&env);
        for id in [
            "config.segments_resolvable",
            "config.theme_installed",
            "config.plugin_dirs_readable",
        ] {
            assert_eq!(
                find_check(&r, id).severity(),
                Severity::Skip,
                "{id} must SKIP when parse fails",
            );
        }
    }

    #[test]
    fn config_segments_pass_when_every_id_is_a_known_built_in() {
        let config = Config {
            line: Some(LineConfig {
                segments: vec!["model".into(), "workspace".into(), "git_branch".into()],
                ..LineConfig::default()
            }),
            ..Config::default()
        };
        let mut env = DoctorEnv::healthy();
        env.config = config_snapshot_loaded(config);
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "config.segments_resolvable").severity(),
            Severity::Pass,
        );
    }

    #[test]
    fn config_segments_warn_on_unknown_id_with_id_in_label() {
        let config = Config {
            line: Some(LineConfig {
                segments: vec!["model".into(), "not_a_real_segment".into()],
                ..LineConfig::default()
            }),
            ..Config::default()
        };
        let mut env = DoctorEnv::healthy();
        env.config = config_snapshot_loaded(config);
        let r = build_report(&env);
        let check = find_check(&r, "config.segments_resolvable");
        assert_eq!(check.severity(), Severity::Warn);
        assert!(
            check.label().contains("not_a_real_segment"),
            "label must name the unknown id so the user can fix it: {}",
            check.label(),
        );
    }

    #[test]
    fn config_segments_warn_on_unknown_id_in_numbered_line_table() {
        // [line.1] / [line.2] segment ids must be validated too —
        // multi-line layouts hit the same plugin-id-typo problem
        // single-line does, and skipping them would create a doctor
        // blind spot for any user on multi-line.
        let raw = r#"
            [line]
            segments = ["model"]

            [line.1]
            segments = ["workspace", "bogus_id"]
        "#;
        let parsed: Config = raw.parse().expect("parse ok");
        let mut env = DoctorEnv::healthy();
        env.config = config_snapshot_loaded(parsed);
        let r = build_report(&env);
        let check = find_check(&r, "config.segments_resolvable");
        assert_eq!(check.severity(), Severity::Warn);
        assert!(
            check.label().contains("bogus_id"),
            "label must name the unknown numbered-line id: {}",
            check.label(),
        );
    }

    #[test]
    fn config_segments_warn_on_malformed_numbered_line_table() {
        // `[line.foo]` (non-integer key) and `[line.1] segments = "x"`
        // (non-array) are runtime-warning cases in the builder; doctor
        // surfaces them up-front so a user fixing config doesn't have
        // to spelunk through a debug log to find them.
        let raw = r#"
            [line]
            segments = ["model"]

            [line.foo]
            segments = ["workspace"]
        "#;
        let parsed: Config = raw.parse().expect("parse ok");
        let mut env = DoctorEnv::healthy();
        env.config = config_snapshot_loaded(parsed);
        let r = build_report(&env);
        let check = find_check(&r, "config.segments_resolvable");
        assert_eq!(check.severity(), Severity::Warn);
        assert!(
            check.label().contains("foo"),
            "label must name the malformed line key: {}",
            check.label(),
        );
    }

    #[test]
    fn config_theme_pass_when_omitted_or_built_in() {
        let mut env = DoctorEnv::healthy();
        env.config = config_snapshot_loaded(Config::default());
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "config.theme_installed").severity(),
            Severity::Pass,
        );

        // Pick a known built-in theme. `default` always exists; the
        // builtin_names() list defines the source of truth.
        let theme_name = crate::theme::builtin_names()
            .next()
            .expect("at least one built-in theme")
            .to_string();
        let config = Config {
            theme: Some(theme_name),
            ..Config::default()
        };
        let mut env = DoctorEnv::healthy();
        env.config = config_snapshot_loaded(config);
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "config.theme_installed").severity(),
            Severity::Pass,
        );
    }

    #[test]
    fn config_theme_warn_on_unknown_name() {
        let config = Config {
            theme: Some("not-a-theme".to_string()),
            ..Config::default()
        };
        let mut env = DoctorEnv::healthy();
        env.config = config_snapshot_loaded(config);
        let r = build_report(&env);
        let check = find_check(&r, "config.theme_installed");
        assert_eq!(check.severity(), Severity::Warn);
        assert!(
            check.label().contains("not-a-theme"),
            "label must name the unknown theme: {}",
            check.label(),
        );
    }

    #[test]
    fn config_plugin_dirs_pass_when_none_configured() {
        let mut env = DoctorEnv::healthy();
        env.config = config_snapshot_loaded(Config::default());
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "config.plugin_dirs_readable").severity(),
            Severity::Pass,
        );
    }

    #[test]
    fn config_plugin_dirs_warn_on_missing_or_not_directory() {
        let mut env = DoctorEnv::healthy();
        env.config = DoctorConfigSnapshot {
            cli_override: None,
            resolved: Some(ConfigPath {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                explicit: false,
            }),
            read: ConfigReadOutcome::Loaded {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                config: Box::new(Config::default()),
            },
            plugin_dirs: vec![
                PluginDirStatus {
                    path: PathBuf::from("/etc/linesmith/plugins"),
                    state: PluginDirState::Missing,
                },
                PluginDirStatus {
                    path: PathBuf::from("/etc/linesmith/plugins.toml"),
                    state: PluginDirState::NotADirectory,
                },
            ],
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        };
        let r = build_report(&env);
        let check = find_check(&r, "config.plugin_dirs_readable");
        assert_eq!(check.severity(), Severity::Warn);
        assert!(check.label().contains("/etc/linesmith/plugins"));
        assert_eq!(r.exit_code(), 0, "missing dirs are WARN, not FAIL");
    }

    #[test]
    fn config_plugin_dirs_fail_on_permission_denied() {
        // Permission denied beats the WARN cases — the user can't
        // ignore it and a silent fallback would hide the cause of
        // missing plugin segments at runtime.
        let mut env = DoctorEnv::healthy();
        env.config = DoctorConfigSnapshot {
            cli_override: None,
            resolved: Some(ConfigPath {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                explicit: false,
            }),
            read: ConfigReadOutcome::Loaded {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                config: Box::new(Config::default()),
            },
            plugin_dirs: vec![
                PluginDirStatus {
                    path: PathBuf::from("/root/secret-plugins"),
                    state: PluginDirState::PermissionDenied {
                        message: "permission denied".to_string(),
                    },
                },
                // A WARN-class entry alongside should not downgrade
                // the FAIL — prove FAIL beats WARN in the aggregator.
                PluginDirStatus {
                    path: PathBuf::from("/etc/missing"),
                    state: PluginDirState::Missing,
                },
            ],
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        };
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "config.plugin_dirs_readable").severity(),
            Severity::Fail,
        );
        assert_eq!(r.exit_code(), 1);
    }

    #[test]
    fn config_plugin_dirs_pass_when_all_ok() {
        let mut env = DoctorEnv::healthy();
        env.config = DoctorConfigSnapshot {
            cli_override: None,
            resolved: Some(ConfigPath {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                explicit: false,
            }),
            read: ConfigReadOutcome::Loaded {
                path: PathBuf::from("/home/user/.config/linesmith/config.toml"),
                config: Box::new(Config::default()),
            },
            plugin_dirs: vec![PluginDirStatus {
                path: PathBuf::from("/home/user/.config/linesmith/plugins"),
                state: PluginDirState::Ok,
            }],
            known_segment_ids: DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: DoctorConfigSnapshot::built_in_theme_names(),
        };
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "config.plugin_dirs_readable").severity(),
            Severity::Pass,
        );
    }

    #[test]
    fn read_config_at_round_trips_a_real_temp_file() {
        // Smoke for the production read+parse helper. A tempfile
        // here is the only way to assert the IoError vs ParseError
        // vs Loaded distinction without relying on a host config.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let valid = tempdir.path().join("valid.toml");
        std::fs::write(&valid, r#"theme = "default""#).expect("write");
        let cp = ConfigPath {
            path: valid.clone(),
            explicit: false,
        };
        match read_config_at(&cp) {
            ConfigReadOutcome::Loaded { config, path } => {
                assert_eq!(path, valid);
                assert_eq!(config.theme.as_deref(), Some("default"));
            }
            other => panic!("expected Loaded, got {other:?}"),
        }

        let parse_err = tempdir.path().join("invalid.toml");
        std::fs::write(&parse_err, "this = is not = valid toml").expect("write");
        let cp = ConfigPath {
            path: parse_err.clone(),
            explicit: false,
        };
        match read_config_at(&cp) {
            ConfigReadOutcome::ParseError { path, message } => {
                assert_eq!(path, parse_err);
                assert!(!message.is_empty(), "parse error message must be populated");
            }
            other => panic!("expected ParseError, got {other:?}"),
        }

        let missing = tempdir.path().join("nope.toml");
        let cp = ConfigPath {
            path: missing.clone(),
            explicit: true,
        };
        match read_config_at(&cp) {
            ConfigReadOutcome::NotFound { path, explicit } => {
                assert_eq!(path, missing);
                assert!(explicit, "explicit flag must propagate");
            }
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn stat_plugin_dirs_distinguishes_ok_missing_and_not_a_directory() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let dir = tempdir.path().join("dir");
        std::fs::create_dir(&dir).expect("mkdir");
        let file = tempdir.path().join("file");
        std::fs::write(&file, "").expect("touch");
        let missing = tempdir.path().join("nope");

        let statuses = stat_plugin_dirs(&[dir.clone(), file.clone(), missing.clone()]);
        assert_eq!(statuses.len(), 3);
        assert!(matches!(statuses[0].state, PluginDirState::Ok));
        assert!(matches!(statuses[1].state, PluginDirState::NotADirectory));
        assert!(matches!(statuses[2].state, PluginDirState::Missing));
    }

    // --- Claude Code category ---

    fn claude_home_state(
        dir: ClaudeDirState,
        claude_json: ClaudeJsonState,
        sessions: ClaudeSessionsState,
    ) -> ClaudeHomeState {
        ClaudeHomeState {
            dir,
            claude_json,
            sessions,
        }
    }

    fn with_claude_snapshot(snapshot: DoctorClaudeCodeSnapshot) -> DoctorEnv {
        let mut env = DoctorEnv::healthy();
        env.claude_code = snapshot;
        env
    }

    #[test]
    fn claude_code_category_emits_four_checks_in_spec_order() {
        // Spec §Claude Code table order: binary → dir → json →
        // sessions. Order matters because the dir-missing
        // short-circuit only triggers on the row above sessions.
        let r = build_report(&DoctorEnv::healthy());
        let ids: Vec<_> = r
            .categories
            .iter()
            .find(|c| c.name == "Claude Code")
            .expect("Claude Code category present")
            .checks
            .iter()
            .map(|c| c.id())
            .collect();
        assert_eq!(
            ids,
            vec![
                "claude.binary_found",
                "claude.dir",
                "claude.json_parses",
                "claude.sessions_recorded",
            ]
        );
    }

    #[test]
    fn claude_code_category_with_healthy_snapshot_passes_every_check() {
        let r = build_report(&DoctorEnv::healthy());
        let category = r
            .categories
            .iter()
            .find(|c| c.name == "Claude Code")
            .unwrap();
        for check in &category.checks {
            assert_eq!(
                check.severity(),
                Severity::Pass,
                "{} should PASS in healthy env, got {:?}",
                check.id(),
                check.severity(),
            );
        }
    }

    #[test]
    fn claude_code_category_skips_filesystem_checks_when_home_unresolved() {
        // Spec §Cross-category short-circuits: $HOME unresolved →
        // Claude Code SKIPs (no override sources for ~/.claude/).
        // The binary check still runs because $PATH resolution is
        // independent of $HOME.
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: None,
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "claude.binary_found").severity(),
            Severity::Pass
        );
        for id in [
            "claude.dir",
            "claude.json_parses",
            "claude.sessions_recorded",
        ] {
            assert_eq!(
                find_check(&r, id).severity(),
                Severity::Skip,
                "{id} should SKIP when $HOME unresolved",
            );
        }
    }

    #[test]
    fn claude_binary_warns_when_dir_exists_but_path_lookup_fails() {
        // Spec WARN row: "Not found, but `~/.claude/` exists
        // anyway." The user has used Claude Code before but the
        // launcher fell off `$PATH` (pkg manager glitch, env
        // wipe). FAIL would over-state the problem when the user's
        // state is recoverable without reinstalling.
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: None,
            path_env: EnvVarState::Set("/usr/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::Ok,
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        let binary = find_check(&r, "claude.binary_found");
        assert_eq!(binary.severity(), Severity::Warn);
        assert!(
            binary.label().contains("$PATH"),
            "label should mention $PATH so the user knows where to look: {}",
            binary.label(),
        );
    }

    #[test]
    fn claude_binary_fails_when_neither_binary_nor_dir_present() {
        // Spec FAIL row: "Neither binary nor `~/.claude/` present."
        // Claude Code was never installed, or both surfaces were
        // wiped — the right hint points at the install page.
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: None,
            path_env: EnvVarState::Set("/usr/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Missing,
                ClaudeJsonState::Missing,
                ClaudeSessionsState::Missing,
            )),
        });
        let r = build_report(&env);
        let binary = find_check(&r, "claude.binary_found");
        assert_eq!(binary.severity(), Severity::Fail);
        assert!(
            binary.hint().unwrap().contains("install Claude Code"),
            "hint should point at the install page: {:?}",
            binary.hint(),
        );
    }

    #[test]
    fn claude_dir_warns_on_permission_denied() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::PermissionDenied {
                    message: "Permission denied (os error 13)".to_string(),
                },
                ClaudeJsonState::Ok,
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        let dir = find_check(&r, "claude.dir");
        assert_eq!(dir.severity(), Severity::Warn);
        assert!(
            dir.label().contains("Permission denied"),
            "label should surface the os error: {}",
            dir.label(),
        );
    }

    #[test]
    fn claude_dir_missing_short_circuits_sessions_to_skip() {
        // Spec §Cross-category short-circuits: `~/.claude/` missing
        // → Recent sessions SKIP. Without this propagation, doctor
        // would emit a redundant FAIL on sessions for the same
        // root cause (no claude dir at all).
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: None,
            path_env: EnvVarState::Set("/usr/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Missing,
                ClaudeJsonState::Missing,
                // Sessions state is Missing too in reality, but
                // the short-circuit should SKIP regardless of this
                // value — pin the gating logic explicitly.
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        let sessions = find_check(&r, "claude.sessions_recorded");
        assert_eq!(sessions.severity(), Severity::Skip);
        assert!(
            sessions.hint().unwrap().contains("`~/.claude/` missing"),
            "skip reason should name the upstream cause: {:?}",
            sessions.hint(),
        );
    }

    #[test]
    fn claude_json_warn_when_oauth_account_missing() {
        // Spec WARN row: "JSON valid but `oauthAccount` missing".
        // The hint says regenerate via `claude` — the JSON is
        // structurally fine but the user isn't logged in.
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::NoOauthAccount,
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        let json = find_check(&r, "claude.json_parses");
        assert_eq!(json.severity(), Severity::Warn);
        assert!(
            json.hint().unwrap().contains("oauthAccount")
                || json.hint().unwrap().contains("log in"),
            "hint should explain the oauth gap: {:?}",
            json.hint(),
        );
    }

    #[test]
    fn claude_json_fail_on_parse_error_carries_message() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::ParseError {
                    message: "expected `:` at line 4, column 12".to_string(),
                },
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        let json = find_check(&r, "claude.json_parses");
        assert_eq!(json.severity(), Severity::Fail);
        assert!(
            json.hint().unwrap().contains("line 4, column 12"),
            "hint should carry the parser's line/col: {:?}",
            json.hint(),
        );
    }

    #[test]
    fn claude_json_fail_when_missing() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::Missing,
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "claude.json_parses").severity(),
            Severity::Fail
        );
    }

    #[test]
    fn claude_sessions_warn_when_directory_empty() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::Ok,
                ClaudeSessionsState::Empty,
            )),
        });
        let r = build_report(&env);
        let sessions = find_check(&r, "claude.sessions_recorded");
        assert_eq!(sessions.severity(), Severity::Warn);
    }

    #[test]
    fn claude_sessions_fail_when_directory_missing_but_dir_ok() {
        // The short-circuit only triggers when `~/.claude/` itself
        // is missing. Sessions can be missing while the parent dir
        // exists (user deleted just the sessions subdir).
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::Ok,
                ClaudeSessionsState::Missing,
            )),
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "claude.sessions_recorded").severity(),
            Severity::Fail,
        );
    }

    // --- Claude Code snapshot helpers ---

    /// Touch a runnable `claude` (or `claude.exe`) under `dir` and
    /// return the resulting path. On Unix, sets the executable bit
    /// — a 0644 file is now correctly ignored by `find_claude_binary`.
    fn touch_runnable_claude(dir: &Path) -> PathBuf {
        let exe_name = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        let path = dir.join(exe_name);
        std::fs::write(&path, "").expect("touch claude");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).expect("metadata").permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).expect("chmod");
        }
        path
    }

    #[test]
    fn find_claude_binary_returns_first_path_match() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let dir1 = tempdir.path().join("a");
        let dir2 = tempdir.path().join("b");
        std::fs::create_dir(&dir1).expect("mkdir a");
        std::fs::create_dir(&dir2).expect("mkdir b");
        // The binary lives in dir2 only; `find` should still
        // discover it by walking past dir1.
        let bin = touch_runnable_claude(&dir2);
        let path_var = std::env::join_paths([&dir1, &dir2])
            .expect("join_paths")
            .into_string()
            .expect("utf8");
        let found = find_claude_binary(Some(&path_var)).expect("found claude");
        assert_eq!(found, bin);
    }

    #[test]
    #[cfg(unix)]
    fn find_claude_binary_skips_non_executable_file() {
        // Contract: a 0644 regular file named `claude` in $PATH is
        // not a runnable binary. Reporting PASS would greenlight a
        // stale placeholder; the user then sees `Permission denied`
        // from the shell with no doctor diagnostic.
        use std::os::unix::fs::PermissionsExt;
        let tempdir = tempfile::tempdir().expect("tempdir");
        let dir = tempdir.path().join("a");
        std::fs::create_dir(&dir).expect("mkdir");
        let stale = dir.join("claude");
        std::fs::write(&stale, "").expect("touch");
        // Verify the fixture is actually non-executable so a future
        // umask shift can't silently make this test vacuous.
        let mode = std::fs::metadata(&stale)
            .expect("metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o111,
            0,
            "fixture must be non-executable; tempfile gave mode {mode:o}",
        );
        let path_var = dir.to_string_lossy().into_owned();
        assert!(
            find_claude_binary(Some(&path_var)).is_none(),
            "non-executable file must not be reported as the claude binary",
        );
    }

    #[test]
    fn find_claude_binary_returns_none_when_path_unset() {
        assert!(find_claude_binary(None).is_none());
        // Empty PATH entries (which arise from shell expansions
        // like `:$NEWPATH`) should not be resolved as the
        // current working directory by accident.
        assert!(find_claude_binary(Some(":")).is_none());
    }

    #[test]
    fn snapshot_claude_home_classifies_real_filesystem_state() {
        // End-to-end smoke for the helper trio. Assembles a fake
        // ~/.claude/ with a valid claude.json + sessions and asserts
        // every state-classifier picks the right variant.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let home = tempdir.path();
        std::fs::create_dir(home.join(".claude")).expect("mkdir .claude");
        std::fs::create_dir(home.join(".claude").join("sessions")).expect("mkdir sessions");
        std::fs::write(
            home.join(".claude")
                .join("sessions")
                .join("session-1.jsonl"),
            "",
        )
        .expect("touch session");
        std::fs::write(
            home.join(".claude.json"),
            r#"{ "oauthAccount": { "accountUuid": "x" } }"#,
        )
        .expect("write claude.json");

        let snapshot = snapshot_claude_home(home);
        assert!(matches!(snapshot.dir, ClaudeDirState::Ok));
        assert!(matches!(snapshot.claude_json, ClaudeJsonState::Ok));
        assert!(matches!(
            snapshot.sessions,
            ClaudeSessionsState::HasFiles { count: 1 }
        ));
    }

    #[test]
    fn read_claude_json_distinguishes_missing_parse_error_and_no_oauth() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let missing = tempdir.path().join("nope.json");
        assert!(matches!(
            read_claude_json(&missing),
            ClaudeJsonState::Missing
        ));

        let bad = tempdir.path().join("bad.json");
        std::fs::write(&bad, "{ this is not json").expect("write bad");
        assert!(matches!(
            read_claude_json(&bad),
            ClaudeJsonState::ParseError { .. }
        ));

        let no_oauth = tempdir.path().join("no_oauth.json");
        std::fs::write(&no_oauth, r#"{ "other": 1 }"#).expect("write no_oauth");
        assert!(matches!(
            read_claude_json(&no_oauth),
            ClaudeJsonState::NoOauthAccount
        ));

        let ok = tempdir.path().join("ok.json");
        std::fs::write(&ok, r#"{ "oauthAccount": null }"#).expect("write ok");
        // `oauthAccount: null` still counts as "block present" per
        // the spec — the WARN row is for the *key* being absent.
        assert!(matches!(read_claude_json(&ok), ClaudeJsonState::Ok));
    }

    #[test]
    fn read_claude_json_parse_error_contains_real_serde_format() {
        // Pin the actual format produced by `serde_json::Error::Display`
        // so the spec hint ("see line/column in the error") survives a
        // serde_json upgrade. The original test asserted a hand-rolled
        // `"line N, column M"` string with a comma, but real
        // serde_json output is `"... at line N column M"` with no
        // comma — so the prior test was vacuously passing against a
        // fixture, not the live format.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("malformed.json");
        std::fs::write(&path, "{ unbalanced").expect("write");
        match read_claude_json(&path) {
            ClaudeJsonState::ParseError { message } => {
                assert!(
                    message.contains(" at line ") && message.contains(" column "),
                    "serde_json error format changed; refresh the substring assertion: {message}",
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn read_claude_json_returns_too_large_above_2mb_cap() {
        // Spec §Edge cases (`docs/specs/doctor.md:333`): "`~/.claude.json`
        // is a 500MB file (pathological) → Cap read at 2MB; FAIL with
        // 'file too large — likely corrupt'". The doctor must fail
        // fast on oversized files rather than burn memory + time
        // parsing them. Use a 2 MB + 1 byte file (smallest case
        // that crosses the cap) to keep the test cheap.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("huge.json");
        std::fs::write(&path, vec![b'x'; 2 * 1024 * 1024 + 1]).expect("write");
        match read_claude_json(&path) {
            ClaudeJsonState::TooLarge { actual_bytes } => {
                assert_eq!(actual_bytes, 2 * 1024 * 1024 + 1);
            }
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    #[test]
    fn check_claude_json_too_large_renders_fail_with_corruption_hint() {
        let mut env = DoctorEnv::healthy();
        if let Some(home) = &mut env.claude_code.home_state {
            home.claude_json = ClaudeJsonState::TooLarge {
                actual_bytes: 500 * 1024 * 1024,
            };
        }
        let r = build_report(&env);
        let check = find_check(&r, "claude.json_parses");
        assert_eq!(check.severity(), Severity::Fail);
        assert!(
            check.label().contains("too large"),
            "label should name the cap violation: {}",
            check.label()
        );
        assert!(
            check
                .hint()
                .is_some_and(|h| h.contains("corrupt") && h.contains("claude")),
            "hint should point at corruption + remediation: {:?}",
            check.hint()
        );
    }

    #[test]
    fn stat_claude_sessions_counts_files_only_not_subdirs_or_dotfiles() {
        // Spec §Claude Code "Recent sessions recorded": "At least one file".
        // A populated-but-fileless dir (only subdirs and dotfile
        // junk like a stray `.DS_Store`) must not PASS.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let sessions = tempdir.path().join("sessions");
        std::fs::create_dir(&sessions).expect("mkdir sessions");
        std::fs::create_dir(sessions.join("archive")).expect("mkdir archive");
        // A real session file → counts toward the total.
        std::fs::write(sessions.join("session-1.jsonl"), "").expect("touch");
        match stat_claude_sessions(&sessions) {
            ClaudeSessionsState::HasFiles { count } => assert_eq!(
                count, 1,
                "subdir must not contribute to session count; got {count}",
            ),
            other => panic!("expected HasFiles, got {other:?}"),
        }
    }

    #[test]
    fn stat_claude_sessions_reports_empty_when_only_subdirs() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let sessions = tempdir.path().join("sessions");
        std::fs::create_dir(&sessions).expect("mkdir");
        std::fs::create_dir(sessions.join("staging")).expect("mkdir staging");
        assert!(matches!(
            stat_claude_sessions(&sessions),
            ClaudeSessionsState::Empty
        ));
    }

    #[test]
    fn stat_claude_sessions_reports_empty_when_only_dotfiles() {
        // `.DS_Store` is the canonical macOS Finder noise; `.gitkeep`
        // appears whenever someone version-controls an empty dir.
        // Counting either would falsely report "session(s) recorded"
        // when the user has never actually opened Claude Code.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let sessions = tempdir.path().join("sessions");
        std::fs::create_dir(&sessions).expect("mkdir");
        std::fs::write(sessions.join(".DS_Store"), "").expect("touch");
        std::fs::write(sessions.join(".gitkeep"), "").expect("touch");
        assert!(matches!(
            stat_claude_sessions(&sessions),
            ClaudeSessionsState::Empty
        ));
    }

    #[test]
    fn stat_claude_sessions_skips_dotfiles_but_counts_real_session_files() {
        // Mixed content: real session JSONLs alongside Finder junk.
        // The count should reflect only the genuine session files.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let sessions = tempdir.path().join("sessions");
        std::fs::create_dir(&sessions).expect("mkdir");
        std::fs::write(sessions.join(".DS_Store"), "").expect("touch");
        std::fs::write(sessions.join("session-1.jsonl"), "").expect("touch");
        std::fs::write(sessions.join("session-2.jsonl"), "").expect("touch");
        match stat_claude_sessions(&sessions) {
            ClaudeSessionsState::HasFiles { count } => assert_eq!(count, 2),
            other => panic!("expected HasFiles {{ count: 2 }}, got {other:?}"),
        }
    }

    // --- Cross-category gating + PATH-broken hint ---

    #[test]
    fn claude_binary_warns_when_dir_permission_denied() {
        // Pin the WARN-vs-FAIL boundary at the dir_present predicate
        // — `PermissionDenied` counts as "the dir is there, we just
        // can't peek." A regression that drops `PermissionDenied`
        // from the predicate would silently flip this from WARN to
        // FAIL on a real user surface.
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: None,
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::PermissionDenied {
                    message: "Permission denied (os error 13)".to_string(),
                },
                ClaudeJsonState::Ok,
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "claude.binary_found").severity(),
            Severity::Warn
        );
    }

    #[test]
    fn claude_binary_warns_when_dir_not_a_directory() {
        // `~/.claude` exists as a *file* — Claude Code state was
        // corrupted, but the user is recoverable (delete the file +
        // launch claude). Reporting "neither present" → install
        // hint would be misleading.
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: None,
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::NotADirectory,
                ClaudeJsonState::Missing,
                ClaudeSessionsState::Missing,
            )),
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "claude.binary_found").severity(),
            Severity::Warn
        );
    }

    #[test]
    fn claude_binary_fail_with_path_unset_hint_when_path_env_unset() {
        // `$PATH` itself is the problem. Reporting "install Claude
        // Code from https://claude.ai/code" would be wrong remediation
        // — the user has Claude Code installed; their shell init
        // wiped PATH.
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: None,
            path_env: EnvVarState::Unset,
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::Ok,
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        let binary = find_check(&r, "claude.binary_found");
        assert_eq!(binary.severity(), Severity::Fail);
        assert!(
            binary.label().contains("$PATH is unset"),
            "label should name the PATH problem class: {}",
            binary.label(),
        );
        assert!(
            binary.hint().unwrap().contains("$PATH"),
            "hint should redirect the user to fix PATH: {:?}",
            binary.hint(),
        );
    }

    #[test]
    fn claude_binary_fail_with_path_nonutf8_hint() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: None,
            path_env: EnvVarState::NonUtf8("/usr/bin\u{FFFD}".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::Ok,
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        let binary = find_check(&r, "claude.binary_found");
        assert_eq!(binary.severity(), Severity::Fail);
        assert!(
            binary.label().contains("UTF-8"),
            "label should name UTF-8 as the failure class: {}",
            binary.label(),
        );
    }

    // --- Missing severity branches per pr-test-analyzer ---

    #[test]
    fn claude_dir_warns_on_not_a_directory() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::NotADirectory,
                ClaudeJsonState::Missing,
                ClaudeSessionsState::Missing,
            )),
        });
        let r = build_report(&env);
        assert_eq!(find_check(&r, "claude.dir").severity(), Severity::Warn);
    }

    #[test]
    fn claude_dir_warns_on_other_io() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::OtherIo {
                    message: "ELOOP".to_string(),
                },
                ClaudeJsonState::Missing,
                ClaudeSessionsState::Missing,
            )),
        });
        let r = build_report(&env);
        assert_eq!(find_check(&r, "claude.dir").severity(), Severity::Warn);
    }

    #[test]
    fn claude_json_fail_on_io_error() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::IoError {
                    message: "Permission denied (os error 13)".to_string(),
                },
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        let json = find_check(&r, "claude.json_parses");
        assert_eq!(json.severity(), Severity::Fail);
        assert!(
            json.label().contains("Permission denied"),
            "label should surface the io::Error message: {}",
            json.label(),
        );
    }

    #[test]
    fn claude_sessions_warn_on_not_a_directory() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::Ok,
                ClaudeSessionsState::NotADirectory,
            )),
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "claude.sessions_recorded").severity(),
            Severity::Warn,
        );
    }

    #[test]
    fn claude_sessions_warn_on_io_error() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::Ok,
                ClaudeJsonState::Ok,
                ClaudeSessionsState::IoError {
                    message: "EIO".to_string(),
                },
            )),
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "claude.sessions_recorded").severity(),
            Severity::Warn,
        );
    }

    #[test]
    fn claude_sessions_skips_when_dir_permission_denied() {
        // Cross-category gate widening: PermissionDenied on the
        // parent dir means `read_dir` of sessions/ would also fail
        // — surfacing both errors is redundant and points at the
        // same root cause. The dir check carries the right hint;
        // sessions just SKIPs.
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::PermissionDenied {
                    message: "Permission denied".to_string(),
                },
                ClaudeJsonState::Ok,
                ClaudeSessionsState::HasFiles { count: 99 },
            )),
        });
        let r = build_report(&env);
        let sessions = find_check(&r, "claude.sessions_recorded");
        assert_eq!(sessions.severity(), Severity::Skip);
        assert!(
            sessions.hint().unwrap().contains("unreadable"),
            "skip reason should match the parent dir state: {:?}",
            sessions.hint(),
        );
    }

    #[test]
    fn claude_sessions_skips_when_dir_not_a_directory() {
        let env = with_claude_snapshot(DoctorClaudeCodeSnapshot {
            binary_path: Some(PathBuf::from("/usr/local/bin/claude")),
            path_env: EnvVarState::Set("/usr/local/bin".to_string()),
            home_state: Some(claude_home_state(
                ClaudeDirState::NotADirectory,
                ClaudeJsonState::Missing,
                ClaudeSessionsState::HasFiles { count: 1 },
            )),
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "claude.sessions_recorded").severity(),
            Severity::Skip,
        );
    }

    // --- Credentials category ---

    use crate::data_context::credentials::CredentialSource;

    fn with_credentials(snapshot: DoctorCredentialsSnapshot) -> DoctorEnv {
        let mut env = DoctorEnv::healthy();
        env.credentials = snapshot;
        env
    }

    #[test]
    fn credentials_category_with_resolved_credentials_passes_all_four() {
        let r = build_report(&DoctorEnv::healthy());
        let category = r
            .categories
            .iter()
            .find(|c| c.name == "Credentials")
            .expect("Credentials category present");
        for check in &category.checks {
            assert_eq!(
                check.severity(),
                Severity::Pass,
                "{} should PASS in healthy env, got {:?}",
                check.id(),
                check.severity(),
            );
        }
    }

    #[test]
    fn credentials_no_credentials_fails_resolvable_and_source() {
        let env = with_credentials(DoctorCredentialsSnapshot::Failed(
            CredentialErrorSummary::NoCredentials,
        ));
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "creds.token_resolvable").severity(),
            Severity::Fail,
        );
        assert_eq!(
            find_check(&r, "creds.source_attested").severity(),
            Severity::Fail,
        );
        // Shape and scopes can't be evaluated without a token —
        // they SKIP rather than double-FAIL.
        assert_eq!(
            find_check(&r, "creds.token_shape_valid").severity(),
            Severity::Skip,
        );
        assert_eq!(
            find_check(&r, "creds.scopes_present").severity(),
            Severity::Skip,
        );
    }

    #[test]
    fn credentials_parse_error_passes_source_fails_shape() {
        // ParseError carries the cascade-winner path → source IS
        // identifiable even though the file is unusable. The shape
        // check is the right place for the FAIL.
        let env = with_credentials(DoctorCredentialsSnapshot::Failed(
            CredentialErrorSummary::ParseError {
                path: PathBuf::from("/home/user/.claude/.credentials.json"),
            },
        ));
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "creds.token_resolvable").severity(),
            Severity::Fail,
        );
        assert_eq!(
            find_check(&r, "creds.source_attested").severity(),
            Severity::Pass,
        );
        assert_eq!(
            find_check(&r, "creds.token_shape_valid").severity(),
            Severity::Fail,
        );
    }

    #[test]
    fn credentials_scopes_fail_when_user_inference_absent() {
        let env = with_credentials(DoctorCredentialsSnapshot::Resolved(CredentialsSummary {
            source: CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/home/user/.claude/.credentials.json"),
            },
            // No `user:inference` — the spec FAIL row.
            scopes: vec!["user:profile".to_string()],
        }));
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "creds.scopes_present").severity(),
            Severity::Fail,
        );
    }

    #[test]
    fn credentials_unresolvable_skips_all_four() {
        let env = with_credentials(DoctorCredentialsSnapshot::Unresolvable);
        let r = build_report(&env);
        for id in [
            "creds.token_resolvable",
            "creds.source_attested",
            "creds.token_shape_valid",
            "creds.scopes_present",
        ] {
            assert_eq!(
                find_check(&r, id).severity(),
                Severity::Skip,
                "{id} should SKIP when cascade unresolvable",
            );
        }
    }

    #[test]
    fn credentials_source_attested_label_redacts_token_bytes() {
        // Pin the redaction contract: the source label includes a
        // path / service description but never the bearer token.
        // A future refactor that surfaces `Credentials::token` here
        // would silently leak.
        let label = source_label(&CredentialSource::ClaudeLegacy {
            path: PathBuf::from("/some/file"),
        });
        assert!(
            !label.contains("Bearer ") && !label.contains("eyJ"),
            "source label must never contain bearer-token shapes: {label}",
        );
    }

    // --- Cache category ---

    fn with_cache(snapshot: DoctorCacheSnapshot) -> DoctorEnv {
        let mut env = DoctorEnv::healthy();
        env.cache = snapshot;
        env
    }

    #[test]
    fn cache_category_with_healthy_snapshot_passes_all_three() {
        let r = build_report(&DoctorEnv::healthy());
        let category = r.categories.iter().find(|c| c.name == "Cache").unwrap();
        for check in &category.checks {
            assert_eq!(
                check.severity(),
                Severity::Pass,
                "{} should PASS in healthy env, got {:?}",
                check.id(),
                check.severity(),
            );
        }
    }

    #[test]
    fn cache_dir_passes_when_absent_with_creatable_hint() {
        // Doctor stat-only, no probe-write. Expected first-run
        // case: directory doesn't exist yet AND first existing
        // ancestor is writable. PASS with a hint that the runtime
        // will create it. If the ancestor is read-only, that's a
        // separate `AbsentParentReadOnly` WARN — see
        // `cache_dir_warns_when_absent_parent_read_only`.
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
            root: CacheRootState::Absent,
            usage_json: UsageJsonState::Missing,
            lock: LockState::Absent,
        });
        let r = build_report(&env);
        let dir = find_check(&r, "cache.dir_writable");
        assert_eq!(dir.severity(), Severity::Pass);
        assert!(
            dir.label().contains("will be created"),
            "label should explain the creatable state: {}",
            dir.label(),
        );
        // Guard against a revert that re-introduces the old
        // probe-write "parent writable" phrasing without the
        // current "will be created" semantics.
        assert!(
            !dir.label().contains("parent writable"),
            "label should not carry the old probe-write phrasing: {}",
            dir.label(),
        );
        assert_eq!(r.exit_code(), 0, "Absent must not gate exit-1");
    }

    #[test]
    fn cache_dir_warns_when_absent_parent_read_only() {
        // Invariant: an Absent cache root under a permanently
        // non-creatable parent (read-only mount, /proc, unwritable
        // XDG_CACHE_HOME) must WARN, not silently PASS. A failed
        // first-fetch leaves the path Absent so neither usage.json
        // nor usage.lock rows catch it on the next run; the
        // AbsentParentReadOnly variant carries the only signal the
        // user gets from doctor for this misconfig.
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/proc/cache/linesmith")),
            root: CacheRootState::AbsentParentReadOnly {
                parent: PathBuf::from("/proc"),
            },
            usage_json: UsageJsonState::Missing,
            lock: LockState::Absent,
        });
        let r = build_report(&env);
        let dir = find_check(&r, "cache.dir_writable");
        assert_eq!(dir.severity(), Severity::Warn);
        assert!(
            dir.label().contains("Cache dir cannot be created"),
            "label should name the failure mode: {}",
            dir.label()
        );
        assert!(
            dir.label().contains("/proc"),
            "label should name the read-only ancestor: {}",
            dir.label()
        );
        assert!(
            dir.hint().is_some_and(|h| h.contains("XDG_CACHE_HOME")),
            "hint should point at the env var to fix: {:?}",
            dir.hint()
        );
        assert_eq!(r.exit_code(), 0, "AbsentParentReadOnly is WARN, not FAIL");
    }

    #[test]
    fn stat_cache_root_returns_exists_for_a_real_directory() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        match stat_cache_root(tempdir.path()) {
            CacheRootState::Exists => {}
            other => panic!("expected Exists for real directory, got {other:?}"),
        }
    }

    #[test]
    fn stat_cache_root_returns_not_a_directory_when_path_is_a_file() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let file = tempdir.path().join("cache");
        std::fs::write(&file, b"not a dir").expect("write");
        match stat_cache_root(&file) {
            CacheRootState::NotADirectory => {}
            other => panic!("expected NotADirectory for a regular file, got {other:?}"),
        }
    }

    #[test]
    fn stat_cache_root_returns_absent_parent_read_only_when_intermediate_is_a_file() {
        // The classify_absent_cache_root walk handles three cases on
        // top of the writable-directory PASS: readonly directory,
        // file-in-the-middle, and PermissionDenied during walk.
        // Pin the file-in-the-middle case directly: stat
        // `tempdir/middle/sub/cache` where `tempdir/middle` is a
        // regular file. The walk should land on `tempdir/middle`
        // and return AbsentParentReadOnly with that path.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let middle = tempdir.path().join("middle");
        std::fs::write(&middle, b"file in the middle of the chain").expect("write");
        let cache = middle.join("sub").join("cache");
        match stat_cache_root(&cache) {
            CacheRootState::AbsentParentReadOnly { parent } => {
                assert_eq!(
                    parent, middle,
                    "parent should point at the file blocking the chain"
                );
            }
            other => panic!("expected AbsentParentReadOnly with file ancestor, got {other:?}"),
        }
    }

    #[test]
    fn stat_cache_root_returns_absent_when_path_missing_under_writable_parent() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let missing = tempdir.path().join("cache").join("linesmith");
        match stat_cache_root(&missing) {
            // Tempdirs default to 0700 — writable by the user, so the
            // parent walk lands in `Absent`, not `AbsentParentReadOnly`.
            CacheRootState::Absent => {}
            other => {
                panic!("expected Absent for missing path under writable parent, got {other:?}")
            }
        }
    }

    #[test]
    fn cache_dir_check_never_produces_fail_severity_for_any_variant() {
        // Spec contract guard: the cache.dir_writable row has no
        // FAIL column (see §Cache row "Cache dir exists or
        // creatable"). A future patch that re-introduces FAIL on
        // any CacheRootState variant must fail this test. Drives
        // every variant through build_report and asserts no Fail
        // surfaces — including the Unresolved/SKIP path.
        //
        // The exhaustive `match` below makes a 6th variant a
        // compile error here, forcing a corresponding addition
        // to the iterated list — without it, a new variant
        // could silently slip past this guard and re-introduce
        // FAIL semantics undetected.
        fn _exhaustiveness_tripwire(v: &CacheRootState) {
            match v {
                CacheRootState::Exists
                | CacheRootState::Absent
                | CacheRootState::AbsentParentReadOnly { .. }
                | CacheRootState::NotADirectory
                | CacheRootState::Unreadable { .. }
                | CacheRootState::Unresolved => {}
            }
        }
        let path = Some(PathBuf::from("/home/user/.cache/linesmith"));
        let variants: [(&'static str, CacheRootState); 5] = [
            ("Exists", CacheRootState::Exists),
            ("Absent", CacheRootState::Absent),
            (
                "AbsentParentReadOnly",
                CacheRootState::AbsentParentReadOnly {
                    parent: PathBuf::from("/proc"),
                },
            ),
            ("NotADirectory", CacheRootState::NotADirectory),
            (
                "Unreadable",
                CacheRootState::Unreadable {
                    message: "Permission denied".to_string(),
                },
            ),
        ];
        for (name, root) in variants {
            let env = with_cache(DoctorCacheSnapshot {
                root_path: path.clone(),
                root,
                usage_json: UsageJsonState::Missing,
                lock: LockState::Absent,
            });
            let r = build_report(&env);
            let dir = find_check(&r, "cache.dir_writable");
            assert_ne!(
                dir.severity(),
                Severity::Fail,
                "{name} variant must not produce FAIL on cache.dir_writable",
            );
        }
        // Unresolved (no path) variant rendered through SKIP.
        let env = with_cache(DoctorCacheSnapshot {
            root_path: None,
            root: CacheRootState::Unresolved,
            usage_json: UsageJsonState::Missing,
            lock: LockState::Absent,
        });
        let r = build_report(&env);
        let dir = find_check(&r, "cache.dir_writable");
        assert_ne!(
            dir.severity(),
            Severity::Fail,
            "Unresolved variant must not produce FAIL"
        );
    }

    #[test]
    fn cache_dir_warns_when_path_is_a_file_not_a_directory() {
        // Real read-only-detectable misconfiguration: a stale leftover
        // where the user has `~/.cache/linesmith` as a file. Runtime
        // would fail to use it, so WARN preemptively.
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
            root: CacheRootState::NotADirectory,
            usage_json: UsageJsonState::Missing,
            lock: LockState::Absent,
        });
        let r = build_report(&env);
        let dir = find_check(&r, "cache.dir_writable");
        assert_eq!(dir.severity(), Severity::Warn);
        assert!(
            dir.hint().is_some_and(|h| h.contains("remove")),
            "hint should suggest removal: {:?}",
            dir.hint()
        );
        assert_eq!(r.exit_code(), 0, "NotADirectory is WARN, not FAIL");
    }

    #[test]
    fn cache_dir_warns_when_unreadable_with_io_message() {
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
            root: CacheRootState::Unreadable {
                message: "Permission denied (os error 13)".to_string(),
            },
            usage_json: UsageJsonState::Missing,
            lock: LockState::Absent,
        });
        let r = build_report(&env);
        let dir = find_check(&r, "cache.dir_writable");
        assert_eq!(dir.severity(), Severity::Warn);
        assert!(
            dir.label().contains("Cache dir unreadable"),
            "label should carry the spec-required prefix: {}",
            dir.label()
        );
        assert!(
            dir.label().contains("Permission denied"),
            "label should surface the io::Error message: {}",
            dir.label()
        );
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn cache_skips_all_three_when_unresolved() {
        let env = with_cache(DoctorCacheSnapshot {
            root_path: None,
            root: CacheRootState::Unresolved,
            usage_json: UsageJsonState::Missing,
            lock: LockState::Absent,
        });
        let r = build_report(&env);
        for id in [
            "cache.dir_writable",
            "cache.usage_json_shape",
            "cache.lock_fresh",
        ] {
            assert_eq!(
                find_check(&r, id).severity(),
                Severity::Skip,
                "{id} should SKIP when cache root unresolved",
            );
        }
    }

    #[test]
    fn cache_usage_json_warns_on_stale_schema() {
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
            root: CacheRootState::Exists,
            usage_json: UsageJsonState::Stale { schema_version: 0 },
            lock: LockState::Absent,
        });
        let r = build_report(&env);
        let usage = find_check(&r, "cache.usage_json_shape");
        assert_eq!(usage.severity(), Severity::Warn);
        assert!(
            usage.hint().unwrap().contains("safe to ignore"),
            "hint should reassure that stale cache self-heals: {:?}",
            usage.hint(),
        );
    }

    #[test]
    fn cache_lock_warns_when_stale() {
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
            root: CacheRootState::Exists,
            usage_json: UsageJsonState::Missing,
            lock: LockState::Stale {
                blocked_until_secs: 1,
            },
        });
        let r = build_report(&env);
        let lock = find_check(&r, "cache.lock_fresh");
        assert_eq!(lock.severity(), Severity::Warn);
        assert!(
            lock.hint().unwrap().contains("rm"),
            "hint should tell user how to clear: {:?}",
            lock.hint(),
        );
    }

    #[test]
    fn cache_lock_active_passes() {
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
            root: CacheRootState::Exists,
            usage_json: UsageJsonState::Missing,
            // Far-future timestamp so the test doesn't break in 50
            // years.
            lock: LockState::Active {
                blocked_until_secs: i64::MAX,
            },
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "cache.lock_fresh").severity(),
            Severity::Pass
        );
    }

    // --- Rate-limit endpoint category ---

    fn with_endpoint(snapshot: DoctorEndpointSnapshot) -> DoctorEnv {
        let mut env = DoctorEnv::healthy();
        env.endpoint = snapshot;
        env
    }

    fn endpoint_probe(outcome: EndpointProbeOutcome, elapsed_ms: u128) -> DoctorEndpointSnapshot {
        DoctorEndpointSnapshot {
            credentials_vanished: false,
            probe: Some(EndpointProbe {
                elapsed_ms,
                outcome,
            }),
        }
    }

    #[test]
    fn endpoint_category_with_healthy_probe_passes_all_three() {
        let r = build_report(&DoctorEnv::healthy());
        let category = r
            .categories
            .iter()
            .find(|c| c.name == "Rate-limit endpoint")
            .unwrap();
        for check in &category.checks {
            assert_eq!(
                check.severity(),
                Severity::Pass,
                "{} should PASS in healthy env",
                check.id(),
            );
        }
    }

    #[test]
    fn endpoint_skips_when_credentials_failed() {
        // Spec §Cross-category: missing token → endpoint SKIP.
        // The skip-reason should reflect the credentials failure,
        // not "endpoint not reachable".
        let mut env = DoctorEnv::healthy();
        env.credentials = DoctorCredentialsSnapshot::Failed(CredentialErrorSummary::NoCredentials);
        env.endpoint = DoctorEndpointSnapshot {
            probe: None,
            credentials_vanished: false,
        };
        let r = build_report(&env);
        for id in [
            "endpoint.reachable",
            "endpoint.shape_current",
            "endpoint.headers_sane",
        ] {
            let check = find_check(&r, id);
            assert_eq!(check.severity(), Severity::Skip, "{id} should SKIP");
            assert!(
                check.hint().unwrap().contains("token"),
                "skip reason should name the missing token: {:?}",
                check.hint(),
            );
        }
    }

    #[test]
    fn endpoint_transport_error_warns_keeps_exit_zero() {
        // Spec §Rate-limit endpoint: transport-level errors are
        // WARN, not FAIL. CI runs without network egress must NOT
        // exit-1.
        let env = with_endpoint(endpoint_probe(EndpointProbeOutcome::TransportError, 250));
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "endpoint.reachable").severity(),
            Severity::Warn,
        );
        assert_eq!(r.exit_code(), 0, "transport error must not gate exit-1");
    }

    #[test]
    fn endpoint_bad_status_fails_with_per_status_hint() {
        let env = with_endpoint(endpoint_probe(
            EndpointProbeOutcome::BadStatus { status: 401 },
            150,
        ));
        let r = build_report(&env);
        let reachable = find_check(&r, "endpoint.reachable");
        assert_eq!(reachable.severity(), Severity::Fail);
        assert!(
            reachable.hint().unwrap().contains("log in"),
            "401 should hint at re-login: {:?}",
            reachable.hint(),
        );
    }

    #[test]
    fn endpoint_slow_response_warns_with_elapsed() {
        let env = with_endpoint(endpoint_probe(EndpointProbeOutcome::Slow, 2500));
        let r = build_report(&env);
        let reachable = find_check(&r, "endpoint.reachable");
        assert_eq!(reachable.severity(), Severity::Warn);
        assert!(
            reachable.label().contains("2500ms"),
            "label should report elapsed time: {}",
            reachable.label(),
        );
    }

    #[test]
    fn endpoint_unexpected_shape_warns_with_extra_keys() {
        let env = with_endpoint(endpoint_probe(
            EndpointProbeOutcome::UnexpectedShape {
                extra_keys: vec!["omelette_5h".into(), "iguana_7d".into()],
            },
            300,
        ));
        let r = build_report(&env);
        let shape = find_check(&r, "endpoint.shape_current");
        assert_eq!(shape.severity(), Severity::Warn);
        assert!(
            shape.label().contains("omelette_5h") && shape.label().contains("iguana_7d"),
            "label should name the forward-compat keys: {}",
            shape.label(),
        );
    }

    #[test]
    fn endpoint_parse_error_fails_shape_check() {
        let env = with_endpoint(endpoint_probe(EndpointProbeOutcome::ParseError, 400));
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "endpoint.shape_current").severity(),
            Severity::Fail,
        );
    }

    #[test]
    fn endpoint_rate_limited_warns_with_reasonable_retry_after() {
        let env = with_endpoint(endpoint_probe(
            EndpointProbeOutcome::RateLimited {
                retry_after_secs: Some(60),
            },
            200,
        ));
        let r = build_report(&env);
        let headers = find_check(&r, "endpoint.headers_sane");
        assert_eq!(headers.severity(), Severity::Warn);
    }

    #[test]
    fn endpoint_rate_limited_fails_on_abusive_retry_after() {
        // >1 hour Retry-After per spec FAIL row.
        let env = with_endpoint(endpoint_probe(
            EndpointProbeOutcome::RateLimited {
                retry_after_secs: Some(3601),
            },
            200,
        ));
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "endpoint.headers_sane").severity(),
            Severity::Fail,
        );
    }

    // --- Snapshot helper smoke tests ---

    #[test]
    fn classify_endpoint_response_maps_2xx_with_extra_keys_to_unexpected_shape() {
        use crate::data_context::fetcher::HttpResponse;
        let body = br#"{"five_hour":null,"omelette_extra":{"x":1}}"#;
        let resp = HttpResponse {
            status: 200,
            body: body.to_vec(),
            retry_after: None,
        };
        let outcome = classify_endpoint_response(Ok(resp), std::time::Duration::from_millis(300));
        assert!(
            matches!(outcome, EndpointProbeOutcome::UnexpectedShape { ref extra_keys } if extra_keys == &vec!["omelette_extra".to_string()]),
            "expected UnexpectedShape, got {outcome:?}",
        );
    }

    #[test]
    fn classify_endpoint_response_maps_401_to_bad_status() {
        use crate::data_context::fetcher::HttpResponse;
        let resp = HttpResponse {
            status: 401,
            body: vec![],
            retry_after: None,
        };
        let outcome = classify_endpoint_response(Ok(resp), std::time::Duration::from_millis(50));
        assert!(matches!(
            outcome,
            EndpointProbeOutcome::BadStatus { status: 401 }
        ));
    }

    #[test]
    fn classify_endpoint_response_maps_io_error_to_transport_error() {
        let outcome = classify_endpoint_response(
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "read timeout",
            )),
            std::time::Duration::from_secs(2),
        );
        assert!(matches!(outcome, EndpointProbeOutcome::TransportError));
    }

    #[test]
    fn classify_endpoint_response_maps_5xx_to_transport_error() {
        // Spec §Rate-limit endpoint reserves FAIL for "4xx other than 429".
        // 5xx is the upstream broken (Anthropic outage); a doctor
        // run during an incident must not exit-1.
        use crate::data_context::fetcher::HttpResponse;
        for status in [500u16, 502, 503, 504] {
            let resp = HttpResponse {
                status,
                body: vec![],
                retry_after: None,
            };
            let outcome =
                classify_endpoint_response(Ok(resp), std::time::Duration::from_millis(50));
            assert!(
                matches!(outcome, EndpointProbeOutcome::TransportError),
                "{status} should map to TransportError, got {outcome:?}",
            );
        }
    }

    #[test]
    fn classify_endpoint_response_maps_3xx_to_transport_error() {
        // ureq follows redirects; a 3xx leaking here means redirect
        // handling exhausted or disabled — same user-actionability
        // as a transport error.
        use crate::data_context::fetcher::HttpResponse;
        let resp = HttpResponse {
            status: 301,
            body: vec![],
            retry_after: None,
        };
        let outcome = classify_endpoint_response(Ok(resp), std::time::Duration::from_millis(50));
        assert!(matches!(outcome, EndpointProbeOutcome::TransportError));
    }

    #[test]
    fn classify_endpoint_response_at_2s_boundary_classifies_as_slow() {
        // Pin the spec's WARN threshold so a `>` vs `>=` swap or a
        // ms/s unit drift in `classify_endpoint_response` would
        // surface as a test failure. Read from `DOCTOR_SLOW_THRESHOLD`
        // (not a literal) so the test follows the constant if it ever
        // moves — and so a regression that re-points the classifier
        // at `DEFAULT_TIMEOUT` (the request budget, which doctor
        // intentionally splits from the user-facing slow signal)
        // doesn't silently still pass.
        use crate::data_context::fetcher::HttpResponse;
        let threshold = DOCTOR_SLOW_THRESHOLD;
        let just_under = threshold - std::time::Duration::from_millis(1);
        let body = br#"{}"#;
        let resp = HttpResponse {
            status: 200,
            body: body.to_vec(),
            retry_after: None,
        };
        assert!(matches!(
            classify_endpoint_response(Ok(resp), just_under),
            EndpointProbeOutcome::Ok
        ));
        let resp = HttpResponse {
            status: 200,
            body: body.to_vec(),
            retry_after: None,
        };
        assert!(matches!(
            classify_endpoint_response(Ok(resp), threshold),
            EndpointProbeOutcome::Slow
        ));
    }

    #[test]
    fn endpoint_5xx_warns_keeps_exit_zero() {
        // End-to-end: an Anthropic 503 must not gate exit-1 in CI.
        use crate::data_context::fetcher::HttpResponse;
        let resp = HttpResponse {
            status: 503,
            body: vec![],
            retry_after: None,
        };
        let outcome = classify_endpoint_response(Ok(resp), std::time::Duration::from_millis(100));
        let env = with_endpoint(DoctorEndpointSnapshot {
            credentials_vanished: false,
            probe: Some(EndpointProbe {
                elapsed_ms: 100,
                outcome,
            }),
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "endpoint.reachable").severity(),
            Severity::Warn,
            "5xx must be WARN per spec §Rate-limit endpoint",
        );
        assert_eq!(r.exit_code(), 0, "Anthropic incident must not gate exit-1");
    }

    #[test]
    fn endpoint_rate_limited_warns_when_no_retry_after_header() {
        // Pin the distinct WARN branch ("429 without Retry-After
        // header") so a refactor that collapses the None arm into
        // the abusive-Some arm flips the user-visible severity.
        let env = with_endpoint(endpoint_probe(
            EndpointProbeOutcome::RateLimited {
                retry_after_secs: None,
            },
            200,
        ));
        let r = build_report(&env);
        let headers = find_check(&r, "endpoint.headers_sane");
        assert_eq!(headers.severity(), Severity::Warn);
        assert!(
            headers.label().contains("without"),
            "label should distinguish the missing-header case: {}",
            headers.label(),
        );
    }

    // --- Credential-error branch coverage ---

    #[test]
    fn credentials_subprocess_failed_fails_resolvable_and_source() {
        // Spec §Credentials explicitly cites SubprocessFailed as a FAIL
        // trigger for `creds.token_resolvable`. macOS keychain
        // locked / `security` binary missing falls here.
        let env = with_credentials(DoctorCredentialsSnapshot::Failed(
            CredentialErrorSummary::SubprocessFailed {
                message: "security: SecKeychainSearchCopyNext: locked".to_string(),
            },
        ));
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "creds.token_resolvable").severity(),
            Severity::Fail,
        );
        assert_eq!(
            find_check(&r, "creds.source_attested").severity(),
            Severity::Fail,
        );
    }

    #[test]
    fn credentials_io_error_fails_resolvable_passes_source_attested() {
        // IoError carries the cascade-winner path (e.g.,
        // `chmod 000 ~/.claude/.credentials.json`) — the source
        // IS identifiable, the file is just unreadable. Same
        // shape as ParseError. The resolvable check carries the
        // FAIL; source PASSes with the path so the user sees
        // exactly which file to fix.
        let env = with_credentials(DoctorCredentialsSnapshot::Failed(
            CredentialErrorSummary::IoError {
                path: PathBuf::from("/home/user/.claude/.credentials.json"),
                message: "Permission denied (os error 13)".to_string(),
            },
        ));
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "creds.token_resolvable").severity(),
            Severity::Fail,
        );
        let source = find_check(&r, "creds.source_attested");
        assert_eq!(source.severity(), Severity::Pass);
        assert!(
            source
                .label()
                .contains("/home/user/.claude/.credentials.json"),
            "source label should name the file that failed: {}",
            source.label(),
        );
    }

    #[test]
    fn credentials_missing_field_renders_distinct_label() {
        let env = with_credentials(DoctorCredentialsSnapshot::Failed(
            CredentialErrorSummary::MissingField {
                path: PathBuf::from("/home/user/.claude/.credentials.json"),
            },
        ));
        let r = build_report(&env);
        let shape = find_check(&r, "creds.token_shape_valid");
        assert_eq!(shape.severity(), Severity::Fail);
        assert!(
            shape.label().contains("claudeAiOauth"),
            "MissingField label should name the missing block: {}",
            shape.label(),
        );
    }

    #[test]
    fn credentials_empty_token_renders_distinct_label() {
        let env = with_credentials(DoctorCredentialsSnapshot::Failed(
            CredentialErrorSummary::EmptyToken {
                path: PathBuf::from("/home/user/.claude/.credentials.json"),
            },
        ));
        let r = build_report(&env);
        let shape = find_check(&r, "creds.token_shape_valid");
        assert_eq!(shape.severity(), Severity::Fail);
        assert!(
            shape.label().contains("accessToken"),
            "EmptyToken label should name the empty field: {}",
            shape.label(),
        );
    }

    #[test]
    fn source_label_redacts_token_bytes_for_every_credential_source_variant() {
        // Iterate every CredentialSource variant; the source label
        // must NEVER contain bearer-shaped substrings, even when an
        // attacker controls path / service / mdat strings (e.g.
        // `service` on MacosKeychainMultiAccount is technically
        // attacker-influenceable since it's parsed from
        // `security dump-keychain` output).
        let attacker_path = PathBuf::from("/sk-ant-secrettoken/eyJabc");
        let attacker_service = "Bearer eyJabc-evil-service".to_string();
        let attacker_mdat = Some("eyJ-mdat-bytes".to_string());
        let cases = [
            CredentialSource::MacosKeychainPrimary,
            CredentialSource::MacosKeychainMultiAccount {
                service: attacker_service,
                mdat: attacker_mdat,
            },
            CredentialSource::EnvDir {
                path: attacker_path.clone(),
            },
            CredentialSource::XdgConfig {
                path: attacker_path.clone(),
            },
            CredentialSource::ClaudeLegacy {
                path: attacker_path,
            },
        ];
        for source in cases {
            let label = source_label(&source);
            // The substrings here are attacker-controlled, so the
            // label SHOULD contain them (pass-through is correct).
            // What must NOT happen: a future refactor adds the
            // bearer token from `Credentials::token()` into the
            // label. Pin that boundary by asserting label length is
            // bounded and doesn't grow disproportionately on
            // pathological inputs (no template injection).
            assert!(
                label.len() < 1024,
                "source label grew unexpectedly long ({} bytes) for {:?}; possible token interpolation: {}",
                label.len(),
                source,
                label,
            );
        }
    }

    // --- Cache: usage.json + lock branch coverage ---

    #[test]
    fn cache_usage_json_warns_on_unreadable_distinct_from_stale() {
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
            root: CacheRootState::Exists,
            usage_json: UsageJsonState::Unreadable {
                message: "filesystem corruption".to_string(),
            },
            lock: LockState::Absent,
        });
        let r = build_report(&env);
        let usage = find_check(&r, "cache.usage_json_shape");
        assert_eq!(usage.severity(), Severity::Warn);
        assert!(
            usage.hint().unwrap().contains("filesystem"),
            "Unreadable hint must distinguish corruption from stale: {:?}",
            usage.hint(),
        );
    }

    #[test]
    fn cache_usage_json_warns_on_future_timestamp() {
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
            root: CacheRootState::Exists,
            usage_json: UsageJsonState::FutureTimestamp,
            lock: LockState::Absent,
        });
        let r = build_report(&env);
        let usage = find_check(&r, "cache.usage_json_shape");
        assert_eq!(usage.severity(), Severity::Warn);
        assert!(
            usage.hint().unwrap().contains("clock"),
            "FutureTimestamp hint should point at clock skew: {:?}",
            usage.hint(),
        );
    }

    #[test]
    fn cache_lock_warns_on_unreadable() {
        let env = with_cache(DoctorCacheSnapshot {
            root_path: Some(PathBuf::from("/home/user/.cache/linesmith")),
            root: CacheRootState::Exists,
            usage_json: UsageJsonState::Missing,
            lock: LockState::Unreadable {
                message: "Permission denied".to_string(),
            },
        });
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "cache.lock_fresh").severity(),
            Severity::Warn
        );
    }

    // --- Endpoint: credential-race short-circuit ---

    #[test]
    fn endpoint_skips_with_race_reason_when_credentials_vanished() {
        let mut env = DoctorEnv::healthy();
        env.endpoint = DoctorEndpointSnapshot {
            probe: None,
            credentials_vanished: true,
        };
        let r = build_report(&env);
        for id in [
            "endpoint.reachable",
            "endpoint.shape_current",
            "endpoint.headers_sane",
        ] {
            let check = find_check(&r, id);
            assert_eq!(check.severity(), Severity::Skip);
            assert!(
                check.hint().unwrap().contains("race"),
                "skip reason should name the race: {:?}",
                check.hint(),
            );
        }
    }

    // --- Credentials: cascade-source short-circuit ---

    #[test]
    #[cfg(target_os = "macos")]
    fn snapshot_credentials_does_not_short_circuit_on_macos_without_env() {
        // The cfg gate in `snapshot_credentials`'s short-circuit is
        // load-bearing rather than decorative: on macOS the
        // `security` subprocess can resolve from the user's Keychain
        // even with no env vars set, so the function MUST run the
        // cascade rather than reporting Unresolvable. A future
        // refactor that drops the `cfg!(target_os = "macos")` guard
        // would silently regress macOS users with no configured
        // env vars but valid Keychain entries.
        use crate::data_context::credentials::FileCascadeEnv;
        let snapshot = snapshot_credentials(&FileCascadeEnv::default());
        // On macOS without env vars the cascade still runs; result
        // is Resolved (real Keychain hit) or Failed (no Keychain
        // entry, NoCredentials). Both are valid; what's NOT valid
        // is short-circuiting to Unresolvable.
        assert!(
            !matches!(snapshot, DoctorCredentialsSnapshot::Unresolvable),
            "macOS must run the cascade even with no env vars; got {snapshot:?}"
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn snapshot_credentials_returns_unresolvable_when_no_path_source_non_macos() {
        // On non-macOS hosts with no `$HOME`, no `$XDG_CONFIG_HOME`,
        // and no `$CLAUDE_CONFIG_DIR`, the file cascade has no root
        // and there's no keychain — the cascade can't even be
        // attempted. Per spec §Cross-category short-circuits this
        // is SKIP, not FAIL.
        use crate::data_context::credentials::FileCascadeEnv;
        let snapshot = snapshot_credentials(&FileCascadeEnv::default());
        assert!(
            matches!(snapshot, DoctorCredentialsSnapshot::Unresolvable),
            "expected Unresolvable, got {snapshot:?}",
        );
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn snapshot_credentials_attempts_cascade_when_xdg_or_claude_dir_overrides_home() {
        // CI / service environments commonly omit `$HOME` but set
        // one of the override env vars (`$XDG_CONFIG_HOME`,
        // `$CLAUDE_CONFIG_DIR`). The file cascade still has a root
        // in those cases, so the snapshot must NOT short-circuit
        // to Unresolvable. The actual `resolve_credentials_with`
        // call will likely return `Failed(NoCredentials)` on a CI
        // host without real credentials, but that's the correct
        // outcome, not a doctor false-negative.
        use crate::data_context::credentials::FileCascadeEnv;
        for env in [
            FileCascadeEnv {
                xdg_config_home: Some(PathBuf::from("/etc/xdg")),
                ..Default::default()
            },
            FileCascadeEnv {
                claude_config_dir: Some(PathBuf::from("/etc/claude")),
                ..Default::default()
            },
        ] {
            let snapshot = snapshot_credentials(&env);
            assert!(
                !matches!(snapshot, DoctorCredentialsSnapshot::Unresolvable),
                "override env should keep the cascade attemptable; got Unresolvable for env={env:?}",
            );
        }
    }

    #[test]
    fn stat_usage_json_distinguishes_current_stale_and_missing() {
        use crate::data_context::cache::CACHE_SCHEMA_VERSION;
        let tempdir = tempfile::tempdir().expect("tempdir");
        let missing = tempdir.path().join("missing.json");
        assert!(matches!(stat_usage_json(&missing), UsageJsonState::Missing));

        let current = tempdir.path().join("current.json");
        std::fs::write(
            &current,
            format!(
                r#"{{"schema_version":{CACHE_SCHEMA_VERSION},"cached_at":"2026-04-30T00:00:00Z"}}"#
            ),
        )
        .expect("write");
        assert!(matches!(
            stat_usage_json(&current),
            UsageJsonState::Current { .. }
        ));

        let stale = tempdir.path().join("stale.json");
        std::fs::write(&stale, r#"{"schema_version":0}"#).expect("write");
        assert!(matches!(
            stat_usage_json(&stale),
            UsageJsonState::Stale { schema_version: 0 }
        ));
    }

    #[test]
    fn stat_usage_lock_distinguishes_active_stale_and_absent() {
        // `stat_usage_lock` now takes the cache ROOT and delegates
        // to `LockStore::read`; place each fixture's `usage.lock`
        // inside its own root subdir so the per-root LockStore
        // sees the right file.
        let tempdir = tempfile::tempdir().expect("tempdir");

        let absent_root = tempdir.path().join("absent");
        std::fs::create_dir(&absent_root).expect("mkdir");
        assert!(matches!(stat_usage_lock(&absent_root), LockState::Absent));

        let active_root = tempdir.path().join("active");
        std::fs::create_dir(&active_root).expect("mkdir");
        std::fs::write(
            active_root.join("usage.lock"),
            r#"{"blocked_until":99999999999}"#,
        )
        .expect("write");
        assert!(matches!(
            stat_usage_lock(&active_root),
            LockState::Active { .. }
        ));

        let stale_root = tempdir.path().join("stale");
        std::fs::create_dir(&stale_root).expect("mkdir");
        std::fs::write(stale_root.join("usage.lock"), r#"{"blocked_until":1}"#).expect("write");
        assert!(matches!(
            stat_usage_lock(&stale_root),
            LockState::Stale { .. }
        ));
    }

    #[test]
    fn stat_usage_lock_legacy_non_json_falls_back_to_mtime_via_lockstore() {
        // Parity-rule pin: doctor must agree with the runtime on
        // legacy non-JSON lock files (which the runtime treats as
        // Active via mtime + LEGACY_LOCK_TTL). Without delegating
        // to `LockStore::read`, doctor would WARN Unreadable
        // here while the runtime quietly treats the lock as
        // Active.
        let tempdir = tempfile::tempdir().expect("tempdir");
        let root = tempdir.path().to_path_buf();
        std::fs::write(root.join("usage.lock"), b"not json bytes").expect("write");
        match stat_usage_lock(&root) {
            LockState::Active { .. } => {}
            other => panic!("expected Active via mtime fallback, got {other:?}"),
        }
    }

    // --- Plugins category ---

    use crate::plugins::errors::{CollisionWinner, PluginError};

    fn with_plugins(snapshot: DoctorPluginsSnapshot) -> DoctorEnv {
        let mut env = DoctorEnv::healthy();
        env.plugins = snapshot;
        env
    }

    #[test]
    fn plugins_category_with_no_sources_skips_all_four() {
        // Spec §Edge cases: "Default plugin directory doesn't
        // exist (no `plugin_dirs` configured) → SKIP the Plugins
        // category with reason 'no plugins configured'; not a
        // failure." A first-run user without `~/.config/linesmith/
        // segments/` shouldn't get a non-zero exit just for
        // that.
        let env = with_plugins(DoctorPluginsSnapshot::NoSources);
        let r = build_report(&env);
        for id in [
            "plugins.compile",
            "plugins.deps_valid",
            "plugins.no_id_collisions",
            "plugins.no_builtin_collisions",
        ] {
            let check = find_check(&r, id);
            assert_eq!(check.severity(), Severity::Skip, "{id} should SKIP");
            assert!(
                check.hint().unwrap().contains("no plugins"),
                "skip reason should name the empty-sources state: {:?}",
                check.hint(),
            );
        }
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn plugins_category_with_zero_plugins_passes_vacuously() {
        // Discovery ran but found no `.rhai` files. Spec PASS:
        // "every discovered .rhai parses" — the empty set
        // satisfies. Distinct from NoSources (which SKIPs); this
        // is the "user created the plugin dir but hasn't dropped
        // a file in yet" case.
        let env = with_plugins(DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
            compiled_count: 0,
            errors: Vec::new(),
        }));
        let r = build_report(&env);
        for id in [
            "plugins.compile",
            "plugins.deps_valid",
            "plugins.no_id_collisions",
            "plugins.no_builtin_collisions",
        ] {
            assert_eq!(
                find_check(&r, id).severity(),
                Severity::Pass,
                "{id} should PASS (vacuous)",
            );
        }
    }

    #[test]
    fn plugins_compile_fails_with_path_and_message_in_hint() {
        // Spec hint: "the reported plugin file has a syntax
        // error at line N". `PluginError::Compile.message`
        // already includes the line/col from rhai's parser; the
        // doctor renders it verbatim so the hint stays
        // actionable.
        let env = with_plugins(DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
            compiled_count: 0,
            errors: vec![PluginError::Compile {
                path: PathBuf::from("/home/user/.config/linesmith/segments/broken.rhai"),
                message: "Syntax error at line 7, position 3".to_string(),
            }],
        }));
        let r = build_report(&env);
        let compile = find_check(&r, "plugins.compile");
        assert_eq!(compile.severity(), Severity::Fail);
        let hint = compile.hint().unwrap();
        assert!(
            hint.contains("broken.rhai") && hint.contains("line 7"),
            "hint must surface the path and the parser line/col: {hint}",
        );
    }

    #[test]
    fn plugins_deps_valid_fails_on_unknown_dep() {
        let env = with_plugins(DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
            compiled_count: 0,
            errors: vec![PluginError::UnknownDataDep {
                path: PathBuf::from("/plugins/dep-typo.rhai"),
                name: "credentialz".to_string(),
            }],
        }));
        let r = build_report(&env);
        let check = find_check(&r, "plugins.deps_valid");
        assert_eq!(check.severity(), Severity::Fail);
        assert!(
            check.hint().unwrap().contains("credentialz"),
            "hint must surface the offending dep name: {:?}",
            check.hint(),
        );
    }

    #[test]
    fn plugins_deps_valid_fails_on_malformed_header() {
        let env = with_plugins(DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
            compiled_count: 0,
            errors: vec![PluginError::MalformedDataDeps {
                path: PathBuf::from("/plugins/bad-header.rhai"),
                message: "expected JSON array, got string".to_string(),
            }],
        }));
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "plugins.deps_valid").severity(),
            Severity::Fail,
        );
    }

    #[test]
    fn plugins_no_id_collisions_fails_when_two_plugins_share_id() {
        // Built-in collision goes to a different check; pin that
        // a plugin-vs-plugin collision lands on the right row.
        let env = with_plugins(DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
            compiled_count: 1,
            errors: vec![PluginError::IdCollision {
                id: "my_segment".to_string(),
                winner: CollisionWinner::Plugin(PathBuf::from("/plugins/first.rhai")),
                loser_path: PathBuf::from("/plugins/second.rhai"),
            }],
        }));
        let r = build_report(&env);
        let check = find_check(&r, "plugins.no_id_collisions");
        assert_eq!(check.severity(), Severity::Fail);
        assert!(
            check.hint().unwrap().contains("first.rhai")
                && check.hint().unwrap().contains("second.rhai"),
            "hint must name both winner and loser: {:?}",
            check.hint(),
        );
        // Built-in collisions check stays clean — collisions are
        // partitioned correctly.
        assert_eq!(
            find_check(&r, "plugins.no_builtin_collisions").severity(),
            Severity::Pass,
        );
    }

    #[test]
    fn plugins_no_builtin_collisions_fails_when_plugin_id_shadows_builtin() {
        let env = with_plugins(DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
            compiled_count: 0,
            errors: vec![PluginError::IdCollision {
                id: "model".to_string(),
                winner: CollisionWinner::BuiltIn,
                loser_path: PathBuf::from("/plugins/model.rhai"),
            }],
        }));
        let r = build_report(&env);
        let check = find_check(&r, "plugins.no_builtin_collisions");
        assert_eq!(check.severity(), Severity::Fail);
        assert!(
            check.label().contains("shadow") || check.hint().unwrap().contains("model.rhai"),
            "label or hint should make the shadowing concrete: label={} hint={:?}",
            check.label(),
            check.hint(),
        );
        // Plugin-vs-plugin collision check stays clean.
        assert_eq!(
            find_check(&r, "plugins.no_id_collisions").severity(),
            Severity::Pass,
        );
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "render-time PluginError reached")]
    fn plugins_render_time_error_panics_in_debug_builds() {
        // In debug builds the doctor `debug_assert!`s on
        // render-time variants in `load_errors()` because their
        // presence indicates a runtime bug — the spec puts doctor
        // panics in the bug-report bucket, not the
        // graceful-degradation bucket. Release behavior (FAIL +
        // hint) is pinned by the sibling test below, which drives
        // `check_plugins_compile` directly so it doesn't trip the
        // debug_assert and runs in any build mode.
        let env = with_plugins(DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
            compiled_count: 1,
            errors: vec![PluginError::Runtime {
                id: "my_seg".to_string(),
                message: "should not surface here".to_string(),
            }],
        }));
        let _ = build_report(&env);
    }

    #[test]
    fn check_plugins_compile_fails_loud_on_unexpected_variants() {
        // Drive `check_plugins_compile` directly with a populated
        // `unexpected` slice, bypassing the orchestrator's
        // debug_assert. This is the release-build behavior: route
        // render-time variants through the "unexpected" bucket and
        // surface a FAIL on `plugins.compile` with a "file a
        // linesmith bug" hint. Loop covers all 4 render-time
        // variants so a future refactor that splits one out by
        // accident (catching only Runtime, leaving the other 3
        // silent) trips here. Runs in any build mode.
        let cases = [
            PluginError::Runtime {
                id: "x".to_string(),
                message: "boom".to_string(),
            },
            PluginError::Timeout {
                id: "x".to_string(),
            },
            PluginError::ResourceExceeded {
                id: "x".to_string(),
                limit: crate::plugins::errors::ResourceLimit::MaxOperations,
            },
            PluginError::MalformedReturn {
                id: "x".to_string(),
                message: "boom".to_string(),
            },
        ];
        for err in &cases {
            let unexpected: Vec<&PluginError> = vec![err];
            let result = check_plugins_compile(0, &[], &unexpected);
            assert_eq!(
                result.severity(),
                Severity::Fail,
                "render-time variant {err:?} should FAIL the compile row",
            );
            assert!(
                result.hint().unwrap().contains("file a linesmith bug"),
                "hint should direct the user to file a bug: {:?}",
                result.hint(),
            );
        }
    }

    // --- Git category ---

    use crate::data_context::git::{Head, RepoKind};

    fn with_git(snapshot: DoctorGitSnapshot) -> DoctorEnv {
        let mut env = DoctorEnv::healthy();
        env.git = snapshot;
        env
    }

    fn git_summary(repo_kind: RepoKind, head: Head) -> GitContextSummary {
        GitContextSummary {
            repo_path: PathBuf::from("/home/user/code/project/.git"),
            repo_kind,
            head,
        }
    }

    #[test]
    fn git_repo_detected_passes_when_repo_present() {
        let r = build_report(&DoctorEnv::healthy());
        for id in ["git.repo_detected", "git.head_resolves", "git.repo_kind"] {
            assert_eq!(
                find_check(&r, id).severity(),
                Severity::Pass,
                "{id} should PASS in healthy env",
            );
        }
    }

    #[test]
    fn git_not_in_repo_skips_all_three_when_no_git_segment_configured() {
        // Spec carve-out: "Not in a repo (SKIP if no git_*
        // segment enabled)". A user who's outside any repo and
        // hasn't enabled `git_branch` shouldn't see a WARN —
        // their config doesn't ask for git data.
        let env = with_git(DoctorGitSnapshot::NotInRepo);
        let r = build_report(&env);
        // Healthy fixture has no config.line, so no segments
        // configured; expect SKIP across all three.
        for id in ["git.repo_detected", "git.head_resolves", "git.repo_kind"] {
            assert_eq!(
                find_check(&r, id).severity(),
                Severity::Skip,
                "{id} should SKIP when not-in-repo + no git_* segment",
            );
        }
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn git_not_in_repo_warns_when_git_segment_configured() {
        // Same spec carve-out, opposite branch: `git_branch` is
        // configured, so doctor surfaces the WARN — the segment
        // would silently hide otherwise and the user might wonder
        // where their branch went.
        let mut env = with_git(DoctorGitSnapshot::NotInRepo);
        let config = Config {
            line: Some(LineConfig {
                segments: vec!["git_branch".into()],
                ..LineConfig::default()
            }),
            ..Config::default()
        };
        env.config = config_snapshot_loaded(config);
        let r = build_report(&env);
        let detected = find_check(&r, "git.repo_detected");
        assert_eq!(detected.severity(), Severity::Warn);
        assert!(
            detected.hint().unwrap().contains("git_"),
            "hint should reference the configured git segment: {:?}",
            detected.hint(),
        );
    }

    #[test]
    fn git_failed_on_corrupt_repo_renders_message() {
        let env = with_git(DoctorGitSnapshot::Failed {
            message: "InvalidFormat: expected git_dir".to_string(),
        });
        let r = build_report(&env);
        let detected = find_check(&r, "git.repo_detected");
        assert_eq!(detected.severity(), Severity::Fail);
        assert!(
            detected.label().contains("InvalidFormat"),
            "label should surface the gix error message: {}",
            detected.label(),
        );
        // Downstream rows SKIP when discover failed — no point
        // asking about HEAD when we couldn't open the repo.
        assert_eq!(
            find_check(&r, "git.head_resolves").severity(),
            Severity::Skip,
        );
        assert_eq!(find_check(&r, "git.repo_kind").severity(), Severity::Skip);
    }

    #[test]
    fn git_repo_kind_label_includes_worktree_name() {
        let env = with_git(DoctorGitSnapshot::Repo(git_summary(
            RepoKind::LinkedWorktree {
                name: "feature-x".to_string(),
            },
            Head::Branch("feature-x".to_string()),
        )));
        let r = build_report(&env);
        let kind = find_check(&r, "git.repo_kind");
        assert_eq!(kind.severity(), Severity::Pass);
        assert!(
            kind.label().contains("feature-x"),
            "worktree label must name the worktree: {}",
            kind.label(),
        );
    }

    #[test]
    fn git_head_resolves_renders_each_variant_distinctly() {
        // Pin the label rendering for each Head variant so a
        // refactor that swaps formatting strings shows up here.
        // gix::ObjectId::null() is the documented "no commit"
        // sentinel; works for the Detached-rendering pin since
        // the doctor doesn't validate the OID against any object
        // database.
        let cases = [
            (Head::Branch("main".to_string()), "HEAD -> main".to_string()),
            (
                Head::Detached(gix::ObjectId::null(gix::hash::Kind::Sha1)),
                "HEAD detached at".to_string(),
            ),
            (
                Head::Unborn {
                    symbolic_ref: "main".to_string(),
                },
                "HEAD unborn".to_string(),
            ),
            (
                Head::OtherRef {
                    full_name: "refs/tags/v1.0".to_string(),
                },
                "HEAD -> refs/tags/v1.0".to_string(),
            ),
        ];
        for (head, expected_substr) in cases {
            let env = with_git(DoctorGitSnapshot::Repo(git_summary(RepoKind::Main, head)));
            let r = build_report(&env);
            let check = find_check(&r, "git.head_resolves");
            assert_eq!(check.severity(), Severity::Pass);
            assert!(
                check.label().contains(&expected_substr),
                "label `{}` should contain `{}`",
                check.label(),
                expected_substr,
            );
        }
    }

    #[test]
    fn git_head_resolves_warns_on_lossy_otherref() {
        // gix decodes refnames via `as_bstr().to_string()`, which
        // substitutes U+FFFD for invalid bytes. A `\u{FFFD}` in
        // `OtherRef.full_name` means the underlying refname is
        // corrupt; PASS would silently hide the problem and the
        // user would chase phantom segment misbehavior.
        let env = with_git(DoctorGitSnapshot::Repo(git_summary(
            RepoKind::Main,
            Head::OtherRef {
                full_name: "refs/heads/bad\u{FFFD}name".to_string(),
            },
        )));
        let r = build_report(&env);
        let check = find_check(&r, "git.head_resolves");
        assert_eq!(check.severity(), Severity::Warn);
        assert!(
            check.label().contains("non-UTF-8") || check.hint().unwrap().contains("UTF-8"),
            "label/hint should call out the UTF-8 issue: label={} hint={:?}",
            check.label(),
            check.hint(),
        );
    }

    #[test]
    fn any_git_segment_enabled_returns_false_when_unloaded() {
        assert!(!any_git_segment_enabled(&ConfigReadOutcome::Unresolved));
        assert!(!any_git_segment_enabled(&ConfigReadOutcome::NotFound {
            path: PathBuf::from("/nope"),
            explicit: false,
        }));
    }

    #[test]
    fn any_git_segment_enabled_ignores_plugin_namespace_collisions() {
        // A plugin author can register an id like `git_typo` —
        // that's a plugin's segment, not the runtime's `git_*`
        // suite. A naive `starts_with("git_")` lens would trigger
        // the WARN ("not in a repo, but a git segment is
        // configured") for users who never asked the runtime to
        // surface git data. Gating against `BUILT_IN_SEGMENT_IDS`
        // keeps the gate tight without breaking forward-compat
        // for future built-in `git_status` / `git_stash`.
        let parsed: Config = r#"[line]
segments = ["model", "git_typo", "git_my_plugin", "cost"]
"#
        .parse()
        .expect("parse");
        let snap = config_snapshot_loaded(parsed);
        assert!(!any_git_segment_enabled(&snap.read));
    }

    #[test]
    fn any_git_segment_enabled_handles_malformed_numbered_line_tables() {
        // Pin the `.is_some_and` chain's short-circuit behavior:
        // malformed `[line.N]` shapes (non-table value, missing
        // `segments` array, non-string array entry) all degrade
        // to `false`. A refactor to `unwrap()` would surface
        // here.
        let cases: &[&str] = &[
            // [line.1] is a string, not a table.
            "[line]\nsegments = [\"model\"]\n[line]\n1 = \"not-a-table\"\n",
            // [line.1].segments is an integer.
            "[line]\nsegments = [\"model\"]\n[line.1]\nsegments = 42\n",
            // [line.1].segments contains a non-string.
            "[line]\nsegments = [\"model\"]\n[line.1]\nsegments = [42]\n",
            // [line.1] table missing `segments` entirely.
            "[line]\nsegments = [\"model\"]\n[line.1]\nfoo = \"bar\"\n",
        ];
        for raw in cases {
            // Some inputs may not parse as TOML; skip those — the
            // doctor's parse-error short-circuit handles them
            // earlier in the chain. We're only testing what
            // `any_git_segment_enabled` does once the value DID
            // parse.
            let Ok(parsed) = raw.parse::<Config>() else {
                continue;
            };
            let snap = config_snapshot_loaded(parsed);
            assert!(
                !any_git_segment_enabled(&snap.read),
                "malformed input should yield false; raw was:\n{raw}",
            );
        }
    }

    #[test]
    fn any_git_segment_enabled_finds_id_in_main_line() {
        let parsed: Config = r#"[line]
segments = ["model", "git_branch", "cost"]
"#
        .parse()
        .expect("parse");
        let snap = config_snapshot_loaded(parsed);
        assert!(any_git_segment_enabled(&snap.read));
    }

    #[test]
    fn any_git_segment_enabled_finds_id_in_numbered_line() {
        let parsed: Config = r#"
[line]
segments = ["model"]

[line.1]
segments = ["git_branch"]
"#
        .parse()
        .expect("parse");
        let snap = config_snapshot_loaded(parsed);
        assert!(any_git_segment_enabled(&snap.read));
    }

    #[test]
    fn any_git_segment_enabled_returns_false_for_no_git_config() {
        let parsed: Config = r#"[line]
segments = ["model", "cost", "context_window"]
"#
        .parse()
        .expect("parse");
        let snap = config_snapshot_loaded(parsed);
        assert!(!any_git_segment_enabled(&snap.read));
    }

    // --- Self/update_available classifier ---
    //
    // Pure-input tests for `classify_update_response`; the network-
    // touching `snapshot_update_probe` is exercised via its end-to-
    // end smoke test (DoctorEnv::from_process must not panic) since
    // its outcome is host-dependent.

    #[test]
    fn classify_update_response_returns_latest_when_tag_matches_local() {
        let body = br#"{"tag_name":"v1.2.3","name":"linesmith 1.2.3"}"#;
        match classify_update_response(body, "1.2.3") {
            DoctorUpdateProbe::Latest => {}
            other => panic!("expected Latest, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_returns_latest_when_local_is_newer() {
        // Dev pull from main, ahead of the latest published release
        // (local 1.3.0 vs published 1.2.3).
        let body = br#"{"tag_name":"v1.2.3"}"#;
        match classify_update_response(body, "1.3.0") {
            DoctorUpdateProbe::Latest => {}
            other => panic!("expected Latest (local newer), got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_returns_newer_when_remote_is_higher() {
        let body = br#"{"tag_name":"v2.0.0"}"#;
        match classify_update_response(body, "1.2.3") {
            DoctorUpdateProbe::Newer { latest } => {
                assert_eq!(latest, "v2.0.0", "tag_name preserved verbatim");
            }
            other => panic!("expected Newer, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_returns_parse_error_on_invalid_json() {
        let body = b"<html>captive portal</html>";
        match classify_update_response(body, "1.2.3") {
            DoctorUpdateProbe::ParseError { message } => {
                assert!(!message.is_empty(), "parse error needs a diagnostic");
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_returns_parse_error_when_tag_name_missing() {
        let body = br#"{"name":"some release"}"#;
        match classify_update_response(body, "1.2.3") {
            DoctorUpdateProbe::ParseError { message } => {
                assert!(message.contains("tag_name"), "diagnostic: {message}");
            }
            other => panic!("expected ParseError for missing tag_name, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_strips_v_prefix_for_comparison() {
        // GitHub conventionally uses `v1.2.3`; CARGO_PKG_VERSION never
        // does. Equality must hold after the parser strips the `v`.
        let body = br#"{"tag_name":"v0.1.1"}"#;
        match classify_update_response(body, "0.1.1") {
            DoctorUpdateProbe::Latest => {}
            other => panic!("expected Latest after v-prefix strip, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_returns_parse_error_when_remote_unparseable_but_local_parses() {
        // Mixed-parseability path: comparing local semver against an
        // unparseable remote tag (`v1.foo.0`, `nightly`) is undefined.
        // Earlier code fell back to string equality, which silently
        // produced an unverified Newer claim or — when string-equal —
        // a Latest PASS the comparator never validated. Spec WARN
        // with a clear "couldn't compare" diagnostic instead.
        let body = br#"{"tag_name":"nightly-build-2026-04-30"}"#;
        match classify_update_response(body, "1.2.3") {
            DoctorUpdateProbe::ParseError { message } => {
                assert!(
                    message.contains("nightly-build-2026-04-30"),
                    "diagnostic should name the offending tag: {message}"
                );
                assert!(
                    message.contains("MAJOR.MINOR.PATCH"),
                    "diagnostic should explain the expected shape: {message}"
                );
            }
            other => panic!("expected ParseError for unparseable remote tag, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_returns_newer_when_both_unparseable_and_unequal() {
        // Monorepo / non-semver tags on both sides — string equality
        // is the only meaningful comparator. Inequality falls to
        // Newer with the verbatim tag so the user can decide.
        let body = br#"{"tag_name":"linesmith-stable"}"#;
        match classify_update_response(body, "nightly") {
            DoctorUpdateProbe::Newer { latest } => {
                assert_eq!(latest, "linesmith-stable");
            }
            other => panic!("expected Newer fallback for both-unparseable, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_returns_latest_when_both_unparseable_and_equal() {
        // String-equality fallback's PASS branch — both sides ship
        // the same non-semver tag.
        let body = br#"{"tag_name":"nightly"}"#;
        match classify_update_response(body, "nightly") {
            DoctorUpdateProbe::Latest => {}
            other => panic!("expected Latest for both-unparseable + string-equal, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_returns_parse_error_when_local_version_invalid() {
        // Defensive: if our own CARGO_PKG_VERSION came out malformed,
        // surface that as a probe ParseError rather than silently
        // PASSing on a bogus comparison.
        let body = br#"{"tag_name":"v1.2.3"}"#;
        match classify_update_response(body, "not-a-version") {
            DoctorUpdateProbe::ParseError { message } => {
                assert!(message.contains("not-a-version"), "diagnostic: {message}");
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_clamps_serde_error_to_one_line_under_200_chars() {
        // Defensive guard: serde_json::Error::to_string() can echo
        // body fragments (an HTML error body whose first line carries
        // a long token-bearing URL, a multi-line proxy splash, etc.).
        // clamp_diag should drop everything past the first newline
        // and cap at 200 chars + ellipsis.
        let body = b"<html>\n<a href=\"https://evil/?token=secret\">click</a>\nbody...";
        match classify_update_response(body, "1.2.3") {
            DoctorUpdateProbe::ParseError { message } => {
                assert!(
                    !message.contains('\n'),
                    "message must be single-line: {message:?}"
                );
                assert!(
                    !message.contains("token="),
                    "post-newline content must be dropped: {message:?}"
                );
                assert!(
                    message.len() <= 203,
                    "message too long: {} bytes",
                    message.len()
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_strips_control_chars_from_remote_tag() {
        // Defensive ANSI-injection guard: a compromised response
        // body could JSON-escape terminal escape sequences in
        // `tag_name`; serde_json decodes `\u001b` and `\u0007` into
        // raw ESC and BEL bytes that would render straight to stdout
        // via the WARN line. sanitize_tag drops them before storing.
        let body = br#"{"tag_name":"v9.9.9\u001b[2J\u0007hostile"}"#;
        match classify_update_response(body, "1.2.3") {
            DoctorUpdateProbe::Newer { latest } => {
                assert!(!latest.contains('\x1b'), "ESC must be stripped: {latest:?}");
                assert!(!latest.contains('\x07'), "BEL must be stripped: {latest:?}");
                assert!(
                    latest.contains("v9.9.9"),
                    "printable content kept: {latest:?}"
                );
            }
            other => panic!("expected Newer with sanitized tag, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_caps_remote_tag_at_64_chars() {
        // Bounded length so a tag the size of the body cap can't
        // paint the user's terminal with screenfuls of "upgrade to ...".
        // Use a non-semver shape (no dots) so the comparator falls
        // through to the both-unparseable Newer branch where
        // sanitize_tag actually applies.
        let huge_tag: String = std::iter::repeat_n('x', 1000).collect();
        let body = format!(r#"{{"tag_name":"{huge_tag}"}}"#);
        match classify_update_response(body.as_bytes(), "nightly") {
            DoctorUpdateProbe::Newer { latest } => {
                assert!(
                    latest.chars().count() <= 64,
                    "got {} chars",
                    latest.chars().count()
                );
            }
            other => panic!("expected Newer with capped tag, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_handles_u32_overflow_in_remote_major() {
        // u32::MAX + 1 = 4294967296. parse_three_part_version returns
        // None → mixed-parseability arm → ParseError, not a silent
        // string-equality fallback.
        let body = br#"{"tag_name":"4294967296.0.0"}"#;
        match classify_update_response(body, "1.2.3") {
            DoctorUpdateProbe::ParseError { message } => {
                assert!(message.contains("4294967296"), "diagnostic: {message}");
            }
            other => panic!("expected ParseError for u32 overflow, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_handles_leading_zeros_as_equal() {
        // "v01.02.03" parses as (1, 2, 3) per u32::from_str's leading-
        // zero acceptance. Equal-tuple compare → Latest.
        let body = br#"{"tag_name":"v01.02.03"}"#;
        match classify_update_response(body, "1.2.3") {
            DoctorUpdateProbe::Latest => {}
            other => panic!("expected Latest for leading-zero remote, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_response_inner_returns_transport_error_when_body_truncated() {
        // Regression test for the 32 KiB → 256 KiB cap bump. If the
        // body cap is exhausted (truncation flag true), the result
        // must be a TransportError with a "bump the cap" pointer —
        // NOT a ParseError pointing the user at a "GitHub API shape
        // changed" red herring (the original symptom that smoke
        // testing the live binary surfaced). Without this guard, a
        // future cap-tuner who lowers the constant introduces the
        // same silent misclassification all over again.
        //
        // The synthetic body must exceed `UPDATE_PROBE_MAX_BYTES` to
        // satisfy the `debug_assert!` linking `truncated` to body
        // length — caller can't claim "truncated" without actually
        // having overshot the cap.
        let body = vec![b'x'; 256 * 1024 + 1];
        match classify_update_response_inner(&body, "0.1.1", true) {
            DoctorUpdateProbe::TransportError { message } => {
                assert!(
                    message.contains("UPDATE_PROBE_MAX_BYTES"),
                    "diagnostic should name the constant to bump: {message}"
                );
            }
            other => panic!("expected TransportError on truncation, got {other:?}"),
        }
    }
}
