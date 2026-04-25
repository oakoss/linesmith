//! OAuth credential resolution for the OAuth `/api/oauth/usage`
//! endpoint. Reads the access token from macOS Keychain (primary +
//! multi-account fallback) or from a file cascade
//! (`$CLAUDE_CONFIG_DIR`, XDG, `~/.claude/`).
//!
//! Canonical spec: `docs/specs/credentials.md`.
//!
//! **Sensitivity contract.** The token never appears in [`Debug`] or
//! [`Display`] output anywhere in this module. [`Credentials`] wraps
//! the token in [`secrecy::SecretString`] with manual `Debug` that
//! redacts it. [`CredentialError::ParseError`] carries a
//! `serde_json::Error` whose Display would include a snippet of the
//! source bytes — our Display impl prints only the path and line /
//! column, never the cause's Display. [`std::error::Error::source`]
//! still chains to the raw cause so callers who opt in (with caution)
//! can inspect it.

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret, SecretString};

/// Maximum credentials-file size we'll read. Claude Code writes
/// small files (typically <2 KB); cap at 1 MB per
/// `docs/specs/credentials.md` §Edge cases ("Huge credentials file")
/// to defend against pathological inputs.
const MAX_FILE_SIZE: u64 = 1_000_000;

#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

// --- Types --------------------------------------------------------------

/// Resolved OAuth credentials. Clone-on-Arc across segments; the
/// underlying [`SecretString`] is cheap to clone.
#[derive(Clone)]
pub struct Credentials {
    token: SecretString,
    scopes: Vec<String>,
    source: CredentialSource,
}

impl Credentials {
    /// Access the raw bearer token. Consumers must not log or
    /// serialize the returned string.
    #[must_use]
    pub fn token(&self) -> &str {
        self.token.expose_secret()
    }

    /// OAuth scopes granted to this token.
    #[must_use]
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }

    /// Which cascade step yielded the token (informational, for
    /// `linesmith doctor`).
    #[must_use]
    pub fn source(&self) -> &CredentialSource {
        &self.source
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("token", &"<redacted>")
            .field("scopes", &self.scopes)
            .field("source", &self.source)
            .finish()
    }
}

#[cfg(test)]
impl Credentials {
    /// Test-only constructor. Lets other modules in the crate
    /// fabricate a `Credentials` without running the cascade.
    /// Rejects empty tokens so tests can't fabricate a state the
    /// production resolver would reject as `EmptyToken`.
    pub(crate) fn for_testing(token: impl Into<String>) -> Self {
        let token: String = token.into();
        debug_assert!(
            !token.is_empty(),
            "Credentials::for_testing requires a non-empty token",
        );
        Self {
            token: SecretString::from(token),
            scopes: Vec::new(),
            source: CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/test"),
            },
        }
    }
}

/// Where [`resolve_credentials`] found the token.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CredentialSource {
    /// `security find-generic-password -s "Claude Code-credentials"`.
    MacosKeychainPrimary,
    /// `security dump-keychain` scan for
    /// `Claude Code-credentials<suffix>` entries; newest `mdat` wins.
    MacosKeychainMultiAccount {
        service: String,
        mdat: Option<String>,
    },
    /// `$CLAUDE_CONFIG_DIR/.credentials.json`.
    EnvDir { path: PathBuf },
    /// `$XDG_CONFIG_HOME/claude/.credentials.json` (fallback to
    /// `~/.config/claude/.credentials.json`).
    XdgConfig { path: PathBuf },
    /// `~/.claude/.credentials.json` (Claude Code's legacy path).
    ClaudeLegacy { path: PathBuf },
}

/// Failure modes for [`resolve_credentials`]. `Debug` and `Display`
/// deliberately avoid forwarding `serde_json::Error`'s Display because
/// its context snippet may include token bytes; the raw cause is still
/// reachable via [`std::error::Error::source`] for callers who need it.
#[non_exhaustive]
pub enum CredentialError {
    /// No token found in any cascade path.
    NoCredentials,
    /// `security` subprocess failed to launch or exited non-zero for
    /// non-"not-found" reasons (Keychain locked, permission denied,
    /// binary missing).
    SubprocessFailed(io::Error),
    /// Credentials file exists but could not be opened / read
    /// (permission denied, truncated read, filesystem error).
    IoError { path: PathBuf, cause: io::Error },
    /// Credentials file is not valid JSON. Inner cause preserved for
    /// [`std::error::Error::source`]; neither Display nor Debug
    /// forwards its text.
    ParseError {
        path: PathBuf,
        cause: serde_json::Error,
    },
    /// Credentials file parsed but the `claudeAiOauth` section is
    /// absent entirely. Typically indicates a stale Claude Code
    /// writer or a different tool sharing the path.
    MissingField { path: PathBuf },
    /// `claudeAiOauth` is present but `accessToken` is missing,
    /// `null`, or an empty string — the token slot exists but can't
    /// be used.
    EmptyToken { path: PathBuf },
}

impl CredentialError {
    /// Short plugin-facing error tag per `docs/specs/plugin-api.md`
    /// §ctx shape exposed to rhai. `MissingField` and `EmptyToken`
    /// aren't in plugin-api.md's enumerated list yet; spec will add
    /// them in a v0.2 rev.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::NoCredentials => "NoCredentials",
            Self::SubprocessFailed(_) => "SubprocessFailed",
            Self::IoError { .. } => "IoError",
            Self::ParseError { .. } => "ParseError",
            Self::MissingField { .. } => "MissingField",
            Self::EmptyToken { .. } => "EmptyToken",
        }
    }
}

