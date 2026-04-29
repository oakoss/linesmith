//! Version segment: renders Claude Code's CLI version from the
//! top-level stdin `version` field. Hidden when the payload doesn't
//! carry it. Opt-in: not in the default segment list.

use std::collections::BTreeMap;

use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::theme::Role;

pub const ID: &str = "version";

/// Informational; drops before cost (192), kept past rate-limit (96).
const PRIORITY: u8 = 160;

const DEFAULT_PREFIX: &str = "v";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) prefix: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            prefix: DEFAULT_PREFIX.to_string(),
        }
    }
}

#[derive(Default)]
pub struct VersionSegment {
    cfg: Config,
}

impl VersionSegment {
    /// Build from `[segments.version]` extras. Reads `prefix` (string,
    /// default `"v"`) — text to prepend to the version. Set to `""` for
    /// raw output.
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut cfg = Config::default();
        if let Some(v) = extras.get("prefix") {
            match v.as_str() {
                Some(s) => cfg.prefix = s.to_string(),
                None => warn(&format!(
                    "segments.{ID}.prefix: expected a string; ignoring"
                )),
            }
        }
        Self { cfg }
    }
}

impl Segment for VersionSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(v) = ctx.status.version.as_deref() else {
            crate::lsm_debug!("version: status.version absent; hiding");
            return Ok(None);
        };
        let text = if self.cfg.prefix.is_empty() {
            v.to_string()
        } else {
            format!("{}{v}", self.cfg.prefix)
        };
        Ok(Some(RenderedSegment::new(text).with_role(Role::Muted)))
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

    fn ctx(version: Option<String>) -> DataContext {
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
            agent_name: None,
            version,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    #[test]
    fn renders_with_default_v_prefix() {
        assert_eq!(
            VersionSegment::default()
                .render(&ctx(Some("2.1.90".into())), &rc())
                .unwrap(),
            Some(RenderedSegment::new("v2.1.90").with_role(Role::Muted))
        );
    }

    #[test]
    fn empty_prefix_emits_raw_version() {
        // `prefix = ""` is the documented opt-out for the default `v`.
        // Pin it so a future "non-empty prefix" guard doesn't silently
        // turn empty back into the default.
        let mut extras = BTreeMap::new();
        extras.insert("prefix".to_string(), toml::Value::String(String::new()));
        let seg = VersionSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(
            seg.render(&ctx(Some("2.1.90".into())), &rc()).unwrap(),
            Some(RenderedSegment::new("2.1.90").with_role(Role::Muted))
        );
    }

    #[test]
    fn custom_prefix_passes_through() {
        let mut extras = BTreeMap::new();
        extras.insert("prefix".to_string(), toml::Value::String("CC ".to_string()));
        let seg = VersionSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(
            seg.render(&ctx(Some("2.1.90".into())), &rc()).unwrap(),
            Some(RenderedSegment::new("CC 2.1.90").with_role(Role::Muted))
        );
    }

    #[test]
    fn hidden_when_absent() {
        assert_eq!(
            VersionSegment::default().render(&ctx(None), &rc()).unwrap(),
            None
        );
    }

    #[test]
    fn from_extras_warns_on_non_string_prefix_and_keeps_default() {
        let mut extras = BTreeMap::new();
        extras.insert("prefix".to_string(), toml::Value::Integer(1));
        let mut warnings = Vec::new();
        let seg = VersionSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.prefix, DEFAULT_PREFIX);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("segments.version.prefix"));
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(VersionSegment::default().defaults().priority, PRIORITY);
    }
}
