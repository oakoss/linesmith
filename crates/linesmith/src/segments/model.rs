//! Model segment: renders the current model's display name.

use super::{RenderedSegment, Segment};
use crate::input::StatusContext;

pub struct ModelSegment;

impl Segment for ModelSegment {
    fn render(&self, ctx: &StatusContext) -> Option<RenderedSegment> {
        let name = ctx.model.display_name.trim();
        if name.is_empty() {
            return None;
        }
        Some(RenderedSegment::new(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn ctx(display_name: &str) -> StatusContext {
        StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: display_name.into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            },
            context_window: None,
            raw: Arc::new(serde_json::Value::Null),
        }
    }

    #[test]
    fn renders_display_name() {
        assert_eq!(
            ModelSegment.render(&ctx("Claude Sonnet 4.6")),
            Some(RenderedSegment::new("Claude Sonnet 4.6"))
        );
    }

    #[test]
    fn hidden_when_display_name_is_empty() {
        assert_eq!(ModelSegment.render(&ctx("")), None);
    }

    #[test]
    fn hidden_when_display_name_is_whitespace_only() {
        assert_eq!(ModelSegment.render(&ctx("   ")), None);
    }
}
