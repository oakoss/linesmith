//! `Config` → `Vec<Box<dyn Segment>>` with validation. Hides built-in
//! registry lookup, duplicate handling, unknown-ID warnings, and
//! per-segment override merging.

use super::{built_in_by_id, OverriddenSegment, Segment, WidthBounds, DEFAULT_SEGMENT_IDS};
use crate::config;
use crate::theme;

/// Build the default segment list: every built-in in canonical order,
/// no overrides applied.
#[must_use]
pub fn build_default_segments() -> Vec<Box<dyn Segment>> {
    DEFAULT_SEGMENT_IDS
        .iter()
        .filter_map(|id| built_in_by_id(id))
        .collect()
}

/// Build a segment list from an optional [`Config`](config::Config).
/// `None` or a config without a `[line]` section uses the default
/// order. `warn` receives a one-line diagnostic for each validation
/// rule triggered (pass `|_| {}` to discard).
///
/// Implements the validation rules in `docs/specs/config.md`
/// §Validation rules: unknown ids skip with a warning, duplicates
/// keep the first, an explicit `segments = []` warns, inverted width
/// bounds drop the override with a warning.
pub fn build_segments(
    config: Option<&config::Config>,
    mut warn: impl FnMut(&str),
) -> Vec<Box<dyn Segment>> {
    let configured_line = config.and_then(|c| c.line.as_ref());
    if let Some(line) = configured_line {
        if line.segments.is_empty() {
            warn("[line].segments is empty; no segments will render");
        }
    }

    let ids: Vec<&str> = match configured_line {
        Some(l) => l.segments.iter().map(String::as_str).collect(),
        None => DEFAULT_SEGMENT_IDS.to_vec(),
    };

    let mut seen = std::collections::HashSet::<&str>::new();
    ids.into_iter()
        .filter_map(|id| {
            if !seen.insert(id) {
                warn(&format!(
                    "segment '{id}' listed more than once; keeping first occurrence"
                ));
                return None;
            }
            let inner = built_in_by_id(id).or_else(|| {
                warn(&format!("unknown segment id '{id}' — skipping"));
                None
            })?;
            let cfg_override = config.and_then(|c| c.segments.get(id));
            Some(apply_override(id, inner, cfg_override, &mut warn))
        })
        .collect()
}

