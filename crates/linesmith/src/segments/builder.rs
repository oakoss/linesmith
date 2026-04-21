//! `Config` → `Vec<Box<dyn Segment>>` with validation. Hides built-in
//! registry lookup, duplicate handling, unknown-ID warnings,
//! per-segment override merging, and plugin-registry consultation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rhai::{Array, Dynamic, Engine, Map};

use super::{built_in_by_id, OverriddenSegment, Segment, WidthBounds, DEFAULT_SEGMENT_IDS};
use crate::config;
use crate::plugins::{CompiledPlugin, PluginRegistry, RhaiSegment};
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
/// `plugins` carries the discovered [`PluginRegistry`] plus its
/// shared engine. Built-in ids win on collision (the registry already
/// rejects plugins shadowing built-ins at load time, so a plugin
/// reaching this function can only collide with another plugin or
/// stand alone).
///
/// Implements the validation rules in `docs/specs/config.md`
/// §Validation rules: unknown ids skip with a warning, duplicates
/// keep the first, an explicit `segments = []` warns, inverted width
/// bounds drop the override with a warning.
pub fn build_segments(
    config: Option<&config::Config>,
    plugins: Option<(PluginRegistry, Arc<Engine>)>,
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

    // Bundle the lookup table with its engine so the borrow checker
    // (rather than a runtime invariant + `expect`) enforces that
    // plugin renders never reach for a missing engine.
    let mut plugin_bundle: Option<(HashMap<String, CompiledPlugin>, Arc<Engine>)> =
        plugins.map(|(registry, engine)| {
            let lookup: HashMap<String, CompiledPlugin> = registry
                .into_plugins()
                .into_iter()
                .map(|p| (p.id().to_string(), p))
                .collect();
            (lookup, engine)
        });

    let mut seen = std::collections::HashSet::<String>::new();
    ids.into_iter()
        .filter_map(|id| {
            if !seen.insert(id.to_string()) {
                warn(&format!(
                    "segment '{id}' listed more than once; keeping first occurrence"
                ));
                return None;
            }
            let cfg_override = config.and_then(|c| c.segments.get(id));
            let inner = if let Some(b) = built_in_by_id(id) {
                Some(b)
            } else if let Some((lookup, engine)) = plugin_bundle.as_mut() {
                lookup.remove(id).map(|plugin| {
                    // Always pass a Map (possibly empty) rather than
                    // `()` so plugins can probe `ctx.config.foo`
                    // without first checking the parent for unit.
                    let plugin_config = cfg_override.map_or_else(
                        || Dynamic::from_map(Map::new()),
                        |ov| toml_table_to_dynamic(&ov.extra),
                    );
                    Box::new(RhaiSegment::from_compiled(
                        plugin,
                        engine.clone(),
                        plugin_config,
                    )) as Box<dyn Segment>
                })
            } else {
                None
            };
            let inner = inner.or_else(|| {
                warn(&format!("unknown segment id '{id}' — skipping"));
                None
            })?;
            Some(apply_override(id, inner, cfg_override, &mut warn))
        })
        .collect()
}

/// Convert the `extra` bag of a `[segments.<plugin-id>]` block into
/// the `rhai::Map` a plugin sees as `ctx.config`.
fn toml_table_to_dynamic(table: &BTreeMap<String, toml::Value>) -> Dynamic {
    let mut map = Map::new();
    for (k, v) in table {
        map.insert(k.as_str().into(), toml_value_to_dynamic(v));
    }
    Dynamic::from_map(map)
}