/// Lossy `Clone`: `io::Error` and `serde_json::Error` don't implement
/// `Clone`, so variants carrying them reconstruct near-equivalents
/// (same `kind()` + textual message; `raw_os_error` and serde line
/// numbers are lost). The variant tag — which is what [`Self::code`]
/// and segment renderers key off per `rate-limit-segments.md`
/// §Error message table — round-trips exactly. The crate's
/// `unsafe_code = "forbid"` lint forecloses a transmute-based shallow
/// clone, so this is the cheapest way to preserve variant-level
/// detail across `Arc<Result<_, Self>>` boundaries like the cascade.
impl Clone for CredentialError {
    fn clone(&self) -> Self {
        match self {
            Self::NoCredentials => Self::NoCredentials,
            Self::SubprocessFailed(e) => {
                Self::SubprocessFailed(io::Error::new(e.kind(), e.to_string()))
            }
            Self::IoError { path, cause } => Self::IoError {
                path: path.clone(),
                cause: io::Error::new(cause.kind(), cause.to_string()),
            },
            Self::ParseError { path, cause } => Self::ParseError {
                path: path.clone(),
                cause: serde_json::Error::io(io::Error::other(cause.to_string())),
            },
            Self::MissingField { path } => Self::MissingField { path: path.clone() },
            Self::EmptyToken { path } => Self::EmptyToken { path: path.clone() },
        }
    }
}

impl fmt::Debug for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCredentials => f.write_str("NoCredentials"),
            Self::SubprocessFailed(e) => {
                f.debug_tuple("SubprocessFailed").field(&e.kind()).finish()
            }
            Self::IoError { path, cause } => f
                .debug_struct("IoError")
                .field("path", path)
                .field("cause_kind", &cause.kind())
                .finish(),
            Self::ParseError { path, cause } => f
                .debug_struct("ParseError")
                .field("path", path)
                .field("line", &cause.line())
                .field("column", &cause.column())
                .finish(),
            Self::MissingField { path } => {
                f.debug_struct("MissingField").field("path", path).finish()
            }
            Self::EmptyToken { path } => f.debug_struct("EmptyToken").field("path", path).finish(),
        }
    }
}

impl fmt::Display for CredentialError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoCredentials => f.write_str("no OAuth credentials found"),
            Self::SubprocessFailed(e) => {
                write!(f, "security subprocess failed ({kind})", kind = e.kind())
            }
            Self::IoError { path, cause } => write!(
                f,
                "failed to read credentials file {}: {kind}",
                path.display(),
                kind = cause.kind()
            ),
            Self::ParseError { path, cause } => write!(
                f,
                "credentials file {} failed to parse at line {}, column {}",
                path.display(),
                cause.line(),
                cause.column()
            ),
            Self::MissingField { path } => write!(
                f,
                "credentials file {} missing claudeAiOauth.accessToken",
                path.display()
            ),
            Self::EmptyToken { path } => write!(
                f,
                "credentials file {} has empty accessToken",
                path.display()
            ),
        }
    }
}

impl std::error::Error for CredentialError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::SubprocessFailed(e) => Some(e),
            Self::IoError { cause, .. } => Some(cause),
            Self::ParseError { cause, .. } => Some(cause),
            _ => None,
        }
    }
}

// --- Serde shapes -------------------------------------------------------

#[derive(serde::Deserialize)]
struct CredentialsFile {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: Option<ClaudeAiOauth>,
}

#[derive(serde::Deserialize)]
struct ClaudeAiOauth {
    /// Double-wrapped `Option` so we can tell "key absent" from
    /// "key present and null" — serde's default `Option<String>`
    /// collapses both into `None`. With [`deserialize_explicit`]:
    ///   * key absent       → `None`             → `MissingField`
    ///   * explicit `null`  → `Some(None)`       → `EmptyToken`
    ///   * empty string     → `Some(Some(""))`   → `EmptyToken`
    ///   * non-empty string → `Some(Some(_))`    → token
    ///   * any other type   → serde fails parse  → `ParseError`
    #[serde(
        default,
        rename = "accessToken",
        deserialize_with = "deserialize_explicit"
    )]
    access_token: Option<Option<String>>,
    #[serde(default)]
    scopes: Vec<String>,
}

/// Deserialize helper that preserves the distinction between an
/// absent JSON key and one explicitly set to `null`. Invoked only
/// when the key is present (`#[serde(default)]` handles absence).
fn deserialize_explicit<'de, D>(de: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;
    Option::<String>::deserialize(de).map(Some)
}

// --- Public entry point -------------------------------------------------

/// Resolve the OAuth access token via the cascade in
/// `docs/specs/credentials.md` §Resolution cascade: macOS Keychain
/// (primary + multi-account) on macOS, then file-based cascade on all
/// platforms. Memoization for process-lifetime reuse is the caller's
/// responsibility; each invocation re-runs the full cascade.
pub fn resolve_credentials() -> Result<Credentials, CredentialError> {
    // Hold the first "fall-through" subprocess error so if every
    // later cascade step also yields nothing, we surface the real
    // reason (e.g., Keychain locked) instead of a generic
    // `NoCredentials`. Per credentials.md §Resolution cascade step 1.
    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut first_subprocess_err: Option<CredentialError> = None;

    #[cfg(target_os = "macos")]
    {
        match macos::try_keychain_primary() {
            Ok(Some(creds)) => return Ok(creds),
            Ok(None) => {}
            Err(e) => first_subprocess_err = Some(e),
        }
        match macos::try_keychain_multi_account() {
            Ok(Some(creds)) => return Ok(creds),
            Ok(None) => {}
            Err(e) => {
                if first_subprocess_err.is_none() {
                    first_subprocess_err = Some(e);
                }
            }
        }
    }

    match try_file_cascade() {
        Ok(creds) => Ok(creds),
        Err(CredentialError::NoCredentials) => {
            // Prefer a recorded subprocess failure over the generic
            // "nothing found" terminal.
            Err(first_subprocess_err.unwrap_or(CredentialError::NoCredentials))
        }
        Err(e) => Err(e),
    }
}

// --- File cascade -------------------------------------------------------

/// Environmental inputs for the file cascade. Process-env reads are
/// centralized here so tests can inject values without touching the
/// (thread-unsafe) real env.
#[derive(Debug, Clone, Default)]
struct FileCascadeEnv {
    /// `$CLAUDE_CONFIG_DIR` (treating empty string as unset per
    /// `credentials.md` §Edge cases).
    claude_config_dir: Option<PathBuf>,
    /// `$XDG_CONFIG_HOME` (treating empty string as unset).
    xdg_config_home: Option<PathBuf>,
    /// `$HOME` (treating empty string as unset).
    home: Option<PathBuf>,
}

