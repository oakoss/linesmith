//! User config: parse `config.toml`, resolve its path, and apply
//! per-segment overrides. Full contract in `docs/specs/config.md`.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

/// Parsed `config.toml`. Serde ignores unknown keys so a file from a
/// newer linesmith still parses on an older binary; fields this
/// version doesn't know are dropped rather than rejected.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Config {
    pub line: Option<LineConfig>,
    pub theme: Option<String>,
    #[serde(default)]
    pub segments: BTreeMap<String, SegmentOverride>,
}

/// `[line]` section: ordered list of segment ids to render.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct LineConfig {
    pub segments: Vec<String>,
}

/// `[segments.<id>]` override block. Each field, when `Some`, replaces
/// the segment's built-in default.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct SegmentOverride {
    pub priority: Option<u8>,
    pub width: Option<WidthBoundsConfig>,
}

/// Width-bounds override. Either side may be omitted; a missing side
/// inherits from the segment's built-in default.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct WidthBoundsConfig {
    pub min: Option<u16>,
    pub max: Option<u16>,
}

/// Failure modes when loading a config. A missing file is not an error;
/// callers treat it as "use defaults."
#[derive(Debug)]
#[non_exhaustive]
pub enum ConfigError {
    /// `fs::read` failed for a reason other than `NotFound`. Carries
    /// the offending path so the stderr diagnostic is self-describing.
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    /// Invalid TOML. `path` is `None` for in-memory parses.
    Parse {
        path: Option<PathBuf>,
        source: toml::de::Error,
    },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "config I/O at {}: {source}", path.display()),
            Self::Parse {
                path: Some(p),
                source,
            } => write!(f, "config parse at {}: {source}", p.display()),
            Self::Parse { path: None, source } => write!(f, "config parse: {source}"),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
        }
    }
}

impl FromStr for Config {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        toml::from_str(s).map_err(|source| ConfigError::Parse { path: None, source })
    }
}

impl Config {
    /// Read and parse the file at `path`. Returns `Ok(None)` when the
    /// file doesn't exist (normal case for first-run users); other I/O
    /// errors propagate so callers can log them.
    pub fn load(path: &Path) -> Result<Option<Self>, ConfigError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_owned(),
                    source,
                })
            }
        };
        toml::from_str(&raw)
            .map(Some)
            .map_err(|source| ConfigError::Parse {
                path: Some(path.to_owned()),
                source,
            })
    }
}

/// Where linesmith found its config path and how. `explicit = true`
/// means the user named a path directly (`--config` or
/// `LINESMITH_CONFIG`); the run-time diagnostics use this to decide
/// whether a missing file is worth warning about (explicit paths
/// warn, implicit XDG fallbacks stay silent for first-run users).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPath {
    pub path: PathBuf,
    pub explicit: bool,
}

/// Where linesmith looks for its config file, in precedence order.
#[must_use]
pub fn resolve_config_path(
    cli_override: Option<PathBuf>,
    env_override: Option<&str>,
    xdg_config_home: Option<&str>,
    home: Option<&str>,
) -> Option<ConfigPath> {
    if let Some(p) = cli_override.filter(|p| !p.as_os_str().is_empty()) {
        return Some(ConfigPath {
            path: p,
            explicit: true,
        });
    }
    if let Some(p) = env_override.filter(|s| !s.is_empty()) {
        return Some(ConfigPath {
            path: PathBuf::from(p),
            explicit: true,
        });
    }
    if let Some(p) = xdg_config_home.filter(|s| !s.is_empty()) {
        return Some(ConfigPath {
            path: PathBuf::from(p).join("linesmith").join("config.toml"),
            explicit: false,
        });
    }
    home.filter(|s| !s.is_empty()).map(|h| ConfigPath {
        path: PathBuf::from(h).join(".config/linesmith/config.toml"),
        explicit: false,
    })
}

