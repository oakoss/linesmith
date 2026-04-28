//! Effort segment: renders the current `/effort` level. Hidden when the
//! payload doesn't carry it.
//!
//! Claude Code does not currently re-emit the effort level when the user
//! runs `/effort max` or similar
//! ([ccstatusline#239](https://github.com/sirmalloc/ccstatusline/issues/239)).
//! Until that ships upstream, this segment will be hidden in practice.
//! Segment is shipped now so it lights up automatically when the payload
//! arrives.

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub struct EffortSegment;

/// Informational; drops before cost (192), kept past rate-limit (96).
const PRIORITY: u8 = 160;

impl Segment for EffortSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(effort) = ctx.status.effort else {
            crate::lsm_debug!("effort: status.effort absent; hiding");
            return Ok(None);
        };
        Ok(Some(
            RenderedSegment::new(effort.as_str()).with_role(Role::Muted),
        ))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{EffortLevel, ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx(effort: Option<EffortLevel>) -> DataContext {
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
            effort,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    #[test]
    fn renders_each_level() {
        for (level, expected) in [
            (EffortLevel::Low, "low"),
            (EffortLevel::Medium, "medium"),
            (EffortLevel::High, "high"),
            (EffortLevel::Max, "max"),
            (EffortLevel::XHigh, "xhigh"),
        ] {
            assert_eq!(
                EffortSegment.render(&ctx(Some(level)), &rc()).unwrap(),
                Some(RenderedSegment::new(expected).with_role(Role::Muted))
            );
        }
    }

    #[test]
    fn hidden_when_effort_absent() {
        assert_eq!(EffortSegment.render(&ctx(None), &rc()).unwrap(), None);
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(EffortSegment.defaults().priority, PRIORITY);
    }
}
