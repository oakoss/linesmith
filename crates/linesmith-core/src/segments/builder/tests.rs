use super::dispatch::*;
use super::layout::*;
use super::plugins::*;

use std::collections::BTreeMap;
use std::str::FromStr;

use linesmith_plugin::PluginRegistry;

use crate::segments::{
    self, built_in_by_id, PowerlineWidth, Segment, Separator, WidthBounds, BUILT_IN_SEGMENT_IDS,
    DEFAULT_SEGMENT_IDS,
};
use crate::{config, input, theme};

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
        model: Some(ModelInfo {
            display_name: "X".into(),
        }),
        workspace: Some(WorkspaceInfo {
            project_dir: PathBuf::from("/r"),
            git_worktree: None,
        }),
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
        model: Some(ModelInfo {
            display_name: display_name.into(),
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
    let registry = PluginRegistry::load_with_xdg(
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
fn build_segments_falls_back_to_first_line_for_multi_line_configs() {
    // Embedders on the single-line API calling `build_segments`
    // against a `layout = "multi-line"` config (e.g. the
    // `power-user` preset) need to render something rather than
    // a blank status line. Pin both the rendered segments and
    // the warning text so the fallback doesn't silently regress.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.1]
            segments = ["model", "workspace"]
            [line.2]
            segments = ["context_window", "cost"]
        "#,
    )
    .expect("parse");
    let (segs, warns) = built_with_warns(Some(&cfg));
    assert_eq!(
        segs.len(),
        2,
        "expected line 1's two segments, got {} segs",
        segs.len()
    );
    let actual: Vec<u8> = segs.iter().map(|s| s.defaults().priority).collect();
    assert_eq!(actual, priorities_for(&["model", "workspace"]));
    assert!(
        warns
            .iter()
            .any(|w| w.contains("multi-line") && w.contains("build_lines")),
        "expected migration hint pointing at build_lines, got: {warns:?}"
    );
}

#[test]
fn build_lines_plugin_referenced_in_two_lines_warns_specifically_on_second() {
    // v0.1 limitation: the shared plugin lookup is consumed on
    // first use, so a plugin id repeated on a later line can't
    // render. The diagnostic must say "rendered on an earlier
    // line" (not the generic "unknown segment id") so users
    // understand the cause is reuse, not a typo. Pin the
    // specific text so wording drift is caught here rather than
    // by a confused user.
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
    let registry = PluginRegistry::load_with_xdg(
        &[tmp.path().to_path_buf()],
        None,
        &engine,
        BUILT_IN_SEGMENT_IDS,
    );
    assert!(registry.load_errors().is_empty());

    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.1]
            segments = ["my_plugin", "model"]
            [line.2]
            segments = ["my_plugin", "workspace"]
        "#,
    )
    .expect("parse");
    let mut warns: Vec<String> = Vec::new();
    let lines = build_lines(Some(&cfg), Some((registry, engine)), |m| {
        warns.push(m.to_string())
    });

    // Line 1 has plugin + model; line 2 only workspace (plugin
    // reuse skipped).
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].len(), 2, "line 1 keeps plugin + model");
    assert_eq!(
        lines[1].len(),
        1,
        "line 2 drops the reused plugin, keeps workspace"
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("'my_plugin'") && w.contains("rendered on an earlier line")),
        "expected specific cross-line plugin warning, got: {warns:?}"
    );
    assert!(
        !warns
            .iter()
            .any(|w| w.contains("unknown segment id 'my_plugin'")),
        "should NOT use the generic 'unknown segment id' text for cross-line reuse, got: {warns:?}"
    );
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
    let registry = PluginRegistry::load_with_xdg(
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
    let registry = PluginRegistry::load_with_xdg(
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
    let registry = PluginRegistry::load_with_xdg(&[tmp.path().to_path_buf()], None, &engine, &[]);

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

// --- build_lines (multi-line layout) ---

fn lines(cfg: Option<&config::Config>) -> Vec<Vec<Box<dyn Segment>>> {
    build_lines(cfg, None, |_| {})
}

fn lines_with_warns(cfg: Option<&config::Config>) -> (Vec<Vec<Box<dyn Segment>>>, Vec<String>) {
    let mut warns = Vec::new();
    let result = build_lines(cfg, None, |m| warns.push(m.to_string()));
    (result, warns)
}

/// Compare ids-per-line by mapping each id to its built-in's
/// declared priority. Segments don't expose `id()` through the
/// trait, so the existing tests use `defaults().priority` as the
/// identity signal. Caller passes the expected id list per line;
/// helper looks up the priority for each and returns that as the
/// comparable shape.
fn priorities_for(ids: &[&str]) -> Vec<u8> {
    ids.iter()
        .map(|id| {
            built_in_by_id(id, None, &mut |_| {})
                .unwrap_or_else(|| panic!("unknown built-in id in test fixture: {id}"))
                .defaults()
                .priority
        })
        .collect()
}

fn priorities_per_line(built: &[Vec<Box<dyn Segment>>]) -> Vec<Vec<u8>> {
    built
        .iter()
        .map(|line| line.iter().map(|s| s.defaults().priority).collect())
        .collect()
}

#[test]
fn build_lines_single_line_default_returns_one_line_with_default_segments() {
    // No config = implicit single-line with default segment list.
    let result = lines(None);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].len(), DEFAULT_SEGMENT_IDS.len());
}

