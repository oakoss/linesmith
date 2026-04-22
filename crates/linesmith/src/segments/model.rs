//! Model segment: renders the current model's display name.

use super::{RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub struct ModelSegment;

/// Between context_window (32) and rate_limit (96): identity matters for
/// multi-model sessions but isn't time-sensitive like the health metrics.
const PRIORITY: u8 = 64;

impl Segment for ModelSegment {
    fn render(&self, ctx: &DataContext) -> RenderResult {
        let name = ctx.status.model.display_name.trim();
        if name.is_empty() {
            return Ok(None);
        }
        Ok(Some(RenderedSegment::new(name).with_role(Role::Primary)))
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

    fn ctx(display_name: &str) -> DataContext {
        DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
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
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    #[test]
    fn renders_display_name_with_primary_role() {
        assert_eq!(
            ModelSegment.render(&ctx("Claude Sonnet 4.6")).unwrap(),
            Some(RenderedSegment::new("Claude Sonnet 4.6").with_role(Role::Primary))
        );
    }

    #[test]
    fn hidden_when_display_name_is_empty() {
        assert_eq!(ModelSegment.render(&ctx("")).unwrap(), None);
    }

    #[test]
    fn hidden_when_display_name_is_whitespace_only() {
        assert_eq!(ModelSegment.render(&ctx("   ")).unwrap(), None);
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(ModelSegment.defaults().priority, PRIORITY);
    }
}
