//! Pure I/O / env-snapshotting helpers used by `from_process` to
//! build a `DoctorEnv` once at the call boundary. Every check in
//! the orchestrator (`mod.rs`) reads from the resulting snapshot
//! and never touches the filesystem / network itself, which keeps
//! the checks pure and the tests hermetic.
//!
//! Type definitions stay in `mod.rs` so the orchestrator's check
//! functions and the test fixtures share one source of truth; this
//! module only owns the production-side code that builds those
//! types from the live process state.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::{
    CacheRootState, ClaudeDirState, ClaudeHomeState, ClaudeJsonState, ClaudeSessionsState,
    ConfigReadOutcome, CredentialErrorSummary, CredentialsSummary, DoctorCacheSnapshot,
    DoctorConfigSnapshot, DoctorCredentialsSnapshot, DoctorEndpointSnapshot, DoctorUpdateProbe,
    EndpointProbe, EndpointProbeOutcome, EnvVarState, LockState, PluginDirState, PluginDirStatus,
    UsageJsonState,
};
use crate::config::{Config, ConfigPath};

/// Resolve `$XDG_CONFIG_HOME/linesmith/<sub>` falling back to
/// `$HOME/.config/linesmith/<sub>`. `None` when neither source has a
/// usable value. Mirrors the runtime helpers in `driver.rs`
/// (`user_themes_dir`, `xdg_segments_dir`) so the doctor's view of
/// the plugin / theme dirs lines up with what the runtime actually
/// loads from.
pub(super) fn xdg_subdir(xdg: &EnvVarState, home: &EnvVarState, sub: &str) -> Option<PathBuf> {
    if let Some(x) = xdg.nonempty() {
        return Some(PathBuf::from(x).join("linesmith").join(sub));
    }
    home.nonempty()
        .map(|h| PathBuf::from(h).join(".config/linesmith").join(sub))
}

/// Run `PluginRegistry::load_with_xdg` once and produce both
/// outputs the doctor needs: the union of built-in + plugin
/// segment ids (for `config.segments_resolvable`) and a snapshot
/// of load errors / compiled count (for the Plugins category).
///
/// Delegating to the runtime predicate is the parity rule from
/// the parity rule — `linesmith doctor` must not run a cheaper
/// validation than the runtime it diagnoses, or PASS verdicts
/// silently drift away from runtime reality.
pub(super) fn snapshot_plugins(
    read: &ConfigReadOutcome,
    xdg_segments_dir: Option<&Path>,
) -> (BTreeSet<String>, super::DoctorPluginsSnapshot) {
    use super::{DoctorPluginsSnapshot, PluginsRegistrySummary};
    let mut ids = DoctorConfigSnapshot::built_in_segment_ids();
    let config_dirs: &[PathBuf] = match read {
        ConfigReadOutcome::Loaded { config, .. } => &config.plugin_dirs,
        _ => &[],
    };

    // Cold-start fast path: skip engine init if no plugin source
    // exists on disk. Mirrors the runtime's `load_plugins` shape
    // in `driver.rs`.
    let xdg_present = xdg_segments_dir.is_some_and(Path::is_dir);
    if config_dirs.is_empty() && !xdg_present {
        return (ids, DoctorPluginsSnapshot::NoSources);
    }

    let engine = crate::plugins::build_engine();
    let registry = crate::plugins::PluginRegistry::load_with_xdg(
        config_dirs,
        xdg_segments_dir,
        &engine,
        crate::segments::BUILT_IN_SEGMENT_IDS,
    );
    let mut compiled_count = 0usize;
    for plugin in registry.iter() {
        ids.insert(plugin.id().to_string());
        compiled_count += 1;
    }
    (
        ids,
        DoctorPluginsSnapshot::Discovered(PluginsRegistrySummary {
            compiled_count,
            errors: registry.load_errors().to_vec(),
        }),
    )
}

/// Snapshot `gix::discover(cwd)` outcome via the runtime's
/// `data_context::git::resolve_repo` helper. The runtime's `git_*`
/// segments consume the same predicate, so any state the doctor
/// reports is a state a `git_branch` segment would render the same
/// way at the next `linesmith` invocation.
pub(super) fn snapshot_git() -> super::DoctorGitSnapshot {
    use super::{DoctorGitSnapshot, GitContextSummary};
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            // Surface the actual `io::Error` so the user can
            // distinguish ENOENT (cwd was deleted) from EACCES
            // (parent unreadable) without a follow-up debugging
            // round.
            return DoctorGitSnapshot::Failed {
                message: format!("cannot read current directory: {e}"),
            };
        }
    };
    match crate::data_context::git::resolve_repo(&cwd) {
        Ok(None) => DoctorGitSnapshot::NotInRepo,
        Ok(Some(ctx)) => DoctorGitSnapshot::Repo(GitContextSummary {
            repo_path: ctx.repo_path,
            repo_kind: ctx.repo_kind,
            head: ctx.head,
        }),
        Err(e) => DoctorGitSnapshot::Failed {
            message: e.to_string(),
        },
    }
}

/// Snapshot every theme name known to the runtime: built-ins plus
/// every `*.toml` loaded from the user themes directory. Theme
/// loading is best-effort — parse errors are silently dropped at
/// snapshot time, matching the runtime which warns and skips bad
/// theme files but doesn't fail to start.
pub(super) fn collect_known_theme_names(xdg_themes_dir: Option<&Path>) -> BTreeSet<String> {
    let mut names = DoctorConfigSnapshot::built_in_theme_names();
    let mut registry = crate::theme::ThemeRegistry::with_built_ins();
    if let Some(dir) = xdg_themes_dir {
        registry = registry.with_user_themes(dir, |_| {});
    }
    for registered in registry.iter() {
        names.insert(registered.theme.name().to_string());
    }
    names
}