#[test]
fn build_lines_explicit_single_line_returns_one_line_from_segments() {
    let cfg = config::Config::from_str(
        r#"
            layout = "single-line"
            [line]
            segments = ["model", "workspace"]
        "#,
    )
    .expect("parse");
    let result = lines(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model", "workspace"])]
    );
}

#[test]
fn build_lines_multi_line_returns_one_inner_vec_per_numbered_table() {
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.1]
            segments = ["model", "context_window"]
            [line.2]
            segments = ["workspace", "cost"]
        "#,
    )
    .expect("parse");
    let result = lines(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![
            priorities_for(&["model", "context_window"]),
            priorities_for(&["workspace", "cost"]),
        ]
    );
}

#[test]
fn build_lines_multi_line_sorts_by_parsed_integer_not_lexicographic() {
    // BTreeMap key order is lexicographic on strings, so "10" sorts
    // before "2". The builder must parse keys as u32 and sort
    // numerically; otherwise [line.10] would render before
    // [line.2] and quietly break user expectations.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.2]
            segments = ["workspace"]
            [line.10]
            segments = ["context_window"]
            [line.1]
            segments = ["model"]
        "#,
    )
    .expect("parse");
    let result = lines(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![
            priorities_for(&["model"]),
            priorities_for(&["workspace"]),
            priorities_for(&["context_window"]),
        ]
    );
}

#[test]
fn build_lines_multi_line_with_no_numbered_tables_falls_back_to_single_line() {
    // Spec edge case: layout="multi-line" without [line.N] warns
    // and uses [line].segments. Without this fallback a typo in
    // `layout` would silently render nothing.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line]
            segments = ["model", "workspace"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model", "workspace"])]
    );
    assert!(
        warns.iter().any(|w| w.contains("no usable [line.N]")),
        "expected fallback warning, got: {warns:?}"
    );
}

#[test]
fn build_lines_single_line_with_numbered_tables_warns_and_ignores_them() {
    // Spec edge case: layout="single-line" + [line.N] present
    // logs and uses [line].segments. Numbered tables are dropped
    // (not silently rendered), so a user mid-migration sees the
    // warning before `linesmith --check-config` would.
    let cfg = config::Config::from_str(
        r#"
            layout = "single-line"
            [line]
            segments = ["model"]
            [line.1]
            segments = ["workspace"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model"])]
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("single-line") && w.contains("[line.N]")),
        "expected mode-mismatch warning, got: {warns:?}"
    );
}

#[test]
fn build_lines_default_layout_with_numbered_tables_warns_and_ignores_them() {
    // No `layout =` field defaults to single-line; numbered tables
    // present should be flagged the same way as explicit single-line.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [line.1]
            segments = ["workspace"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model"])]
    );
    assert!(
        warns.iter().any(|w| w.contains("[line.N]")),
        "expected mode-mismatch warning, got: {warns:?}"
    );
}

#[test]
fn build_lines_promotes_to_multi_line_when_layout_unset_and_segments_empty() {
    // CX-2-B: a user who defines [line.1]/[line.2] but forgets
    // `layout = "multi-line"` AND leaves [line].segments empty
    // clearly meant multi-line. Auto-promote with a hint to add
    // the missing key, rather than silently rendering blank.
    let cfg = config::Config::from_str(
        r#"
            [line.1]
            segments = ["model"]
            [line.2]
            segments = ["workspace"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model"]), priorities_for(&["workspace"]),],
        "must render both numbered lines, not a blank single-line"
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("treating as multi-line") && w.contains("layout")),
        "expected auto-promote hint, got: {warns:?}"
    );
}

