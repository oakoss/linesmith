//! `context_bar` segment: visual bar for context-window fill.
//!
//! Renders a fixed-width Unicode block-character bar (e.g. `████▓░░░░░`)
//! colored by fill threshold (green/yellow/red). Companion to the
//! `context_window` segment, which renders the textual `42% · 200k`.
//! Hidden when the payload doesn't carry context-window data.

use std::collections::BTreeMap;

use unicode_width::UnicodeWidthStr;

use super::progress_bar::{self, BarChars as PbChars, BarStyle, FillMode, Thresholds};
use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;

/// Drops earlier than `rate_limit_5h` (96) and `context_window` (32):
/// the bar is redundant with the textual percentage, so it should
/// disappear before actionable rate-limit telemetry or the percentage
/// itself.
const PRIORITY: u8 = 112;

const ID: &str = "context_bar";

const DEFAULT_WIDTH: u16 = 10;
const DEFAULT_GREEN_THRESHOLD: u8 = 50;
const DEFAULT_YELLOW_THRESHOLD: u8 = 80;

const DEFAULT_BRACKETS: bool = true;
const DEFAULT_PERCENTAGE: bool = true;
const DEFAULT_DIM_EMPTY: bool = true;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) width: u16,
    pub(crate) thresholds: Thresholds,
    pub(crate) fill: FillMode,
    pub(crate) chars: BarChars,
    /// Wrap the cells in `chars.open`/`chars.close`.
    pub(crate) brackets: bool,
    /// Append ` NN%` after the bar, banker's-rounded to agree with the
    /// textual `context_window` segment at every boundary.
    pub(crate) percentage: bool,
    /// Render the empty (trough) cells in [`DIM_ROLE`] instead of the
    /// fill's threshold role.
    pub(crate) dim_empty: bool,
}

/// User-facing glyph overrides. `partial` is the `Half`-mode partial
/// cell; `full`/`empty` override the [`FillMode`] preset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BarChars {
    pub(crate) full: String,
    pub(crate) partial: String,
    pub(crate) empty: String,
    pub(crate) open: String,
    pub(crate) close: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            width: DEFAULT_WIDTH,
            thresholds: Thresholds::new(DEFAULT_GREEN_THRESHOLD, DEFAULT_YELLOW_THRESHOLD)
                .expect("DEFAULT_GREEN_THRESHOLD <= DEFAULT_YELLOW_THRESHOLD by construction"),
            fill: FillMode::Half,
            chars: BarChars {
                full: progress_bar::DEFAULT_FULL.to_string(),
                partial: progress_bar::DEFAULT_HALF.to_string(),
                empty: progress_bar::DEFAULT_EMPTY.to_string(),
                open: progress_bar::DEFAULT_OPEN.to_string(),
                close: progress_bar::DEFAULT_CLOSE.to_string(),
            },
            brackets: DEFAULT_BRACKETS,
            percentage: DEFAULT_PERCENTAGE,
            dim_empty: DEFAULT_DIM_EMPTY,
        }
    }
}

#[derive(Default)]
pub struct ContextBarSegment {
    pub(crate) cfg: Config,
}

