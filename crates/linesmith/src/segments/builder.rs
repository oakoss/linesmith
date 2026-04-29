//! `Config` → `Vec<Box<dyn Segment>>` with validation. Hides built-in
//! registry lookup, duplicate handling, unknown-ID warnings,
//! per-segment override merging, and plugin-registry consultation.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rhai::{Array, Dynamic, Engine, Map};

use super::{
    built_in_by_id, OverriddenSegment, PowerlineWidth, Segment, Separator, WidthBounds,
    DEFAULT_SEGMENT_IDS,
};
use crate::config;
use crate::plugins::{CompiledPlugin, PluginRegistry, RhaiSegment};
use crate::theme;

/// Build the default segment list: every built-in in canonical order,
/// no overrides applied.
#[must_use]
pub fn build_default_segments() -> Vec<Box<dyn Segment>> {
    DEFAULT_SEGMENT_IDS
        .iter()
        .filter_map(|id| built_in_by_id(id, None, &mut |_| {}))
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

    let powerline_width = config
        .and_then(|c| c.layout_options.as_ref())
        .and_then(|lo| lo.powerline_width)
        .map(|w| validate_powerline_width(w, &mut warn))
        .unwrap_or_default();
    let layout_separator = config
        .and_then(|c| c.layout_options.as_ref())
        .and_then(|lo| lo.separator.as_deref())
        .map(|s| parse_layout_separator(s, powerline_width, &mut warn))
        .unwrap_or(Separator::Space);

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
            let extras = cfg_override.map(|ov| &ov.extra);
            let inner = if let Some(b) = built_in_by_id(id, extras, &mut warn) {
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
            let with_per_segment = apply_override(id, inner, cfg_override, &mut warn);
            Some(apply_layout_separator(with_per_segment, &layout_separator))
        })
        .collect()
}

/// Parse a `[layout_options].separator` string into a [`Separator`].
///
/// - `"space"` → [`Separator::Space`]
/// - `"powerline"` → [`Separator::Powerline`] (`powerline_width`
///   controls the chevron cell count, 1 or 2)
/// - `"capsule"` / `"flex"` — reserved for v0.2+; warn + fall back to
///   `Space` so today's configs migrate cleanly when the v0.2
///   renderers land
/// - `""` (truly empty) → [`Separator::None`]
/// - anything else → [`Separator::Literal`] verbatim, e.g.
///   `separator = " | "` for ccstatusline-parity
///
/// Reserved-keyword matching is case- and whitespace-insensitive
/// against the trimmed input. Typos do not warn — `"powereline"`
/// renders as the literal word, since "anything not a reserved
/// keyword is a literal" is the contract.
fn parse_layout_separator(
    value: &str,
    powerline_width: PowerlineWidth,
    warn: &mut impl FnMut(&str),
) -> Separator {
    // Empty (truly zero-length) is "no separator." Whitespace-only is
    // a deliberate literal — `separator = "   "` means "three spaces
    // between segments," not "no separator." Distinguishing these
    // here prevents the `value.trim()` keyword pre-pass from eating
    // user-meaningful whitespace literals.
    if value.is_empty() {
        return Separator::None;
    }
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "space" => Separator::Space,
        "powerline" => Separator::Powerline {
            width: powerline_width,
        },
        "capsule" | "flex" => {
            warn(&format!(
                "[layout_options].separator '{value}' is reserved for v0.2+; rendering as 'space'"
            ));
            Separator::Space
        }
        _ => Separator::Literal(std::borrow::Cow::Owned(value.to_string())),
    }
}

/// Validate `[layout_options].powerline_width`. Only `1` and `2` are
/// meaningful — most Nerd Fonts render U+E0B0 as 1 cell, some
/// fonts/sizes render it as 2. Any other value warns and falls back
/// to `1` so a typo doesn't silently desync the layout math.
fn validate_powerline_width(width: u16, warn: &mut impl FnMut(&str)) -> PowerlineWidth {
    match width {
        1 => PowerlineWidth::One,
        2 => PowerlineWidth::Two,
        other => {
            warn(&format!(
                "[layout_options].powerline_width = {other} is not 1 or 2; using 1"
            ));
            PowerlineWidth::One
        }
    }
}