fn apply_override(
    id: &str,
    inner: Box<dyn Segment>,
    ov: Option<&config::SegmentOverride>,
    warn: &mut impl FnMut(&str),
) -> Box<dyn Segment> {
    let Some(ov) = ov else { return inner };
    let base_width = inner.defaults().width;
    let mut wrapped = OverriddenSegment::new(inner);
    if let Some(p) = ov.priority {
        wrapped = wrapped.with_priority(p);
    }
    if let Some(w) = ov.width {
        // Half-specified widths inherit the missing side from the
        // segment's built-in default; 0 / u16::MAX are the open-ended
        // fallback only when the segment itself has no default.
        let min = w.min.or_else(|| base_width.map(|b| b.min())).unwrap_or(0);
        let max = w
            .max
            .or_else(|| base_width.map(|b| b.max()))
            .unwrap_or(u16::MAX);
        match WidthBounds::new(min, max) {
            Some(bounds) => wrapped = wrapped.with_width(bounds),
            None => warn(&format!(
                "segments.{id}.width: min ({min}) > max ({max}); ignoring override"
            )),
        }
    }
    // `style = ""` is a no-op — an empty string almost never means
    // "strip my declared role"; require an explicit token to override.
    if let Some(style_str) = ov.style.as_deref().filter(|s| !s.trim().is_empty()) {
        match theme::parse_style(style_str) {
            Ok(style) => wrapped = wrapped.with_user_style(style),
            Err(e) => warn(&format!("segments.{id}.style: {e}; ignoring override")),
        }
    }
    Box::new(wrapped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input;
    use crate::segments;
    use std::str::FromStr;

    fn built(cfg: Option<&config::Config>) -> Vec<Box<dyn Segment>> {
        build_segments(cfg, |_| {})
    }

    #[test]
    fn build_segments_uses_default_order_when_config_missing() {
        assert_eq!(built(None).len(), DEFAULT_SEGMENT_IDS.len());
    }

    #[test]
    fn build_segments_empty_config_falls_back_to_defaults() {
        let cfg = config::Config::default();
        assert_eq!(built(Some(&cfg)).len(), DEFAULT_SEGMENT_IDS.len());
    }

    #[test]
    fn build_segments_uses_configured_line_order() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["workspace", "model"]
            "#,
        )
        .expect("parse");
        let got = built(Some(&cfg));
        // Compare by default priority since we can't name-check dyn
        // Segments directly.
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].defaults().priority, 16); // workspace
        assert_eq!(got[1].defaults().priority, 64); // model
    }

    #[test]
    fn build_segments_applies_priority_override() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [segments.model]
                priority = 0
            "#,
        )
        .expect("parse");
        let got = built(Some(&cfg));
        assert_eq!(got[0].defaults().priority, 0);
    }

    #[test]
    fn build_segments_applies_width_override() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["workspace"]
                [segments.workspace.width]
                min = 5
                max = 30
            "#,
        )
        .expect("parse");
        let got = built(Some(&cfg));
        let bounds = got[0].defaults().width.expect("width set");
        assert_eq!(bounds.min(), 5);
        assert_eq!(bounds.max(), 30);
    }

    #[test]
    fn build_segments_skips_unknown_ids_and_warns() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model", "does_not_exist", "workspace"]
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let got = build_segments(Some(&cfg), |msg| warnings.push(msg.to_string()));
        assert_eq!(got.len(), 2);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("does_not_exist"));
    }

    #[test]
    fn build_segments_dedupes_duplicates_with_warning() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model", "model", "workspace"]
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let got = build_segments(Some(&cfg), |msg| warnings.push(msg.to_string()));
        assert_eq!(got.len(), 2); // one model, one workspace
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("model"));
        assert!(warnings[0].contains("more than once"));
    }

    #[test]
    fn build_segments_warns_on_explicitly_empty_segment_list() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = []
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let got = build_segments(Some(&cfg), |msg| warnings.push(msg.to_string()));
        assert!(got.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("empty"));
    }

    #[test]
    fn build_segments_warns_on_inverted_width_bounds() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["workspace"]
                [segments.workspace.width]
                min = 40
                max = 10
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let got = build_segments(Some(&cfg), |msg| warnings.push(msg.to_string()));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].defaults().width, None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("min"));
        assert!(warnings[0].contains("max"));
    }

    // Stub with a baseline `width` default pins `apply_override`'s
    // inherit-from-inner branches. No built-in segment exercises
    // these today, so the contract needs an explicit guard.
    struct StubWithWidth;

    impl Segment for StubWithWidth {
        fn render(&self, _: &crate::data_context::DataContext) -> segments::RenderResult {
            Ok(Some(segments::RenderedSegment::new("x")))
        }
        fn defaults(&self) -> segments::SegmentDefaults {
            segments::SegmentDefaults::with_priority(128)
                .with_width(WidthBounds::new(10, 50).expect("valid"))
        }
    }

    fn merge_width(min: Option<u16>, max: Option<u16>) -> WidthBounds {
        let ov = config::SegmentOverride {
            priority: None,
            width: Some(config::WidthBoundsConfig { min, max }),
            style: None,
        };
        let wrapped = apply_override("stub", Box::new(StubWithWidth), Some(&ov), &mut |_| {});
        wrapped.defaults().width.expect("width preserved")
    }

    #[test]
    fn width_merge_min_only_inherits_max_from_inner_default() {
        let got = merge_width(Some(5), None);
        assert_eq!(got.min(), 5);
        assert_eq!(got.max(), 50);
    }

    #[test]
    fn width_merge_max_only_inherits_min_from_inner_default() {
        let got = merge_width(None, Some(80));
        assert_eq!(got.min(), 10);
        assert_eq!(got.max(), 80);
    }

    #[test]
    fn width_merge_both_sides_override_inner_default() {
        let got = merge_width(Some(3), Some(40));
        assert_eq!(got.min(), 3);
        assert_eq!(got.max(), 40);
    }

    #[test]
    fn width_merge_empty_override_keeps_inner_default() {
        // An empty [segments.<id>.width] table still appears as
        // `Some(WidthBoundsConfig { min: None, max: None })`; the
        // merged bounds must equal the inner's default.
        let got = merge_width(None, None);
        assert_eq!(got.min(), 10);
        assert_eq!(got.max(), 50);
    }

    fn model_ctx(display_name: &str) -> crate::data_context::DataContext {
        use crate::input::{ModelInfo, Tool, WorkspaceInfo};
        use std::path::PathBuf;
        use std::sync::Arc;
        crate::data_context::DataContext::new(input::StatusContext {
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
            rate_limits: None,
            effort: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    #[test]
    fn style_override_replaces_segment_declared_style_at_render_time() {
        use crate::theme::{Color, Role};
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [segments.model]
                style = "role:accent bold italic"
            "#,
        )
        .expect("parse");
        let built = build_segments(Some(&cfg), |_| {});
        let rendered = built[0]
            .render(&model_ctx("Claude Sonnet 4.6"))
            .expect("render ok")
            .expect("visible");
        assert_eq!(rendered.style.role, Some(Role::Accent));
        assert_eq!(rendered.style.fg, None::<Color>);
        assert!(rendered.style.bold);
        assert!(rendered.style.italic);
        assert!(!rendered.style.underline);
        assert!(!rendered.style.dim);
    }

    #[test]
    fn style_override_with_explicit_fg_populates_fg_slot() {
        use crate::theme::Color;
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [segments.model]
                style = "fg:#ff0000 underline"
            "#,
        )
        .expect("parse");
        let built = build_segments(Some(&cfg), |_| {});
        let rendered = built[0]
            .render(&model_ctx("Claude Sonnet 4.6"))
            .expect("render ok")
            .expect("visible");
        assert_eq!(
            rendered.style.fg,
            Some(Color::TrueColor { r: 255, g: 0, b: 0 })
        );
        assert!(rendered.style.underline);
    }

    #[test]
    fn invalid_style_string_warns_and_leaves_segment_style_unchanged() {
        use crate::theme::Role;
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [segments.model]
                style = "role:mauve"
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let built = build_segments(Some(&cfg), |m| warnings.push(m.to_string()));
        let rendered = built[0]
            .render(&model_ctx("Claude Sonnet 4.6"))
            .expect("render ok")
            .expect("visible");
        assert_eq!(rendered.style.role, Some(Role::Primary));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("segments.model.style"));
        assert!(warnings[0].contains("mauve"));
        assert!(warnings[0].contains("ignoring"));
    }

    #[test]
    fn empty_style_string_is_noop_and_preserves_segment_declared_style() {
        use crate::theme::Role;
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [segments.model]
                style = ""
            "#,
        )
        .expect("parse");
        let built = build_segments(Some(&cfg), |_| {});
        let rendered = built[0]
            .render(&model_ctx("Claude Sonnet 4.6"))
            .expect("render ok")
            .expect("visible");
        assert_eq!(rendered.style.role, Some(Role::Primary));
    }

    #[test]
    fn whitespace_only_style_string_is_noop_and_preserves_segment_declared_style() {
        use crate::theme::Role;
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [segments.model]
                style = "   "
            "#,
        )
        .expect("parse");
        let built = build_segments(Some(&cfg), |_| {});
        let rendered = built[0]
            .render(&model_ctx("Claude Sonnet 4.6"))
            .expect("render ok")
            .expect("visible");
        assert_eq!(rendered.style.role, Some(Role::Primary));
    }

    #[test]
    fn build_segments_forward_compat_keys_dont_break_parsing() {
        let cfg = config::Config::from_str(
            r#"
                theme = "catppuccin-mocha"
                preset = "developer"
                layout = "single-line"
                [line]
                segments = ["model"]
                [layout_options]
                separator = "powerline"
            "#,
        )
        .expect("parse");
        assert_eq!(built(Some(&cfg)).len(), 1);
    }
}
