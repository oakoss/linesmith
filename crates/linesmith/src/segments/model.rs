//! Model segment: renders the current model's display name.
//!
//! Anthropic's `display_name` for context-extended variants follows the
//! shape `Opus 4.7 (1M context)`. The default `format = "compact"`
//! strips the trailing word "context" so the segment renders
//! `Opus 4.7 (1M)` — the qualifier is the meaningful capability tag,
//! the word "context" is filler. Users who prefer the verbatim wire
//! value set `format = "full"`.
//!
//! The transform is gated on `Tool::ClaudeCode`. The `(X context)`
//! suffix is Claude Code's documented shape; other tools (Qwen,
//! Codex CLI, Copilot CLI) may use the same suffix with different
//! semantics, so their `display_name` renders verbatim regardless of
//! `format`. Cross-tool compaction can grow into a per-tool
//! transform table when a real second case appears.

use std::collections::BTreeMap;

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::input::Tool;
use crate::theme::Role;

/// Between context_window (32) and rate_limit (96): identity matters for
/// multi-model sessions but isn't time-sensitive like the health metrics.
const PRIORITY: u8 = 64;

const ID: &str = "model";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum ModelFormat {
    /// Strip the word "context" from a trailing `(X context)`
    /// parenthetical: `Opus 4.7 (1M context)` → `Opus 4.7 (1M)`.
    /// No-op when `display_name` doesn't match the suffix shape.
    #[default]
    Compact,
    /// Render `display_name` exactly as Anthropic sent it.
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Config {
    pub(crate) format: ModelFormat,
}

#[derive(Default)]
pub struct ModelSegment {
    cfg: Config,
}

impl ModelSegment {
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut cfg = Config::default();
        if let Some(v) = extras.get("format") {
            match v.as_str() {
                Some("compact") => cfg.format = ModelFormat::Compact,
                Some("full") => cfg.format = ModelFormat::Full,
                _ => warn(&format!(
                    "segments.{ID}.format: expected \"compact\" or \"full\"; ignoring"
                )),
            }
        }
        Self { cfg }
    }
}

impl Segment for ModelSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let raw = ctx.status.model.display_name.trim();
        if raw.is_empty() {
            return Ok(None);
        }
        let text = match self.cfg.format {
            ModelFormat::Full => raw.to_string(),
            ModelFormat::Compact if matches!(ctx.status.tool, Tool::ClaudeCode) => {
                shorten_context_label(raw).unwrap_or_else(|| raw.to_string())
            }
            ModelFormat::Compact => raw.to_string(),
        };
        Ok(Some(RenderedSegment::new(text).with_role(Role::Primary)))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

