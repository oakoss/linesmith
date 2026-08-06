use super::dispatch::*;
use super::layout::*;
use super::plugins::*;

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::str::FromStr;

use linesmith_plugin::PluginRegistry;

use crate::segments::{
    self, built_in_by_id, LineItem, PowerlineWidth, Segment, Separator, WidthBounds,
    BUILT_IN_SEGMENT_IDS, DEFAULT_SEGMENT_IDS,
};
use crate::{config, input, theme};

fn built(cfg: Option<&config::Config>) -> Vec<LineItem> {
    build_segments(cfg, None, |_| {})
}

fn built_with_warns(cfg: Option<&config::Config>) -> (Vec<LineItem>, Vec<String>) {
    let mut warns = Vec::new();
    let items = build_segments(cfg, None, |m| warns.push(m.to_string()));
    (items, warns)
}

/// Number of `LineItem::Segment` slots. Tests that previously
/// asserted on `built(...).len()` use this to ignore the inline
/// separators the builder now interleaves.
fn segment_count(items: &[LineItem]) -> usize {
    items
        .iter()
        .filter(|i| matches!(i, LineItem::Segment { .. }))
        .count()
}

/// The Nth `LineItem::Segment` (skipping separators). Panics with
/// a descriptive message when there are fewer than `n + 1` segments.
#[track_caller]
fn nth_segment(items: &[LineItem], n: usize) -> &dyn Segment {
    items
        .iter()
        .filter_map(|i| match i {
            LineItem::Segment { segment, .. } => Some(segment.as_ref()),
            LineItem::Separator(_) => None,
        })
        .nth(n)
        .unwrap_or_else(|| {
            panic!(
                "expected at least {} segments, got {}",
                n + 1,
                segment_count(items)
            )
        })
}

/// Resolved global separator the builder laid down between adjacent
/// segments. Returns the first inline `LineItem::Separator`'s value;
/// returns `None` for lines with fewer than two segments. All inline
/// separators in a single-line build are equal (the resolved
/// `[layout_options].separator`), so checking the first is sufficient.
fn first_inline_separator(items: &[LineItem]) -> Option<&Separator> {
    items.iter().find_map(|i| match i {
        LineItem::Separator(s) => Some(s),
        LineItem::Segment { .. } => None,
    })
}

/// Build a config with two segments and the supplied `[layout_options]
/// .separator` value, then return the inline separator the builder
/// laid down between them. Compresses the boilerplate of the
/// `layout_separator_*` round-trip tests.
fn resolve_inline_separator(separator_toml_value: &str) -> (Separator, Vec<String>) {
    let cfg = config::Config::from_str(&format!(
        r#"
            [line]
            segments = ["model", "workspace"]
            [layout_options]
            separator = {separator_toml_value}
        "#,
    ))
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    let sep = first_inline_separator(&items)
        .expect("two-segment line must have one inline separator")
        .clone();
    (sep, warns)
}

#[test]
fn build_segments_uses_default_order_when_config_missing() {
    assert_eq!(segment_count(&built(None)), DEFAULT_SEGMENT_IDS.len());
}

#[test]
fn layout_separator_powerline_lays_chevrons_between_segments() {
    // With `separator = "powerline"` configured, the builder
    // interleaves `LineItem::Separator(Powerline)` between every
    // adjacent pair of segments — that's the only signal the
    // layout engine needs to emit chevrons.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", "workspace"]
            [layout_options]
            separator = "powerline"
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(
        first_inline_separator(&items),
        Some(&Separator::powerline()),
    );
}

#[test]
fn layout_separator_space_is_default() {
    // `separator = "space"` (or the implicit default when the key
    // is absent) lays down `Separator::Space` between segments.
    let (sep, warns) = resolve_inline_separator("\"space\"");
    assert_eq!(sep, Separator::Space);
    assert!(warns.is_empty());
}

#[test]
fn layout_separator_capsule_warns_and_falls_back_to_space() {
    // Capsule + flex are spec'd for v0.2+; a config file written
    // today must not error on them. Warn loudly and treat as
    // space until the v0.2 renderers land.
    let (sep, warns) = resolve_inline_separator("\"capsule\"");
    assert_eq!(sep, Separator::Space);
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
    let (sep, warns) = resolve_inline_separator("\" | \"");
    assert_eq!(
        sep,
        Separator::Literal(std::borrow::Cow::Owned(" | ".to_string()))
    );
    assert!(warns.is_empty(), "no warnings on literal: {warns:?}");
}

#[test]
fn layout_separator_empty_string_yields_none() {
    // Explicit `separator = ""` is the user saying "no separator";
    // emit nothing between segments. Distinct from absence of the
    // key (which falls through to the default Space).
    let (sep, warns) = resolve_inline_separator("\"\"");
    assert_eq!(sep, Separator::None);
    assert!(
        warns.is_empty(),
        "empty string is a valid choice: {warns:?}"
    );
}