#[test]
fn build_lines_does_not_promote_when_segments_populated() {
    // The auto-promote only fires when [line].segments is EMPTY.
    // A populated segments list signals "the user picked single-
    // line on purpose"; numbered tables are noise to be warned
    // about and dropped (existing behavior, must not regress).
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [line.1]
            segments = ["workspace"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model"])],
        "must render single-line `[line].segments`, not promote"
    );
    assert!(
        warns.iter().any(|w| w.contains("ignoring numbered tables")),
        "expected the existing 'ignoring' warning, not the promote hint, got: {warns:?}"
    );
    assert!(
        !warns.iter().any(|w| w.contains("treating as multi-line")),
        "must NOT auto-promote when segments is populated, got: {warns:?}"
    );
}

#[test]
fn build_lines_unknown_scalar_key_under_line_warns_and_drops() {
    // CX-2-A part 2 (validation side): a typo'd scalar key like
    // [line] segmnts = [...] flows through the flatten map as a
    // toml::Value::Array. The builder must warn and drop rather
    // than crash.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line]
            segmnts = ["model"]
            [line.1]
            segments = ["workspace"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["workspace"])]
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("unknown key 'segmnts'") && w.contains("array")),
        "expected unknown-key warning naming the key + type, got: {warns:?}"
    );
}

#[test]
fn build_lines_consumed_plugins_threads_across_three_or_more_lines() {
    // pr-test-analyzer pass-2: the 2-line cross-line plugin test
    // doesn't catch a regression that resets the consumed set
    // after line 2. Three lines, plugin reused on every line:
    // line 1 renders, lines 2 AND 3 emit the specific "rendered
    // on an earlier line" warning. Workspace fills the rest.
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
    let registry = PluginRegistry::load_with_xdg(
        &[tmp.path().to_path_buf()],
        None,
        &engine,
        BUILT_IN_SEGMENT_IDS,
    );

    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.1]
            segments = ["my_plugin", "model"]
            [line.2]
            segments = ["my_plugin", "workspace"]
            [line.3]
            segments = ["my_plugin", "context_window"]
        "#,
    )
    .expect("parse");
    let mut warns: Vec<String> = Vec::new();
    let lines = build_lines(Some(&cfg), Some((registry, engine)), |m| {
        warns.push(m.to_string())
    });

    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].len(), 2, "line 1: plugin + model");
    assert_eq!(lines[1].len(), 1, "line 2: plugin dropped, only workspace");
    assert_eq!(
        lines[2].len(),
        1,
        "line 3: plugin dropped, only context_window"
    );
    let cross_line_warns = warns
        .iter()
        .filter(|w| w.contains("rendered on an earlier line"))
        .count();
    assert_eq!(
        cross_line_warns, 2,
        "expected exactly two cross-line warnings (lines 2 + 3), got {cross_line_warns}: {warns:?}"
    );
}

#[test]
fn build_segments_falls_back_to_line_one_even_when_top_segments_populated() {
    // pr-test-analyzer pass-2: the multi-line fallback for the
    // single-line API must use [line.1].segments, not [line]
    // .segments, even when both are populated. The numbered
    // tables' precedence-wins rule applies here too; without
    // this test, a future "merge" refactor could silently flip
    // the precedence and embedders would render the wrong line.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line]
            segments = ["cost"]
            [line.1]
            segments = ["model"]
        "#,
    )
    .expect("parse");
    let (segs, _warns) = built_with_warns(Some(&cfg));
    let actual: Vec<u8> = segs.iter().map(|s| s.defaults().priority).collect();
    assert_eq!(
        actual,
        priorities_for(&["model"]),
        "fallback must use [line.1].segments, not the top-level [line].segments"
    );
}

