//! Per-tool normalizers + heuristic dispatch.
//!
//! See `docs/specs/input-schema.md` §"Heuristic detection" for the
//! authoritative rules. Today only [`claude`] has a concrete normalizer;
//! the other tools route through it as the documented **Fallback**
//! because their detection rules are stubs pending a discriminator that
//! can survive CC 2.x emitting `version` itself.
//!
//! Detection precedence (first match wins):
//!
//! 1. [`ParseOpts::tool`] explicit override.
//! 2. `LINESMITH_TOOL` env var. Case-insensitive aliases for the four
//!    known tools (`claude`, `qwen`, `codex`, `copilot`); any other
//!    non-empty value becomes [`Tool::Other`] with the trimmed input
//!    preserved as the variant payload so plugins and diagnostics see
//!    the value the operator actually set.
//! 3. Heuristic — `cost.total_api_duration_ms` present ⇒ ClaudeCode.
//! 4. Fallback — [`Tool::ClaudeCode`].

use std::borrow::Cow;
use std::env;
use std::sync::Arc;

use crate::input::{ParseError, ParseOpts, StatusContext, Tool};

pub(super) mod claude;
pub(super) mod other;
pub(super) mod qwen;

const ENV_TOOL_OVERRIDE: &str = "LINESMITH_TOOL";

pub(super) fn dispatch(
    raw: Arc<serde_json::Value>,
    opts: &ParseOpts,
) -> Result<StatusContext, ParseError> {
    let tool = detect_tool(&raw, opts);
    claude::normalize(raw, tool)
}

fn detect_tool(raw: &serde_json::Value, opts: &ParseOpts) -> Tool {
    if let Some(tool) = opts.tool.as_ref() {
        return tool.clone();
    }
    if let Some(tool) = tool_from_env() {
        return tool;
    }
    detect_from_shape(raw)
}

fn tool_from_env() -> Option<Tool> {
    let raw = match env::var(ENV_TOOL_OVERRIDE) {
        Ok(s) => s,
        Err(env::VarError::NotPresent) => return None,
        Err(env::VarError::NotUnicode(_)) => {
            crate::lsm_warn!(
                "{ENV_TOOL_OVERRIDE}: value is not valid UTF-8; falling through to heuristic"
            );
            return None;
        }
    };
    let tool = tool_from_str(&raw)?;
    if let Tool::Other(ref name) = tool {
        // warn-level so a typo'd LINESMITH_TOOL is visible without LINESMITH_LOG set.
        crate::lsm_warn!(
            "{ENV_TOOL_OVERRIDE}={name:?}: not a known alias (claude, qwen, codex, copilot); routing as Tool::Other with the trimmed value preserved"
        );
    }
    Some(tool)
}

/// Used by [`tool_from_env`] and tested directly so the alias table
/// is verified without touching the process env (`std::env::set_var`
/// is `unsafe` since Rust 1.91).
///
/// Returns `None` for empty/whitespace-only input. Unknown non-empty
/// values become [`Tool::Other`] with the trimmed text preserved for
/// downstream diagnostics and plugin segments.
fn tool_from_str(raw: &str) -> Option<Tool> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Accept underscore, no-separator, AND dash forms — `claude-code`
    // is the dominant package-naming convention upstream, so collapsing
    // it to `Tool::ClaudeCode` avoids the "Other(\"claude-code\") is
    // not Tool::ClaudeCode" identity surprise.
    let tool = match trimmed.to_ascii_lowercase().as_str() {
        "claude" | "claude_code" | "claude-code" | "claudecode" => Tool::ClaudeCode,
        "qwen" | "qwen_code" | "qwen-code" | "qwencode" => Tool::QwenCode,
        "codex" | "codex_cli" | "codex-cli" | "codexcli" => Tool::CodexCli,
        "copilot" | "copilot_cli" | "copilot-cli" | "copilotcli" => Tool::CopilotCli,
        _ => Tool::Other(Cow::Owned(trimmed.to_string())),
    };
    Some(tool)
}

fn has_claude_signature(raw: &serde_json::Value) -> bool {
    raw.as_object()
        .and_then(|root| root.get("cost"))
        .and_then(serde_json::Value::as_object)
        .is_some_and(|cost| cost.contains_key("total_api_duration_ms"))
}