#[test]
fn build_segments_empty_config_falls_back_to_defaults() {
    let cfg = config::Config::default();
    assert_eq!(segment_count(&built(Some(&cfg))), DEFAULT_SEGMENT_IDS.len());
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
fn plugin_runtime_separator_override_replaces_inline_separator() {
    // End-to-end pin: with `[layout_options].separator = "powerline"`
    // laying down chevrons, a segment that returns
    // `RenderedSegment::with_separator(text, Separator::None)`
    // suppresses the chevron at its right edge. The plugin per-
    // render override beats the inline (config-time) separator.
    struct OverrideNoneSeg(&'static str);
    impl segments::Segment for OverrideNoneSeg {
        fn render(
            &self,
            _: &crate::data_context::DataContext,
            _: &segments::RenderContext,
        ) -> segments::RenderResult {
            Ok(Some(segments::RenderedSegment::with_separator(
                self.0,
                Separator::None,
            )))
        }
        fn defaults(&self) -> segments::SegmentDefaults {
            segments::SegmentDefaults::with_priority(0)
        }
    }
    let items: Vec<LineItem> = vec![
        LineItem::seg(Cow::Borrowed("a"), Box::new(OverrideNoneSeg("a"))),
        LineItem::Separator(Separator::powerline()),
        LineItem::seg(Cow::Borrowed("b"), Box::new(OverrideNoneSeg("b"))),
    ];
    let mut warn = |_: &str| {};
    let mut observers = crate::layout::LayoutObservers::new(&mut warn);
    let line = crate::layout::render_with_observers(
        &items,
        &stub_ctx(),
        100,
        &mut observers,
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    // Chevron suppressed by the runtime override; "ab" emits with
    // no glyph between.
    assert_eq!(line, "ab");
}

#[test]
fn plugin_runtime_literal_override_replaces_inline_powerline() {
    // Inverse-direction precedence pin (deleted v0.6 test
    // `layout_separator_powerline_preserves_runtime_literal_right_separator`
    // covered this against the legacy default-separator field; the
    // new architecture must hold the same contract). A plugin
    // returning `right_separator: Some(Literal(" | "))` replaces the
    // inline Powerline with the literal at that one boundary.
    struct OverrideLiteralSeg(&'static str);
    impl segments::Segment for OverrideLiteralSeg {
        fn render(
            &self,
            _: &crate::data_context::DataContext,
            _: &segments::RenderContext,
        ) -> segments::RenderResult {
            Ok(Some(segments::RenderedSegment::with_separator(
                self.0,
                Separator::Literal(std::borrow::Cow::Borrowed(" | ")),
            )))
        }
        fn defaults(&self) -> segments::SegmentDefaults {
            segments::SegmentDefaults::with_priority(0)
        }
    }
    let items: Vec<LineItem> = vec![
        LineItem::seg(Cow::Borrowed("a"), Box::new(OverrideLiteralSeg("a"))),
        LineItem::Separator(Separator::powerline()),
        LineItem::seg(Cow::Borrowed("b"), Box::new(OverrideLiteralSeg("b"))),
    ];
    let mut warn = |_: &str| {};
    let mut observers = crate::layout::LayoutObservers::new(&mut warn);
    let line = crate::layout::render_with_observers(
        &items,
        &stub_ctx(),
        100,
        &mut observers,
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    assert_eq!(line, "a | b");
}

#[test]
fn plugin_runtime_override_on_last_segment_is_silently_discarded() {
    // Spec contract: "An override on the rightmost segment ... has
    // no boundary to apply to and is silently discarded." The line
    // emits the segment with no trailing chevron / glyph, exactly
    // as if no override had been set.
    struct OverrideNoneSeg(&'static str);
    impl segments::Segment for OverrideNoneSeg {
        fn render(
            &self,
            _: &crate::data_context::DataContext,
            _: &segments::RenderContext,
        ) -> segments::RenderResult {
            Ok(Some(segments::RenderedSegment::with_separator(
                self.0,
                Separator::powerline(),
            )))
        }
        fn defaults(&self) -> segments::SegmentDefaults {
            segments::SegmentDefaults::with_priority(0)
        }
    }
    // Single-segment line — no inline-separator slot to the right.
    let items: Vec<LineItem> = vec![LineItem::seg(
        Cow::Borrowed("a"),
        Box::new(OverrideNoneSeg("a")),
    )];
    let mut warn = |_: &str| {};
    let mut observers = crate::layout::LayoutObservers::new(&mut warn);
    let line = crate::layout::render_with_observers(
        &items,
        &stub_ctx(),
        100,
        &mut observers,
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    assert_eq!(line, "a");
}

#[test]
fn plugin_compact_form_separator_override_wins_over_pre_shrink_inline() {
    // Codex-flagged regression: when `shrink_to_fit` returns a
    // compact `RenderedSegment` whose `right_separator` differs
    // from the full render, `apply_layout` must propagate the new
    // override to the adjacent inline separator. Without the
    // post-shrink re-application, the line would emit the stale
    // pre-shrink value.
    //
    // Shape: full render carries `Some(Powerline)`; compact form
    // carries `Some(None)` (suppress the chevron once compacted).
    // Layout pressure forces shrink; the inline Powerline must
    // become None.
    struct ChevronUnlessCompactSeg;
    impl segments::Segment for ChevronUnlessCompactSeg {
        fn render(
            &self,
            _: &crate::data_context::DataContext,
            _: &segments::RenderContext,
        ) -> segments::RenderResult {
            Ok(Some(segments::RenderedSegment::with_separator(
                "longprefix-with-tail",
                Separator::powerline(),
            )))
        }
        fn shrink_to_fit(
            &self,
            _: &crate::data_context::DataContext,
            _: &segments::RenderContext,
            target: u16,
        ) -> Option<segments::RenderedSegment> {
            let r = segments::RenderedSegment::with_separator("compact", Separator::None);
            (r.width() <= target).then_some(r)
        }
        fn defaults(&self) -> segments::SegmentDefaults {
            segments::SegmentDefaults::with_priority(200)
        }
    }
    struct AnchorSeg;
    impl segments::Segment for AnchorSeg {
        fn render(
            &self,
            _: &crate::data_context::DataContext,
            _: &segments::RenderContext,
        ) -> segments::RenderResult {
            Ok(Some(segments::RenderedSegment::new("X")))
        }
        fn defaults(&self) -> segments::SegmentDefaults {
            segments::SegmentDefaults::with_priority(0)
        }
    }
    let items: Vec<LineItem> = vec![
        LineItem::seg(Cow::Borrowed("chevron"), Box::new(ChevronUnlessCompactSeg)),
        LineItem::Separator(Separator::powerline()),
        LineItem::seg(Cow::Borrowed("anchor"), Box::new(AnchorSeg)),
    ];
    // Full assembly: 20 + 3 + 1 = 24 cells. Budget 11 forces shrink.
    // Compact: 7 cells; with the override propagated, the
    // separator goes to None: 7 + 0 + 1 = 8 cells.
    let mut warn = |_: &str| {};
    let mut observers = crate::layout::LayoutObservers::new(&mut warn);
    let line = crate::layout::render_with_observers(
        &items,
        &stub_ctx(),
        11,
        &mut observers,
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    // Pin: no chevron in the line (override suppressed it), and
    // the compact text + anchor are concatenated with no gap.
    assert!(
        !line.contains('\u{E0B0}'),
        "chevron must be suppressed: {line:?}"
    );
    assert_eq!(line, "compactX");
}

#[test]
fn user_constructed_adjacent_separators_drop_second() {
    // `LineItem` is `pub` + `#[non_exhaustive]`; external callers
    // can build `Vec<LineItem>` by hand and produce shapes the
    // builder never emits. Two consecutive `Separator` items with
    // no segment between them is one such shape; the second drops
    // because `collect_items_with` only pushes a separator when
    // the previously-pushed item is a Segment.
    struct RawSeg(&'static str);
    impl segments::Segment for RawSeg {
        fn render(
            &self,
            _: &crate::data_context::DataContext,
            _: &segments::RenderContext,
        ) -> segments::RenderResult {
            Ok(Some(segments::RenderedSegment::new(self.0)))
        }
        fn defaults(&self) -> segments::SegmentDefaults {
            segments::SegmentDefaults::with_priority(0)
        }
    }
    let items: Vec<LineItem> = vec![
        LineItem::seg(Cow::Borrowed("a"), Box::new(RawSeg("a"))),
        LineItem::Separator(Separator::Literal(std::borrow::Cow::Borrowed(" | "))),
        LineItem::Separator(Separator::Literal(std::borrow::Cow::Borrowed(" - "))),
        LineItem::seg(Cow::Borrowed("b"), Box::new(RawSeg("b"))),
    ];
    let mut warn = |_: &str| {};
    let mut observers = crate::layout::LayoutObservers::new(&mut warn);
    let line = crate::layout::render_with_observers(
        &items,
        &stub_ctx(),
        100,
        &mut observers,
        theme::default_theme(),
        theme::Capability::None,
        false,
    );
    // Only the first separator survives; the second is dropped.
    assert_eq!(line, "a | b");
}

fn stub_ctx() -> crate::data_context::DataContext {
    use crate::input::{ModelInfo, StatusContext, Tool, WorkspaceInfo};
    use std::path::PathBuf;
    use std::sync::Arc;
    crate::data_context::DataContext::new(StatusContext {
        tool: Tool::ClaudeCode,
        model: Some(ModelInfo {
            display_name: "X".into(),
            id: None,
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
    // and `Space` render identically.
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
fn powerline_width_2_propagates_to_inline_separator() {
    // Pin the Codex-flagged correctness path: users on
    // 2-cell-rendering Nerd Fonts set
    // `[layout_options].powerline_width = 2`, and that width
    // reaches `Separator::Powerline { width }` so total_width()
    // charges 2 cells per chevron instead of undercounting by 1.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", "workspace"]
            [layout_options]
            separator = "powerline"
            powerline_width = 2
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(
        first_inline_separator(&items),
        Some(&Separator::Powerline {
            width: PowerlineWidth::Two,
        }),
    );
}

#[test]
fn powerline_width_default_is_1_when_unset() {
    // Absent `powerline_width` means 1 — the most-common Nerd Font
    // size + standard terminal combination.
    let (sep, _) = resolve_inline_separator("\"powerline\"");
    assert_eq!(sep, Separator::powerline());
}

#[test]
fn powerline_width_invalid_warns_and_falls_back_to_1() {
    // A typo'd `powerline_width = 3` falls back to 1 with a
    // visible warning. Pins the validate-and-warn contract so a
    // future change can't silently accept arbitrary values.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", "workspace"]
            [layout_options]
            separator = "powerline"
            powerline_width = 3
        "#,
    )
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert_eq!(
        first_inline_separator(&items),
        Some(&Separator::powerline())
    );
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
            segments = ["model", "workspace"]
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(first_inline_separator(&items), Some(&Separator::Space));
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
    assert_eq!(segment_count(&got), 2);
    assert_eq!(nth_segment(&got, 0).defaults().priority, 16); // workspace
    assert_eq!(nth_segment(&got, 1).defaults().priority, 64); // model
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
    assert_eq!(nth_segment(&got, 0).defaults().priority, 0);
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
    let bounds = nth_segment(&got, 0).defaults().width.expect("width set");
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
    assert_eq!(segment_count(&got), 2);
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
    assert_eq!(segment_count(&got), 2); // one model, one workspace
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
    assert_eq!(segment_count(&got), 1);
    assert_eq!(nth_segment(&got, 0).defaults().width, None);
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
        icon: None,
        extra: BTreeMap::new(),
    };
    let wrapped = apply_override(
        "stub",
        Box::new(StubWithWidth),
        Some(&ov),
        config::IconMode::NerdFont,
        &mut |_| {},
    );
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
            id: None,
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
    let rendered = nth_segment(&built, 0)
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
    let rendered = nth_segment(&built, 0)
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
    let rendered = nth_segment(&built, 0)
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
    let rendered = nth_segment(&built, 0)
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
    let rendered = nth_segment(&built, 0)
        .render(&model_ctx("Claude Sonnet 4.6"), &rc())
        .expect("render ok")
        .expect("visible");
    assert_eq!(rendered.style.role, Some(Role::Primary));
}

#[test]
fn default_icon_renders_when_icons_mode_is_nerdfont() {
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [layout_options]
            icons = "nerdfont"
        "#,
    )
    .expect("parse");
    let built = build_segments(Some(&cfg), None, |_| {});
    let rendered = nth_segment(&built, 0)
        .render(&model_ctx("Claude Sonnet 4.6"), &rc())
        .expect("render ok")
        .expect("visible");
    assert_eq!(rendered.text(), "\u{2726} Claude Sonnet 4.6");
}

#[test]
fn default_icon_renders_when_layout_options_are_absent() {
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
        "#,
    )
    .expect("parse");
    let built = build_segments(Some(&cfg), None, |_| {});
    let rendered = nth_segment(&built, 0)
        .render(&model_ctx("Claude Sonnet 4.6"), &rc())
        .expect("render ok")
        .expect("visible");
    assert_eq!(rendered.text(), "\u{2726} Claude Sonnet 4.6");
}

#[test]
fn segment_icon_override_replaces_default() {
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [segments.model]
            icon = "AI"
        "#,
    )
    .expect("parse");
    let built = build_segments(Some(&cfg), None, |_| {});
    let rendered = nth_segment(&built, 0)
        .render(&model_ctx("Claude Sonnet 4.6"), &rc())
        .expect("render ok")
        .expect("visible");
    assert_eq!(rendered.text(), "AI Claude Sonnet 4.6");
}

#[test]
fn empty_segment_icon_disables_default() {
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [segments.model]
            icon = ""
        "#,
    )
    .expect("parse");
    let built = build_segments(Some(&cfg), None, |_| {});
    let rendered = nth_segment(&built, 0)
        .render(&model_ctx("Claude Sonnet 4.6"), &rc())
        .expect("render ok")
        .expect("visible");
    assert_eq!(rendered.text(), "Claude Sonnet 4.6");
}

#[test]
fn icons_off_suppresses_default_icon() {
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [layout_options]
            icons = "off"
        "#,
    )
    .expect("parse");
    let built = build_segments(Some(&cfg), None, |_| {});
    let rendered = nth_segment(&built, 0)
        .render(&model_ctx("Claude Sonnet 4.6"), &rc())
        .expect("render ok")
        .expect("visible");
    assert_eq!(rendered.text(), "Claude Sonnet 4.6");
}

#[test]
fn segment_icon_override_renders_when_icons_off() {
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model"]
            [layout_options]
            icons = "off"
            [segments.model]
            icon = "AI"
        "#,
    )
    .expect("parse");
    let built = build_segments(Some(&cfg), None, |_| {});
    let rendered = nth_segment(&built, 0)
        .render(&model_ctx("Claude Sonnet 4.6"), &rc())
        .expect("render ok")
        .expect("visible");
    assert_eq!(rendered.text(), "AI Claude Sonnet 4.6");
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
    assert_eq!(segment_count(&built), 2);
    // Order matches `[line].segments`: built-in `model` first,
    // plugin `my_plugin` second. `model` defaults to priority 64;
    // a plugin with no override defaults to the trait's 128.
    assert_eq!(nth_segment(&built, 0).defaults().priority, 64);
    assert_eq!(nth_segment(&built, 1).defaults().priority, 128);
    // The plugin's render emits a known string — pin it so a
    // wiring regression that swaps slots fails loudly.
    let dc = model_ctx("Sonnet");
    let plugin_render = nth_segment(&built, 1)
        .render(&dc, &rc())
        .expect("plugin render ok")
        .expect("visible");
    assert_eq!(plugin_render.text(), "from-plugin");
    // Plugin ids must land as `Cow::Owned` (ADR-0026) — `resolve_segment_id`
    // short-circuits to `Cow::Borrowed` only for built-ins.
    let plugin_item = built
        .iter()
        .find_map(|i| match i {
            LineItem::Segment { id, .. } if id.as_ref() == "my_plugin" => Some(id.clone()),
            _ => None,
        })
        .expect("plugin slot in built items");
    assert!(
        matches!(plugin_item, Cow::Owned(_)),
        "plugin id must be Cow::Owned, got {plugin_item:?}",
    );
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
        segment_count(&segs),
        2,
        "expected line 1's two segments, got {} segs",
        segment_count(&segs),
    );
    let actual: Vec<u8> = segs
        .iter()
        .filter_map(|i| match i {
            LineItem::Segment { segment, .. } => Some(segment.defaults().priority),
            LineItem::Separator(_) => None,
        })
        .collect();
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
    assert_eq!(segment_count(&lines[0]), 2, "line 1 keeps plugin + model");
    assert_eq!(
        segment_count(&lines[1]),
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
    assert_eq!(segment_count(&built), 1);
    let dc = model_ctx("Sonnet");
    let rendered = nth_segment(&built, 0)
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
    assert_eq!(segment_count(&built), 1);
    assert_eq!(nth_segment(&built, 0).defaults().priority, 64);
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
    assert_eq!(segment_count(&built(Some(&cfg))), 1);
}

// --- build_lines (multi-line layout) ---

fn lines(cfg: Option<&config::Config>) -> Vec<Vec<LineItem>> {
    build_lines(cfg, None, |_| {})
}

fn lines_with_warns(cfg: Option<&config::Config>) -> (Vec<Vec<LineItem>>, Vec<String>) {
    let mut warns = Vec::new();
    let result = build_lines(cfg, None, |m| warns.push(m.to_string()));
    (result, warns)
}

/// Map each line's segment slots to the configured `[line.N]` ids
/// by reading their priorities, using `priorities_for` (defined
/// below) as the comparable canonical form.
fn line_segment_priorities(items: &[LineItem]) -> Vec<u8> {
    items
        .iter()
        .filter_map(|i| match i {
            LineItem::Segment { segment, .. } => Some(segment.defaults().priority),
            LineItem::Separator(_) => None,
        })
        .collect()
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

fn priorities_per_line(built: &[Vec<LineItem>]) -> Vec<Vec<u8>> {
    built
        .iter()
        .map(|line| line_segment_priorities(line))
        .collect()
}

#[test]
fn build_lines_single_line_default_returns_one_line_with_default_segments() {
    // No config = implicit single-line with default segment list.
    let result = lines(None);
    assert_eq!(result.len(), 1);
    assert_eq!(segment_count(&result[0]), DEFAULT_SEGMENT_IDS.len());
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
    assert_eq!(segment_count(&lines[0]), 2, "line 1: plugin + model");
    assert_eq!(
        segment_count(&lines[1]),
        1,
        "line 2: plugin dropped, only workspace"
    );
    assert_eq!(
        segment_count(&lines[2]),
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
    let actual = line_segment_priorities(&segs);
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

// -------------------------------------------------------------------
// Per-boundary separator + merge entries (ADR-0024)
// -------------------------------------------------------------------

/// Collect every `LineItem::Separator` in source order. Pairs with
/// `nth_segment` for tests asserting the exact `Segment, Separator,
/// Segment, ...` shape produced by `build_one_line`.
fn separators_in_order(items: &[LineItem]) -> Vec<&Separator> {
    items
        .iter()
        .filter_map(|i| match i {
            LineItem::Separator(s) => Some(s),
            LineItem::Segment { .. } => None,
        })
        .collect()
}

#[test]
fn inline_table_separator_with_character_overrides_global_default() {
    // Pin ADR-0024's single-boundary override: an explicit
    // `{ type = "separator", character = " | " }` between two
    // segments emits `Separator::Literal(" | ")` rather than the
    // global `[layout_options].separator = "space"` default. The
    // entry's `character` field IS load-bearing — a regression
    // that ignores it would silently fall back to Space and
    // confuse the user about why their custom glyph isn't showing.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", { type = "separator", character = " | " }, "workspace"]
        "#,
    )
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert!(warns.is_empty(), "no warnings expected: {warns:?}");
    assert_eq!(segment_count(&items), 2);
    let seps = separators_in_order(&items);
    assert_eq!(seps.len(), 1, "exactly one separator between two segments");
    assert_eq!(
        seps[0],
        &Separator::Literal(std::borrow::Cow::Owned(" | ".to_string())),
    );
}

#[test]
fn inline_table_separator_without_character_uses_layout_default() {
    // Pin the fallback chain: `{ type = "separator" }` with no
    // `character` field consults `[layout_options].separator`.
    // Tests that the items editor's "insert with default glyph"
    // contract (Space → `{ type = "separator" }`) doesn't accidentally
    // drop the user's global preference.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", { type = "separator" }, "workspace"]
            [layout_options]
            separator = " · "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    let seps = separators_in_order(&items);
    assert_eq!(seps.len(), 1);
    assert_eq!(
        seps[0],
        &Separator::Literal(std::borrow::Cow::Owned(" · ".to_string())),
    );
}

#[test]
fn merge_flag_suppresses_implicit_interleave_at_boundary() {
    // The simplest merge case: `{ type = "model", merge = true }`
    // followed by another segment — the implicit layout-options
    // separator does NOT fire between them. Pin the count of
    // separators (zero between the merged segment and its right
    // neighbor); a regression in `merge_pending` clearing logic
    // would fail this directly.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [{ type = "model", merge = true }, "workspace"]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(segment_count(&items), 2);
    assert_eq!(
        separators_in_order(&items).len(),
        0,
        "merge=true on left segment must drop the boundary separator",
    );
}

#[test]
fn merge_flag_suppresses_explicit_separator_entry_at_boundary() {
    // The non-obvious case the doc-comment for `merge_pending` calls
    // out: `seg(merge), |, seg` drops BOTH the explicit separator
    // entry AND the implicit interleave. The merge flag persists
    // across the explicit separator. A regression that only handled
    // the implicit case would emit the explicit `|` here.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [
                { type = "model", merge = true },
                { type = "separator", character = " | " },
                "workspace",
            ]
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(segment_count(&items), 2);
    assert_eq!(
        separators_in_order(&items).len(),
        0,
        "merge=true must consume the next explicit separator AND skip implicit interleave",
    );
}

#[test]
fn merge_flag_clears_after_one_boundary() {
    // Pin the re-arming contract: merge_pending only suppresses ONE
    // boundary. After the merging segment's right neighbor lands,
    // the flag clears and subsequent separators interleave normally.
    // `seg(merge), seg, seg` → only the first boundary is suppressed;
    // the second gets the global default.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [{ type = "model", merge = true }, "workspace", "cost"]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(segment_count(&items), 3);
    let seps = separators_in_order(&items);
    assert_eq!(seps.len(), 1, "only the second boundary gets a separator");
    assert_eq!(
        seps[0],
        &Separator::Literal(std::borrow::Cow::Owned(" | ".to_string())),
    );
}

#[test]
fn back_to_back_merge_chains_drop_every_intermediate_separator() {
    // Two merging segments in a row: every boundary between them
    // and through to the final segment is suppressed. Pin so a
    // future relaxation that clears merge_pending too eagerly (e.g.
    // on the explicit separator skip) doesn't silently insert one.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [
                { type = "model", merge = true },
                { type = "workspace", merge = true },
                "cost",
            ]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(segment_count(&items), 3);
    assert_eq!(separators_in_order(&items).len(), 0);
}

/// `fuses_left` per `LineItem::Segment` in order (ADR-0029); separators
/// skipped. `true` marks a segment grouped with the segment to its left.
fn fuses_flags(items: &[LineItem]) -> Vec<bool> {
    items
        .iter()
        .filter_map(|i| match i {
            LineItem::Segment { fuses_left, .. } => Some(*fuses_left),
            LineItem::Separator(_) => None,
        })
        .collect()
}

#[test]
fn group_absent_divides_with_separator() {
    // (merge absent, group absent) — the default: a separator renders
    // and the right segment opens its own color group.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", "workspace"]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(
        separators_in_order(&items).len(),
        1,
        "no merge → separator stays"
    );
    assert_eq!(
        fuses_flags(&items),
        vec![false, false],
        "no group → divides"
    );
}

#[test]
fn group_true_fuses_while_keeping_the_separator() {
    // (merge absent, group = true) — the dogfood case: color fuses
    // rightward, the separator between the members is untouched.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [{ type = "model", group = true }, "workspace"]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(
        separators_in_order(&items).len(),
        1,
        "group must NOT suppress the separator (that is merge's job)"
    );
    assert_eq!(
        fuses_flags(&items),
        vec![false, true],
        "right member fuses into the lead"
    );
}

#[test]
fn merge_implies_group_when_group_unset() {
    // (merge = true, group absent) — an abutted pair is one visual
    // unit, so it fuses for color without an explicit `group`.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [{ type = "model", merge = true }, "workspace"]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(separators_in_order(&items).len(), 0, "merge abuts");
    assert_eq!(
        fuses_flags(&items),
        vec![false, true],
        "merge implies grouping"
    );
}

#[test]
fn group_false_overrides_the_merge_implication() {
    // (merge = true, group = false) — abut for spacing but keep two
    // colors; the explicit opt-out beats merge's implied grouping.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [{ type = "model", merge = true, group = false }, "workspace"]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(separators_in_order(&items).len(), 0, "merge still abuts");
    assert_eq!(
        fuses_flags(&items),
        vec![false, false],
        "group = false divides despite merge"
    );
}

#[test]
fn group_persists_across_an_explicit_separator() {
    // The canonical ADR-0028 line: a window fuses its reset across a
    // literal space separator. group_pending must survive the explicit
    // separator entry, mirroring merge_pending.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [
                { type = "model", group = true },
                { type = "separator", character = " " },
                "workspace",
            ]
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(
        separators_in_order(&items).len(),
        1,
        "the space separator stays"
    );
    assert_eq!(
        fuses_flags(&items),
        vec![false, true],
        "fusion carries across the explicit separator"
    );
}

#[test]
fn group_on_separator_entry_warns_and_is_ignored() {
    // `group` is a segment-side flag; on a separator entry it warns
    // (like `merge`) and does not affect grouping.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [
                "model",
                { type = "separator", character = " | ", group = true },
                "workspace",
            ]
        "#,
    )
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert_eq!(
        fuses_flags(&items),
        vec![false, false],
        "separator `group` has no effect"
    );
    assert!(
        warns
            .iter()
            .any(|w| w.contains("separator entry has `group")),
        "expected a group-on-separator warning, got {warns:?}"
    );
}

#[test]
fn group_chains_across_three_segments() {
    // ADR-0029 defines a color group as a MAXIMAL run: two grouping
    // segments in a row plus their lead form one three-member group.
    // This is the case the group-lead color pass (lsm-p0p2) leans on.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [
                { type = "model", group = true },
                { type = "workspace", group = true },
                "cost",
            ]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(
        separators_in_order(&items).len(),
        2,
        "grouping keeps both separators"
    );
    assert_eq!(
        fuses_flags(&items),
        vec![false, true, true],
        "one three-member group led by the leftmost"
    );
}