impl ContextBarSegment {
    /// Parse the `[segments.context_bar]` extras bag. Unknown values
    /// warn and fall back to defaults. Thresholds are parsed as a pair
    /// and validated together — supplying a monotonic pair like
    /// `green = 90, yellow = 95` is accepted regardless of declaration
    /// order, and the whole pair is rejected (with the bad fields
    /// warned individually) if any field is out of range or the pair
    /// inverts the ramp.
    pub fn from_extras(
        extras: &BTreeMap<String, toml::Value>,
        warn: &mut impl FnMut(&str),
    ) -> Self {
        let mut cfg = Config::default();

        if let Some(v) = extras.get("cells") {
            match v.as_integer().and_then(|n| u16::try_from(n).ok()) {
                Some(n) if n >= 1 => cfg.width = n,
                _ => warn(&format!(
                    "segments.{ID}.cells: expected 1..=65535; ignoring"
                )),
            }
        }

        if let Some(t) = extras.get("thresholds").and_then(|v| v.as_table()) {
            // Parse both fields against the table first, then validate
            // as a pair. Per-field-against-just-applied is order-
            // dependent and silently rejects valid monotonic pairs that
            // raise both above the defaults (e.g. green=90 yellow=95
            // would reject green against the default yellow=80 even
            // though the user-supplied yellow=95 makes the pair fine).
            let parse_field = |field: &str, warn: &mut dyn FnMut(&str)| -> Option<u8> {
                let v = t.get(field)?;
                match v.as_integer().and_then(|n| u8::try_from(n).ok()) {
                    Some(n) if n <= 100 => Some(n),
                    _ => {
                        warn(&format!(
                            "segments.{ID}.thresholds.{field}: expected 0..=100; ignoring"
                        ));
                        None
                    }
                }
            };
            let green = parse_field("green", &mut |m| warn(m));
            let yellow = parse_field("yellow", &mut |m| warn(m));
            let candidate = (
                green.unwrap_or(cfg.thresholds.green()),
                yellow.unwrap_or(cfg.thresholds.yellow()),
            );
            match Thresholds::new(candidate.0, candidate.1) {
                Some(t) => cfg.thresholds = t,
                None => warn(&format!(
                    "segments.{ID}.thresholds: green ({}) must be <= yellow ({}); ignoring both",
                    candidate.0, candidate.1
                )),
            }
        }

        // Parse `fill` before `characters` so the preset's full/empty
        // glyphs land first and an explicit `characters.full`/`.empty`
        // still wins over them.
        if let Some(v) = extras.get("fill") {
            match v.as_str().and_then(FillMode::parse) {
                Some(mode) => {
                    cfg.fill = mode;
                    let (full, empty) = mode.preset_chars();
                    cfg.chars.full = full.to_string();
                    cfg.chars.empty = empty.to_string();
                }
                None => warn(&format!(
                    "segments.{ID}.fill: expected one of half|whole|eighth|braille; ignoring"
                )),
            }
        }

        for (key, slot) in [
            ("brackets", &mut cfg.brackets),
            ("percentage", &mut cfg.percentage),
            ("dim_empty", &mut cfg.dim_empty),
        ] {
            let Some(v) = extras.get(key) else { continue };
            match v.as_bool() {
                Some(b) => *slot = b,
                None => warn(&format!("segments.{ID}.{key}: expected boolean; ignoring")),
            }
        }

        if let Some(c) = extras.get("characters").and_then(|v| v.as_table()) {
            for (field, slot) in [
                ("full", &mut cfg.chars.full),
                ("partial", &mut cfg.chars.partial),
                ("empty", &mut cfg.chars.empty),
                ("open", &mut cfg.chars.open),
                ("close", &mut cfg.chars.close),
            ] {
                let Some(v) = c.get(field) else { continue };
                // `width(s) == 1` (not `<= 1`) is intentional: ZWJ
                // sequences and combining marks render as 1 cell on
                // most terminals but `unicode-width` reports 0 because
                // they have no advance width on their own. Accepting
                // them here would let the bar desync from `cfg.width`
                // on terminals that treat them differently.
                match v.as_str() {
                    Some(s) if UnicodeWidthStr::width(s) == 1 => *slot = s.to_string(),
                    Some(s) => warn(&format!(
                        "segments.{ID}.characters.{field}: expected a single-cell string, got {} cell(s); ignoring",
                        UnicodeWidthStr::width(s)
                    )),
                    None => warn(&format!(
                        "segments.{ID}.characters.{field}: expected string; ignoring"
                    )),
                }
            }
        }

        Self { cfg }
    }
}

impl Segment for ContextBarSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(cw) = ctx.status.context_window.as_ref() else {
            crate::lsm_debug!("context_bar: status.context_window absent; hiding");
            return Ok(None);
        };
        // Per ADR-0014, `used` is per-leaf Option: the bar can't
        // render without a percentage, so hide when null (mirrors the
        // text `context_window` segment).
        let Some(used) = cw.used else {
            crate::lsm_debug!("context_bar: used null; hiding");
            return Ok(None);
        };
        // Round-ties-to-even so the bar geometry, color role, and the
        // `{:.0}` percentage suffix all use the same integer pct — and
        // that pct matches the textual `context_window` segment's
        // `{:.0}` (also banker's), keeping the two in lockstep at every
        // boundary.
        let pct = f64::from(used.value().round_ties_even());
        let role = self.cfg.thresholds.role_for(pct);
        let spans = progress_bar::render_bar(pct, &self.cfg.bar_style(), role);
        Ok(Some(RenderedSegment::from_spans(spans).with_role(role)))
    }

    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY).with_icon("\u{f035b}")
    }
}