/// Read + parse the file at `cp.path`. Lives outside `Config::load`
/// because doctor needs the four-way distinction (NotFound / IoError /
/// ParseError / Loaded), which `Config::load` collapses into
/// `Result<Option<…>>`.
pub(super) fn read_config_at(cp: &ConfigPath) -> ConfigReadOutcome {
    let raw = match std::fs::read_to_string(&cp.path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ConfigReadOutcome::NotFound {
                path: cp.path.clone(),
                explicit: cp.explicit,
            };
        }
        Err(e) => {
            return ConfigReadOutcome::IoError {
                path: cp.path.clone(),
                message: e.to_string(),
            };
        }
    };
    match toml::from_str::<Config>(&raw) {
        Ok(config) => ConfigReadOutcome::Loaded {
            path: cp.path.clone(),
            config: Box::new(config),
        },
        Err(e) => ConfigReadOutcome::ParseError {
            path: cp.path.clone(),
            message: e.to_string(),
        },
    }
}

/// `stat` each configured plugin directory. WARN-vs-FAIL severity
/// lives in the [`PluginDirState`] variant chosen here so the check
/// renders without re-running I/O.
pub(super) fn stat_plugin_dirs(paths: &[PathBuf]) -> Vec<PluginDirStatus> {
    paths
        .iter()
        .map(|path| {
            let state = match std::fs::metadata(path) {
                Ok(meta) if meta.is_dir() => PluginDirState::Ok,
                Ok(_) => PluginDirState::NotADirectory,
                Err(e) => match e.kind() {
                    std::io::ErrorKind::NotFound => PluginDirState::Missing,
                    std::io::ErrorKind::PermissionDenied => PluginDirState::PermissionDenied {
                        message: e.to_string(),
                    },
                    _ => PluginDirState::OtherIo {
                        message: e.to_string(),
                    },
                },
            };
            PluginDirStatus {
                path: path.clone(),
                state,
            }
        })
        .collect()
}

/// Walk `$PATH` for the Claude Code launcher and return the first
/// match. Existence + executable-bit (Unix) is the runnable
/// predicate — keeps the category under the spec's `<10ms` budget
/// for Claude Code without invoking the binary itself.
///
/// On Windows the launcher commonly ships as a npm-style shim
/// (`claude.cmd`) rather than a native `claude.exe`, so probe both;
/// pick the first match in PATHEXT order so a user with both
/// installed gets the same one their shell would invoke.
pub(super) fn find_claude_binary(path_env: Option<&str>) -> Option<PathBuf> {
    let path = path_env?;
    let candidates: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd", "claude.bat"]
    } else {
        &["claude"]
    };
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for exe_name in candidates {
            let candidate = dir.join(exe_name);
            if is_runnable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Whether `path` is a regular file the user can execute. On Unix,
/// requires the file bit AND any of the `0o111` execute bits —
/// `which` uses the same predicate. Without the bit check, doctor
/// would PASS on a stale 0644 placeholder file in `$PATH` that
/// fails the moment the user actually runs it. Windows has no
/// executable bit; existence + PATHEXT filename is the standard
/// predicate.
#[cfg(unix)]
fn is_runnable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_runnable(path: &Path) -> bool {
    path.is_file()
}

/// Snapshot the filesystem state under `$HOME` that the Claude Code
/// category inspects. All three calls are best-effort stat / read —
/// failures map to the appropriate state variant rather than
/// propagating up.
pub(super) fn snapshot_claude_home(home: &Path) -> ClaudeHomeState {
    let claude_dir = home.join(".claude");
    let dir = stat_claude_dir(&claude_dir);
    let claude_json = read_claude_json(&home.join(".claude.json"));
    let sessions = stat_claude_sessions(&claude_dir.join("sessions"));
    ClaudeHomeState {
        dir,
        claude_json,
        sessions,
    }
}

fn stat_claude_dir(path: &Path) -> ClaudeDirState {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => ClaudeDirState::Ok,
        Ok(_) => ClaudeDirState::NotADirectory,
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound => ClaudeDirState::Missing,
            std::io::ErrorKind::PermissionDenied => ClaudeDirState::PermissionDenied {
                message: e.to_string(),
            },
            _ => ClaudeDirState::OtherIo {
                message: e.to_string(),
            },
        },
    }
}

/// Per `docs/specs/doctor.md` §Edge cases: cap the read at 2 MB and
/// FAIL fast on oversized files. A real `~/.claude.json` is well
/// under 1 MB; a 500 MB file is pathological / corrupt and parsing
/// it would burn memory + time during a diagnostic command.
const CLAUDE_JSON_MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Read + parse `~/.claude.json`. Distinguishes missing / I/O error
/// / too-large / parse error / parsed-without-oauth / parsed-with-
/// oauth so the check renders distinct hints per case.
pub(super) fn read_claude_json(path: &Path) -> ClaudeJsonState {
    // Stat first so we can short-circuit on oversized files before
    // attempting to read them into memory.
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > CLAUDE_JSON_MAX_BYTES => {
            return ClaudeJsonState::TooLarge {
                actual_bytes: meta.len(),
            };
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ClaudeJsonState::Missing,
        Err(e) => {
            return ClaudeJsonState::IoError {
                message: e.to_string(),
            };
        }
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ClaudeJsonState::Missing,
        Err(e) => {
            return ClaudeJsonState::IoError {
                message: e.to_string(),
            };
        }
    };
    match serde_json::from_str::<serde_json::Value>(&raw) {
        Ok(value) => {
            if value.get("oauthAccount").is_some() {
                ClaudeJsonState::Ok
            } else {
                ClaudeJsonState::NoOauthAccount
            }
        }
        Err(e) => ClaudeJsonState::ParseError {
            message: e.to_string(),
        },
    }
}