/// Strip the trailing word "context" from a `(X context)` parenthetical.
/// Returns `None` when `s` doesn't end in `" context)"` or when the
/// matching opening `" ("` can't be found — the segment then falls
/// through to verbatim rendering. Avoids pulling in `regex` for one
/// suffix transform.
fn shorten_context_label(s: &str) -> Option<String> {
    let stripped = s.strip_suffix(" context)")?;
    let open_idx = stripped.rfind(" (")?;
    let qualifier = &stripped[open_idx + 2..];
    let prefix = &stripped[..open_idx];
    Some(format!("{prefix} ({qualifier})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx(display_name: &str) -> DataContext {
        ctx_for_tool(Tool::ClaudeCode, display_name)
    }

    fn ctx_for_tool(tool: Tool, display_name: &str) -> DataContext {
        DataContext::new(StatusContext {
            tool,
            model: ModelInfo {
                display_name: display_name.into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    #[test]
    fn compact_strips_context_word_from_parenthetical() {
        let seg = ModelSegment::default();
        assert_eq!(
            seg.render(&ctx("Opus 4.7 (1M context)"), &rc()).unwrap(),
            Some(RenderedSegment::new("Opus 4.7 (1M)").with_role(Role::Primary))
        );
    }

    #[test]
    fn compact_passes_through_when_no_parenthetical() {
        let seg = ModelSegment::default();
        assert_eq!(
            seg.render(&ctx("Sonnet 4.5"), &rc()).unwrap(),
            Some(RenderedSegment::new("Sonnet 4.5").with_role(Role::Primary))
        );
    }

    #[test]
    fn compact_preserves_parenthetical_not_ending_in_context() {
        // Hypothetical future format like "(beta)" — no `context` word,
        // not our suffix to strip, render verbatim.
        let seg = ModelSegment::default();
        assert_eq!(
            seg.render(&ctx("Opus 4.7 (beta)"), &rc()).unwrap(),
            Some(RenderedSegment::new("Opus 4.7 (beta)").with_role(Role::Primary))
        );
    }

    #[test]
    fn compact_handles_multi_word_qualifier() {
        // Hypothetical "(1M extended context)" → "(1M extended)".
        let seg = ModelSegment::default();
        assert_eq!(
            seg.render(&ctx("Opus 4.7 (1M extended context)"), &rc())
                .unwrap(),
            Some(RenderedSegment::new("Opus 4.7 (1M extended)").with_role(Role::Primary))
        );
    }

    #[test]
    fn compact_does_not_mutate_non_claude_code_display_names() {
        // The `(X context)` suffix is Claude Code's documented shape;
        // other tools may use the same suffix with different semantics.
        // Compact must leave their `display_name` untouched.
        let seg = ModelSegment::default();
        for tool in [
            Tool::QwenCode,
            Tool::CodexCli,
            Tool::CopilotCli,
            Tool::Other(std::borrow::Cow::Borrowed("custom-tool")),
        ] {
            let dc = ctx_for_tool(tool.clone(), "Foo (beta context)");
            assert_eq!(
                seg.render(&dc, &rc()).unwrap(),
                Some(RenderedSegment::new("Foo (beta context)").with_role(Role::Primary)),
                "tool {tool:?} should not be compacted"
            );
        }
    }

    #[test]
    fn full_preserves_anthropics_verbatim_string() {
        let extras = BTreeMap::from([("format".to_string(), toml::Value::String("full".into()))]);
        let seg = ModelSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(
            seg.render(&ctx("Opus 4.7 (1M context)"), &rc()).unwrap(),
            Some(RenderedSegment::new("Opus 4.7 (1M context)").with_role(Role::Primary))
        );
    }

    #[test]
    fn hidden_when_display_name_is_empty() {
        assert_eq!(
            ModelSegment::default().render(&ctx(""), &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn hidden_when_display_name_is_whitespace_only() {
        assert_eq!(
            ModelSegment::default().render(&ctx("   "), &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(ModelSegment::default().defaults().priority, PRIORITY);
    }

    #[test]
    fn from_extras_default_is_compact() {
        let seg = ModelSegment::from_extras(&BTreeMap::new(), &mut |_| {});
        assert_eq!(seg.cfg.format, ModelFormat::Compact);
    }

    #[test]
    fn from_extras_accepts_compact_value() {
        let extras =
            BTreeMap::from([("format".to_string(), toml::Value::String("compact".into()))]);
        let seg = ModelSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.format, ModelFormat::Compact);
    }

    #[test]
    fn from_extras_accepts_full_value() {
        let extras = BTreeMap::from([("format".to_string(), toml::Value::String("full".into()))]);
        let seg = ModelSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.format, ModelFormat::Full);
    }

    #[test]
    fn from_extras_warns_on_unknown_format_and_keeps_default() {
        let extras = BTreeMap::from([("format".to_string(), toml::Value::String("brief".into()))]);
        let mut warnings = vec![];
        let seg = ModelSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.format, ModelFormat::Compact);
        // Pin the namespaced warning prefix users grep for in stderr.
        assert!(warnings.iter().any(|w| w.contains("segments.model.format")));
    }

    #[test]
    fn from_extras_warns_on_non_string_format() {
        let extras = BTreeMap::from([("format".to_string(), toml::Value::Integer(1))]);
        let mut warnings = vec![];
        let seg = ModelSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.format, ModelFormat::Compact);
        assert!(warnings.iter().any(|w| w.contains("segments.model.format")));
    }

    #[test]
    fn shorten_context_label_returns_none_for_no_suffix() {
        assert_eq!(shorten_context_label("Sonnet 4.5"), None);
    }

    #[test]
    fn shorten_context_label_returns_none_when_paren_has_no_leading_space() {
        // `(1M context)` with no leading prose: `strip_suffix` succeeds
        // but `rfind(" (")` fails because the opening paren has no
        // space before it. Falls through to verbatim.
        assert_eq!(shorten_context_label("(1M context)"), None);
    }

    #[test]
    fn shorten_context_label_returns_none_for_bare_suffix() {
        // "context)" without any opening `" ("` at all → no transform.
        assert_eq!(shorten_context_label("context)"), None);
    }

    #[test]
    fn compact_picks_rightmost_paren_pair_with_multiple_parentheticals() {
        // Hypothetical `Opus 4.7 (preview) (1M context)`: only the
        // trailing `(... context)` is stripped; the earlier `(preview)`
        // stays. Pins the `rfind` (right-most-wins) choice against a
        // future `find` (left-most) refactor that would silently swap
        // the semantics.
        let seg = ModelSegment::default();
        assert_eq!(
            seg.render(&ctx("Opus 4.7 (preview) (1M context)"), &rc())
                .unwrap(),
            Some(RenderedSegment::new("Opus 4.7 (preview) (1M)").with_role(Role::Primary))
        );
    }
}