#[test]
fn merge_and_group_true_together_abut_and_fuse() {
    // Explicit `merge = true, group = true` (redundant but legal):
    // abut for spacing AND fuse for color — `unwrap_or` returns the
    // explicit Some(true), same outcome as the merge-implied case.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [{ type = "model", merge = true, group = true }, "workspace"]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(separators_in_order(&items).len(), 0, "merge abuts");
    assert_eq!(fuses_flags(&items), vec![false, true], "group fuses");
}

#[test]
fn group_pending_carries_to_the_next_surviving_segment() {
    // A dropped segment (unknown id) leaves no hole in the OUTPUT, so
    // the surviving neighbors are genuinely adjacent and the fusion
    // carries — identical to `merge_pending` across a drop. Pin it so a
    // future change to either flag's drop semantics is deliberate.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [{ type = "model", group = true }, "nonexistent_seg", "workspace"]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert_eq!(segment_count(&items), 2, "the unknown segment is dropped");
    assert!(warns.iter().any(|w| w.contains("nonexistent_seg")));
    assert_eq!(
        fuses_flags(&items),
        vec![false, true],
        "fusion carries to the next surviving segment, like merge"
    );
}

#[test]
fn group_true_on_trailing_segment_is_inert() {
    // `group = true` on the last segment arms group_pending with no
    // following segment to consume it: a harmless no-op, symmetric with
    // a trailing `merge`.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", { type = "workspace", group = true }]
            [layout_options]
            separator = " | "
        "#,
    )
    .expect("parse");
    let items = built(Some(&cfg));
    assert_eq!(
        fuses_flags(&items),
        vec![false, false],
        "trailing group fuses nothing"
    );
}

