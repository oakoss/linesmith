//! Vim segment: renders Claude Code's active vim mode (normal, insert,
//! visual, command, replace). Hidden when the payload doesn't carry
//! `vim`. Opt-in: not in the default segment list.

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub struct VimSegment;

/// Informational; drops before cost (192), kept past rate-limit (96).
const PRIORITY: u8 = 160;

impl Segment for VimSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(mode) = ctx.status.vim else {
            crate::lsm_debug!("vim: status.vim absent; hiding");
            return Ok(None);
        };
        Ok(Some(
            RenderedSegment::new(mode.as_str()).with_role(Role::Muted),
        ))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ModelInfo, StatusContext, Tool, VimMode, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx(vim: Option<VimMode>) -> DataContext {
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
            vim,
            output_style: None,
            agent_name: None,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    #[test]
    fn renders_each_mode() {
        for (mode, expected) in [
            (VimMode::Normal, "normal"),
            (VimMode::Insert, "insert"),
            (VimMode::Visual, "visual"),
            (VimMode::Command, "command"),
            (VimMode::Replace, "replace"),
        ] {
            assert_eq!(
                VimSegment.render(&ctx(Some(mode)), &rc()).unwrap(),
                Some(RenderedSegment::new(expected).with_role(Role::Muted))
            );
        }
    }

    #[test]
    fn hidden_when_absent() {
        assert_eq!(VimSegment.render(&ctx(None), &rc()).unwrap(), None);
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(VimSegment.defaults().priority, PRIORITY);
    }
}
