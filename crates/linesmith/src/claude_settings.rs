//! Detection and install/uninstall of linesmith's `statusLine` block
//! in `~/.claude/settings.json`. Used by:
//!
//! - The `linesmith install` / `linesmith uninstall` CLI subcommands
//!   (non-interactive paths for scripted setup).
//! - The TUI's `InstallToClaudeCode` screen (interactive surface).
//!
//! All writes go through [`crate::atomic::atomic_write`] (temp file,
//! fsync, then rename) and back up any prior `settings.json` to a
//! `.bak` sibling before overwriting. JSON parsing uses
//! `serde_json::Value` so unknown top-level keys round-trip unchanged;
//! only the `statusLine` key is mutated.
//!
//! Forward-compat note: the detection rule for "is linesmith
//! installed?" is `statusLine.command.contains("linesmith")`. A future
//! rename of the binary would break this heuristic; the alternative
//! (matching against `current_exe()`) breaks tests and cross-platform
//! builds in ways that aren't worth the precision.
//!
//! [`crate::atomic::atomic_write`]: crate::atomic

// `unreachable_pub` and `redundant_pub_crate` collide for every item
// here: the module is private (`pub(crate)`), but `pub(crate)` items
// inside it are flagged as redundant. Plain `pub` triggers
// `unreachable_pub`. Allow the redundancy lint module-wide so the
// public surface of this file reads cleanly.
#![allow(clippy::redundant_pub_crate)]

use std::io;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::atomic::atomic_write;
use crate::driver::CliEnv;

/// `$HOME/.claude/settings.json` — Claude Code's default settings
/// location. Returns `None` when `$HOME` is unset or empty (rare but
/// real on minimal containers / sandbox runners — and `HOME=""` would
/// otherwise resolve to `.claude/settings.json` relative to the
/// current working directory, silently installing into the project
/// instead of the home directory).
#[must_use]
pub(crate) fn default_settings_path(env: &CliEnv) -> Option<PathBuf> {
    env.home
        .as_ref()
        .filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".claude").join("settings.json"))
}

/// Where the linesmith statusLine entry stands in the user's Claude
/// Code settings. Drives the install screen's "✓ Installed" vs "✗
/// Not installed" indicator and the install/uninstall verb gating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallationStatus {
    /// `settings.json` doesn't exist at all. Install will create it
    /// (and any missing parent directories).
    NotPresent,
    /// File exists, but no `statusLine` key (or it's null). Install
    /// will add the key; the rest of the JSON round-trips.
    NotInstalled,
    /// `statusLine.command` references linesmith. Carries the
    /// current command string so the UI can show what's wired up
    /// (`linesmith` vs `linesmith --config /path`).
    Installed { command: String },
    /// `statusLine` exists but points to something else (e.g.
    /// `ccstatusline`). Install will back up to `.bak` and overwrite,
    /// but the caller should warn the user first.
    Other { command: String },
}

/// Read `settings.json` and classify the current state. Returns
/// `Ok(NotPresent)` (not an error) when the file is missing so the
/// caller can treat "not yet installed" as a normal state. Other I/O
/// errors propagate. Malformed JSON surfaces as `InvalidData`.
pub(crate) fn detect_installation_status(path: &Path) -> io::Result<InstallationStatus> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(InstallationStatus::NotPresent),
        Err(e) => return Err(e),
    };
    let value: Value = serde_json::from_str(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    match value.get("statusLine") {
        None | Some(Value::Null) => Ok(InstallationStatus::NotInstalled),
        Some(obj) => {
            let command = obj
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if command.contains("linesmith") {
                Ok(InstallationStatus::Installed { command })
            } else {
                Ok(InstallationStatus::Other { command })
            }
        }
    }
}

/// Outcome of [`install`] — exposes whether a prior file existed so
/// the caller can show "wrote new settings.json" vs "updated
/// settings.json (backed up to .bak)".
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InstallOutcome {
    /// File didn't exist; install created it.
    Created,
    /// File existed and was overwritten in place after backup.
    Updated,
}

/// Install the linesmith statusLine. Reads the existing
/// `settings.json` (preserving all non-`statusLine` keys), backs the
/// raw bytes up to `<path>.bak`, sets `statusLine` to the linesmith
/// block, and atomically writes the merged JSON. Creates the parent
/// directory if absent so the first-run flow doesn't need
/// `mkdir -p ~/.claude/`.
///
/// `command` is the value to put in `statusLine.command` — typically
/// `"linesmith"` or `"linesmith --config /path/to/config.toml"`. The
/// caller is responsible for shell-quoting / escaping per the same
/// rules `driver::json_command` uses for the init snippet.
pub(crate) fn install(path: &Path, command: &str) -> io::Result<InstallOutcome> {
    let (mut root, outcome) = match std::fs::read_to_string(path) {
        Ok(raw) => {
            let v: Value = serde_json::from_str(&raw)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
            backup_to_bak(path, &raw)?;
            (v, InstallOutcome::Updated)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => (
            Value::Object(serde_json::Map::new()),
            InstallOutcome::Created,
        ),
        Err(e) => return Err(e),
    };
    let obj = root.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "settings.json root must be a JSON object",
        )
    })?;
    obj.insert(
        "statusLine".to_string(),
        json!({
            "type": "command",
            "command": command,
            "padding": 0,
        }),
    );
    let serialized = serde_json::to_string_pretty(&root)? + "\n";
    atomic_write(path, &serialized)?;
    Ok(outcome)
}