#[test]
fn default_line_fuses_nothing() {
    // The default-order line (interleave_separators) never groups:
    // every segment is its own color context. Guards against a future
    // change that wires grouping into the default path.
    let items = built(None);
    assert!(
        fuses_flags(&items).iter().all(|&f| !f),
        "default line must fuse nothing: {:?}",
        fuses_flags(&items)
    );
}

#[test]
fn consecutive_separator_entries_warn_with_specific_message() {
    // Adjacent `|, |` → second skipped with the "consecutive" warn,
    // not the misleading "without a preceding segment" one. Pin
    // the message text so a regression to the catch-all wording
    // fails this assertion directly.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [
                "model",
                { type = "separator", character = " | " },
                { type = "separator", character = " · " },
                "workspace",
            ]
        "#,
    )
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert_eq!(segment_count(&items), 2);
    assert_eq!(
        separators_in_order(&items).len(),
        1,
        "duplicate adjacent separator entries collapse to one",
    );
    assert!(
        warns.iter().any(|w| w.contains("consecutive separator")),
        "missing 'consecutive separator' warn: {warns:?}",
    );
    assert!(
        !warns.iter().any(|w| w.contains("without a preceding")),
        "incorrect 'without a preceding' warn fired for adjacent separators: {warns:?}",
    );
}