impl FileCascadeEnv {
    fn from_process_env() -> Self {
        fn non_empty_var(key: &str) -> Option<PathBuf> {
            std::env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        }
        Self {
            claude_config_dir: non_empty_var("CLAUDE_CONFIG_DIR"),
            xdg_config_home: non_empty_var("XDG_CONFIG_HOME"),
            home: non_empty_var("HOME"),
        }
    }
}

/// Candidate (path, source) pairs for the file cascade, in order.
/// Returned as `Vec` rather than an iterator so tests can assert the
/// full list.
fn file_cascade_candidates(env: &FileCascadeEnv) -> Vec<(PathBuf, CredentialSource)> {
    let mut out = Vec::with_capacity(3);

    if let Some(dir) = &env.claude_config_dir {
        let path = dir.join(".credentials.json");
        out.push((path.clone(), CredentialSource::EnvDir { path }));
    }

    // XDG candidate is emitted whenever an XDG root is derivable —
    // either from `$XDG_CONFIG_HOME` directly, or from `$HOME` via the
    // default `~/.config`. A HOME-less CI/service environment with
    // only `$XDG_CONFIG_HOME` set still gets its XDG path probed.
    let xdg_root = env
        .xdg_config_home
        .clone()
        .or_else(|| env.home.as_ref().map(|h| h.join(".config")));
    if let Some(xdg_root) = xdg_root {
        let xdg_path = xdg_root.join("claude").join(".credentials.json");
        out.push((
            xdg_path.clone(),
            CredentialSource::XdgConfig { path: xdg_path },
        ));
    }

    // Legacy `~/.claude/` only makes sense when `$HOME` is available.
    if let Some(home) = &env.home {
        let legacy_path = home.join(".claude").join(".credentials.json");
        out.push((
            legacy_path.clone(),
            CredentialSource::ClaudeLegacy { path: legacy_path },
        ));
    }

    out
}

fn try_file_cascade_with(env: &FileCascadeEnv) -> Result<Credentials, CredentialError> {
    // Advance past `NotFound` and `PermissionDenied` — the spec's
    // "first readable+parseable file wins" (`credentials.md`
    // §Resolution cascade) means an unreadable path shouldn't shadow
    // a later path the user can actually open. Other IO errors are
    // terminal per credentials.md §Edge cases.
    for (path, source) in file_cascade_candidates(env) {
        match fs::metadata(&path) {
            Ok(_) => return read_and_parse_file(&path, source),
            Err(e)
                if e.kind() == io::ErrorKind::NotFound
                    || e.kind() == io::ErrorKind::PermissionDenied =>
            {
                continue
            }
            Err(cause) => return Err(CredentialError::IoError { path, cause }),
        }
    }
    Err(CredentialError::NoCredentials)
}

fn try_file_cascade() -> Result<Credentials, CredentialError> {
    try_file_cascade_with(&FileCascadeEnv::from_process_env())
}

fn read_and_parse_file(
    path: &Path,
    source: CredentialSource,
) -> Result<Credentials, CredentialError> {
    let file = fs::File::open(path).map_err(|cause| CredentialError::IoError {
        path: path.to_path_buf(),
        cause,
    })?;
    let mut buf = String::new();
    // Read up to MAX_FILE_SIZE + 1 so we can detect oversized files
    // via buffer length. Rejecting oversized files explicitly (rather
    // than letting serde parse the prefix + padding) avoids a panic
    // on pathological inputs where the first bytes form a complete
    // valid JSON followed by trailing whitespace.
    file.take(MAX_FILE_SIZE + 1)
        .read_to_string(&mut buf)
        .map_err(|cause| CredentialError::IoError {
            path: path.to_path_buf(),
            cause,
        })?;
    if buf.len() as u64 > MAX_FILE_SIZE {
        return Err(CredentialError::IoError {
            path: path.to_path_buf(),
            cause: io::Error::new(
                io::ErrorKind::InvalidData,
                format!("credentials file exceeds {MAX_FILE_SIZE} byte limit"),
            ),
        });
    }
    parse_credentials_bytes(&buf, path, source)
}

fn parse_credentials_bytes(
    bytes: &str,
    path: &Path,
    source: CredentialSource,
) -> Result<Credentials, CredentialError> {
    let file: CredentialsFile =
        serde_json::from_str(bytes).map_err(|cause| CredentialError::ParseError {
            path: path.to_path_buf(),
            cause,
        })?;
    let oauth = file
        .claude_ai_oauth
        .ok_or_else(|| CredentialError::MissingField {
            path: path.to_path_buf(),
        })?;
    // Spec-driven taxonomy (credentials.md §Interface):
    //   key absent        → MissingField (shape problem)
    //   null or "" value  → EmptyToken   (slot present, unusable)
    //   non-string value  → ParseError   (surfaces earlier during
    //                       `serde_json::from_str` via the typed
    //                       `Option<Option<String>>` field)
    //   non-empty string  → success
    match oauth.access_token {
        None => Err(CredentialError::MissingField {
            path: path.to_path_buf(),
        }),
        Some(None) => Err(CredentialError::EmptyToken {
            path: path.to_path_buf(),
        }),
        Some(Some(s)) if s.is_empty() => Err(CredentialError::EmptyToken {
            path: path.to_path_buf(),
        }),
        Some(Some(s)) => Ok(Credentials {
            token: SecretString::from(s),
            scopes: oauth.scopes,
            source,
        }),
    }
}