pub(super) fn stat_claude_sessions(path: &Path) -> ClaudeSessionsState {
    let entries = match std::fs::read_dir(path) {
        Ok(it) => it,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ClaudeSessionsState::Missing,
        Err(e) => {
            // `read_dir` returns NotADirectory on most platforms when
            // the path is a file; the variant isn't stable in stable
            // Rust yet, so probe via metadata to distinguish the
            // case from a generic I/O error.
            if std::fs::metadata(path).is_ok_and(|m| !m.is_dir()) {
                return ClaudeSessionsState::NotADirectory;
            }
            return ClaudeSessionsState::IoError {
                message: e.to_string(),
            };
        }
    };
    let mut count = 0usize;
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                return ClaudeSessionsState::IoError {
                    message: e.to_string(),
                };
            }
        };
        // Spec §Claude Code "Recent sessions recorded": "At least
        // one file in `~/.claude/sessions/`". Counting subdirs or
        // dotfiles (`.DS_Store`, `.tmp/`, etc.) toward the total
        // would PASS a sessions dir whose only contents are OS /
        // editor junk — false positives the user can't see.
        let is_session_file = entry.file_type().is_ok_and(|t| t.is_file())
            && !entry.file_name().to_string_lossy().starts_with('.');
        if is_session_file {
            count += 1;
        }
    }
    if count == 0 {
        ClaudeSessionsState::Empty
    } else {
        ClaudeSessionsState::HasFiles { count }
    }
}

/// Snapshot the credentials cascade. Maps the rich `CredentialError`
/// to the lossy `CredentialErrorSummary` that the doctor snapshot
/// can carry without risking token leakage through
/// `serde_json::Error::Display`.
///
/// Returns `Unresolvable` (not `Failed`) when the cascade has no
/// usable source at all: no `$CLAUDE_CONFIG_DIR`, no
/// `$XDG_CONFIG_HOME`, no `$HOME`, AND we're not on macOS (where
/// the keychain is a path-independent source). CI/service
/// environments that omit `$HOME` but set one of the override env
/// vars still hit the file cascade and resolve normally — only the
/// genuinely-no-source case maps to SKIP per spec §Cross-category
/// short-circuits.
pub(super) fn snapshot_credentials(
    home_env: &EnvVarState,
    xdg_config_home: &EnvVarState,
    claude_config_dir: &EnvVarState,
) -> DoctorCredentialsSnapshot {
    let has_path_source = home_env.nonempty().is_some()
        || xdg_config_home.nonempty().is_some()
        || claude_config_dir.nonempty().is_some();
    if !has_path_source && !cfg!(target_os = "macos") {
        return DoctorCredentialsSnapshot::Unresolvable;
    }
    use crate::data_context::credentials::{resolve_credentials, CredentialError};
    match resolve_credentials() {
        Ok(creds) => DoctorCredentialsSnapshot::Resolved(CredentialsSummary {
            source: creds.source().clone(),
            scopes: creds.scopes().to_vec(),
        }),
        Err(err) => DoctorCredentialsSnapshot::Failed(match err {
            CredentialError::NoCredentials => CredentialErrorSummary::NoCredentials,
            CredentialError::SubprocessFailed(e) => CredentialErrorSummary::SubprocessFailed {
                message: e.to_string(),
            },
            CredentialError::IoError { path, cause } => CredentialErrorSummary::IoError {
                path,
                message: cause.to_string(),
            },
            // Per `CredentialError`'s safety contract, the
            // `serde_json::Error` Display can include token bytes
            // — drop the message and surface only the variant tag
            // and the path.
            CredentialError::ParseError { path, .. } => CredentialErrorSummary::ParseError { path },
            CredentialError::MissingField { path } => CredentialErrorSummary::MissingField { path },
            CredentialError::EmptyToken { path } => CredentialErrorSummary::EmptyToken { path },
        }),
    }
}

/// Snapshot the cache state — root dir, `usage.json` schema, and
/// `usage.lock` blocked-until timestamp. Reads files but never
/// writes (doctor is read-only by spec).
pub(super) fn snapshot_cache(
    xdg_cache_home: &EnvVarState,
    home_env: &EnvVarState,
) -> DoctorCacheSnapshot {
    let root_path = derive_cache_root(xdg_cache_home, home_env);
    let Some(root) = root_path.clone() else {
        return DoctorCacheSnapshot {
            root_path: None,
            root: CacheRootState::Unresolved,
            usage_json: UsageJsonState::Missing,
            lock: LockState::Absent,
        };
    };
    let root_state = stat_cache_root(&root);
    let usage_json = stat_usage_json(&root.join("usage.json"));
    let lock = stat_usage_lock(&root);
    DoctorCacheSnapshot {
        root_path: Some(root),
        root: root_state,
        usage_json,
        lock,
    }
}