#[test]
fn leading_separator_entry_warns_with_specific_message() {
    // Pin the head-of-array case separately: `|, seg` → first
    // separator skipped with "leads with a separator entry". Distinct
    // from the consecutive case so user diagnostics point at the
    // right fix.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [{ type = "separator", character = " | " }, "model"]
        "#,
    )
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert_eq!(segment_count(&items), 1);
    assert_eq!(separators_in_order(&items).len(), 0);
    assert!(
        warns.iter().any(|w| w.contains("leads with a separator")),
        "missing 'leads with a separator' warn: {warns:?}",
    );
}

#[test]
fn kindless_inline_table_entry_warns_and_drops() {
    // `{ character = " | " }` (no `type`) is malformed per ADR-0024.
    // Pin the warn-and-drop semantics so a future schema relax
    // doesn't silently treat kindless entries as separators.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", { character = " | " }, "workspace"]
        "#,
    )
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert_eq!(segment_count(&items), 2, "kindless entry dropped");
    assert!(
        warns.iter().any(|w| w.contains("missing `type`")),
        "missing kindless-entry warn: {warns:?}",
    );
}

#[test]
fn merge_field_on_separator_entry_warns_and_is_ignored() {
    // `merge` is a segment-only flag per ADR-0024. A separator with
    // `merge = true` set must NOT suppress the next boundary —
    // separators don't have a "right neighbor" semantic for merge.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [
                "model",
                { type = "separator", character = " | ", merge = true },
                "workspace",
                "cost",
            ]
            [layout_options]
            separator = " · "
        "#,
    )
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert_eq!(segment_count(&items), 3);
    let seps = separators_in_order(&items);
    assert_eq!(
        seps.len(),
        2,
        "boundary count unaffected by separator merge"
    );
    assert!(
        warns.iter().any(|w| w.contains("`merge")),
        "missing merge-on-separator warn: {warns:?}",
    );
}