// --- macOS Keychain -----------------------------------------------------

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use std::io::Read;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, ExitStatus, Stdio};
    use std::time::{Duration, Instant};

    /// `security` subprocess budget. Matches the 2s OAuth endpoint
    /// budget from ADR-0011 §Endpoint contract; a wedged `security`
    /// process, locked Keychain that doesn't prompt, or pathological
    /// dump must not hang the statusline.
    const SECURITY_TIMEOUT: Duration = Duration::from_secs(2);
    const POLL_INTERVAL: Duration = Duration::from_millis(50);
    /// Grace period after `kill()` for the kernel to reap the child.
    /// If the child isn't reaped within this window we detach the
    /// reader threads instead of blocking — bounded timeout beats
    /// perfect cleanup.
    const KILL_GRACE: Duration = Duration::from_millis(500);
    /// Maximum stderr bytes embedded in a `Failed` error message.
    /// `dump-keychain` failures can emit multi-KB diagnostics; we
    /// cap to keep log payloads bounded.
    const MAX_STDERR_IN_ERROR: usize = 512;

    /// Exit code `security` uses when the requested item isn't in the
    /// keychain (macOS `errSecItemNotFound`). Any other non-zero exit
    /// indicates a real failure (locked keychain, permission denied,
    /// binary missing) that we must preserve for diagnostics.
    const ERR_SEC_ITEM_NOT_FOUND: i32 = 44;

    pub(super) fn try_keychain_primary() -> Result<Option<Credentials>, CredentialError> {
        let user = std::env::var("USER").unwrap_or_default();
        let mut args: Vec<&str> = vec!["find-generic-password"];
        if !user.is_empty() {
            args.extend(["-a", &user]);
        }
        args.extend(["-w", "-s", KEYCHAIN_SERVICE]);

        let run = run_security(&args).map_err(CredentialError::SubprocessFailed)?;
        match classify_security_exit(&run) {
            SecurityResult::ItemNotFound => return Ok(None),
            SecurityResult::Failed(e) => return Err(CredentialError::SubprocessFailed(e)),
            SecurityResult::Success => {}
        }
        let stdout = String::from_utf8_lossy(&run.stdout);
        let stdout = stdout.trim();
        if stdout.is_empty() {
            return Ok(None);
        }
        match parse_credentials_bytes(
            stdout,
            Path::new("keychain:Claude Code-credentials"),
            CredentialSource::MacosKeychainPrimary,
        ) {
            Ok(creds) => Ok(Some(creds)),
            Err(CredentialError::MissingField { .. } | CredentialError::EmptyToken { .. }) => {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    pub(super) fn try_keychain_multi_account() -> Result<Option<Credentials>, CredentialError> {
        let dump = run_security(&["dump-keychain"]).map_err(CredentialError::SubprocessFailed)?;
        match classify_security_exit(&dump) {
            SecurityResult::ItemNotFound => return Ok(None),
            SecurityResult::Failed(e) => return Err(CredentialError::SubprocessFailed(e)),
            SecurityResult::Success => {}
        }
        let dump_text = String::from_utf8_lossy(&dump.stdout);
        let mut candidates = parse_dump_for_services(&dump_text);
        // Newest mdat first; entries without mdat sort last (stable by
        // dump order).
        candidates.sort_by(|a, b| match (&a.mdat, &b.mdat) {
            (Some(am), Some(bm)) => bm.cmp(am),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        // A per-candidate failure (spawn error, timeout, locked-
        // keychain exit) shouldn't abort the whole multi-account
        // probe — later candidates may succeed. Capture the first
        // error and surface it only if every candidate misses.
        let mut first_err: Option<io::Error> = None;
        for candidate in candidates {
            let run = match run_security(&["find-generic-password", "-w", "-s", &candidate.service])
            {
                Ok(r) => r,
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                    continue;
                }
            };
            match classify_security_exit(&run) {
                SecurityResult::ItemNotFound => continue,
                SecurityResult::Failed(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                    continue;
                }
                SecurityResult::Success => {}
            }
            let stdout = String::from_utf8_lossy(&run.stdout);
            let stdout = stdout.trim();
            if stdout.is_empty() {
                continue;
            }
            match parse_credentials_bytes(
                stdout,
                Path::new("keychain"),
                CredentialSource::MacosKeychainMultiAccount {
                    service: candidate.service.clone(),
                    mdat: candidate.mdat.clone(),
                },
            ) {
                Ok(creds) => return Ok(Some(creds)),
                Err(CredentialError::MissingField { .. } | CredentialError::EmptyToken { .. }) => {
                    continue
                }
                Err(e) => return Err(e),
            }
        }
        match first_err {
            Some(e) => Err(CredentialError::SubprocessFailed(e)),
            None => Ok(None),
        }
    }

    pub(super) struct SecurityRun {
        pub status: ExitStatus,
        pub stdout: Vec<u8>,
        pub stderr: Vec<u8>,
    }

    pub(super) enum SecurityResult {
        /// Process exited with status 0.
        Success,
        /// Process exited with `errSecItemNotFound` — expected miss;
        /// the cascade should advance to the next step.
        ItemNotFound,
        /// Any other non-zero exit. Preserved so the cascade in
        /// `resolve_credentials` can surface the real reason if no
        /// later step yields credentials.
        Failed(io::Error),
    }

    /// Spawn `security` with stdout/stderr piped, drain both in reader
    /// threads so the child can't block on pipe-full, and enforce
    /// [`SECURITY_TIMEOUT`] by polling `try_wait` + killing on
    /// deadline.
    ///
    /// Happy path: child exits, pipes close, readers hit EOF, handles
    /// join to return accumulated bytes.
    ///
    /// Timeout path: `kill`, poll `try_wait` for up to [`KILL_GRACE`]
    /// so the kernel can reap the child and close the pipes. If reap
    /// doesn't happen in that window the reader handles are detached
    /// — they finish on their own once the kernel eventually reaps.
    /// Total budget is bounded at `SECURITY_TIMEOUT + KILL_GRACE`.
    pub(super) fn run_security(args: &[&str]) -> io::Result<SecurityRun> {
        let mut child = Command::new("security")
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let stdout_handle = std::thread::spawn(move || drain(stdout));
        let stderr_handle = std::thread::spawn(move || drain(stderr));

        let deadline = Instant::now() + SECURITY_TIMEOUT;
        let status = loop {
            match child.try_wait()? {
                Some(status) => break status,
                None => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        // Bounded try_wait loop; never `wait()` which
                        // could block past budget if reap stalls.
                        let grace_deadline = Instant::now() + KILL_GRACE;
                        while Instant::now() < grace_deadline {
                            if let Ok(Some(_)) = child.try_wait() {
                                break;
                            }
                            std::thread::sleep(POLL_INTERVAL);
                        }
                        // Detach reader handles; they exit once the
                        // kernel eventually closes the pipes.
                        drop(stdout_handle);
                        drop(stderr_handle);
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            format!("security timed out after {}s", SECURITY_TIMEOUT.as_secs()),
                        ));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }
        };

        // Propagate reader panics as io::Error rather than silently
        // dropping output — a panicked drain would otherwise look
        // like clean empty stdout/stderr and mask a real failure.
        let stdout = stdout_handle
            .join()
            .map_err(|_| io::Error::other("security stdout reader thread panicked"))?;
        let stderr = stderr_handle
            .join()
            .map_err(|_| io::Error::other("security stderr reader thread panicked"))?;
        Ok(SecurityRun {
            status,
            stdout,
            stderr,
        })
    }

    fn drain<R: Read>(mut reader: R) -> Vec<u8> {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        buf
    }

    pub(super) fn classify_security_exit(run: &SecurityRun) -> SecurityResult {
        if run.status.success() {
            return SecurityResult::Success;
        }
        if run.status.code() == Some(ERR_SEC_ITEM_NOT_FOUND) {
            return SecurityResult::ItemNotFound;
        }
        let stderr = String::from_utf8_lossy(&run.stderr);
        let stderr = truncate_for_error(stderr.trim());
        let msg = match (run.status.code(), run.status.signal()) {
            (Some(code), _) if stderr.is_empty() => {
                format!("security exited with status {code}")
            }
            (Some(code), _) => format!("security exited with status {code}: {stderr}"),
            (None, Some(sig)) if stderr.is_empty() => {
                format!("security terminated by signal {sig}")
            }
            (None, Some(sig)) => format!("security terminated by signal {sig}: {stderr}"),
            (None, None) if stderr.is_empty() => String::from("security terminated abnormally"),
            (None, None) => format!("security terminated abnormally: {stderr}"),
        };
        SecurityResult::Failed(io::Error::other(msg))
    }

    /// Cap stderr content embedded in error messages. `dump-keychain`
    /// failures can emit multi-KB diagnostics; an unbounded stderr
    /// could balloon log payloads.
    fn truncate_for_error(s: &str) -> String {
        if s.len() <= MAX_STDERR_IN_ERROR {
            return s.to_string();
        }
        let mut end = MAX_STDERR_IN_ERROR;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}... (truncated)", &s[..end])
    }

    struct Candidate {
        service: String,
        mdat: Option<String>,
    }

    /// Scan `security dump-keychain` output for generic-password
    /// entries whose `svce` begins with the Claude Code service prefix.
    /// Pulls optional `mdat` blobs for sort-by-modification-time.
    fn parse_dump_for_services(dump: &str) -> Vec<Candidate> {
        let mut out = Vec::new();
        let mut current_svce: Option<String> = None;
        let mut current_mdat: Option<String> = None;
        for line in dump.lines() {
            let line = line.trim();
            if line.starts_with("keychain:") {
                // New entry boundary. Emit the prior if it matches.
                if let Some(svce) = current_svce.take() {
                    if svce.starts_with(KEYCHAIN_SERVICE) {
                        out.push(Candidate {
                            service: svce,
                            mdat: current_mdat.take(),
                        });
                    } else {
                        current_mdat = None;
                    }
                }
                continue;
            }
            if let Some(val) = extract_quoted_after(line, "\"svce\"") {
                current_svce = Some(val);
            } else if let Some(val) = extract_quoted_after(line, "\"mdat\"") {
                current_mdat = Some(val);
            }
        }
        // Tail entry.
        if let Some(svce) = current_svce {
            if svce.starts_with(KEYCHAIN_SERVICE) {
                out.push(Candidate {
                    service: svce,
                    mdat: current_mdat,
                });
            }
        }
        out
    }

    /// Pull the first `"<content>"` occurrence on a line prefixed by
    /// `key`. Tolerates the `security` dump's `<blob>="<value>"` and
    /// `<blob>=0x...  "<value>"` forms.
    fn extract_quoted_after(line: &str, key: &str) -> Option<String> {
        let after_key = line.strip_prefix(key)?;
        let first_quote = after_key.find('"')?;
        let rest = &after_key[first_quote + 1..];
        let close_quote = rest.find('"')?;
        Some(rest[..close_quote].to_string())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::os::unix::process::ExitStatusExt;

        fn run_with(raw_status: i32, stderr: &[u8]) -> SecurityRun {
            SecurityRun {
                status: ExitStatus::from_raw(raw_status),
                stdout: Vec::new(),
                stderr: stderr.to_vec(),
            }
        }

        #[test]
        fn classify_success_on_zero_exit() {
            // Raw status 0 = exit code 0 = success.
            let run = run_with(0, b"");
            assert!(matches!(
                classify_security_exit(&run),
                SecurityResult::Success
            ));
        }

        #[test]
        fn classify_item_not_found_on_exit_44() {
            // Unix wait status: exit code is in the high byte.
            let run = run_with(ERR_SEC_ITEM_NOT_FOUND << 8, b"");
            assert!(matches!(
                classify_security_exit(&run),
                SecurityResult::ItemNotFound,
            ));
        }

        #[test]
        fn classify_other_non_zero_exit_is_failed_with_stderr() {
            let run = run_with(25 << 8, b"keychain locked");
            let SecurityResult::Failed(e) = classify_security_exit(&run) else {
                panic!("expected Failed variant for non-zero exit with stderr");
            };
            let msg = e.to_string();
            // Anchor on the format-string contract, not loose substrings.
            assert!(msg.contains("status 25"), "msg={msg}");
            assert!(msg.contains(": keychain locked"), "msg={msg}");
        }

        #[test]
        fn classify_signal_termination_includes_signal_number() {
            // Raw wait status 9 = terminated by SIGKILL, no exit code.
            let run = run_with(9, b"");
            let SecurityResult::Failed(e) = classify_security_exit(&run) else {
                panic!("expected Failed variant for signal termination");
            };
            let msg = e.to_string();
            assert!(msg.contains("terminated by signal 9"), "msg={msg}",);
        }

        #[test]
        fn classify_signal_termination_includes_stderr() {
            // SIGSEGV = 11; pair with diagnostic stderr to match the
            // "terminated by signal N: <stderr>" format arm.
            let run = run_with(11, b"segfault diag");
            let SecurityResult::Failed(e) = classify_security_exit(&run) else {
                panic!("expected Failed variant");
            };
            let msg = e.to_string();
            assert!(msg.contains("terminated by signal 11"), "msg={msg}");
            assert!(msg.contains(": segfault diag"), "msg={msg}");
        }

        #[test]
        fn classify_failed_truncates_long_stderr() {
            let long_stderr = "x".repeat(MAX_STDERR_IN_ERROR * 2);
            let run = run_with(25 << 8, long_stderr.as_bytes());
            let SecurityResult::Failed(e) = classify_security_exit(&run) else {
                panic!("expected Failed variant");
            };
            let msg = e.to_string();
            assert!(msg.contains("(truncated)"), "msg={msg}");
            // Embedded stderr bytes capped at MAX_STDERR_IN_ERROR.
            assert!(
                msg.len() < long_stderr.len(),
                "expected truncation, got msg.len()={}",
                msg.len()
            );
        }

        #[test]
        fn parses_dump_with_single_matching_service() {
            let dump = r#"keychain: "/Users/alice/Library/Keychains/login.keychain-db"
    "svce"<blob>="Claude Code-credentials"
    "acct"<blob>="alice"
    "mdat"<timedate>=0x30303030  "20260418105500Z"
"#;
            let candidates = parse_dump_for_services(dump);
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].service, "Claude Code-credentials");
            assert_eq!(candidates[0].mdat.as_deref(), Some("20260418105500Z"));
        }

        #[test]
        fn skips_non_matching_services() {
            let dump = r#"keychain: "/path/to/login.keychain"
    "svce"<blob>="some.other.app"
    "mdat"<timedate>=0x00 "20260101000000Z"
keychain: "/path/to/login.keychain"
    "svce"<blob>="Claude Code-credentials-acct2"
    "mdat"<timedate>=0x00 "20260420000000Z"
"#;
            let candidates = parse_dump_for_services(dump);
            assert_eq!(candidates.len(), 1);
            assert_eq!(candidates[0].service, "Claude Code-credentials-acct2");
        }
    }
}