#[test]
fn build_segments_multi_line_with_only_invalid_numbered_keys_falls_through_to_single_line() {
    // pr-test-analyzer pass-2: when the multi-line branch finds
    // no usable [line.N], `validated_numbered_lines` returns
    // None and we fall through to the single-line render path
    // (which warns on empty segments). Pin both warnings so a
    // refactor that swaps the None/empty handling order doesn't
    // silently swallow one.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.foo]
            segments = ["bogus"]
        "#,
    )
    .expect("parse");
    let (segs, warns) = built_with_warns(Some(&cfg));
    assert!(segs.is_empty(), "no usable line means no segments rendered");
    assert!(
        warns
            .iter()
            .any(|w| w.contains("[line.foo]") && w.contains("not a positive integer")),
        "must warn about the dropped non-numeric key, got: {warns:?}"
    );
    assert!(
        warns.iter().any(|w| w.contains("[line].segments is empty")),
        "must warn that the fallback finds nothing to render, got: {warns:?}"
    );
}

#[test]
fn build_lines_multi_line_drops_non_numeric_keys_with_warning() {
    // [line.foo] is structurally valid TOML (parser accepts it)
    // but semantically junk for multi-line ordering. Drop with a
    // warning rather than silently sorting it somewhere arbitrary.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.1]
            segments = ["model"]
            [line.foo]
            segments = ["bogus"]
            [line.2]
            segments = ["workspace"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model"]), priorities_for(&["workspace"])]
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("[line.foo]") && w.contains("not a positive integer")),
        "expected non-numeric-key warning, got: {warns:?}"
    );
}

#[test]
fn build_lines_multi_line_drops_zero_and_negative_keys() {
    // Positive integers only: `0` fails the `n > 0` guard and
    // `-1` fails the `u32::from_str` parse. Pin both so the
    // predicate doesn't drift to "any u32 including 0."
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.0]
            segments = ["context_window"]
            [line.1]
            segments = ["model"]
            [line."-1"]
            segments = ["cost"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model"])]
    );
    assert!(warns.iter().any(|w| w.contains("[line.0]")));
    assert!(warns.iter().any(|w| w.contains("[line.-1]")));
}

#[test]
fn build_lines_multi_line_with_only_invalid_keys_falls_back_to_single_line() {
    // If every numbered key is invalid, the validator returns None
    // and the multi-line path falls back to single-line rendering
    // of [line].segments. Two warnings: one per invalid key, plus
    // the "no usable [line.N]" fallback notice.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line]
            segments = ["model"]
            [line.foo]
            segments = ["bogus"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model"])]
    );
    assert!(warns.iter().any(|w| w.contains("[line.foo]")));
    assert!(warns.iter().any(|w| w.contains("no usable [line.N]")));
}

#[test]
fn build_lines_multi_line_warns_per_empty_numbered_segments() {
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.1]
            segments = ["model"]
            [line.2]
            segments = []
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model"]), Vec::<u8>::new()]
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("[line.2].segments is empty")),
        "expected empty-segments warning for line 2, got: {warns:?}"
    );
}

#[test]
fn build_lines_multi_line_ignores_top_level_segments_when_numbered_present() {
    // Spec precedence (edge case #3): in multi-line mode, when
    // both [line].segments and [line.N] exist, the numbered tables
    // win. Single-line callers go through `build_segments`
    // directly; build_lines doesn't double-render the fallback.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line]
            segments = ["workspace"]
            [line.1]
            segments = ["model"]
        "#,
    )
    .expect("parse");
    let result = lines(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![priorities_for(&["model"])]
    );
}

#[test]
fn build_lines_multi_line_dedupes_within_line_but_not_across_lines() {
    // The dedup rule (warn + skip) applies within a single line.
    // The same built-in id can appear in multiple lines because
    // each render call is independent state.
    let cfg = config::Config::from_str(
        r#"
            layout = "multi-line"
            [line.1]
            segments = ["model", "model", "workspace"]
            [line.2]
            segments = ["model"]
        "#,
    )
    .expect("parse");
    let (result, warns) = lines_with_warns(Some(&cfg));
    assert_eq!(
        priorities_per_line(&result),
        vec![
            priorities_for(&["model", "workspace"]),
            priorities_for(&["model"]),
        ]
    );
    // Exactly one dedup warning (from line 1), not two.
    let dedup_warns: Vec<_> = warns
        .iter()
        .filter(|w| w.contains("listed more than once"))
        .collect();
    assert_eq!(
        dedup_warns.len(),
        1,
        "expected one dedup warning, got: {warns:?}"
    );
}