/// Mirror of `cache::default_root` taking env state from a snapshot
/// rather than reading process env. Keeps the snapshot pure.
fn derive_cache_root(xdg_cache_home: &EnvVarState, home_env: &EnvVarState) -> Option<PathBuf> {
    if let Some(xdg) = xdg_cache_home.nonempty() {
        return Some(PathBuf::from(xdg).join("linesmith"));
    }
    home_env
        .nonempty()
        .map(|h| PathBuf::from(h).join(".cache").join("linesmith"))
}

/// Read-only stat of the cache root. Doctor must not perform
/// filesystem writes — an earlier probe-write to the first existing
/// ancestor violated the read-only contract on fresh-setup runs
/// (`XDG_CACHE_HOME=/tmp/new-root`).
///
/// Also routes `NotADirectory` errors (e.g. the leaf of
/// `/tmp/file/sub/cache` where `/tmp/file` is a regular file)
/// through the parent-walk path: `metadata()` doesn't return
/// `NotFound` for those, so a naive "only NotFound walks" rule
/// would land them in `Unreadable` with a confusing message
/// instead of pointing at the real cause (a file blocking the
/// chain) via `AbsentParentReadOnly`.
///
/// On `NotFound`, walks up the parent chain to the first existing
/// ancestor and inspects its `permissions().readonly()` bit. A
/// readonly ancestor (e.g. `XDG_CACHE_HOME` under `/proc`, or under
/// a path whose user-write bit is cleared) means `create_dir_all`
/// will fail on every runtime fetch; flag it as
/// `AbsentParentReadOnly` so the WARN steers the user toward the
/// real fix instead of silently passing forever.
///
/// The check is intentionally weaker than a probe-write because
/// `permissions().readonly()` only inspects mode bits. Known false
/// negatives (paths the bit reports writable but the process can
/// not actually write):
/// - cross-user ownership: a directory owned by another user with
///   mode 0755 reads as not-readonly even though the doctor process
///   can't create entries in it (the dominant case)
/// - POSIX ACLs that deny write to the running user
/// - the BSD/macOS user-immutable flag (`chflags uchg`)
/// - NFS root-squash mapping the user to `nobody`
///
/// The unstatable-ancestor branch in `classify_absent_cache_root`
/// catches the cross-user case for unstatable intermediate
/// directories; everything else still false-PASSes here, surfacing
/// only as a runtime error on first fetch.
pub(super) fn stat_cache_root(path: &Path) -> CacheRootState {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_dir() => CacheRootState::Exists,
        Ok(_) => CacheRootState::NotADirectory,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => classify_absent_cache_root(path),
        // ENOTDIR from a path like `/tmp/file/sub/cache` where
        // `/tmp/file` is a regular file: walk the parent chain so
        // we land at the file ancestor as `AbsentParentReadOnly`
        // instead of leaving the user with a generic "Not a
        // directory" Unreadable message.
        Err(e) if is_not_a_directory(&e) => classify_absent_cache_root(path),
        Err(e) => CacheRootState::Unreadable {
            message: e.to_string(),
        },
    }
}

/// `io::ErrorKind::NotADirectory` is unstable in older Rust
/// versions; substring-match the OS error's display. Linux + macOS
/// surface this as "Not a directory" + ENOTDIR (errno 20).
fn is_not_a_directory(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(20)
}

/// Walk parent chain to classify a missing cache root. Branches:
/// - first stat-able ancestor is a writable directory AND no
///   ancestor along the way returned `PermissionDenied` → `Absent`
///   (PASS — runtime can `create_dir_all` it on first fetch)
/// - first stat-able ancestor is a directory marked `readonly()` OR
///   any earlier ancestor returned `PermissionDenied` →
///   `AbsentParentReadOnly` with that ancestor as the WARN target.
///   The PermissionDenied bias is load-bearing: a stat error on
///   `/root/cache` followed by a writable `/root` doesn't mean the
///   runtime can create `/root/cache` — `create_dir_all` needs
///   traverse on every path component, and an unstatable middle
///   directory blocks that. Without this bias an unprivileged user
///   with `XDG_CACHE_HOME=/root/cache` would PASS forever.
/// - first stat-able ancestor is not a directory (file in the
///   middle of the chain) → `AbsentParentReadOnly` with that path
///   as the offending ancestor.
///
/// Other walk-time errors (NotFound on intermediate components,
/// transient I/O) are NOT bias triggers — they're the common case
/// of "this whole subtree doesn't exist yet" and the runtime
/// `create_dir_all` will create through them fine.
///
/// Read-only stat only — no probe-write.
fn classify_absent_cache_root(path: &Path) -> CacheRootState {
    let mut existing = path.parent();
    let mut blocked_by_perm: Option<PathBuf> = None;
    while let Some(dir) = existing {
        match std::fs::metadata(dir) {
            Ok(meta) if meta.is_dir() => {
                let parent = if let Some(blocked) = blocked_by_perm {
                    blocked
                } else if meta.permissions().readonly() {
                    dir.to_path_buf()
                } else {
                    return CacheRootState::Absent;
                };
                return CacheRootState::AbsentParentReadOnly { parent };
            }
            // Ancestor exists but isn't a directory (e.g. a file
            // earlier in the chain) — caller can't create the cache
            // through it; same WARN class.
            Ok(_) => {
                return CacheRootState::AbsentParentReadOnly {
                    parent: dir.to_path_buf(),
                };
            }
            // PermissionDenied: capture the path so we'll WARN
            // even if a higher writable ancestor exists. The
            // runtime needs traverse permission on every component;
            // a writable `/root` doesn't help if `/root/cache`
            // can't be stat'd.
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                if blocked_by_perm.is_none() {
                    blocked_by_perm = Some(dir.to_path_buf());
                }
                existing = dir.parent();
            }
            // Other walk errors (NotFound on intermediate
            // components is the common case) — keep walking but
            // don't bias toward WARN. The runtime's
            // `create_dir_all` creates through missing
            // intermediates fine.
            Err(_) => existing = dir.parent(),
        }
    }
    // No ancestor was stat-able (path is the filesystem root, or
    // every component up to the root failed). `derive_cache_root`
    // never produces such a path; this exists for direct unit-test
    // callers of `stat_cache_root` and as a defensive fallback if
    // `derive_cache_root` ever changes shape.
    CacheRootState::AbsentParentReadOnly {
        parent: blocked_by_perm.unwrap_or_else(|| PathBuf::from("/")),
    }
}