// Rules 1 and 5 both resolve to ClaudeCode today; the branch is kept
// so adding Qwen/Codex/Copilot rules is a one-line edit, and the
// rule-5 path emits a debug trace to distinguish "matched rule 1"
// from "fell through to default" once real per-tool normalizers ship.
#[allow(clippy::if_same_then_else)]
fn detect_from_shape(raw: &serde_json::Value) -> Tool {
    if has_claude_signature(raw) {
        Tool::ClaudeCode // rule 1
    } else {
        crate::lsm_debug!("tool detection: no shape signature matched; falling back to ClaudeCode");
        Tool::ClaudeCode // rule 5 (Fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ParseOpts {
        ParseOpts::default()
    }

    #[test]
    fn detect_falls_back_to_claude_when_no_signal() {
        let raw = serde_json::json!({});
        assert_eq!(detect_tool(&raw, &opts()), Tool::ClaudeCode);
    }

    #[test]
    fn detect_returns_claude_when_cost_total_api_duration_ms_present() {
        let raw = serde_json::json!({
            "cost": { "total_api_duration_ms": 0 }
        });
        assert_eq!(detect_tool(&raw, &opts()), Tool::ClaudeCode);
    }

    #[test]
    fn detect_matches_claude_when_total_api_duration_ms_is_null() {
        // Pins the contract: "key present" wins regardless of value
        // (the heuristic checks `contains_key`, not the value).
        let raw = serde_json::json!({
            "cost": { "total_api_duration_ms": serde_json::Value::Null }
        });
        assert_eq!(detect_tool(&raw, &opts()), Tool::ClaudeCode);
    }

    #[test]
    fn detect_falls_back_when_cost_is_non_object() {
        // Future cost-shape drift (string instead of object) must not
        // satisfy the Claude signature; falls through to Fallback.
        let raw = serde_json::json!({ "cost": "broken" });
        assert_eq!(detect_tool(&raw, &opts()), Tool::ClaudeCode);
    }

    #[test]
    fn detect_opts_override_wins_over_heuristic() {
        let raw = serde_json::json!({
            "cost": { "total_api_duration_ms": 0 }
        });
        let opts = ParseOpts::default().with_tool(Tool::QwenCode);
        assert_eq!(detect_tool(&raw, &opts), Tool::QwenCode);
    }

    #[test]
    fn detect_opts_override_used_when_env_unset() {
        let raw = serde_json::json!({});
        let opts = ParseOpts::default().with_tool(Tool::CodexCli);
        assert_eq!(detect_tool(&raw, &opts), Tool::CodexCli);
    }

    #[test]
    fn tool_from_str_parses_canonical_aliases() {
        for (input, want) in [
            ("claude", Tool::ClaudeCode),
            ("claude_code", Tool::ClaudeCode),
            ("claudecode", Tool::ClaudeCode),
            ("qwen", Tool::QwenCode),
            ("qwen_code", Tool::QwenCode),
            ("qwencode", Tool::QwenCode),
            ("codex", Tool::CodexCli),
            ("codex_cli", Tool::CodexCli),
            ("codexcli", Tool::CodexCli),
            ("copilot", Tool::CopilotCli),
            ("copilot_cli", Tool::CopilotCli),
            ("copilotcli", Tool::CopilotCli),
        ] {
            assert_eq!(tool_from_str(input), Some(want.clone()), "input={input:?}");
        }
    }

    #[test]
    fn tool_from_str_is_case_insensitive() {
        assert_eq!(tool_from_str("CLAUDE"), Some(Tool::ClaudeCode));
        assert_eq!(tool_from_str("Qwen"), Some(Tool::QwenCode));
        assert_eq!(tool_from_str("CoPiLoT_CLI"), Some(Tool::CopilotCli));
    }

    #[test]
    fn tool_from_str_trims_surrounding_whitespace() {
        assert_eq!(tool_from_str("  qwen  "), Some(Tool::QwenCode));
    }

    #[test]
    fn tool_from_str_returns_none_for_empty_and_whitespace() {
        assert_eq!(tool_from_str(""), None);
        assert_eq!(tool_from_str("   "), None);
        assert_eq!(tool_from_str("\t\n "), None);
    }

    #[test]
    fn tool_from_str_unknown_value_routes_to_tool_other_with_trimmed_input() {
        let got = tool_from_str("  gemini  ").expect("non-empty input");
        assert_eq!(got, Tool::Other(Cow::Owned("gemini".to_string())));
    }

    #[test]
    fn tool_from_str_accepts_dash_separated_aliases() {
        // Defense against the operator-supplied `LINESMITH_TOOL=claude-code`
        // producing a `Tool::Other("claude-code")` that compares unequal
        // to `Tool::ClaudeCode` — the alias table folds the dominant
        // package-naming convention into the canonical variant.
        assert_eq!(tool_from_str("claude-code"), Some(Tool::ClaudeCode));
        assert_eq!(tool_from_str("qwen-code"), Some(Tool::QwenCode));
        assert_eq!(tool_from_str("codex-cli"), Some(Tool::CodexCli));
        assert_eq!(tool_from_str("copilot-cli"), Some(Tool::CopilotCli));
    }

    #[test]
    fn tool_from_str_unknown_with_known_substring_stays_other() {
        // The alias table is exact-after-fold; a longer name that
        // happens to contain a known token (e.g. "claude-haiku") still
        // routes to Tool::Other.
        assert_eq!(
            tool_from_str("claude-haiku"),
            Some(Tool::Other(Cow::Owned("claude-haiku".to_string())))
        );
    }

    #[test]
    fn tool_from_str_every_canonical_variant_has_all_four_spellings() {
        // Forgetting one spelling in the match arm regresses to
        // Tool::Other silently — the alias table has 4 names × 4
        // spellings (bare, _suffix, -suffix, suffix-no-separator) so
        // pin them all in one place that fails loudly when the table
        // drifts.
        for (base, suffix, expected) in [
            ("claude", "code", Tool::ClaudeCode),
            ("qwen", "code", Tool::QwenCode),
            ("codex", "cli", Tool::CodexCli),
            ("copilot", "cli", Tool::CopilotCli),
        ] {
            for spelling in [
                base.to_string(),
                format!("{base}_{suffix}"),
                format!("{base}-{suffix}"),
                format!("{base}{suffix}"),
            ] {
                assert_eq!(
                    tool_from_str(&spelling),
                    Some(expected.clone()),
                    "spelling {spelling:?} did not fold to {expected:?}"
                );
            }
        }
    }
}