/// Wrap the segment in [`OverriddenSegment`] so the configured
/// separator replaces its `Space`/`Theme` default. No-op when `sep`
/// is `Space` (the implicit default — nothing to override) or when
/// the segment's default is anything else (`Literal`, `None`,
/// `Powerline`) — that's an explicit segment-side choice we leave
/// alone.
fn apply_layout_separator(segment: Box<dyn Segment>, sep: &Separator) -> Box<dyn Segment> {
    if matches!(sep, Separator::Space) {
        return segment;
    }
    match segment.defaults().default_separator {
        Separator::Space | Separator::Theme => {
            Box::new(OverriddenSegment::new(segment).with_default_separator(sep.clone()))
        }
        _ => segment,
    }
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

    fn built_with_warns(cfg: Option<&config::Config>) -> (Vec<Box<dyn Segment>>, Vec<String>) {
        let mut warns = Vec::new();
        let segs = build_segments(cfg, None, |m| warns.push(m.to_string()));
        (segs, warns)
    }

    #[test]
    fn build_segments_uses_default_order_when_config_missing() {
        assert_eq!(built(None).len(), DEFAULT_SEGMENT_IDS.len());
    }

    #[test]
    fn layout_separator_powerline_swaps_default_separator() {
        // With `separator = "powerline"` configured, every segment
        // whose built-in default is `Space` or `Theme` reports
        // `Powerline` in `defaults().default_separator`. Pins the
        // wholesale-swap behavior the layout engine relies on to
        // emit chevrons between segments.
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model", "workspace"]
                [layout_options]
                separator = "powerline"
            "#,
        )
        .expect("parse");
        let segs = built(Some(&cfg));
        for seg in &segs {
            assert_eq!(
                seg.defaults().default_separator,
                Separator::powerline(),
                "segment didn't pick up powerline separator"
            );
        }
    }

    #[test]
    fn layout_separator_space_is_passthrough() {
        // Default `separator = "space"` (or absent) leaves segments
        // unwrapped — no extra OverriddenSegment layer for the
        // common case.
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [layout_options]
                separator = "space"
            "#,
        )
        .expect("parse");
        let segs = built(Some(&cfg));
        assert_eq!(segs[0].defaults().default_separator, Separator::Space);
    }

    #[test]
    fn layout_separator_capsule_warns_and_falls_back_to_space() {
        // Capsule + flex are spec'd for v0.2+; a config file written
        // today must not error on them. Warn loudly and treat as
        // space until the v0.2 renderers land.
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [layout_options]
                separator = "capsule"
            "#,
        )
        .expect("parse");
        let (segs, warns) = built_with_warns(Some(&cfg));
        assert_eq!(segs[0].defaults().default_separator, Separator::Space);
        assert!(
            warns
                .iter()
                .any(|m| m.contains("capsule") && m.contains("v0.2+")),
            "missing capsule deferral warning: {warns:?}"
        );
    }

    #[test]
    fn layout_separator_arbitrary_string_renders_as_literal() {
        // ccstatusline parity: configs like `separator = " | "` or
        // `separator = " · "` set the literal text emitted between
        // segments. Anything other than the reserved keywords falls
        // through to Separator::Literal preserving user input verbatim
        // (whitespace included).
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [layout_options]
                separator = " | "
            "#,
        )
        .expect("parse");
        let (segs, warns) = built_with_warns(Some(&cfg));
        assert_eq!(
            segs[0].defaults().default_separator,
            Separator::Literal(std::borrow::Cow::Owned(" | ".to_string()))
        );
        assert!(warns.is_empty(), "no warnings on literal: {warns:?}");
    }

    #[test]
    fn layout_separator_empty_string_yields_none() {
        // Explicit `separator = ""` is the user saying "no separator";
        // emit nothing between segments. Distinct from absence of the
        // key (which falls through to the default Space).
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [layout_options]
                separator = ""
            "#,
        )
        .expect("parse");
        let (segs, warns) = built_with_warns(Some(&cfg));
        assert_eq!(segs[0].defaults().default_separator, Separator::None);
        assert!(
            warns.is_empty(),
            "empty string is a valid choice: {warns:?}"
        );
    }

    #[test]
    fn build_segments_empty_config_falls_back_to_defaults() {
        let cfg = config::Config::default();
        assert_eq!(built(Some(&cfg)).len(), DEFAULT_SEGMENT_IDS.len());
    }

    #[test]
    fn layout_separator_preserves_segment_literal_default() {
        // Forward-compat pin: a segment whose built-in default is
        // `Literal(...)` keeps its declared boundary even under
        // `[layout_options].separator = "powerline"`. No segment uses
        // Literal today; this protects the contract for ones that will.
        struct PipeSeg;
        impl segments::Segment for PipeSeg {
            fn render(
                &self,
                _: &crate::data_context::DataContext,
                _: &segments::RenderContext,
            ) -> segments::RenderResult {
                Ok(Some(segments::RenderedSegment::new("x")))
            }
            fn defaults(&self) -> segments::SegmentDefaults {
                segments::SegmentDefaults::with_priority(0)
                    .with_default_separator(Separator::Literal(std::borrow::Cow::Borrowed(" | ")))
            }
        }
        let wrapped = apply_layout_separator(Box::new(PipeSeg), &Separator::powerline());
        assert_eq!(
            wrapped.defaults().default_separator,
            Separator::Literal(std::borrow::Cow::Borrowed(" | ")),
        );
    }

    #[test]
    fn layout_separator_preserves_segment_none_default() {
        // Same forward-compat pin for `Separator::None` — a segment
        // that explicitly suppresses its right-edge separator must
        // keep that suppression even when the user configures
        // powerline.
        struct NoSepSeg;
        impl segments::Segment for NoSepSeg {
            fn render(
                &self,
                _: &crate::data_context::DataContext,
                _: &segments::RenderContext,
            ) -> segments::RenderResult {
                Ok(Some(segments::RenderedSegment::new("x")))
            }
            fn defaults(&self) -> segments::SegmentDefaults {
                segments::SegmentDefaults::with_priority(0).with_default_separator(Separator::None)
            }
        }
        let wrapped = apply_layout_separator(Box::new(NoSepSeg), &Separator::powerline());
        assert_eq!(wrapped.defaults().default_separator, Separator::None);
    }

    #[test]
    fn layout_separator_does_not_double_wrap_when_default_already_powerline() {
        // A segment whose built-in default is already `Powerline`
        // falls through the `_` arm of the match — no wrap layer
        // added. Pins the contract for any future segment that
        // declares `Powerline` directly.
        struct PowerlineSeg;
        impl segments::Segment for PowerlineSeg {
            fn render(
                &self,
                _: &crate::data_context::DataContext,
                _: &segments::RenderContext,
            ) -> segments::RenderResult {
                Ok(Some(segments::RenderedSegment::new("x")))
            }
            fn defaults(&self) -> segments::SegmentDefaults {
                segments::SegmentDefaults::with_priority(0)
                    .with_default_separator(Separator::powerline())
            }
        }
        let wrapped = apply_layout_separator(Box::new(PowerlineSeg), &Separator::powerline());
        assert_eq!(wrapped.defaults().default_separator, Separator::powerline());
    }

    #[test]
    fn layout_separator_handles_mixed_case_and_whitespace() {
        // TOML doesn't normalize string values; users typing
        // `"Powerline"` or `" powerline "` shouldn't fall into the
        // unknown-value warn path. The keyword match runs against
        // `trim().to_ascii_lowercase()` of the (non-empty) input.
        let mut warns = Vec::new();
        let mut warn = |m: &str| warns.push(m.to_string());
        assert_eq!(
            parse_layout_separator("Powerline", PowerlineWidth::One, &mut warn),
            Separator::powerline()
        );
        assert_eq!(
            parse_layout_separator("  POWERLINE  ", PowerlineWidth::One, &mut warn),
            Separator::powerline()
        );
        assert_eq!(
            parse_layout_separator(" Space ", PowerlineWidth::One, &mut warn),
            Separator::Space
        );
        assert!(
            warns.is_empty(),
            "no warnings on case/whitespace: {warns:?}"
        );
    }

    #[test]
    fn layout_separator_whitespace_only_renders_as_literal() {
        // `value.trim()` would otherwise eat user-meaningful whitespace.
        // `separator = "   "` should produce a 3-space literal between
        // segments, not `Separator::None`.
        let mut warns = Vec::new();
        let mut warn = |m: &str| warns.push(m.to_string());
        assert_eq!(
            parse_layout_separator("   ", PowerlineWidth::One, &mut warn),
            Separator::Literal(std::borrow::Cow::Owned("   ".to_string()))
        );
        // Truly empty stays None.
        assert_eq!(
            parse_layout_separator("", PowerlineWidth::One, &mut warn),
            Separator::None
        );
        assert!(
            warns.is_empty(),
            "no warns on whitespace literal: {warns:?}"
        );
    }

    #[test]
    fn layout_separator_typo_renders_as_literal_not_warn() {
        // Pin the documented "typos don't warn" contract: the parser
        // doc explicitly promises `"powereline"` becomes a literal,
        // not a typo-detection warn. A future contributor adding
        // "did you mean?" detection would silently invert this; the
        // test forces a deliberate review.
        let mut warns = Vec::new();
        let mut warn = |m: &str| warns.push(m.to_string());
        assert_eq!(
            parse_layout_separator("powereline", PowerlineWidth::One, &mut warn),
            Separator::Literal(std::borrow::Cow::Owned("powereline".to_string()))
        );
        assert!(warns.is_empty(), "typos don't warn: {warns:?}");
    }

    #[test]
    fn layout_separator_powerline_overrides_runtime_right_separator() {
        // `apply_layout_separator` only swaps `default_separator`, but
        // `effective_separator()` prefers a per-render `right_separator`
        // set via `RenderedSegment::with_separator`. Plugin segments
        // that return `right_separator: Some(Space)` would otherwise
        // bypass the global layout-options separator; `OverriddenSegment`
        // therefore rewrites `right_separator` on render output too.
        struct RuntimeSpaceSeg;
        impl segments::Segment for RuntimeSpaceSeg {
            fn render(
                &self,
                _: &crate::data_context::DataContext,
                _: &segments::RenderContext,
            ) -> segments::RenderResult {
                Ok(Some(segments::RenderedSegment::with_separator(
                    "x",
                    Separator::Space,
                )))
            }
            fn defaults(&self) -> segments::SegmentDefaults {
                segments::SegmentDefaults::with_priority(0)
            }
        }
        let layout_sep = parse_layout_separator("powerline", PowerlineWidth::One, &mut |_| {});
        let wrapped = apply_layout_separator(Box::new(RuntimeSpaceSeg), &layout_sep);
        let rendered = wrapped
            .render(&stub_ctx(), &stub_rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(
            rendered.right_separator(),
            Some(&Separator::powerline()),
            "layout-options separator must override runtime Space"
        );
    }

    #[test]
    fn plugin_runtime_space_emits_chevron_through_render_with_warn() {
        // End-to-end pin for the original Codex regression. The
        // struct-level override test above proves the rendered struct
        // carries the right separator; this test follows that through
        // the full layout + emit pipeline. A future refactor that
        // bypasses `OverriddenSegment::render` (e.g., a fast path
        // pulling `defaults().default_separator` directly) would still
        // pass the struct-level test but emit a Space here.
        struct RuntimeSpaceSeg(&'static str);
        impl segments::Segment for RuntimeSpaceSeg {
            fn render(
                &self,
                _: &crate::data_context::DataContext,
                _: &segments::RenderContext,
            ) -> segments::RenderResult {
                Ok(Some(segments::RenderedSegment::with_separator(
                    self.0,
                    Separator::Space,
                )))
            }
            fn defaults(&self) -> segments::SegmentDefaults {
                segments::SegmentDefaults::with_priority(0)
            }
        }
        let layout_sep = parse_layout_separator("powerline", PowerlineWidth::One, &mut |_| {});
        let segs: Vec<Box<dyn segments::Segment>> = vec![
            apply_layout_separator(Box::new(RuntimeSpaceSeg("a")), &layout_sep),
            apply_layout_separator(Box::new(RuntimeSpaceSeg("b")), &layout_sep),
        ];
        let line = crate::layout::render_with_warn(
            &segs,
            &stub_ctx(),
            100,
            &mut |_| {},
            theme::default_theme(),
            theme::Capability::None,
            false,
        );
        assert!(line.contains(" \u{E0B0} "), "chevron in output: {line:?}");
        assert!(
            !line.contains("a b"),
            "Space should not survive between a and b: {line:?}"
        );
    }

    #[test]
    fn layout_separator_powerline_preserves_runtime_literal_right_separator() {
        // Companion pin to the override test above: a per-render
        // Literal `right_separator` is the segment saying "I picked
        // this exactly" — layout-options separator must NOT clobber.
        // Same Literal/None preservation as default_separator.
        struct RuntimePipeSeg;
        impl segments::Segment for RuntimePipeSeg {
            fn render(
                &self,
                _: &crate::data_context::DataContext,
                _: &segments::RenderContext,
            ) -> segments::RenderResult {
                Ok(Some(segments::RenderedSegment::with_separator(
                    "x",
                    Separator::Literal(std::borrow::Cow::Borrowed(" | ")),
                )))
            }
            fn defaults(&self) -> segments::SegmentDefaults {
                segments::SegmentDefaults::with_priority(0)
            }
        }
        let layout_sep = parse_layout_separator("powerline", PowerlineWidth::One, &mut |_| {});
        let wrapped = apply_layout_separator(Box::new(RuntimePipeSeg), &layout_sep);
        let rendered = wrapped
            .render(&stub_ctx(), &stub_rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(
            rendered.right_separator(),
            Some(&Separator::Literal(std::borrow::Cow::Borrowed(" | ")))
        );
    }

    fn stub_ctx() -> crate::data_context::DataContext {
        use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
        use std::path::PathBuf;
        use std::sync::Arc;
        crate::data_context::DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "X".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/r"),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    fn stub_rc() -> segments::RenderContext {
        segments::RenderContext::new(80)
    }

    #[test]
    fn layout_separator_pipe_literal_no_warning() {
        // Direct parser-level test for the `|` shorthand ccstatusline
        // users reach for. Pinned alongside the config-driven test
        // above so a regression that forces literal values through
        // the warn-path is caught at both layers.
        let mut warns = Vec::new();
        let mut warn = |m: &str| warns.push(m.to_string());
        assert_eq!(
            parse_layout_separator("|", PowerlineWidth::One, &mut warn),
            Separator::Literal(std::borrow::Cow::Owned("|".to_string()))
        );
        assert!(warns.is_empty(), "no warning on literal: {warns:?}");
    }

    #[test]
    fn layout_separator_single_space_renders_as_literal_not_keyword() {
        // `separator = " "` (one literal space, not the keyword) is a
        // user-visible distinction: the bypass at the top of the
        // parser short-circuits truly-empty inputs *before* trim, so
        // a single space falls into the literal arm. `Literal(" ")`
        // and `Space` render identically but flow through different
        // wrap policies in apply_layout_separator (Literal preserves
        // segment-side defaults; Space is a no-op).
        let mut warns = Vec::new();
        let mut warn = |m: &str| warns.push(m.to_string());
        assert_eq!(
            parse_layout_separator(" ", PowerlineWidth::One, &mut warn),
            Separator::Literal(std::borrow::Cow::Owned(" ".to_string()))
        );
        assert!(
            warns.is_empty(),
            "no warning on single-space literal: {warns:?}"
        );
    }

    #[test]
    fn apply_layout_separator_wraps_when_configured_literal_replaces_space_default() {
        // Configured `Literal(" | ")` against a segment whose default
        // is `Space` must wrap so the literal reaches the layout engine.
        struct SpaceDefaultSeg;
        impl segments::Segment for SpaceDefaultSeg {
            fn render(
                &self,
                _: &crate::data_context::DataContext,
                _: &segments::RenderContext,
            ) -> segments::RenderResult {
                Ok(Some(segments::RenderedSegment::new("x")))
            }
            fn defaults(&self) -> segments::SegmentDefaults {
                segments::SegmentDefaults::with_priority(0)
            }
        }
        let sep = Separator::Literal(std::borrow::Cow::Owned(" | ".to_string()));
        let wrapped = apply_layout_separator(Box::new(SpaceDefaultSeg), &sep);
        assert_eq!(wrapped.defaults().default_separator, sep);
    }

    #[test]
    fn apply_layout_separator_wraps_when_configured_none_replaces_space_default() {
        // Configured `None` (user typed `separator = ""`) against a
        // Space-default segment must wrap so the layout engine emits
        // no separator. The `if matches!(sep, Space)` early-return
        // guard at the top of apply_layout_separator must NOT
        // accidentally include None.
        struct SpaceDefaultSeg;
        impl segments::Segment for SpaceDefaultSeg {
            fn render(
                &self,
                _: &crate::data_context::DataContext,
                _: &segments::RenderContext,
            ) -> segments::RenderResult {
                Ok(Some(segments::RenderedSegment::new("x")))
            }
            fn defaults(&self) -> segments::SegmentDefaults {
                segments::SegmentDefaults::with_priority(0)
            }
        }
        let wrapped = apply_layout_separator(Box::new(SpaceDefaultSeg), &Separator::None);
        assert_eq!(wrapped.defaults().default_separator, Separator::None);
    }

    #[test]
    fn powerline_width_2_propagates_to_separator_variant() {
        // Pin the Codex-flagged correctness path: users on
        // 2-cell-rendering Nerd Fonts set
        // `[layout_options].powerline_width = 2`, and that width
        // reaches `Separator::Powerline { width }` so total_width()
        // charges 2 cells per chevron instead of undercounting by 1.
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [layout_options]
                separator = "powerline"
                powerline_width = 2
            "#,
        )
        .expect("parse");
        let segs = built(Some(&cfg));
        assert_eq!(
            segs[0].defaults().default_separator,
            Separator::Powerline {
                width: PowerlineWidth::Two,
            }
        );
    }

    #[test]
    fn powerline_width_default_is_1_when_unset() {
        // Absent `powerline_width` means 1 — the most-common Nerd Font
        // size + standard terminal combination.
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [layout_options]
                separator = "powerline"
            "#,
        )
        .expect("parse");
        let segs = built(Some(&cfg));
        assert_eq!(segs[0].defaults().default_separator, Separator::powerline(),);
    }

    #[test]
    fn powerline_width_invalid_warns_and_falls_back_to_1() {
        // A typo'd `powerline_width = 3` falls back to 1 with a
        // visible warning. Pins the validate-and-warn contract so a
        // future change can't silently accept arbitrary values.
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [layout_options]
                separator = "powerline"
                powerline_width = 3
            "#,
        )
        .expect("parse");
        let (segs, warns) = built_with_warns(Some(&cfg));
        assert_eq!(segs[0].defaults().default_separator, Separator::powerline());
        assert!(
            warns
                .iter()
                .any(|m| m.contains("powerline_width") && m.contains("3")),
            "missing invalid-width warning: {warns:?}"
        );
    }

    #[test]
    fn powerline_width_zero_warns_and_falls_back_to_1() {
        // Boundary: 0 is invalid. The `PowerlineWidth` enum makes this
        // unrepresentable downstream; the validator is the single
        // boundary that maps `u16` config inputs to the typed value.
        let mut warns = Vec::new();
        let mut warn = |m: &str| warns.push(m.to_string());
        assert_eq!(validate_powerline_width(0, &mut warn), PowerlineWidth::One);
        assert!(
            warns
                .iter()
                .any(|m| m.contains("powerline_width") && m.contains('0')),
            "missing zero warning: {warns:?}"
        );
    }

    #[test]
    fn powerline_width_max_warns_and_falls_back_to_1() {
        // Boundary: u16::MAX is invalid. The `PowerlineWidth` enum
        // means downstream layout math can't see this value at all —
        // the validator is the only thing standing between user input
        // and a typed cell count.
        let mut warns = Vec::new();
        let mut warn = |m: &str| warns.push(m.to_string());
        assert_eq!(
            validate_powerline_width(u16::MAX, &mut warn),
            PowerlineWidth::One
        );
        assert!(
            warns.iter().any(|m| m.contains("powerline_width")),
            "missing max-width warning: {warns:?}"
        );
    }

    #[test]
    fn layout_separator_absent_section_resolves_to_space() {
        // Config present but no `[layout_options]` section at all.
        // Threads through the `unwrap_or(Separator::Space)` chain in
        // build_segments — the failure mode would be a panic on the
        // `and_then` / `as_deref` hops.
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
            "#,
        )
        .expect("parse");
        let segs = built(Some(&cfg));
        assert_eq!(segs[0].defaults().default_separator, Separator::Space);
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
        fn render(
            &self,
            _: &crate::data_context::DataContext,
            _: &segments::RenderContext,
        ) -> segments::RenderResult {
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

    fn rc() -> crate::segments::RenderContext {
        crate::segments::RenderContext::new(80)
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
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
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
            .render(&model_ctx("Claude Sonnet 4.6"), &rc())
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
            .render(&model_ctx("Claude Sonnet 4.6"), &rc())
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
            .render(&model_ctx("Claude Sonnet 4.6"), &rc())
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
            .render(&model_ctx("Claude Sonnet 4.6"), &rc())
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
            .render(&model_ctx("Claude Sonnet 4.6"), &rc())
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
            .render(&dc, &rc())
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
        let rendered = built[0]
            .render(&dc, &rc())
            .expect("render ok")
            .expect("visible");
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