#[test]
fn character_field_on_segment_entry_warns_and_is_ignored() {
    // `character` is a separator-only field. A segment entry with
    // `character` set must not affect anything; warn so the user
    // knows the field is inert.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [{ type = "model", character = "ignored" }, "workspace"]
        "#,
    )
    .expect("parse");
    let (_, warns) = built_with_warns(Some(&cfg));
    assert!(
        warns.iter().any(|w| w.contains("`character")),
        "missing character-on-segment warn: {warns:?}",
    );
}

#[test]
fn unknown_inline_table_keys_round_trip_through_extra_bag() {
    // Forward-compat contract from ADR-0024: a v0.2-only field like
    // `{ type = "separator", color = "red" }` parses cleanly into
    // `LineEntryItem.extra` and the v0.1 builder doesn't fail. Pin
    // through `Config::from_str` since this is the load-time
    // forward-compat surface a downgrading user hits first.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = [
                "model",
                { type = "separator", character = " | ", color = "red", bold = true },
                "workspace",
            ]
        "#,
    )
    .expect("config with unknown inline-table keys must parse");
    // The unknown keys land in `extra`; the builder still emits a
    // valid LineItem::Separator with the known `character`.
    let (items, _warns) = built_with_warns(Some(&cfg));
    let seps = separators_in_order(&items);
    assert_eq!(
        seps[0],
        &Separator::Literal(std::borrow::Cow::Owned(" | ".to_string())),
    );
    // And the LineEntryItem itself preserves the unknown keys.
    let line = cfg.line.as_ref().expect("line config present");
    let entry = &line.segments[1];
    let extra_keys: Vec<&str> = match entry {
        config::LineEntry::Item(item) => item.extra.keys().map(String::as_str).collect(),
        config::LineEntry::Id(_) => panic!("expected LineEntry::Item"),
    };
    assert!(
        extra_keys.contains(&"color"),
        "color preserved: {extra_keys:?}",
    );
    assert!(
        extra_keys.contains(&"bold"),
        "bold preserved: {extra_keys:?}",
    );
}