/// Outcome of [`uninstall`] — distinguishes the three observable
/// states so the caller can produce a sensible diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UninstallOutcome {
    /// File didn't exist; nothing to uninstall.
    NoFile,
    /// File exists but no `statusLine` key was present. Idempotent
    /// no-op; no backup written, no file modified.
    NoStatusLine,
    /// Removed the `statusLine` key and wrote the remaining JSON
    /// back. Prior contents backed up to `.bak`.
    Removed,
}

/// Remove the linesmith statusLine from `settings.json`. Idempotent
/// — multiple uninstalls on the same file return `NoStatusLine`
/// after the first.
///
/// **Does not check whether the existing `statusLine` belongs to
/// linesmith.** A user who has ccstatusline installed and runs
/// `linesmith uninstall` will see their ccstatusline removed. The
/// CLI surface guards against this by checking
/// [`detect_installation_status`] first and bailing on
/// [`InstallationStatus::Other`].
pub(crate) fn uninstall(path: &Path) -> io::Result<UninstallOutcome> {
    let raw = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(UninstallOutcome::NoFile),
        Err(e) => return Err(e),
    };
    let mut root: Value = serde_json::from_str(&raw)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    let obj = root.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "settings.json root must be a JSON object",
        )
    })?;
    if obj.remove("statusLine").is_none() {
        return Ok(UninstallOutcome::NoStatusLine);
    }
    backup_to_bak(path, &raw)?;
    let serialized = serde_json::to_string_pretty(&root)? + "\n";
    atomic_write(path, &serialized)?;
    Ok(UninstallOutcome::Removed)
}