pub(super) fn stat_usage_json(path: &Path) -> UsageJsonState {
    use crate::data_context::cache::{CachedUsage, CACHE_SCHEMA_VERSION};
    // Intentional divergence from `CacheStore::read` (which uses
    // `fs::read` + `from_utf8` and silently collapses non-UTF-8 to
    // a cache miss): doctor's job is to surface corruption so the
    // user can fix it, not to silently treat it as transient. A
    // non-UTF-8 file lands in `Unreadable` here so the next render
    // points at the actual problem. The runtime will *attempt* to
    // overwrite on next fetch via `atomic_write_json`; if that also
    // fails (read-only mount, EACCES on the file, ENOSPC, EDQUOT),
    // it logs `lsm_error!` to stderr — visible to the user only when
    // stderr is open.
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return UsageJsonState::Missing,
        Err(e) => {
            return UsageJsonState::Unreadable {
                message: e.to_string(),
            };
        }
    };
    // Schema-version + `cached_at <= now` gates match the runtime
    // cache reader's predicate exactly (cache.rs::CacheStore::read).
    // A peek-only check that ignores `cached_at` would PASS files
    // the runtime treats as a miss (clock skew) and leave the user
    // puzzled when their data looks stale.
    //
    // Read the schema_version separately first so we can render
    // `Stale { schema_version }` rather than collapsing every
    // version mismatch into `Unreadable` (the runtime's miss path).
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return UsageJsonState::Unreadable {
            message: "usage.json is not valid JSON".to_string(),
        };
    };
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .map(|n| n as u32);
    if !matches!(schema_version, Some(v) if v == CACHE_SCHEMA_VERSION) {
        return match schema_version {
            Some(v) => UsageJsonState::Stale { schema_version: v },
            None => UsageJsonState::Unreadable {
                message: "usage.json missing `schema_version`".to_string(),
            },
        };
    }
    match serde_json::from_value::<CachedUsage>(value) {
        Ok(entry) if entry.cached_at <= chrono::Utc::now() => UsageJsonState::Current {
            schema_version: entry.schema_version,
        },
        Ok(_) => UsageJsonState::FutureTimestamp,
        Err(e) => UsageJsonState::Unreadable {
            message: format!("usage.json shape mismatch: {e}"),
        },
    }
}

pub(super) fn stat_usage_lock(cache_root: &Path) -> LockState {
    // Delegate to the runtime's `LockStore::read`: matches the
    // legacy non-JSON mtime fallback, the `blocked_until` cap,
    // and the absence detection exactly. Without this, doctor's
    // hand-rolled JSON peek would WARN on legacy lock files the
    // runtime treats as Active, and PASS pathological far-future
    // values the runtime caps. Doctor-runtime parity rule.
    let store = crate::data_context::cache::LockStore::new(cache_root.to_path_buf());
    match store.read() {
        Ok(None) => LockState::Absent,
        Ok(Some(lock)) => {
            let now = chrono::Utc::now().timestamp();
            if lock.blocked_until > now {
                LockState::Active {
                    blocked_until_secs: lock.blocked_until,
                }
            } else {
                LockState::Stale {
                    blocked_until_secs: lock.blocked_until,
                }
            }
        }
        Err(e) => LockState::Unreadable {
            message: e.to_string(),
        },
    }
}

/// Doctor-owned WARN threshold for the spec's 2s endpoint budget.
/// Kept separate from `fetcher::DEFAULT_TIMEOUT` so a future timeout
/// bump for transport resilience doesn't silently move the
/// user-facing slow signal.
pub(super) const DOCTOR_SLOW_THRESHOLD: std::time::Duration = std::time::Duration::from_secs(2);