#[test]
fn malformed_segment_entry_with_wrong_value_type_warns_at_build_time() {
    // `{ type = 42 }` (integer instead of string) is shape-invalid
    // per ADR-0024. Both the single-line and numbered-line parse
    // paths now warn-and-drop the malformed entry rather than
    // aborting the whole file load — the typed `LineConfig.segments`
    // field uses a per-item-tolerant deserializer that lands the bad
    // entry in a kindless `LineEntry::Item`, and the builder warns
    // on kindless entries with a "missing `type`" diagnostic. Pin
    // both halves: parse succeeds AND build warns AND the well-
    // formed neighbors render unaffected.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", { type = 42 }, "workspace"]
        "#,
    )
    .expect("malformed entry must not abort the whole parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert_eq!(
        segment_count(&items),
        2,
        "well-formed neighbors render; malformed entry drops",
    );
    assert!(
        warns.iter().any(|w| w.contains("missing `type`")),
        "missing-type warn must fire on the malformed entry: {warns:?}",
    );
}

#[test]
fn resolve_segment_id_returns_borrowed_for_every_built_in() {
    // Pins the zero-alloc-per-emit guarantee from ADR-0026. A regression
    // that always returns `Cow::Owned` (e.g. flipping the map_or_else
    // arms or using `Cow::from(id)` on a `&str`) would silently break
    // the perf contract.
    for built_in in BUILT_IN_SEGMENT_IDS {
        let resolved = resolve_segment_id(built_in);
        assert_eq!(
            resolved.as_ref(),
            *built_in,
            "resolved id content must round-trip — catches a regression that returns a fixed borrowed constant",
        );
        assert!(
            matches!(resolved, Cow::Borrowed(_)),
            "built-in id {built_in:?} must resolve to Cow::Borrowed, got {resolved:?}",
        );
    }
}