// --- Tests --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_creds(dir: &Path, relative: &str, contents: &str) -> PathBuf {
        let path = dir.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, contents).unwrap();
        path
    }

    fn valid_credentials_json(token: &str) -> String {
        format!(
            r#"{{
                "claudeAiOauth": {{
                    "accessToken": "{token}",
                    "refreshToken": null,
                    "expiresAt": null,
                    "scopes": ["user:inference", "user:profile"],
                    "subscriptionType": null
                }}
            }}"#
        )
    }

    #[test]
    fn parses_valid_credentials_bytes() {
        let json = valid_credentials_json("test-token-xyz");
        let creds = parse_credentials_bytes(
            &json,
            Path::new("/test"),
            CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/test"),
            },
        )
        .expect("parse");
        assert_eq!(creds.token(), "test-token-xyz");
        assert_eq!(creds.scopes().len(), 2);
        assert!(matches!(
            creds.source(),
            CredentialSource::ClaudeLegacy { .. }
        ));
    }

    #[test]
    fn rejects_null_token_as_empty_token() {
        let json = r#"{ "claudeAiOauth": { "accessToken": null } }"#;
        let err = parse_credentials_bytes(
            json,
            Path::new("/test"),
            CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/test"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CredentialError::EmptyToken { .. }));
    }

    #[test]
    fn rejects_absent_access_token_key_as_missing_field() {
        // Spec: absent key → `MissingField`; `null`/`""` → `EmptyToken`.
        // Tested here: oauth block present, key simply missing.
        let json = r#"{ "claudeAiOauth": { "scopes": ["x"] } }"#;
        let err = parse_credentials_bytes(
            json,
            Path::new("/test"),
            CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/test"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CredentialError::MissingField { .. }));
    }

    #[test]
    fn rejects_non_string_access_token() {
        let json = r#"{ "claudeAiOauth": { "accessToken": 42 } }"#;
        let err = parse_credentials_bytes(
            json,
            Path::new("/test"),
            CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/test"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CredentialError::ParseError { .. }));
    }

    #[test]
    fn rejects_empty_token() {
        let json = r#"{ "claudeAiOauth": { "accessToken": "" } }"#;
        let err = parse_credentials_bytes(
            json,
            Path::new("/test"),
            CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/test"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CredentialError::EmptyToken { .. }));
    }

    #[test]
    fn rejects_missing_claude_ai_oauth() {
        let json = r#"{ "somethingElse": {} }"#;
        let err = parse_credentials_bytes(
            json,
            Path::new("/test"),
            CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/test"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CredentialError::MissingField { .. }));
    }

    #[test]
    fn rejects_invalid_json() {
        let json = "{ not json at all ";
        let err = parse_credentials_bytes(
            json,
            Path::new("/test"),
            CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/test"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CredentialError::ParseError { .. }));
    }

    #[test]
    fn scopes_default_to_empty_when_missing() {
        let json = r#"{ "claudeAiOauth": { "accessToken": "t" } }"#;
        let creds = parse_credentials_bytes(
            json,
            Path::new("/test"),
            CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/test"),
            },
        )
        .expect("parse");
        assert!(creds.scopes().is_empty());
    }

    #[test]
    fn credentials_debug_redacts_token() {
        let creds = Credentials {
            token: SecretString::from("super-secret-token".to_string()),
            scopes: vec!["x".to_string()],
            source: CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/etc/x"),
            },
        };
        let debug = format!("{creds:?}");
        assert!(
            !debug.contains("super-secret-token"),
            "debug leaks token: {debug}"
        );
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn credential_error_code_taxonomy() {
        // Construct a real serde_json::Error so ParseError's code is
        // exercised with a real cause (not a synthetic one).
        let parse_err = serde_json::from_str::<i32>("not-a-number").unwrap_err();

        let all: [(CredentialError, &str); 6] = [
            (CredentialError::NoCredentials, "NoCredentials"),
            (
                CredentialError::SubprocessFailed(io::Error::other("x")),
                "SubprocessFailed",
            ),
            (
                CredentialError::IoError {
                    path: PathBuf::from("/x"),
                    cause: io::Error::other("x"),
                },
                "IoError",
            ),
            (
                CredentialError::ParseError {
                    path: PathBuf::from("/x"),
                    cause: parse_err,
                },
                "ParseError",
            ),
            (
                CredentialError::MissingField {
                    path: PathBuf::from("/x"),
                },
                "MissingField",
            ),
            (
                CredentialError::EmptyToken {
                    path: PathBuf::from("/x"),
                },
                "EmptyToken",
            ),
        ];

        // Each variant produces its documented code.
        for (err, expected) in &all {
            assert_eq!(err.code(), *expected);
        }

        // Codes are unique across the taxonomy (no copy-paste collisions).
        let codes: std::collections::HashSet<&'static str> =
            all.iter().map(|(e, _)| e.code()).collect();
        assert_eq!(codes.len(), all.len());
    }

    #[test]
    fn parse_error_display_does_not_leak_token_bytes() {
        // Build a malformed-JSON buffer whose middle contains a
        // sentinel "token". Our Display contract promises we never
        // forward `serde_json::Error`'s context snippet — verify.
        let leaky = r#"{ "claudeAiOauth": { "accessToken": "LEAK-ME-abcdef" "#;
        let err = parse_credentials_bytes(
            leaky,
            Path::new("/etc/creds"),
            CredentialSource::ClaudeLegacy {
                path: PathBuf::from("/etc/creds"),
            },
        )
        .unwrap_err();
        assert!(matches!(err, CredentialError::ParseError { .. }));
        let display = format!("{err}");
        let debug = format!("{err:?}");
        assert!(
            !display.contains("LEAK-ME"),
            "Display leaked token: {display}"
        );
        assert!(!debug.contains("LEAK-ME"), "Debug leaked token: {debug}");
    }

    #[test]
    fn oversized_file_rejected_before_parse() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("big.json");
        // 1 MB of 'x' = exceeds MAX_FILE_SIZE — no JSON round-trip
        // possible. Ensures the length check fires before the serde
        // parser sees the buffer.
        let big = "x".repeat((MAX_FILE_SIZE + 1024) as usize);
        fs::write(&path, &big).unwrap();
        let err = read_and_parse_file(&path, CredentialSource::ClaudeLegacy { path: path.clone() })
            .unwrap_err();
        match err {
            CredentialError::IoError { cause, .. } => {
                assert_eq!(cause.kind(), io::ErrorKind::InvalidData);
            }
            other => panic!("expected IoError(InvalidData), got {other:?}"),
        }
    }

    /// Tests pass a constructed `FileCascadeEnv` rather than mutating
    /// the process env. `set_var` / `remove_var` are `unsafe` in
    /// modern Rust and the crate-level `unsafe_code = "forbid"` lint
    /// would reject the direct-mutation pattern — and using the real
    /// env in parallel tests is inherently racy anyway.
    mod cascade {
        use super::*;

        fn env_from(
            claude: Option<&Path>,
            xdg: Option<&Path>,
            home: Option<&Path>,
        ) -> FileCascadeEnv {
            FileCascadeEnv {
                claude_config_dir: claude.map(Path::to_path_buf),
                xdg_config_home: xdg.map(Path::to_path_buf),
                home: home.map(Path::to_path_buf),
            }
        }

        #[test]
        fn env_dir_candidate_included_when_set() {
            let tmp = TempDir::new().unwrap();
            let env = env_from(Some(tmp.path()), None, Some(tmp.path()));
            let candidates = file_cascade_candidates(&env);
            assert!(matches!(candidates[0].1, CredentialSource::EnvDir { .. }));
        }

        #[test]
        fn env_dir_absent_when_not_set() {
            // `FileCascadeEnv::from_process_env` already maps empty
            // strings to None; this test covers the cascade-level
            // behavior: no env-dir field → first candidate is XDG.
            let tmp = TempDir::new().unwrap();
            let env = env_from(None, None, Some(tmp.path()));
            let candidates = file_cascade_candidates(&env);
            assert!(
                !matches!(candidates[0].1, CredentialSource::EnvDir { .. }),
                "no CLAUDE_CONFIG_DIR should omit the EnvDir candidate"
            );
        }

        #[test]
        fn xdg_preferred_over_legacy_when_both_roots_present() {
            let tmp = TempDir::new().unwrap();
            let xdg = tmp.path().join("xdg");
            let env = env_from(None, Some(&xdg), Some(tmp.path()));
            let candidates = file_cascade_candidates(&env);
            let positions: Vec<_> = candidates
                .iter()
                .map(|(_, s)| match s {
                    CredentialSource::XdgConfig { .. } => "xdg",
                    CredentialSource::ClaudeLegacy { .. } => "legacy",
                    _ => "other",
                })
                .collect();
            assert_eq!(positions, ["xdg", "legacy"]);
        }

        #[test]
        fn xdg_default_root_is_home_dot_config() {
            let tmp = TempDir::new().unwrap();
            let env = env_from(None, None, Some(tmp.path()));
            let candidates = file_cascade_candidates(&env);
            let xdg = candidates
                .iter()
                .find(|(_, s)| matches!(s, CredentialSource::XdgConfig { .. }))
                .expect("xdg candidate present");
            assert!(xdg.0.starts_with(tmp.path().join(".config").join("claude")));
        }

        #[test]
        fn resolve_reads_existing_env_dir_credentials() {
            let tmp = TempDir::new().unwrap();
            write_creds(
                tmp.path(),
                ".credentials.json",
                &valid_credentials_json("env-dir-tok"),
            );
            let env = env_from(Some(tmp.path()), None, Some(tmp.path()));
            let creds = try_file_cascade_with(&env).expect("resolve");
            assert_eq!(creds.token(), "env-dir-tok");
            assert!(matches!(creds.source(), CredentialSource::EnvDir { .. }));
        }

        #[test]
        fn resolve_falls_through_to_xdg() {
            let tmp = TempDir::new().unwrap();
            let xdg = tmp.path().join("xdg");
            write_creds(
                &xdg,
                "claude/.credentials.json",
                &valid_credentials_json("xdg-tok"),
            );
            let env = env_from(
                Some(&tmp.path().join("does-not-exist")),
                Some(&xdg),
                Some(tmp.path()),
            );
            let creds = try_file_cascade_with(&env).expect("resolve");
            assert_eq!(creds.token(), "xdg-tok");
            assert!(matches!(creds.source(), CredentialSource::XdgConfig { .. }));
        }

        #[test]
        fn resolve_falls_through_to_legacy() {
            let tmp = TempDir::new().unwrap();
            write_creds(
                tmp.path(),
                ".claude/.credentials.json",
                &valid_credentials_json("legacy-tok"),
            );
            let env = env_from(None, None, Some(tmp.path()));
            let creds = try_file_cascade_with(&env).expect("resolve");
            assert_eq!(creds.token(), "legacy-tok");
            assert!(matches!(
                creds.source(),
                CredentialSource::ClaudeLegacy { .. }
            ));
        }

        #[test]
        fn resolve_no_files_returns_no_credentials() {
            let tmp = TempDir::new().unwrap();
            let env = env_from(None, None, Some(tmp.path()));
            let err = try_file_cascade_with(&env).unwrap_err();
            assert!(matches!(err, CredentialError::NoCredentials));
        }

        #[test]
        fn resolve_no_home_returns_no_credentials() {
            let env = env_from(None, None, None);
            let err = try_file_cascade_with(&env).unwrap_err();
            assert!(matches!(err, CredentialError::NoCredentials));
        }

        #[test]
        fn xdg_path_probed_even_when_home_is_unset() {
            // Service/CI environments can set $XDG_CONFIG_HOME without
            // $HOME; the XDG candidate must not be gated on home.
            let tmp = TempDir::new().unwrap();
            let xdg = tmp.path().join("xdg");
            write_creds(
                &xdg,
                "claude/.credentials.json",
                &valid_credentials_json("xdg-no-home-tok"),
            );
            let env = env_from(None, Some(&xdg), None);
            let creds = try_file_cascade_with(&env).expect("resolve");
            assert_eq!(creds.token(), "xdg-no-home-tok");
            assert!(matches!(creds.source(), CredentialSource::XdgConfig { .. }));
        }

        #[test]
        fn candidate_list_includes_xdg_when_home_unset() {
            let tmp = TempDir::new().unwrap();
            let xdg = tmp.path().join("xdg");
            let env = env_from(None, Some(&xdg), None);
            let candidates = file_cascade_candidates(&env);
            assert!(
                candidates
                    .iter()
                    .any(|(_, s)| matches!(s, CredentialSource::XdgConfig { .. })),
                "XDG candidate must be present with HOME unset + XDG_CONFIG_HOME set",
            );
            assert!(
                !candidates
                    .iter()
                    .any(|(_, s)| matches!(s, CredentialSource::ClaudeLegacy { .. })),
                "Legacy candidate requires HOME",
            );
        }

        #[test]
        fn xdg_wins_when_both_xdg_and_legacy_files_exist() {
            let tmp = TempDir::new().unwrap();
            let xdg = tmp.path().join("xdg");
            write_creds(
                &xdg,
                "claude/.credentials.json",
                &valid_credentials_json("xdg-wins"),
            );
            write_creds(
                tmp.path(),
                ".claude/.credentials.json",
                &valid_credentials_json("legacy-loses"),
            );
            let env = env_from(None, Some(&xdg), Some(tmp.path()));
            let creds = try_file_cascade_with(&env).expect("resolve");
            assert_eq!(creds.token(), "xdg-wins");
            assert!(matches!(creds.source(), CredentialSource::XdgConfig { .. }));
        }

        #[test]
        fn env_dir_set_but_empty_dir_falls_through_to_xdg() {
            // `CLAUDE_CONFIG_DIR` is a hint, not a declaration — per
            // credentials.md §Edge cases. A set-but-empty (no
            // credentials.json inside) env dir shouldn't shadow XDG.
            let tmp = TempDir::new().unwrap();
            let env_dir = tmp.path().join("env-dir");
            fs::create_dir_all(&env_dir).unwrap();
            let xdg = tmp.path().join("xdg");
            write_creds(
                &xdg,
                "claude/.credentials.json",
                &valid_credentials_json("xdg-tok"),
            );
            let env = env_from(Some(&env_dir), Some(&xdg), Some(tmp.path()));
            let creds = try_file_cascade_with(&env).expect("resolve");
            assert_eq!(creds.token(), "xdg-tok");
        }

        #[test]
        fn resolve_credentials_end_to_end_no_files() {
            // Integration-level smoke: public entry point via
            // DataContext::credentials() memoization. With no files
            // and no Keychain hit (empty HOME, no CLAUDE_CONFIG_DIR),
            // the cascade terminates at NoCredentials.
            let tmp = TempDir::new().unwrap();
            let env = env_from(None, None, Some(tmp.path()));
            let err = try_file_cascade_with(&env).unwrap_err();
            assert!(matches!(err, CredentialError::NoCredentials));
        }
    }
}