impl Config {
    /// Project the user-facing config onto the shared [`BarStyle`].
    fn bar_style(&self) -> BarStyle {
        BarStyle {
            width: self.width,
            chars: PbChars {
                full: self.chars.full.clone(),
                empty: self.chars.empty.clone(),
                open: self.chars.open.clone(),
                close: self.chars.close.clone(),
                partial: self.fill.partial(&self.chars.partial),
            },
            brackets: self.brackets,
            percentage: self.percentage.then_some(0),
            dim_empty: self.dim_empty,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::progress_bar::{
        DEFAULT_EMPTY, DEFAULT_FULL, DEFAULT_HALF as DEFAULT_PARTIAL, DEFAULT_OPEN, DIM_ROLE,
        FRAME_ROLE,
    };
    use super::*;
    use crate::input::{ContextWindow, ModelInfo, Percent, StatusContext, Tool, WorkspaceInfo};
    use crate::theme::Role;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx(window: Option<ContextWindow>) -> DataContext {
        DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: Some(ModelInfo {
                display_name: "X".into(),
                id: None,
            }),
            workspace: Some(WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            }),
            context_window: window,
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    fn window(used: f32, size: u32) -> ContextWindow {
        ContextWindow {
            used: Some(Percent::new(used).expect("in range")),
            size: Some(size),
            total_input_tokens: Some(0),
            total_output_tokens: Some(0),
            current_usage: None,
        }
    }

    /// Default config with the three decoration toggles off, so
    /// `render().text()` is exactly the bar cells. Geometry tests use
    /// this to pin cell math without the brackets/percentage/dim that
    /// the ON-by-default shape adds.
    fn bare() -> ContextBarSegment {
        strip_decoration(ContextBarSegment::default())
    }

    /// Turn off the decoration toggles on an already-built segment
    /// (e.g. one from `from_extras`) so only the cells render.
    fn strip_decoration(mut seg: ContextBarSegment) -> ContextBarSegment {
        seg.cfg.brackets = false;
        seg.cfg.percentage = false;
        seg.cfg.dim_empty = false;
        seg
    }

    #[test]
    fn renders_zero_percent_as_all_empty() {
        let r = bare()
            .render(&ctx(Some(window(0.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "░░░░░░░░░░");
        assert_eq!(r.style().role, Some(Role::Success));
    }

    #[test]
    fn renders_full_at_one_hundred() {
        let r = bare()
            .render(&ctx(Some(window(100.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "██████████");
        assert_eq!(r.style().role, Some(Role::Error));
    }

    #[test]
    fn renders_partial_block_when_fraction_geq_half() {
        // 45% of 10 cells = 4.5 → 4 full + 1 partial + 5 empty
        let r = bare()
            .render(&ctx(Some(window(45.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "████▓░░░░░");
    }

    #[test]
    fn rounds_down_when_fraction_lt_half() {
        // 42% of 10 cells = 4.2 → 4 full + 0 partial + 6 empty
        let r = bare()
            .render(&ctx(Some(window(42.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "████░░░░░░");
    }

    #[test]
    fn renders_fifty_percent_at_threshold_boundary_yellow() {
        // pct >= green (50) → Warning; 50% of 10 = 5.0 → 5 full + 5 empty.
        let r = bare()
            .render(&ctx(Some(window(50.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "█████░░░░░");
        assert_eq!(r.style().role, Some(Role::Warning));
    }

    #[test]
    fn red_threshold_at_eighty_percent() {
        // pct >= yellow (80) → Error.
        let r = ContextBarSegment::default()
            .render(&ctx(Some(window(80.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.style().role, Some(Role::Error));
    }

    #[test]
    fn green_at_one_below_threshold() {
        let r = ContextBarSegment::default()
            .render(&ctx(Some(window(49.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.style().role, Some(Role::Success));
    }

    #[test]
    fn yellow_at_one_below_red_threshold() {
        let r = ContextBarSegment::default()
            .render(&ctx(Some(window(79.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.style().role, Some(Role::Warning));
    }

    #[test]
    fn hidden_when_context_window_absent() {
        assert_eq!(
            ContextBarSegment::default()
                .render(&ctx(None), &rc())
                .unwrap(),
            None
        );
    }

    #[test]
    fn defaults_use_expected_priority() {
        assert_eq!(ContextBarSegment::default().defaults().priority, PRIORITY);
    }

    #[test]
    fn rendered_width_matches_configured_cells_for_default_chars() {
        // Default chars are all single-cell, so cell-width == bar-width.
        let r = bare()
            .render(&ctx(Some(window(45.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.width(), 10);
    }

    #[test]
    fn from_extras_sets_width() {
        let extras = BTreeMap::from([("cells".to_string(), toml::Value::Integer(5))]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.width, 5);
    }

    #[test]
    fn from_extras_warns_on_zero_width() {
        let extras = BTreeMap::from([("cells".to_string(), toml::Value::Integer(0))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.width, DEFAULT_WIDTH);
        assert!(warnings.iter().any(|w| w.contains("cells")));
    }

    #[test]
    fn from_extras_warns_on_negative_width() {
        let extras = BTreeMap::from([("cells".to_string(), toml::Value::Integer(-1))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.width, DEFAULT_WIDTH);
        assert!(warnings.iter().any(|w| w.contains("cells")));
    }

    #[test]
    fn from_extras_reads_thresholds_table() {
        let mut t = toml::value::Table::new();
        t.insert("green".to_string(), toml::Value::Integer(30));
        t.insert("yellow".to_string(), toml::Value::Integer(70));
        let extras = BTreeMap::from([("thresholds".to_string(), toml::Value::Table(t))]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.thresholds.green(), 30);
        assert_eq!(seg.cfg.thresholds.yellow(), 70);
    }

    #[test]
    fn from_extras_accepts_high_pair_above_defaults() {
        // green=90 yellow=95 is a monotonic pair. The parser must read
        // both fields before validating, otherwise green would be
        // checked against the default yellow=80 and silently dropped.
        let mut t = toml::value::Table::new();
        t.insert("green".to_string(), toml::Value::Integer(90));
        t.insert("yellow".to_string(), toml::Value::Integer(95));
        let extras = BTreeMap::from([("thresholds".to_string(), toml::Value::Table(t))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.thresholds.green(), 90);
        assert_eq!(seg.cfg.thresholds.yellow(), 95);
        assert!(
            warnings.is_empty(),
            "no warnings expected; got {warnings:?}"
        );
    }

    #[test]
    fn from_extras_accepts_low_pair_below_defaults() {
        // Symmetric to high-pair: green=10 yellow=20 inverts against
        // the default green=50, would be wrongly rejected without
        // pair-based parsing.
        let mut t = toml::value::Table::new();
        t.insert("green".to_string(), toml::Value::Integer(10));
        t.insert("yellow".to_string(), toml::Value::Integer(20));
        let extras = BTreeMap::from([("thresholds".to_string(), toml::Value::Table(t))]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.thresholds.green(), 10);
        assert_eq!(seg.cfg.thresholds.yellow(), 20);
    }

    #[test]
    fn from_extras_rejects_inverted_pair_and_keeps_defaults() {
        // green=80 yellow=50 inverts the ramp. Both fields revert to
        // defaults; one warning describes the pair failure.
        let mut t = toml::value::Table::new();
        t.insert("green".to_string(), toml::Value::Integer(80));
        t.insert("yellow".to_string(), toml::Value::Integer(50));
        let extras = BTreeMap::from([("thresholds".to_string(), toml::Value::Table(t))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.thresholds.green(), DEFAULT_GREEN_THRESHOLD);
        assert_eq!(seg.cfg.thresholds.yellow(), DEFAULT_YELLOW_THRESHOLD);
        assert!(warnings
            .iter()
            .any(|w| w.contains("must be <=") && w.contains("ignoring both")));
    }

    #[test]
    fn from_extras_rejects_lone_green_against_default_yellow() {
        // Only green is set, and 90 > default yellow=80. Pair (90, 80)
        // is invalid; both revert to defaults.
        let mut t = toml::value::Table::new();
        t.insert("green".to_string(), toml::Value::Integer(90));
        let extras = BTreeMap::from([("thresholds".to_string(), toml::Value::Table(t))]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.thresholds.green(), DEFAULT_GREEN_THRESHOLD);
        assert_eq!(seg.cfg.thresholds.yellow(), DEFAULT_YELLOW_THRESHOLD);
    }

    #[test]
    fn from_extras_warns_when_threshold_out_of_range() {
        // Bad-typed/out-of-range field is warned individually; the
        // remaining good field plus the unchanged default still form
        // a valid pair, so the rest of the config survives.
        let mut t = toml::value::Table::new();
        t.insert("green".to_string(), toml::Value::Integer(150));
        let extras = BTreeMap::from([("thresholds".to_string(), toml::Value::Table(t))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.thresholds.green(), DEFAULT_GREEN_THRESHOLD);
        assert_eq!(seg.cfg.thresholds.yellow(), DEFAULT_YELLOW_THRESHOLD);
        assert!(warnings.iter().any(|w| w.contains("green")));
    }

    #[test]
    fn thresholds_new_rejects_inverted() {
        assert!(Thresholds::new(80, 50).is_none());
        assert!(Thresholds::new(50, 80).is_some());
        assert!(Thresholds::new(50, 50).is_some());
        assert!(Thresholds::new(0, 100).is_some());
        assert!(Thresholds::new(0, 101).is_none());
    }

    #[test]
    fn from_extras_reads_characters_table() {
        let mut c = toml::value::Table::new();
        c.insert("full".to_string(), toml::Value::String("#".to_string()));
        c.insert("partial".to_string(), toml::Value::String("=".to_string()));
        c.insert("empty".to_string(), toml::Value::String("-".to_string()));
        let extras = BTreeMap::from([("characters".to_string(), toml::Value::Table(c))]);
        let seg = strip_decoration(ContextBarSegment::from_extras(&extras, &mut |_| {}));
        let r = seg
            .render(&ctx(Some(window(45.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "####=-----");
    }

    #[test]
    fn custom_width_changes_bar_length() {
        let extras = BTreeMap::from([("cells".to_string(), toml::Value::Integer(5))]);
        let seg = strip_decoration(ContextBarSegment::from_extras(&extras, &mut |_| {}));
        let r = seg
            .render(&ctx(Some(window(40.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        // 40% of 5 = 2.0 → 2 full + 3 empty.
        assert_eq!(r.text(), "██░░░");
    }

    #[test]
    fn pct_is_rounded_before_threshold_so_text_and_bar_agree() {
        // 49.9 rounds to 50 → Warning. Without rounding, `pct < 50`
        // would keep the bar green while the textual segment renders
        // "50%" — the two segments must agree at every boundary.
        let r = ContextBarSegment::default()
            .render(&ctx(Some(window(49.9, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.style().role, Some(Role::Warning));
    }

    #[test]
    fn pct_is_rounded_so_high_fractional_paints_red_with_full_bar() {
        // 99.9 → rounds to 100; bar fully filled, role Error.
        let r = bare()
            .render(&ctx(Some(window(99.9, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "██████████");
        assert_eq!(r.style().role, Some(Role::Error));
    }

    #[test]
    fn frac_above_half_renders_partial_distinct_from_round() {
        // 47% of 10 cells = 4.7 → floor=4 + partial=1 = `████▓░░░░░`.
        // If bar_cells regressed to `round()` instead of `floor()`,
        // the same input would produce 5 full blocks and no partial:
        // `█████░░░░░`. The exact-string assertion catches that.
        let r = bare()
            .render(&ctx(Some(window(47.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "████▓░░░░░");
    }

    #[test]
    fn pct_round_ties_to_even_matches_format_rounding() {
        // 50.5 rounds to 50 under banker's (matches `format!("{:.0}",
        // 50.5_f32) == "50"`); plain `f32::round` would give 51 and
        // diverge from the textual `context_window` segment.
        let r = bare()
            .render(&ctx(Some(window(50.5, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        // 50% → Warning band (50 = green threshold), and 5/10 cells
        // filled exactly (frac=0).
        assert_eq!(r.text(), "█████░░░░░");
        assert_eq!(r.style().role, Some(Role::Warning));
    }

    #[test]
    fn cells_one_below_half_renders_empty_with_color_role() {
        // cells=1 + pct=30 → 0.3 cells filled → 0 full + 0 partial =
        // `░`. The bar is visually empty but the color role still
        // reflects fill (Success here). Pinning so a future change
        // that "fixes" empty bars by hiding them doesn't break the
        // contract that color is always set on a rendered bar.
        let extras = BTreeMap::from([("cells".to_string(), toml::Value::Integer(1))]);
        let seg = strip_decoration(ContextBarSegment::from_extras(&extras, &mut |_| {}));
        let r = seg
            .render(&ctx(Some(window(30.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "░");
        assert_eq!(r.style().role, Some(Role::Success));
    }

    #[test]
    fn frac_just_below_half_drops_partial_block() {
        // After rounding, integer pct only: 44 -> 4.4 cells -> 4 full + 0
        // partial. Locks the `>= 0.5` rule against a regression to `> 0.5`.
        let r = bare()
            .render(&ctx(Some(window(44.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "████░░░░░░");
    }

    #[test]
    fn from_extras_warns_on_non_string_character() {
        let mut c = toml::value::Table::new();
        c.insert("full".to_string(), toml::Value::Integer(42));
        let extras = BTreeMap::from([("characters".to_string(), toml::Value::Table(c))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.chars.full, DEFAULT_FULL.to_string());
        assert!(warnings
            .iter()
            .any(|w| w.contains("full") && w.contains("string")));
    }

    #[test]
    fn from_extras_rejects_multi_cell_glyph() {
        // Wide glyphs would desync RenderedSegment::width() from cfg.width.
        let mut c = toml::value::Table::new();
        c.insert("full".to_string(), toml::Value::String("漢".to_string()));
        let extras = BTreeMap::from([("characters".to_string(), toml::Value::Table(c))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.chars.full, DEFAULT_FULL.to_string());
        assert!(warnings.iter().any(|w| w.contains("single-cell")));
    }

    #[test]
    fn from_extras_partial_characters_override_leaves_others_default() {
        let mut c = toml::value::Table::new();
        c.insert("full".to_string(), toml::Value::String("#".to_string()));
        let extras = BTreeMap::from([("characters".to_string(), toml::Value::Table(c))]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.chars.full, "#");
        assert_eq!(seg.cfg.chars.partial, DEFAULT_PARTIAL.to_string());
        assert_eq!(seg.cfg.chars.empty, DEFAULT_EMPTY.to_string());
    }

    #[test]
    fn priority_drops_before_context_window() {
        // Higher number drops first; the bar must drop before the textual
        // segment so users keep the canonical health metric under pressure.
        let bar_pri = ContextBarSegment::default().defaults().priority;
        let window_pri = super::super::context_window::ContextWindowSegment
            .defaults()
            .priority;
        assert!(bar_pri > window_pri);
    }

    #[test]
    fn one_cell_width_renders_single_char() {
        let extras = BTreeMap::from([("cells".to_string(), toml::Value::Integer(1))]);
        let seg = strip_decoration(ContextBarSegment::from_extras(&extras, &mut |_| {}));
        let empty = seg
            .render(&ctx(Some(window(0.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(empty.text(), "░");
        let full = seg
            .render(&ctx(Some(window(100.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(full.text(), "█");
    }

    fn span_pairs(r: &RenderedSegment) -> Vec<(String, Option<Role>)> {
        r.spans()
            .expect("context_bar always renders spans")
            .iter()
            .map(|s| (s.text().to_string(), s.style().role))
            .collect()
    }

    #[test]
    fn default_renders_brackets_percentage_and_dim_trough() {
        // The ON-by-default shape: `[████░░░░░░] 42%`, matching the work
        // statusline / ccstatusline. 42% of 10 = 4 full + 6 empty.
        let r = ContextBarSegment::default()
            .render(&ctx(Some(window(42.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "[████░░░░░░] 42%");
        assert_eq!(r.style().role, Some(Role::Success));
    }

    #[test]
    fn default_spans_color_frame_fill_trough_and_percentage() {
        // Open bracket (Muted) + filled (Success) + dim trough then close
        // bracket coalesced (both Muted) + percentage (Success). The
        // trough+`]` merge because `from_spans` coalesces same-style runs.
        let r = ContextBarSegment::default()
            .render(&ctx(Some(window(42.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(
            span_pairs(&r),
            vec![
                ("[".to_string(), Some(FRAME_ROLE)),
                ("████".to_string(), Some(Role::Success)),
                ("░░░░░░]".to_string(), Some(DIM_ROLE)),
                (" 42%".to_string(), Some(Role::Success)),
            ]
        );
    }

    #[test]
    fn brackets_false_omits_delimiters() {
        let mut seg = ContextBarSegment::default();
        seg.cfg.brackets = false;
        let r = seg
            .render(&ctx(Some(window(42.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "████░░░░░░ 42%");
    }

    #[test]
    fn percentage_false_omits_suffix() {
        let mut seg = ContextBarSegment::default();
        seg.cfg.percentage = false;
        let r = seg
            .render(&ctx(Some(window(42.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "[████░░░░░░]");
    }

    #[test]
    fn dim_empty_false_colors_trough_with_threshold_role() {
        // With dim_empty off, filled+trough share the threshold role and
        // coalesce into one span; brackets stay Muted, percentage tracks
        // fill.
        let mut seg = ContextBarSegment::default();
        seg.cfg.dim_empty = false;
        let r = seg
            .render(&ctx(Some(window(42.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(
            span_pairs(&r),
            vec![
                ("[".to_string(), Some(FRAME_ROLE)),
                ("████░░░░░░".to_string(), Some(Role::Success)),
                ("]".to_string(), Some(FRAME_ROLE)),
                (" 42%".to_string(), Some(Role::Success)),
            ]
        );
    }

    #[test]
    fn all_decoration_off_collapses_to_a_single_styled_run() {
        // Brackets + percentage + dim all off → every cell shares the
        // threshold role, so from_spans folds to a single-style segment
        // (spans None): byte-identical to the pre-qol7 bar.
        let r = bare()
            .render(&ctx(Some(window(42.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert!(r.spans().is_none(), "single-style bar folds to no spans");
        assert_eq!(r.text(), "████░░░░░░");
        assert_eq!(r.style().role, Some(Role::Success));
    }

    #[test]
    fn percentage_uses_bankers_rounding_to_match_text() {
        // 50.5 → "50%" under banker's, matching the textual
        // `context_window` segment's `{:.0}` formatting.
        let mut seg = ContextBarSegment::default();
        seg.cfg.brackets = false;
        let r = seg
            .render(&ctx(Some(window(50.5, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert!(r.text().ends_with(" 50%"), "got {:?}", r.text());
    }

    #[test]
    fn from_extras_reads_open_close_chars() {
        let mut c = toml::value::Table::new();
        c.insert("open".to_string(), toml::Value::String("(".to_string()));
        c.insert("close".to_string(), toml::Value::String(")".to_string()));
        let extras = BTreeMap::from([("characters".to_string(), toml::Value::Table(c))]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.chars.open, "(");
        assert_eq!(seg.cfg.chars.close, ")");
        let r = seg
            .render(&ctx(Some(window(100.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "(██████████) 100%");
    }

    #[test]
    fn from_extras_reads_toggle_bools() {
        let extras = BTreeMap::from([
            ("brackets".to_string(), toml::Value::Boolean(false)),
            ("percentage".to_string(), toml::Value::Boolean(false)),
            ("dim_empty".to_string(), toml::Value::Boolean(false)),
        ]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert!(!seg.cfg.brackets);
        assert!(!seg.cfg.percentage);
        assert!(!seg.cfg.dim_empty);
    }

    #[test]
    fn from_extras_warns_on_non_bool_toggle() {
        let extras = BTreeMap::from([("percentage".to_string(), toml::Value::Integer(1))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert!(seg.cfg.percentage);
        assert!(warnings
            .iter()
            .any(|w| w.contains("percentage") && w.contains("boolean")));
    }

    #[test]
    fn from_extras_rejects_multi_cell_bracket() {
        let mut c = toml::value::Table::new();
        c.insert("open".to_string(), toml::Value::String("漢".to_string()));
        let extras = BTreeMap::from([("characters".to_string(), toml::Value::Table(c))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.chars.open, DEFAULT_OPEN.to_string());
        assert!(warnings
            .iter()
            .any(|w| w.contains("open") && w.contains("single-cell")));
    }

    #[test]
    fn from_extras_parses_eighth_fill() {
        let extras = BTreeMap::from([("fill".to_string(), toml::Value::String("eighth".into()))]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.fill, FillMode::Eighth);
    }

    #[test]
    fn braille_fill_swaps_full_and_empty_preset_glyphs() {
        // `fill = "braille"` sets the braille full/blank preset before
        // `characters.*` would override; with no override the bar uses
        // ⣿/⠀ and the eighth-style sub-cell ramp.
        let extras = BTreeMap::from([("fill".to_string(), toml::Value::String("braille".into()))]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.fill, FillMode::Braille);
        assert_eq!(seg.cfg.chars.full, "⣿");
        assert_eq!(seg.cfg.chars.empty, "⠀");
        let r = seg
            .render(&ctx(Some(window(100.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.text(), "[⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿] 100%");
    }

    #[test]
    fn characters_override_wins_over_fill_preset() {
        // An explicit `characters.full` overrides the braille preset
        // because `fill` is parsed first.
        let mut c = toml::value::Table::new();
        c.insert("full".to_string(), toml::Value::String("#".into()));
        let extras = BTreeMap::from([
            ("fill".to_string(), toml::Value::String("braille".into())),
            ("characters".to_string(), toml::Value::Table(c)),
        ]);
        let seg = ContextBarSegment::from_extras(&extras, &mut |_| {});
        assert_eq!(seg.cfg.chars.full, "#");
        assert_eq!(seg.cfg.chars.empty, "⠀");
    }

    #[test]
    fn from_extras_warns_on_unknown_fill() {
        let extras = BTreeMap::from([("fill".to_string(), toml::Value::String("rainbow".into()))]);
        let mut warnings = vec![];
        let seg = ContextBarSegment::from_extras(&extras, &mut |m| warnings.push(m.to_string()));
        assert_eq!(seg.cfg.fill, FillMode::Half);
        assert!(warnings.iter().any(|w| w.contains("fill")));
    }

    #[test]
    fn zero_percent_span_structure_is_all_trough() {
        // No fill span at 0%, so the open bracket, dim trough, and close
        // bracket all share FRAME_ROLE/DIM_ROLE (both Muted) and coalesce
        // into a single run; the percentage tracks the threshold role.
        let r = ContextBarSegment::default()
            .render(&ctx(Some(window(0.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(
            span_pairs(&r),
            vec![
                ("[░░░░░░░░░░]".to_string(), Some(DIM_ROLE)),
                (" 0%".to_string(), Some(Role::Success)),
            ]
        );
    }

    #[test]
    fn full_percent_span_structure_has_no_trough() {
        // No trough span at 100%; the close bracket must not merge into
        // the fill span (Error vs Muted roles differ).
        let r = ContextBarSegment::default()
            .render(&ctx(Some(window(100.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(
            span_pairs(&r),
            vec![
                ("[".to_string(), Some(FRAME_ROLE)),
                ("██████████".to_string(), Some(Role::Error)),
                ("]".to_string(), Some(FRAME_ROLE)),
                (" 100%".to_string(), Some(Role::Error)),
            ]
        );
    }

    #[test]
    fn decorated_render_width_matches_text_width() {
        // Layout trusts width(); the bracket + percentage spans must
        // contribute to it. "[████░░░░░░] 42%" = 16 cells.
        let r = ContextBarSegment::default()
            .render(&ctx(Some(window(42.0, 200_000))), &rc())
            .unwrap()
            .expect("rendered");
        assert_eq!(r.width(), 16);
        assert_eq!(r.width(), super::super::text_width(r.text()));
    }

    #[test]
    fn percentage_rounding_matches_format_at_more_boundaries() {
        // Banker's rounding: 49.5 → 50 (up to even), 50.4 → 50,
        // 50.6 → 51. Locks the rule against a swap to plain round().
        let mut seg = ContextBarSegment::default();
        seg.cfg.brackets = false;
        for (used, expected) in [(49.5, " 50%"), (50.4, " 50%"), (50.6, " 51%")] {
            let r = seg
                .render(&ctx(Some(window(used, 200_000))), &rc())
                .unwrap()
                .expect("rendered");
            assert!(
                r.text().ends_with(expected),
                "{used} → expected suffix {expected:?}, got {:?}",
                r.text()
            );
        }
    }
}
