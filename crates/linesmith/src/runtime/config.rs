//! Runtime config-load wrapper. Both driver and doctor parse user
//! config through [`Config::load_validated`] so unknown-key warnings
//! ride alongside the parsed config (or the parse error). Doctor's
//! read is documented in `docs/specs/doctor.md` §Config.
//!
//! Pre-routing, doctor used bare `toml::from_str` and silently
//! accepted typos like `[segmnts.echo]` that the runtime warned
//! about — the parity gap this module closes.

use std::path::PathBuf;

use crate::config::{Config, ConfigError, ConfigPath};

/// Outcome of loading the user config from a resolved path. Five
/// variants; runtime and doctor render each differently.
///
/// Outer `path` is authoritative. `ConfigError`'s inner `path` is
/// `Option<PathBuf>` upstream because `from_str_validated` parses
/// in-memory strings without one — the outer field guarantees a
/// path here regardless of the source variant.
///
/// `ParseError` carries `warnings` because [`Config::load_validated`]
/// calls `validate_keys` between the syntactic TOML parse and the
/// typed `try_into`: unknown-key warnings can fire and *then* a
/// type-mismatch error returns. Dropping them would force users to
/// fix typos one at a time. `IoError` carries the field too for
/// shape symmetry; it'll always be empty (the read fails before any
/// parsing).
#[derive(Debug)]
#[non_exhaustive]
pub(crate) enum ConfigLoadOutcome {
    /// No config-path source resolved. Doctor renders this as
    /// "Unresolved" (different from "NotFound" — the cascade itself
    /// failed); driver treats it as "use defaults silently."
    Unresolved,
    /// Path resolved but file doesn't exist. `explicit` is `true`
    /// when the path came from `--config` / `LINESMITH_CONFIG` so
    /// the diagnostic can warn loudly; implicit XDG/HOME paths are
    /// silent for first-run users.
    NotFound { path: PathBuf, explicit: bool },
    /// `fs::read_to_string` failed for a reason other than NotFound
    /// (permission denied, invalid UTF-8, etc.). `source` carries
    /// the underlying [`ConfigError`] for verbatim diagnostic
    /// rendering — `Display` includes the path. `warnings` is
    /// always empty (the read fails before validation runs).
    IoError {
        path: PathBuf,
        source: ConfigError,
        warnings: Vec<String>,
    },
    /// TOML parse or type-mismatch failed. `source` carries the
    /// underlying [`ConfigError::Parse`] verbatim so the line/column
    /// span survives to the renderer. `warnings` carries any
    /// unknown-key diagnostics that fired before the typed
    /// `try_into` rejected the document — see the type-level doc
    /// for why this matters.
    ParseError {
        path: PathBuf,
        source: ConfigError,
        warnings: Vec<String>,
    },
    /// Loaded successfully. `warnings` contains one entry per
    /// unknown key encountered by [`Config::load_validated`]; empty
    /// for clean configs.
    Loaded {
        path: PathBuf,
        config: Box<Config>,
        warnings: Vec<String>,
    },
}

/// Single config-load entry both runtime and doctor route through.
/// Calls [`Config::load_validated`] so unknown-key warnings flow to
/// the caller via the [`ConfigLoadOutcome::Loaded`] variant rather
/// than being collected by side effect on a sink.
#[must_use]
pub(crate) fn load_config(resolved: Option<&ConfigPath>) -> ConfigLoadOutcome {
    let Some(cp) = resolved else {
        return ConfigLoadOutcome::Unresolved;
    };
    let mut warnings = Vec::new();
    // The match arms below assume `ConfigError`'s only variants are
    // `Io` and `Parse`. That holds because `ConfigError` lives in
    // this same crate; a new variant would surface as a missing-arm
    // compile error. If `config.rs` ever moves to `linesmith-core`
    // (per ADR-0018), this match becomes a silent-drop hazard and
    // needs an explicit fallthrough.
    match Config::load_validated(&cp.path, |msg| warnings.push(msg.to_string())) {
        Ok(Some(config)) => ConfigLoadOutcome::Loaded {
            path: cp.path.clone(),
            config: Box::new(config),
            warnings,
        },
        Ok(None) => ConfigLoadOutcome::NotFound {
            path: cp.path.clone(),
            explicit: cp.explicit,
        },
        Err(source @ ConfigError::Io { .. }) => ConfigLoadOutcome::IoError {
            path: cp.path.clone(),
            source,
            warnings,
        },
        Err(source @ ConfigError::Parse { .. }) => ConfigLoadOutcome::ParseError {
            path: cp.path.clone(),
            source,
            warnings,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_error_preserves_validation_warnings_collected_before_type_mismatch() {
        // `validate_keys` runs between toml::from_str and try_into,
        // so an unknown key + a typed parse failure must surface
        // both — users shouldn't have to fix typos one at a time.
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"unknown_top = 1\ntheme = 123\n").unwrap();
        let cp = ConfigPath {
            path: tmp.path().to_owned(),
            explicit: true,
        };
        match load_config(Some(&cp)) {
            ConfigLoadOutcome::ParseError { warnings, .. } => {
                assert_eq!(warnings.len(), 1, "expected one unknown-key warning");
                assert!(
                    warnings[0].contains("unknown_top"),
                    "warning: {:?}",
                    warnings[0]
                );
            }
            other => panic!("expected ParseError, got {other:?}"),
        }
    }
}
