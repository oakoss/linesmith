//! Embedded `config.toml` presets per `docs/specs/config.md` §Presets.
//!
//! Presets are authored as TOML bodies shipped with the binary via
//! `include_str!`. `linesmith presets list` prints [`names`];
//! `linesmith presets apply <name>` writes [`body`] to the resolved
//! config path.

const MINIMAL: &str = include_str!("fixtures/minimal.toml");
const DEVELOPER: &str = include_str!("fixtures/developer.toml");
const POWER_USER: &str = include_str!("fixtures/power_user.toml");
const COST_FOCUSED: &str = include_str!("fixtures/cost_focused.toml");
const WORKTREE_HEAVY: &str = include_str!("fixtures/worktree_heavy.toml");

/// A shipped preset. Display names are kebab-case for the CLI surface;
/// file stems use underscore to match the Rust const idents (e.g.
/// `POWER_USER` ↔ `power_user.toml` ↔ `"power-user"`).
#[derive(Debug, Clone, Copy)]
struct Preset {
    name: &'static str,
    body: &'static str,
}

/// Registry, in the order `linesmith presets list` emits.
const PRESETS: &[Preset] = &[
    Preset {
        name: "minimal",
        body: MINIMAL,
    },
    Preset {
        name: "developer",
        body: DEVELOPER,
    },
    Preset {
        name: "power-user",
        body: POWER_USER,
    },
    Preset {
        name: "cost-focused",
        body: COST_FOCUSED,
    },
    Preset {
        name: "worktree-heavy",
        body: WORKTREE_HEAVY,
    },
];

/// Preset display names in registry order.
pub fn names() -> impl Iterator<Item = &'static str> {
    PRESETS.iter().map(|p| p.name)
}

/// Return the TOML body for a preset, or `None` if the name isn't
/// registered. Case-sensitive.
#[must_use]
pub fn body(name: &str) -> Option<&'static str> {
    PRESETS.iter().find(|p| p.name == name).map(|p| p.body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build_segments;
    use crate::config::Config;
    use std::str::FromStr;

    fn segments_of(preset: &str) -> Vec<String> {
        let body = body(preset).expect("preset registered");
        let cfg = Config::from_str(body).expect("parse");
        cfg.line
            .expect("preset has [line]")
            .segments
            .into_iter()
            .collect()
    }

    #[test]
    fn registry_has_five_presets_in_stable_order() {
        let got: Vec<&str> = names().collect();
        assert_eq!(
            got,
            vec![
                "minimal",
                "developer",
                "power-user",
                "cost-focused",
                "worktree-heavy",
            ]
        );
    }

    #[test]
    fn every_preset_parses_without_warnings() {
        for name in names() {
            let body = body(name).expect("preset registered");
            let cfg = Config::from_str(body)
                .unwrap_or_else(|e| panic!("preset '{name}' failed to parse: {e}"));
            let mut warnings: Vec<String> = Vec::new();
            let _ = build_segments(Some(&cfg), None, |m: &str| warnings.push(m.to_string()));
            assert!(
                warnings.is_empty(),
                "preset '{name}' emitted warnings: {warnings:?}"
            );
        }
    }

    #[test]
    fn minimal_preset_segments_are_model_and_context_window() {
        assert_eq!(segments_of("minimal"), vec!["model", "context_window"]);
    }

    #[test]
    fn developer_preset_segments_match_spec() {
        assert_eq!(
            segments_of("developer"),
            vec![
                "model",
                "workspace",
                "context_window",
                "cost",
                "session_duration",
                "rate_limit_5h",
                "rate_limit_7d",
            ]
        );
    }

    #[test]
    fn power_user_preset_segments_match_spec() {
        assert_eq!(
            segments_of("power-user"),
            vec![
                "model",
                "context_window",
                "workspace",
                "rate_limit_5h",
                "rate_limit_7d",
                "cost",
                "effort",
                "tokens_total",
            ]
        );
    }

    #[test]
    fn cost_focused_preset_segments_match_spec() {
        assert_eq!(
            segments_of("cost-focused"),
            vec![
                "model",
                "context_window",
                "cost",
                "rate_limit_5h",
                "rate_limit_7d",
            ]
        );
    }

    #[test]
    fn worktree_heavy_preset_segments_match_spec() {
        assert_eq!(
            segments_of("worktree-heavy"),
            vec!["model", "workspace", "context_window"]
        );
    }

    #[test]
    fn preset_names_are_unique() {
        // `body()` returns the first match, so a duplicate `name` would
        // silently shadow any later entry.
        let mut seen = std::collections::HashSet::new();
        for name in names() {
            assert!(seen.insert(name), "duplicate preset name: {name}");
        }
    }

    #[test]
    fn body_returns_none_for_unknown_name() {
        assert!(body("definitely-not-a-preset").is_none());
        assert!(body("").is_none());
    }

    #[test]
    fn body_lookup_is_case_sensitive() {
        assert!(body("minimal").is_some());
        assert!(body("Minimal").is_none());
        assert!(body("MINIMAL").is_none());
        assert!(body("Developer").is_none());
    }
}
