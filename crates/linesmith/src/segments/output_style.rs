//! Output style segment: renders Claude Code's active `output_style.name`
//! (e.g. "default", "concise", "explanatory"). Hidden when the payload
//! doesn't carry it. Opt-in: not in the default segment list.

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub struct OutputStyleSegment;

/// Informational; drops before cost (192), kept past rate-limit (96).
const PRIORITY: u8 = 160;

impl Segment for OutputStyleSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(style) = ctx.status.output_style.as_ref() else {
            crate::lsm_debug!("output_style: status.output_style absent; hiding");
            return Ok(None);
        };
        Ok(Some(
            RenderedSegment::new(style.name.as_str()).with_role(Role::Muted),
        ))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ModelInfo, OutputStyle, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx(output_style: Option<OutputStyle>) -> DataContext {
        DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "X".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style,
            agent_name: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    #[test]
    fn renders_named_style() {
        for name in ["default", "concise", "explanatory", "custom-flavor"] {
            assert_eq!(
                OutputStyleSegment
                    .render(
                        &ctx(Some(OutputStyle {
                            name: name.to_string(),
                        })),
                        &rc(),
                    )
                    .unwrap(),
                Some(RenderedSegment::new(name).with_role(Role::Muted)),
                "for name={name}"
            );
        }
    }

    #[test]
    fn hidden_when_absent() {
        assert_eq!(OutputStyleSegment.render(&ctx(None), &rc()).unwrap(), None);
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(OutputStyleSegment.defaults().priority, PRIORITY);
    }
}