/// Probe the rate-limit endpoint with the resolved credentials.
/// Request timeout uses `fetcher::DEFAULT_TIMEOUT` (parity with the
/// runtime); see `DOCTOR_SLOW_THRESHOLD` for the Slow-vs-Ok cutoff.
/// Any non-`Resolved` credentials short-circuit before this is
/// reached.
pub(super) fn probe_endpoint_via_ureq() -> DoctorEndpointSnapshot {
    use crate::data_context::credentials::resolve_credentials;
    use crate::data_context::fetcher::{UreqTransport, UsageTransport, DEFAULT_TIMEOUT};
    use std::time::Instant;

    // Re-resolve here because `CredentialsSummary` deliberately
    // drops the token bytes before the snapshot crosses the
    // module boundary.
    let Ok(creds) = resolve_credentials() else {
        // Caller already checked the snapshot's credentials variant
        // before invoking us; landing here means the second cascade
        // call diverged from the first (race: keychain locked, file
        // removed, login expired). Flag it so the orchestrator can
        // render the right SKIP reason.
        return DoctorEndpointSnapshot {
            probe: None,
            credentials_vanished: true,
        };
    };
    let transport = UreqTransport::new();
    let start = Instant::now();
    let url = format!(
        "{}{}",
        crate::data_context::cascade::DEFAULT_API_BASE_URL,
        crate::data_context::fetcher::OAUTH_USAGE_PATH,
    );
    let result = transport.get(&url, creds.token(), DEFAULT_TIMEOUT);
    let elapsed = start.elapsed();
    let outcome = classify_endpoint_response(result, elapsed);
    DoctorEndpointSnapshot {
        probe: Some(EndpointProbe {
            elapsed_ms: elapsed.as_millis(),
            outcome,
        }),
        credentials_vanished: false,
    }
}

/// Pure classifier for the endpoint probe's HTTP outcome —
/// separated from the probe so tests can feed synthetic
/// `HttpResponse` values without exercising the network.
pub(super) fn classify_endpoint_response(
    result: std::io::Result<crate::data_context::fetcher::HttpResponse>,
    elapsed: std::time::Duration,
) -> EndpointProbeOutcome {
    use crate::data_context::usage::UsageApiResponse;
    let resp = match result {
        Ok(r) => r,
        // Per spec §Rate-limit endpoint: transport-level errors
        // (DNS / connect / read timeout / proxy refusal / captive
        // portal) are WARN, not FAIL. A user behind an air-gapped
        // CI shouldn't gate exit-1 on network reachability.
        Err(_) => return EndpointProbeOutcome::TransportError,
    };
    match resp.status {
        200..=299 => match serde_json::from_slice::<serde_json::Value>(&resp.body) {
            Ok(value) => {
                // Body is structurally JSON. If it doesn't fit the
                // strict UsageApiResponse shape, that IS a real API
                // contract failure (Anthropic changed the wire
                // format) — FAIL.
                if serde_json::from_value::<UsageApiResponse>(value.clone()).is_err() {
                    return EndpointProbeOutcome::ParseError;
                }
                let extra_keys = collect_unexpected_endpoint_keys(&value);
                if !extra_keys.is_empty() {
                    return EndpointProbeOutcome::UnexpectedShape { extra_keys };
                }
                // Slow threshold is doctor-owned; it tracks the
                // spec's user-facing 2s budget, not the runtime's
                // request-timeout constant.
                if elapsed >= DOCTOR_SLOW_THRESHOLD {
                    EndpointProbeOutcome::Slow
                } else {
                    EndpointProbeOutcome::Ok
                }
            }
            // Body isn't JSON at all — almost certainly a captive
            // portal / proxy splash / cache poisoning that returns
            // `200 text/html`. Treat as a network problem (WARN),
            // not as a hard API-contract failure (FAIL). Spec
            // §Rate-limit endpoint explicitly puts captive portals
            // in the transport-level WARN bucket; routing them to
            // ParseError would force CI exit-1 on perfectly
            // healthy setups behind a corporate proxy.
            Err(_) => EndpointProbeOutcome::TransportError,
        },
        429 => {
            let retry_after_secs = resp
                .retry_after
                .as_deref()
                .and_then(|s| s.trim().parse::<u64>().ok());
            EndpointProbeOutcome::RateLimited { retry_after_secs }
        }
        // Spec §Rate-limit endpoint: only "4xx other than 429" is a definitive
        // bad answer / FAIL. 5xx is the upstream broken (Anthropic
        // outage), 3xx leaks here only when ureq's redirect handling
        // is exhausted — both have the same user-actionability as a
        // transport error and must NOT gate exit-1 (CI runs of
        // `linesmith doctor` during an Anthropic incident shouldn't
        // exit nonzero through no fault of the user).
        400..=499 => EndpointProbeOutcome::BadStatus {
            status: resp.status,
        },
        _ => EndpointProbeOutcome::TransportError,
    }
}

/// The `UsageApiResponse` shape uses `#[serde(flatten)]` to capture
/// codenamed buckets in `unknown_buckets` — so a value that
/// deserializes successfully has zero "extra keys" by definition.
/// To honor the spec's WARN row for "Extra unknown fields present
/// (forward-compat)", peek at the top-level JSON object and report
/// keys outside the known bucket set. Production decoding still
/// flows through the strict `UsageApiResponse` deserializer.
fn collect_unexpected_endpoint_keys(value: &serde_json::Value) -> Vec<String> {
    use crate::data_context::usage::{KNOWN_BUCKETS, RESEARCH_DOCUMENTED_BUCKETS};
    let Some(obj) = value.as_object() else {
        return Vec::new();
    };
    obj.keys()
        .filter(|k| {
            !KNOWN_BUCKETS.contains(&k.as_str())
                && !RESEARCH_DOCUMENTED_BUCKETS.contains(&k.as_str())
        })
        .cloned()
        .collect()
}