fn toml_value_to_dynamic(value: &toml::Value) -> Dynamic {
    match value {
        toml::Value::String(s) => Dynamic::from(s.clone()),
        toml::Value::Integer(i) => Dynamic::from(*i),
        toml::Value::Float(f) => Dynamic::from(*f),
        toml::Value::Boolean(b) => Dynamic::from(*b),
        // toml::Datetime has no native rhai equivalent; surface as the
        // RFC 3339 string the spec already uses for time fields.
        toml::Value::Datetime(dt) => Dynamic::from(dt.to_string()),
        toml::Value::Array(items) => {
            let arr: Array = items.iter().map(toml_value_to_dynamic).collect();
            Dynamic::from_array(arr)
        }
        toml::Value::Table(t) => {
            let mut m = Map::new();
            for (k, v) in t {
                m.insert(k.as_str().into(), toml_value_to_dynamic(v));
            }
            Dynamic::from_map(m)
        }
    }
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
    use crate::segments::{self, BUILT_IN_SEGMENT_IDS};
    use std::str::FromStr;

    fn built(cfg: Option<&config::Config>) -> Vec<Box<dyn Segment>> {
        build_segments(cfg, None, |_| {})
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
        let got = build_segments(Some(&cfg), None, |msg| warnings.push(msg.to_string()));
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
        let got = build_segments(Some(&cfg), None, |msg| warnings.push(msg.to_string()));
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
        let got = build_segments(Some(&cfg), None, |msg| warnings.push(msg.to_string()));
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
        let got = build_segments(Some(&cfg), None, |msg| warnings.push(msg.to_string()));
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
            extra: BTreeMap::new(),
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
        let built = build_segments(Some(&cfg), None, |_| {});
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
        let built = build_segments(Some(&cfg), None, |_| {});
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
        let built = build_segments(Some(&cfg), None, |m| warnings.push(m.to_string()));
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
        let built = build_segments(Some(&cfg), None, |_| {});
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
        let built = build_segments(Some(&cfg), None, |_| {});
        let rendered = built[0]
            .render(&model_ctx("Claude Sonnet 4.6"))
            .expect("render ok")
            .expect("visible");
        assert_eq!(rendered.style.role, Some(Role::Primary));
    }

    // --- plugin integration ----------------------------------------

    fn write_plugin(dir: &std::path::Path, name: &str, src: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, src).expect("write plugin");
        p
    }

    #[test]
    fn plugin_id_resolves_through_build_segments() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "p.rhai",
            r#"
            const ID = "my_plugin";
            fn render(ctx) { #{ runs: [#{ text: "from-plugin" }] } }
            "#,
        );
        let engine = crate::plugins::build_engine();
        let registry = crate::plugins::PluginRegistry::load_with_xdg(
            &[tmp.path().to_path_buf()],
            None,
            &engine,
            BUILT_IN_SEGMENT_IDS,
        );
        assert!(
            registry.load_errors().is_empty(),
            "load errors: {:?}",
            registry.load_errors()
        );

        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model", "my_plugin"]
            "#,
        )
        .expect("parse");
        let built = build_segments(Some(&cfg), Some((registry, engine)), |_| {});
        assert_eq!(built.len(), 2);
        // Order matches `[line].segments`: built-in `model` first,
        // plugin `my_plugin` second. `model` defaults to priority 64;
        // a plugin with no override defaults to the trait's 128.
        assert_eq!(built[0].defaults().priority, 64);
        assert_eq!(built[1].defaults().priority, 128);
        // The plugin's render emits a known string — pin it so a
        // wiring regression that swaps slots fails loudly.
        let dc = model_ctx("Sonnet");
        let plugin_render = built[1]
            .render(&dc)
            .expect("plugin render ok")
            .expect("visible");
        assert_eq!(plugin_render.text(), "from-plugin");
    }

    #[test]
    fn unknown_id_with_plugin_registry_still_warns() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "p.rhai",
            r#"
            const ID = "loaded";
            fn render(ctx) { () }
            "#,
        );
        let engine = crate::plugins::build_engine();
        let registry = crate::plugins::PluginRegistry::load_with_xdg(
            &[tmp.path().to_path_buf()],
            None,
            &engine,
            BUILT_IN_SEGMENT_IDS,
        );

        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["loaded", "missing_plugin"]
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let built = build_segments(Some(&cfg), Some((registry, engine)), |m| {
            warnings.push(m.to_string())
        });
        assert_eq!(built.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("missing_plugin"));
    }

    #[test]
    fn plugin_receives_extra_keys_from_segments_table_as_ctx_config() {
        // Pins the TOML → SegmentOverride.extra → toml_table_to_dynamic
        // → ctx.config round-trip. Without it, the wiring could
        // silently regress to passing Dynamic::UNIT and a plugin
        // reading `ctx.config.foo` would crash at render time.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "labelled.rhai",
            r#"
            const ID = "labelled";
            fn render(ctx) {
                #{ runs: [#{ text: ctx.config.label }] }
            }
            "#,
        );
        let engine = crate::plugins::build_engine();
        let registry = crate::plugins::PluginRegistry::load_with_xdg(
            &[tmp.path().to_path_buf()],
            None,
            &engine,
            BUILT_IN_SEGMENT_IDS,
        );

        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["labelled"]
                [segments.labelled]
                label = "from-toml"
            "#,
        )
        .expect("parse");
        let built = build_segments(Some(&cfg), Some((registry, engine)), |_| {});
        assert_eq!(built.len(), 1);
        let dc = model_ctx("Sonnet");
        let rendered = built[0].render(&dc).expect("render ok").expect("visible");
        assert_eq!(rendered.text(), "from-toml");
    }

    #[test]
    fn built_in_id_wins_over_plugin_with_same_id() {
        // The registry's load-time check normally rejects a plugin
        // whose `const ID` shadows a built-in. This test smuggles
        // such a plugin past the registry (empty built-in list at
        // load time) and configures the colliding id in `[line]`,
        // then asserts `build_segments` still picks the built-in.
        // Locks the belt-and-suspenders precedence in case the
        // registry-level check ever regresses.
        let tmp = tempfile::TempDir::new().expect("tempdir");
        write_plugin(
            tmp.path(),
            "ghost.rhai",
            r#"
            const ID = "model";
            fn render(_) { #{ runs: [#{ text: "from-plugin" }] } }
            "#,
        );
        let engine = crate::plugins::build_engine();
        let registry = crate::plugins::PluginRegistry::load_with_xdg(
            &[tmp.path().to_path_buf()],
            None,
            &engine,
            &[],
        );

        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
            "#,
        )
        .expect("parse");
        let built = build_segments(Some(&cfg), Some((registry, engine)), |_| {});
        // The built-in model segment uses display_name from ctx; a
        // text comparison wouldn't be stable across changes there.
        // Priority 64 belongs to the built-in; the plugin would have
        // the trait default of 128.
        assert_eq!(built.len(), 1);
        assert_eq!(built[0].defaults().priority, 64);
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
