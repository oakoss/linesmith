//! Agent segment: renders Claude Code's active sub-agent name from
//! `agent.name`. Hidden when the payload doesn't carry it. Opt-in: not
//! in the default segment list.

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub struct AgentSegment;

/// Informational; drops before cost (192), kept past rate-limit (96).
const PRIORITY: u8 = 160;

impl Segment for AgentSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(name) = ctx.status.agent_name.as_deref() else {
            crate::lsm_debug!("agent: status.agent_name absent; hiding");
            return Ok(None);
        };
        Ok(Some(RenderedSegment::new(name).with_role(Role::Muted)))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
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

    fn ctx(agent_name: Option<String>) -> DataContext {
        DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: Some(ModelInfo {
                display_name: "X".into(),
            }),
            workspace: Some(WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            }),
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    #[test]
    fn renders_active_agent() {
        assert_eq!(
            AgentSegment
                .render(&ctx(Some("research".into())), &rc())
                .unwrap(),
            Some(RenderedSegment::new("research").with_role(Role::Muted))
        );
    }

    #[test]
    fn hidden_when_absent() {
        assert_eq!(AgentSegment.render(&ctx(None), &rc()).unwrap(), None);
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(AgentSegment.defaults().priority, PRIORITY);
    }
}
