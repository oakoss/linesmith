//! Context window segment: renders `{used}% · {size}` where size is
//! formatted in thousands (`200k`, `1M`). Hidden when the payload
//! doesn't carry context-window data.

use super::{RenderedSegment, Segment};
use crate::input::{ContextWindow, StatusContext};

pub struct ContextWindowSegment;

impl Segment for ContextWindowSegment {
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment> {
        let cw = ctx.context_window.as_ref()?;
        Some(RenderedSegment::new(format!(
            "{pct:.0}% · {size}",
            pct = cw.used.value(),
            size = format_size(cw),
        )))
    }
}

fn format_size(cw: &ContextWindow) -> String {
    let size = cw.size;
    if size >= 1_000_000 && size % 1_000_000 == 0 {
        format!("{}M", size / 1_000_000)
    } else if size >= 1_000 && size % 1_000 == 0 {
        format!("{}k", size / 1_000)
    } else {
        size.to_string()
    }
}

#[cfg(test)]
fn format_size_for(size: u64) -> String {
    format_size(&ContextWindow {
        used: crate::input::Percent::new(0.0).expect("in range"),
        size,
        total_input_tokens: 0,
        total_output_tokens: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ContextWindow, ModelInfo, Percent, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx(window: Option<ContextWindow>) -> StatusContext {
        StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "X".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            },
            context_window: window,
            cost: None,
            rate_limits: None,
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
        }
    }

    fn window(used: f32, size: u64) -> ContextWindow {
        ContextWindow {
            used: Percent::new(used).expect("in range"),
            size,
            total_input_tokens: 0,
            total_output_tokens: 0,
        }
    }

    #[test]
    fn renders_percent_and_sonnet_200k() {
        assert_eq!(
            ContextWindowSegment.render(&ctx(Some(window(42.3, 200_000)))),
            Some(RenderedSegment::new("42% · 200k"))
        );
    }

    #[test]
    fn renders_one_million_size_as_m_suffix() {
        assert_eq!(
            ContextWindowSegment.render(&ctx(Some(window(5.0, 1_000_000)))),
            Some(RenderedSegment::new("5% · 1M"))
        );
    }

    #[test]
    fn renders_non_round_size_literally() {
        assert_eq!(
            ContextWindowSegment.render(&ctx(Some(window(10.0, 131_072)))),
            Some(RenderedSegment::new("10% · 131072"))
        );
    }

    #[test]
    fn rounds_percent_to_nearest_integer() {
        assert_eq!(
            ContextWindowSegment.render(&ctx(Some(window(99.9, 200_000)))),
            Some(RenderedSegment::new("100% · 200k"))
        );
    }

    #[test]
    fn hidden_when_context_window_absent() {
        assert_eq!(ContextWindowSegment.render(&ctx(None)), None);
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
}