/// Write the prior raw contents to `<path>.bak` so a user who
/// regrets the install can restore by hand. Overwrites any earlier
/// backup — the last-good baseline is more useful than archeology.
fn backup_to_bak(path: &Path, contents: &str) -> io::Result<()> {
    let mut bak = path.as_os_str().to_owned();
    bak.push(".bak");
    atomic_write(Path::new(&bak), contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn settings_path(tmp: &TempDir) -> PathBuf {
        tmp.path().join("settings.json")
    }

    #[test]
    fn detect_returns_not_present_when_file_missing() {
        let tmp = TempDir::new().expect("tempdir");
        let status = detect_installation_status(&settings_path(&tmp)).expect("detect");
        assert_eq!(status, InstallationStatus::NotPresent);
    }

    #[test]
    fn detect_returns_not_installed_when_no_status_line_key() {
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        fs::write(&path, r#"{ "theme": "dracula" }"#).expect("seed");
        let status = detect_installation_status(&path).expect("detect");
        assert_eq!(status, InstallationStatus::NotInstalled);
    }

    #[test]
    fn detect_returns_installed_when_status_line_references_linesmith() {
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        fs::write(
            &path,
            r#"{ "statusLine": { "type": "command", "command": "linesmith --config /tmp/x" } }"#,
        )
        .expect("seed");
        match detect_installation_status(&path).expect("detect") {
            InstallationStatus::Installed { command } => {
                assert!(command.contains("linesmith"));
                assert!(command.contains("--config"));
            }
            other => panic!("expected Installed, got {other:?}"),
        }
    }

    #[test]
    fn detect_returns_other_when_status_line_points_elsewhere() {
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        fs::write(
            &path,
            r#"{ "statusLine": { "type": "command", "command": "ccstatusline" } }"#,
        )
        .expect("seed");
        match detect_installation_status(&path).expect("detect") {
            InstallationStatus::Other { command } => assert_eq!(command, "ccstatusline"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn detect_surfaces_invalid_json_as_io_invalid_data() {
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        fs::write(&path, "{ not json").expect("seed");
        let err = detect_installation_status(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn install_creates_file_when_absent() {
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        let outcome = install(&path, "linesmith").expect("install");
        assert_eq!(outcome, InstallOutcome::Created);
        let raw = fs::read_to_string(&path).expect("read");
        let parsed: Value = serde_json::from_str(&raw).expect("parse");
        assert_eq!(parsed["statusLine"]["command"].as_str(), Some("linesmith"),);
        assert_eq!(parsed["statusLine"]["type"].as_str(), Some("command"));
        assert!(!path.with_extension("json.bak").exists(),);
    }

    #[test]
    fn install_preserves_unrelated_top_level_keys() {
        // Pin: the merge mutates only `statusLine` — everything
        // else round-trips. A future refactor that overwrites the
        // whole document would silently wipe the user's other
        // Claude Code settings (model choice, MCP servers, etc.).
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        fs::write(
            &path,
            r#"{ "model": "claude-sonnet-4-5", "permissions": { "allow": ["Edit"] } }"#,
        )
        .expect("seed");
        install(&path, "linesmith").expect("install");
        let raw = fs::read_to_string(&path).expect("read");
        let parsed: Value = serde_json::from_str(&raw).expect("parse");
        assert_eq!(parsed["model"].as_str(), Some("claude-sonnet-4-5"));
        assert_eq!(parsed["permissions"]["allow"][0].as_str(), Some("Edit"),);
        assert_eq!(parsed["statusLine"]["command"].as_str(), Some("linesmith"),);
    }

    #[test]
    fn install_backs_up_prior_settings_to_bak_sibling() {
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        let prior = r#"{ "statusLine": { "type": "command", "command": "ccstatusline" } }"#;
        fs::write(&path, prior).expect("seed");
        let outcome = install(&path, "linesmith").expect("install");
        assert_eq!(outcome, InstallOutcome::Updated);
        let bak = path.with_extension("json.bak");
        assert!(bak.exists(), "expected .bak sibling at {}", bak.display());
        assert_eq!(fs::read_to_string(&bak).expect("read bak"), prior);
        let raw = fs::read_to_string(&path).expect("read");
        assert!(raw.contains("linesmith"));
    }

    #[test]
    fn install_writes_linesmith_block_with_padding_zero() {
        // Pin the snippet shape so a regression that drops `padding`
        // (or sets the wrong `type`) is caught — Claude Code's
        // statusLine schema requires both.
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        install(&path, "linesmith --config /tmp/x").expect("install");
        let raw = fs::read_to_string(&path).expect("read");
        let parsed: Value = serde_json::from_str(&raw).expect("parse");
        let block = &parsed["statusLine"];
        assert_eq!(block["type"].as_str(), Some("command"));
        assert_eq!(block["command"].as_str(), Some("linesmith --config /tmp/x"),);
        assert_eq!(block["padding"].as_i64(), Some(0));
    }

    #[test]
    fn uninstall_returns_no_file_when_settings_absent() {
        let tmp = TempDir::new().expect("tempdir");
        let outcome = uninstall(&settings_path(&tmp)).expect("uninstall");
        assert_eq!(outcome, UninstallOutcome::NoFile);
    }

    #[test]
    fn uninstall_returns_no_status_line_when_key_missing() {
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        fs::write(&path, r#"{ "model": "claude-sonnet-4-5" }"#).expect("seed");
        let outcome = uninstall(&path).expect("uninstall");
        assert_eq!(outcome, UninstallOutcome::NoStatusLine);
        // File untouched, no .bak.
        assert!(!path.with_extension("json.bak").exists());
    }

    #[test]
    fn uninstall_removes_status_line_and_preserves_other_keys() {
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        let prior = r#"{
  "model": "claude-sonnet-4-5",
  "statusLine": { "type": "command", "command": "linesmith" }
}"#;
        fs::write(&path, prior).expect("seed");
        let outcome = uninstall(&path).expect("uninstall");
        assert_eq!(outcome, UninstallOutcome::Removed);
        let raw = fs::read_to_string(&path).expect("read");
        let parsed: Value = serde_json::from_str(&raw).expect("parse");
        assert_eq!(parsed["model"].as_str(), Some("claude-sonnet-4-5"));
        assert!(parsed.get("statusLine").is_none());
        // Prior contents preserved in .bak.
        let bak = path.with_extension("json.bak");
        assert_eq!(fs::read_to_string(&bak).expect("read bak"), prior);
    }

    #[test]
    fn install_then_uninstall_round_trips_to_no_status_line() {
        // End-to-end pin: install + uninstall composes back to a
        // settings.json that's structurally equivalent to "we never
        // touched it" — the `statusLine` key is absent. The .bak
        // captures the intermediate state.
        let tmp = TempDir::new().expect("tempdir");
        let path = settings_path(&tmp);
        fs::write(&path, r#"{ "model": "claude-sonnet-4-5" }"#).expect("seed");
        install(&path, "linesmith").expect("install");
        uninstall(&path).expect("uninstall");
        let final_raw = fs::read_to_string(&path).expect("read");
        let parsed: Value = serde_json::from_str(&final_raw).expect("parse");
        assert!(parsed.get("statusLine").is_none());
        assert_eq!(parsed["model"].as_str(), Some("claude-sonnet-4-5"));
    }
}