#[test]
fn resolve_segment_id_returns_owned_for_non_built_in_ids() {
    for non_built_in in &["my_plugin", "totally-not-a-segment", ""] {
        let resolved = resolve_segment_id(non_built_in);
        assert!(
            matches!(resolved, Cow::Owned(_)),
            "non-built-in id {non_built_in:?} must resolve to Cow::Owned, got {resolved:?}",
        );
    }
}

#[test]
fn build_default_segments_emits_borrowed_ids_in_canonical_order() {
    // `build_default_segments` bypasses `resolve_segment_id` and
    // constructs `Cow::Borrowed(*id)` directly per ADR-0026's
    // zero-alloc shortcut for the default no-config path. The
    // `debug_assert!` at the call site catches DEFAULT/BUILT_IN
    // drift in debug builds; this test pins the release-mode
    // contract that each emitted id is Cow::Borrowed.
    let items = build_default_segments();
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            LineItem::Segment { id, .. } => Some(id.as_ref()),
            LineItem::Separator(_) => None,
        })
        .collect();
    assert_eq!(ids, DEFAULT_SEGMENT_IDS.to_vec());
    for item in &items {
        if let LineItem::Segment { id, .. } = item {
            assert!(
                matches!(id, Cow::Borrowed(_)),
                "default-path id {id:?} must be Cow::Borrowed",
            );
        }
    }
}

#[test]
fn build_segments_records_config_id_on_each_segment() {
    // Pins ADR-0026's addressing contract end-to-end: every segment
    // the builder emits carries the user's config-side id, in
    // declaration order. A regression that hard-codes an id or swaps
    // the resolver chain trips here. Built-in ids stay `Cow::Borrowed`
    // (matching the zero-alloc-per-emit cost model).
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", "git_branch", "workspace"]
        "#,
    )
    .expect("parse");
    let (items, warns) = built_with_warns(Some(&cfg));
    assert!(warns.is_empty(), "expected no warnings, got {warns:?}");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| match i {
            LineItem::Segment { id, .. } => Some(id.as_ref()),
            LineItem::Separator(_) => None,
        })
        .collect();
    assert_eq!(ids, vec!["model", "git_branch", "workspace"]);
    for item in &items {
        if let LineItem::Segment { id, .. } = item {
            assert!(
                matches!(id, Cow::Borrowed(_)),
                "built-in id {id:?} must be Cow::Borrowed on the production-config path",
            );
        }
    }
}

#[test]
fn inline_table_separator_round_trips_through_config_parse() {
    // Parse-layer half of the round-trip contract: a hand-written
    // mixed-array config parses to `LineEntry::Item` with the
    // expected `kind`/`character` shape. The full editor → save →
    // reload preservation lives in the linesmith TUI tests where
    // `toml_edit::DocumentMut` is available.
    let cfg = config::Config::from_str(
        r#"
            [line]
            segments = ["model", { type = "separator", character = " | " }, "workspace"]
        "#,
    )
    .expect("parse");
    let line = cfg.line.as_ref().expect("line present");
    match &line.segments[1] {
        config::LineEntry::Item(item) => {
            assert_eq!(item.kind.as_deref(), Some("separator"));
            assert_eq!(item.character.as_deref(), Some(" | "));
        }
        other => panic!("expected LineEntry::Item, got {other:?}"),
    }
}