/// Thin wrapper around [`resolve_config_path`] that reads the process
/// env directly. Used at startup.
#[must_use]
pub fn detect_config_path(cli_override: Option<PathBuf>) -> Option<ConfigPath> {
    let env_override = std::env::var("LINESMITH_CONFIG").ok();
    let xdg_config_home = std::env::var("XDG_CONFIG_HOME").ok();
    let home = std::env::var("HOME").ok();
    resolve_config_path(
        cli_override,
        env_override.as_deref(),
        xdg_config_home.as_deref(),
        home.as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- parse ---

    #[test]
    fn empty_config_parses() {
        let c = Config::from_str("").expect("parse ok");
        assert_eq!(c.line, None);
        assert!(c.segments.is_empty());
    }

    #[test]
    fn line_segments_parse_in_order() {
        let c = Config::from_str(
            r#"
                [line]
                segments = ["model", "workspace", "cost"]
            "#,
        )
        .expect("parse ok");
        let line = c.line.expect("line present");
        assert_eq!(line.segments, vec!["model", "workspace", "cost"]);
    }

    #[test]
    fn segment_override_priority_parses() {
        let c = Config::from_str(
            r#"
                [segments.model]
                priority = 16
            "#,
        )
        .expect("parse ok");
        assert_eq!(c.segments["model"].priority, Some(16));
        assert_eq!(c.segments["model"].width, None);
    }

    #[test]
    fn segment_override_width_parses_both_sides() {
        let c = Config::from_str(
            r#"
                [segments.workspace.width]
                min = 10
                max = 40
            "#,
        )
        .expect("parse ok");
        let w = c.segments["workspace"].width.expect("width present");
        assert_eq!(w.min, Some(10));
        assert_eq!(w.max, Some(40));
    }

    #[test]
    fn unknown_top_level_key_is_forward_compatible() {
        // Config files from a newer linesmith must still parse on an
        // older binary; fields this version doesn't implement are
        // ignored rather than rejected.
        let c = Config::from_str(
            r#"
                theme = "catppuccin-mocha"
                layout = "single-line"
                [layout_options]
                separator = "powerline"
            "#,
        )
        .expect("parse ok");
        assert_eq!(c.line, None);
        assert!(c.segments.is_empty());
    }

    #[test]
    fn malformed_toml_reports_parse_error() {
        let err = Config::from_str("[line").unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn io_error_carries_path_in_display() {
        use std::io::ErrorKind;
        let err = ConfigError::Io {
            path: PathBuf::from("/etc/linesmith/config.toml"),
            source: std::io::Error::new(ErrorKind::PermissionDenied, "denied"),
        };
        let rendered = err.to_string();
        assert!(rendered.contains("/etc/linesmith/config.toml"));
        assert!(rendered.contains("denied"));
    }

    #[test]
    fn bom_prefixed_config_parses() {
        // Windows editors sometimes save configs with a leading UTF-8
        // BOM. The `toml` crate tolerates it, so no explicit strip is
        // needed; this test locks that behavior.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "\u{FEFF}[line]\nsegments = [\"model\"]\n").unwrap();
        let c = Config::load(&path).expect("ok").expect("present");
        assert_eq!(c.line.expect("line").segments, vec!["model".to_string()]);
    }

    #[test]
    fn load_returns_none_for_missing_file() {
        let dir = tempdir();
        let path = dir.path().join("nonexistent.toml");
        assert!(Config::load(&path).unwrap().is_none());
    }

    // --- path resolution ---

    fn resolved(
        cli: Option<&str>,
        env: Option<&str>,
        xdg: Option<&str>,
        home: Option<&str>,
    ) -> Option<ConfigPath> {
        resolve_config_path(cli.map(PathBuf::from), env, xdg, home)
    }

    #[test]
    fn cli_override_wins_over_everything_and_is_explicit() {
        let got = resolved(
            Some("/explicit.toml"),
            Some("/env.toml"),
            Some("/xdg"),
            Some("/home"),
        )
        .expect("resolved");
        assert_eq!(got.path, PathBuf::from("/explicit.toml"));
        assert!(got.explicit);
    }

    #[test]
    fn env_wins_over_xdg_and_home_and_is_explicit() {
        let got = resolved(None, Some("/env.toml"), Some("/xdg"), Some("/home")).expect("resolved");
        assert_eq!(got.path, PathBuf::from("/env.toml"));
        assert!(got.explicit);
    }

    #[test]
    fn xdg_config_home_is_implicit() {
        let got = resolved(None, None, Some("/xdg"), Some("/home")).expect("resolved");
        assert_eq!(got.path, PathBuf::from("/xdg/linesmith/config.toml"));
        assert!(!got.explicit);
    }

    #[test]
    fn home_fallback_is_implicit() {
        let got = resolved(None, None, None, Some("/home")).expect("resolved");
        assert_eq!(
            got.path,
            PathBuf::from("/home/.config/linesmith/config.toml")
        );
        assert!(!got.explicit);
    }

    #[test]
    fn returns_none_when_no_home_and_no_xdg() {
        assert_eq!(resolved(None, None, None, None), None);
    }

    #[test]
    fn empty_env_values_are_ignored() {
        let got = resolved(None, Some(""), Some(""), Some("/home")).expect("resolved");
        assert_eq!(
            got.path,
            PathBuf::from("/home/.config/linesmith/config.toml")
        );
    }

    #[test]
    fn empty_cli_override_does_not_count_as_explicit() {
        // A shell expansion like `--config "$MISSING_VAR"` can produce
        // an empty path; skip past it rather than silently treating it
        // as "load ''" which would NotFound-swallow.
        let got = resolved(Some(""), None, Some("/xdg"), None).expect("resolved");
        assert_eq!(got.path, PathBuf::from("/xdg/linesmith/config.toml"));
        assert!(!got.explicit);
    }

    // --- helpers ---

    struct TempDir(PathBuf);

    impl TempDir {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        let base = std::env::temp_dir().join(format!(
            "linesmith-config-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ));
        std::fs::create_dir_all(&base).expect("mkdir");
        TempDir(base)
    }
}