/// GitHub releases endpoint for the canonical linesmith repo. Pinned
/// here (rather than read from a config knob) because it's part of
/// the doctor's contract: every install of `linesmith` checks the
/// same upstream coordinate.
const GITHUB_RELEASES_LATEST_URL: &str =
    "https://api.github.com/repos/oakoss/linesmith/releases/latest";

/// Spec §Timing: ~2s timeout for the network probe so doctor's
/// total budget stays within the published worst case.
const UPDATE_PROBE_TIMEOUT_SECS: u64 = 2;

/// Cap on the GitHub response body. The `/releases/latest` payload
/// bundles full release notes + every asset's metadata, so a current
/// response runs ~41 KiB (measured 2026-04-30 via `gh api
/// repos/oakoss/linesmith/releases/latest | wc -c`). 256 KiB keeps
/// the MITM/OOM protection rationale from `MAX_RESPONSE_BYTES` and
/// leaves ~6× growth headroom; an earlier 32 KiB cap truncated the
/// live response mid-JSON and surfaced as a spurious ParseError
/// WARN. Truncation is now distinguished from short bodies (see the
/// `take(N + 1)` + `body.len() > N` check in `snapshot_update_probe`)
/// so a future cap-tuner gets a clear "bump UPDATE_PROBE_MAX_BYTES"
/// signal instead of a misleading parse error.
const UPDATE_PROBE_MAX_BYTES: u64 = 256 * 1024;

/// Fire one GET against the GitHub releases API and classify the
/// outcome per spec §Self. Always-run (no skip path); transport
/// failures land as `TransportError`, GitHub-shape changes as
/// `ParseError`. Direct ureq instead of `UsageTransport` because the
/// latter requires an auth token and unconditionally sets the
/// Anthropic-only `anthropic-beta` header — wiring it to github.com
/// would misroute the Anthropic OAuth token and the beta header to
/// the wrong upstream.
pub(super) fn snapshot_update_probe() -> DoctorUpdateProbe {
    use std::io::Read;
    use std::time::Duration;

    let mut builder = ureq::Agent::config_builder().http_status_as_error(false);
    if let Some(proxy) = ureq::Proxy::try_from_env() {
        builder = builder.proxy(Some(proxy));
    }
    let agent = ureq::Agent::new_with_config(builder.build());
    let user_agent = format!("linesmith/{}", env!("CARGO_PKG_VERSION"));

    let result = agent
        .get(GITHUB_RELEASES_LATEST_URL)
        .config()
        .timeout_global(Some(Duration::from_secs(UPDATE_PROBE_TIMEOUT_SECS)))
        .build()
        .header("User-Agent", &user_agent)
        .header("Accept", "application/vnd.github+json")
        .call();

    let mut response = match result {
        Ok(r) => r,
        Err(e) => {
            // `ureq::Error::Display` carries the URL plus a chained
            // source; clamp here for the same reason the body-read
            // error path does — a captive-portal proxy injecting a
            // multi-line HTML page during connection establishment
            // could otherwise leak unbounded body fragments through
            // this WARN message into terminal output and bug reports.
            return DoctorUpdateProbe::TransportError {
                message: clamp_diag(&e.to_string()),
            };
        }
    };
    let status = response.status().as_u16();
    if !(200..=299).contains(&status) {
        // GitHub 404 (renamed repo, draft-only release window),
        // 5xx (incident), 403 (rate-limited unauthenticated probe)
        // — all WARN per spec §Self.
        return DoctorUpdateProbe::TransportError {
            message: format!("HTTP {status}"),
        };
    }
    // Read one byte past the cap so we can distinguish "complete
    // short body" (`body.len() <= MAX`) from "cap exhausted, body
    // truncated" (`body.len() > MAX`). Without the +1, `take(MAX)`
    // returns `Ok(MAX)` on truncation just as it does on a complete
    // body of exactly `MAX` bytes; the truncated JSON then surfaces
    // as `ParseError`, sending the user chasing a "GitHub API shape
    // changed" red herring instead of "bump the cap".
    let mut body = Vec::new();
    if let Err(e) = response
        .body_mut()
        .as_reader()
        .take(UPDATE_PROBE_MAX_BYTES + 1)
        .read_to_end(&mut body)
    {
        return DoctorUpdateProbe::TransportError {
            message: clamp_diag(&e.to_string()),
        };
    }
    let truncated = body.len() as u64 > UPDATE_PROBE_MAX_BYTES;
    classify_update_response_inner(&body, env!("CARGO_PKG_VERSION"), truncated)
}

/// Trim arbitrary diagnostic strings to a single line + 200 chars.
/// Defensive: `serde_json::Error::to_string()` and `io::Error` sources
/// can echo input fragments — clamping prevents an upstream surprise
/// (a captive-portal proxy injecting an HTML page with a session
/// cookie, say) from leaking through `TransportError` /
/// `ParseError` `message` strings into terminal output and pasted
/// bug reports.
fn clamp_diag(s: &str) -> String {
    let first_line = s.lines().next().unwrap_or("");
    let mut out: String = first_line.chars().take(200).collect();
    if first_line.chars().count() > 200 || s.lines().count() > 1 {
        out.push_str("...");
    }
    out
}

/// Test-only convenience wrapper that pins `truncated = false`.
/// Tests construct synthetic JSON bodies that are always complete by
/// definition, so the truncation flag is uninteresting; production
/// wiring goes through `classify_update_response_inner` so the
/// body-cap signal can reach the classifier.
#[cfg(test)]
pub(super) fn classify_update_response(body: &[u8], current_version: &str) -> DoctorUpdateProbe {
    classify_update_response_inner(body, current_version, false)
}

