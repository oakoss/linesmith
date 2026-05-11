//! Atomic file writes via temp + rename. Shared across the TUI's
//! config save path ([`tui::Model::save`]) and the Claude Code
//! settings install/uninstall path ([`claude_settings`]). Both need
//! the same crash-safe write semantics: a partial write or interrupted
//! rename must not corrupt the user's existing file.
//!
//! The implementation puts the temp on the same filesystem as the
//! destination (via `NamedTempFile::new_in(parent)`) so `persist`'s
//! rename can't hit `EXDEV` (cross-device move) and lose atomicity.
//! The parent directory is created on demand to handle the first-run
//! flow where the target's directory hasn't been created yet.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Write `contents` to `path` atomically: write to a tempfile in the
/// same parent directory, fsync, then rename over the destination.
/// Creates the parent directory if absent so first-run paths under
/// `~/.config/linesmith/` or `~/.claude/` don't `NotFound` on the
/// very first save.
///
/// Absolute-path conversion happens up front so a host process that
/// changes cwd between writes can't drift the temp into a different
/// filesystem and lose `persist`'s atomic contract via cross-device
/// rename.
// `pub(crate)` is redundant inside a `pub(crate) mod`, but plain
// `pub` triggers `unreachable_pub`. Allow the redundancy lint at the
// item level so the convention matches the rest of the crate.
#[allow(clippy::redundant_pub_crate)]
pub(crate) fn atomic_write(path: &Path, contents: &str) -> io::Result<()> {
    let absolute: PathBuf = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let parent = absolute.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic_write: path has no parent directory",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(contents.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(&absolute).map_err(|e| {
        // Log the orphaned temp so a user investigating "why did save
        // fail" doesn't also have to wonder where the leftover
        // `.tmpXXXXXX` came from. `Drop` on `e.file` cleans up if it
        // can, but a partial-persist on Windows ("destination open by
        // another process") can leave the temp behind.
        linesmith_core::lsm_warn!(
            "atomic_write: persist failed; orphaned temp at {} may be removed manually",
            e.file.path().display(),
        );
        e.error
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn creates_new_file_with_contents() {
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        atomic_write(&path, "[line]\nsegments = []\n").expect("write");
        let read = fs::read_to_string(&path).expect("read");
        assert_eq!(read, "[line]\nsegments = []\n");
    }

    #[test]
    fn replaces_existing_file_atomically() {
        // Pin the round-trip: writing fresh contents over an
        // existing file leaves the destination with the new bytes,
        // not concatenated. Failure mode would be a non-atomic write
        // that appends or partially overwrites.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        fs::write(&path, "old contents that should disappear").expect("seed");
        atomic_write(&path, "new bytes").expect("write");
        assert_eq!(fs::read_to_string(&path).expect("read"), "new bytes");
    }

    #[test]
    fn creates_missing_parent_directory() {
        // Pointing the write at a path under a directory that
        // doesn't exist (first-run `~/.config/linesmith/` or
        // `~/.claude/`) creates the parent before writing.
        let tmp = TempDir::new().expect("tempdir");
        let nested = tmp.path().join("nested/subdir/config.toml");
        atomic_write(&nested, "x").expect("write");
        assert_eq!(fs::read_to_string(&nested).expect("read"), "x");
    }

    #[test]
    fn does_not_leave_temp_file_on_success() {
        // The tempfile crate's persist() consumes the temp; pin that
        // no `*.tmp` siblings linger after a successful write. A
        // regression that drops persist() would silently leave temps
        // behind.
        let tmp = TempDir::new().expect("tempdir");
        let path = tmp.path().join("config.toml");
        atomic_write(&path, "x").expect("write");
        let entries: Vec<_> = fs::read_dir(tmp.path())
            .expect("read_dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected only the destination file, got {entries:?}",
        );
    }
}
