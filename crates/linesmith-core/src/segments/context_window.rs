//! Context window segment: renders `{used}% · {size}` where size is
//! formatted in thousands (`200k`, `1M`). Hidden when the payload
//! doesn't carry context-window data.

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub struct ContextWindowSegment;

/// Just above workspace (16): the context-window percentage is the
/// health metric that silently breaks sessions when it hits 100%, so it
/// should outlive everything except orientation.
const PRIORITY: u8 = 32;

impl Segment for ContextWindowSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(cw) = ctx.status.context_window.as_ref() else {
            crate::lsm_debug!("context_window: status.context_window absent; hiding");
            return Ok(None);
        };
        // Per ADR-0014, leaves degrade independently. Render whatever
        // we have: full `42% · 200k` when both populate, `200k` alone
        // during the pre-first-API-call window where `used` is null
        // but `size` is known (orientation outweighs hiding). Hide
        // when neither is available — there's nothing to say.
        let text = match (cw.used, cw.size) {
            (Some(used), Some(size)) => format!("{:.0}% · {}", used.value(), format_size(size)),
            (None, Some(size)) => format_size(size),
            (Some(used), None) => format!("{:.0}%", used.value()),
            (None, None) => {
                crate::lsm_debug!("context_window: used and size both null; hiding");
                return Ok(None);
            }
        };
        Ok(Some(RenderedSegment::new(text).with_role(Role::Info)))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

fn format_size(size: u32) -> String {
    if size >= 1_000_000 && size.is_multiple_of(1_000_000) {
        format!("{}M", size / 1_000_000)
    } else if size >= 1_000 && size.is_multiple_of(1_000) {
        format!("{}k", size / 1_000)
    } else {
        size.to_string()
    }
}

#[cfg(test)]
fn format_size_for(size: u32) -> String {
    format_size(size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ContextWindow, ModelInfo, Percent, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx(window: Option<ContextWindow>) -> DataContext {
        DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: Some(ModelInfo {
                display_name: "X".into(),
            }),
            workspace: Some(WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            }),
            context_window: window,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    fn window(used: f32, size: u32) -> ContextWindow {
        ContextWindow {
            used: Some(Percent::new(used).expect("in range")),
            size: Some(size),
            total_input_tokens: Some(0),
            total_output_tokens: Some(0),
            current_usage: None,
        }
    }

    #[test]
    fn renders_percent_and_sonnet_200k() {
        assert_eq!(
            ContextWindowSegment
                .render(&ctx(Some(window(42.3, 200_000))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("42% · 200k").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_one_million_size_as_m_suffix() {
        assert_eq!(
            ContextWindowSegment
                .render(&ctx(Some(window(5.0, 1_000_000))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("5% · 1M").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_non_round_size_literally() {
        assert_eq!(
            ContextWindowSegment
                .render(&ctx(Some(window(10.0, 131_072))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("10% · 131072").with_role(Role::Info))
        );
    }

    #[test]
    fn rounds_percent_to_nearest_integer() {
        assert_eq!(
            ContextWindowSegment
                .render(&ctx(Some(window(99.9, 200_000))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("100% · 200k").with_role(Role::Info))
        );
    }

    #[test]
    fn hidden_when_context_window_absent() {
        assert_eq!(
            ContextWindowSegment.render(&ctx(None), &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn renders_size_only_when_used_is_null() {
        // ADR-0014 partial-render goal: pre-first-API-call payloads
        // have used_percentage null but size populated. Show the
        // size so orientation survives the ~15s startup window.
        let cw = ContextWindow {
            used: None,
            size: Some(200_000),
            total_input_tokens: Some(0),
            total_output_tokens: Some(0),
            current_usage: None,
        };
        assert_eq!(
            ContextWindowSegment.render(&ctx(Some(cw)), &rc()).unwrap(),
            Some(RenderedSegment::new("200k").with_role(Role::Info))
        );
    }

    #[test]
    fn renders_percent_only_when_size_is_null() {
        // Symmetric partial: hypothetical schema-drift case where
        // used_percentage survives but context_window_size doesn't.
        let cw = ContextWindow {
            used: Some(Percent::new(42.0).expect("in range")),
            size: None,
            total_input_tokens: Some(0),
            total_output_tokens: Some(0),
            current_usage: None,
        };
        assert_eq!(
            ContextWindowSegment.render(&ctx(Some(cw)), &rc()).unwrap(),
            Some(RenderedSegment::new("42%").with_role(Role::Info))
        );
    }

    #[test]
    fn hidden_when_both_used_and_size_null() {
        let cw = ContextWindow {
            used: None,
            size: None,
            total_input_tokens: None,
            total_output_tokens: None,
            current_usage: None,
        };
        assert_eq!(
            ContextWindowSegment.render(&ctx(Some(cw)), &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn format_size_round_values_use_k_suffix() {
        assert_eq!(format_size_for(1_000), "1k");
        assert_eq!(format_size_for(200_000), "200k");
    }

    #[test]
    fn format_size_round_millions_use_m_suffix() {
        assert_eq!(format_size_for(1_000_000), "1M");
        assert_eq!(format_size_for(2_000_000), "2M");
    }

    #[test]
    fn format_size_non_round_values_rendered_literally() {
        assert_eq!(format_size_for(999), "999");
        assert_eq!(format_size_for(131_072), "131072");
        // 1.5M is not a round million, so the 'k' branch catches it.
        assert_eq!(format_size_for(1_500_000), "1500k");
    }

    #[test]
    fn format_size_zero_renders_literally() {
        // `0 % 1_000_000 == 0` and `0 >= 1_000_000` is false; same for
        // the k-branch. Falls through to literal "0" — guard against
        // a future refactor that flips the order or drops the >= check.
        assert_eq!(format_size_for(0), "0");
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(ContextWindowSegment.defaults().priority, PRIORITY);
    }
}