/// Pure classifier for the GitHub releases response. Comparison is
/// "newer" iff the parsed upstream version-tuple is strictly greater
/// than the local one (`current_version`, normally
/// `CARGO_PKG_VERSION`).
///
/// `truncated` is `true` when the reader hit `UPDATE_PROBE_MAX_BYTES
/// + 1` bytes, meaning the upstream response was larger than the cap
/// and `body` is incomplete. The truncated case short-circuits to a
/// `TransportError` with a "bump the cap" pointer so a future cap
/// regression doesn't masquerade as a `ParseError`.
pub(super) fn classify_update_response_inner(
    body: &[u8],
    current_version: &str,
    truncated: bool,
) -> DoctorUpdateProbe {
    // Pin the `(body, truncated)` coupling: the only legitimate way
    // to set `truncated == true` is for the caller to have read past
    // the cap and observed the overshoot. A future refactor that
    // splits these inputs (e.g. accepts a pre-read body without the
    // overshoot) would silently break the truncation-detection
    // contract; this assert turns that into a loud test failure.
    debug_assert!(
        !truncated || body.len() as u64 > UPDATE_PROBE_MAX_BYTES,
        "truncated=true requires body.len() ({}) > UPDATE_PROBE_MAX_BYTES ({})",
        body.len(),
        UPDATE_PROBE_MAX_BYTES,
    );
    if truncated {
        return DoctorUpdateProbe::TransportError {
            message: format!(
                "response body exceeded {UPDATE_PROBE_MAX_BYTES} bytes; bump UPDATE_PROBE_MAX_BYTES"
            ),
        };
    }
    let value: serde_json::Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(e) => {
            return DoctorUpdateProbe::ParseError {
                message: clamp_diag(&e.to_string()),
            }
        }
    };
    let Some(tag_name) = value.get("tag_name").and_then(|v| v.as_str()) else {
        return DoctorUpdateProbe::ParseError {
            message: "response missing `tag_name` field".to_string(),
        };
    };
    let safe_tag = sanitize_tag(tag_name);
    let local = parse_three_part_version(current_version);
    let remote = parse_three_part_version(tag_name);
    match (remote, local) {
        // Both parse: numeric semver compare.
        (Some(r), Some(l)) if r > l => DoctorUpdateProbe::Newer { latest: safe_tag },
        (Some(_), Some(_)) => DoctorUpdateProbe::Latest,
        // Local couldn't parse: treat as ParseError on our own build
        // (CARGO_PKG_VERSION came out malformed somehow). Surface it
        // as a probe ParseError so the WARN points at a real
        // diagnosis instead of silently passing.
        (Some(_), None) => DoctorUpdateProbe::ParseError {
            message: format!("local version {current_version} unparseable as MAJOR.MINOR.PATCH"),
        },
        // Remote parsed-shape disagrees with local — we can't compare
        // semantically. Past versions of this code fell back to string
        // equality here, which silently downgraded to PASS or invented
        // a `Newer` claim from a tag the comparator never validated.
        // Spec WARN with a clear "couldn't compare" diagnostic instead.
        (None, Some(_)) => DoctorUpdateProbe::ParseError {
            message: format!("remote tag {safe_tag} unparseable as MAJOR.MINOR.PATCH"),
        },
        // Neither parsed (e.g. both sides date-based or monorepo-
        // prefixed). String equality is the only meaningful comparator
        // we can offer; the Newer branch surfaces the upstream tag
        // verbatim so the user can decide.
        (None, None) => {
            if tag_name == current_version || tag_name.strip_prefix('v') == Some(current_version) {
                DoctorUpdateProbe::Latest
            } else {
                DoctorUpdateProbe::Newer { latest: safe_tag }
            }
        }
    }
}

/// Strip control bytes and clamp length on a `tag_name` we got from
/// GitHub before it lands in user-facing rendered output. Defensive:
/// the OAuth Bearer header was already removed (`snapshot_update_probe`
/// doesn't authenticate), so a tampered or attacker-controlled
/// response body carrying ANSI escape sequences in `tag_name` could
/// otherwise paint terminal output and bug-report pastes.
fn sanitize_tag(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(64).collect()
}

/// Best-effort `vMAJOR.MINOR.PATCH[-pre][+build]` parser. `None` for
/// any input that doesn't yield three numeric segments. Strips a
/// single leading `v` (GitHub's convention) and truncates the patch
/// segment at the first non-digit so `1.2.3-rc1` parses as `(1,2,3)`.
///
/// Pre-release suffixes are silently dropped: `1.2.3-rc1` and `1.2.3`
/// compare equal as a result. The `Ord` derive on `(u32, u32, u32)`
/// does NOT respect semver pre-release ordering; this is intentional
/// for the `linesmith doctor` use case (we want to know if the user
/// is broadly behind, not whether their `-rc1` predates the published
/// stable). A future need for full semver semantics should add the
/// `semver` crate rather than extending this helper.
fn parse_three_part_version(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    let mut parts = s.splitn(3, '.');
    let major: u32 = parts.next()?.parse().ok()?;
    let minor: u32 = parts.next()?.parse().ok()?;
    let patch_part = parts.next()?;
    let patch_str: String = patch_part
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if patch_str.is_empty() {
        return None;
    }
    let patch: u32 = patch_str.parse().ok()?;
    Some((major, minor, patch))
}
