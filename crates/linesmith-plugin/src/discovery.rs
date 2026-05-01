//! Plugin file discovery. Scans the configured `plugin_dirs` plus the
//! default `$XDG_CONFIG_HOME/linesmith/segments/` (falling back to
//! `~/.config/linesmith/segments/`) for `.rhai` files, returning an
//! ordered deduplicated path list.
//!
//! Discovery order per `docs/specs/plugin-api.md` §Plugin file
//! location: config-declared directories first (in list order), then
//! the XDG default. Dedupe is by canonical absolute path — symlinks
//! to the same file appear once — but not by plugin id. Id-dedup
//! lands in the registry step when the script is compiled and its
//! `const ID` is known.
//!
//! Missing directories are treated as empty (not an error); the spec
//! §Edge cases row for "Plugin dir doesn't exist" says the dir is
//! silently skipped and `linesmith doctor` handles the hint.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Walk `config_dirs` followed by the XDG default, returning every
/// `.rhai` file in discovery order with duplicates (by canonical
/// path) removed. Non-recursive: files in subdirectories are not
/// picked up.
#[must_use]
pub fn scan_plugin_dirs(config_dirs: &[PathBuf]) -> Vec<PathBuf> {
    scan_dirs(config_dirs, xdg_segments_dir().as_deref())
}

/// Pure-logic helper used by [`scan_plugin_dirs`], by the plugin
/// registry's test-isolated load path, and by tests. Kept separate
/// from the env-reading public API so callers (and tests) can pin
/// `xdg_dir` to a specific value without mutating process env (which
/// is racy under the default parallel test runner).
pub(crate) fn scan_dirs(config_dirs: &[PathBuf], xdg_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut ordered: Vec<&Path> = config_dirs.iter().map(PathBuf::as_path).collect();
    if let Some(p) = xdg_dir {
        ordered.push(p);
    }

    let mut results = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for dir in ordered {
        for path in list_rhai_files(dir) {
            let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if seen.insert(key) {
                results.push(path);
            }
        }
    }
    results
}

/// `$XDG_CONFIG_HOME/linesmith/segments/` if `XDG_CONFIG_HOME` is set
/// and non-empty, otherwise `$HOME/.config/linesmith/segments/`.
/// Returns `None` when neither env var is usable (CI sandbox with no
/// home).
fn xdg_segments_dir() -> Option<PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return Some(PathBuf::from(xdg).join("linesmith/segments"));
        }
    }
    let home = std::env::var("HOME").ok().filter(|h| !h.is_empty())?;
    Some(PathBuf::from(home).join(".config/linesmith/segments"))
}

/// Non-recursive listing of `.rhai` files in `dir`. Missing directory
/// or read error returns empty. Result is sorted by path for
/// deterministic ordering within a directory.
fn list_rhai_files(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|ext| ext == "rhai"))
        .collect();
    paths.sort();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Write an empty `.rhai` file at `dir/name` and return the path.
    fn touch_rhai(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, "fn render(ctx) {}\n").expect("write plugin fixture");
        path
    }

    // Tests target the pure `scan_dirs` helper so they don't have
    // to mutate `XDG_CONFIG_HOME` (process-wide env mutation is racy
    // under the default parallel test runner).

    #[test]
    fn missing_directory_returns_empty() {
        let nonexistent = PathBuf::from("/does/not/exist/path/for/linesmith/test");
        assert!(scan_dirs(&[nonexistent], None).is_empty());
    }

    #[test]
    fn empty_directory_returns_empty() {
        let tmp = TempDir::new().expect("tempdir");
        assert!(scan_dirs(&[tmp.path().to_path_buf()], None).is_empty());
    }

    #[test]
    fn finds_rhai_files_in_single_dir_sorted() {
        let tmp = TempDir::new().expect("tempdir");
        let b = touch_rhai(tmp.path(), "b.rhai");
        let a = touch_rhai(tmp.path(), "a.rhai");
        let found = scan_dirs(&[tmp.path().to_path_buf()], None);
        assert_eq!(found, vec![a, b]);
    }

    #[test]
    fn ignores_non_rhai_extensions() {
        let tmp = TempDir::new().expect("tempdir");
        fs::write(tmp.path().join("note.txt"), "not rhai").expect("write txt");
        fs::write(tmp.path().join("script.rs"), "fn main(){}").expect("write rs");
        let plugin = touch_rhai(tmp.path(), "real.rhai");
        assert_eq!(scan_dirs(&[tmp.path().to_path_buf()], None), vec![plugin]);
    }

    #[test]
    fn ignores_subdirectories_non_recursive() {
        let tmp = TempDir::new().expect("tempdir");
        let subdir = tmp.path().join("sub");
        fs::create_dir(&subdir).expect("mkdir");
        touch_rhai(&subdir, "nested.rhai");
        let top = touch_rhai(tmp.path(), "top.rhai");
        assert_eq!(scan_dirs(&[tmp.path().to_path_buf()], None), vec![top]);
    }

    #[test]
    fn config_dirs_scanned_before_xdg_default() {
        // Config dir and XDG dir each have a distinct plugin. Config
        // entry must appear first in the returned order.
        let cfg_dir = TempDir::new().expect("tempdir");
        let xdg_dir = TempDir::new().expect("tempdir");
        let cfg_plugin = touch_rhai(cfg_dir.path(), "from_config.rhai");
        let xdg_plugin = touch_rhai(xdg_dir.path(), "from_xdg.rhai");

        let found = scan_dirs(&[cfg_dir.path().to_path_buf()], Some(xdg_dir.path()));
        assert_eq!(found, vec![cfg_plugin, xdg_plugin]);
    }

    #[test]
    fn duplicate_path_across_dirs_deduped_by_canonical() {
        // Put the same dir in config_dirs twice; the plugin inside
        // should only appear once.
        let tmp = TempDir::new().expect("tempdir");
        let plugin = touch_rhai(tmp.path(), "shared.rhai");
        let dir = tmp.path().to_path_buf();
        let found = scan_dirs(&[dir.clone(), dir], None);
        assert_eq!(found, vec![plugin]);
    }

    #[test]
    fn same_plugin_in_config_and_xdg_keeps_config_copy() {
        // Config-declared dir wins over XDG default for a duplicate
        // canonical path (the first occurrence in discovery order).
        let tmp = TempDir::new().expect("tempdir");
        let plugin = touch_rhai(tmp.path(), "shared.rhai");
        let found = scan_dirs(&[tmp.path().to_path_buf()], Some(tmp.path()));
        assert_eq!(found, vec![plugin]);
    }

    #[test]
    fn multiple_config_dirs_preserve_declaration_order() {
        let a_dir = TempDir::new().expect("tempdir");
        let b_dir = TempDir::new().expect("tempdir");
        let a_plugin = touch_rhai(a_dir.path(), "from_a.rhai");
        let b_plugin = touch_rhai(b_dir.path(), "from_b.rhai");
        let found = scan_dirs(
            &[a_dir.path().to_path_buf(), b_dir.path().to_path_buf()],
            None,
        );
        assert_eq!(found, vec![a_plugin, b_plugin]);
    }

    #[test]
    fn missing_xdg_dir_silently_skipped() {
        let cfg_dir = TempDir::new().expect("tempdir");
        let plugin = touch_rhai(cfg_dir.path(), "only.rhai");
        let missing = PathBuf::from("/nonexistent/xdg/path");
        let found = scan_dirs(&[cfg_dir.path().to_path_buf()], Some(&missing));
        assert_eq!(found, vec![plugin]);
    }
}
