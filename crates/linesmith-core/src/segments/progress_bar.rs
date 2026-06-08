//! Shared progress-bar renderer for `context_bar` and the rate-limit
//! `format = "progress"` segments. One bar implementation, two callers:
//! each maps its own config onto [`BarStyle`], supplies a per-render
//! fill [`Role`], and folds the returned spans into a `RenderedSegment`.
//! Returning [`StyledRun`]s (rather than a `RenderedSegment`) lets the
//! rate-limit caller prepend its `5h:` label span before composing.

use unicode_width::UnicodeWidthStr;

use crate::theme::{Role, Style, StyledRun};

/// Brackets and the dim trough render in [`Role::Muted`]: a neutral
/// frame that stays legible without competing with the threshold-
/// colored fill (the same role the powerline separator uses, chosen
/// there for the same "visible but recessive" reason).
pub(crate) const FRAME_ROLE: Role = Role::Muted;
pub(crate) const DIM_ROLE: Role = Role::Muted;

/// Eighth-block partial ramp (U+258F..U+2589): `▏▎▍▌▋▊▉` render 1/8
/// through 7/8 of a cell, with `█` as the 8/8 full cell.
pub(crate) const EIGHTH_RAMP: [&str; 7] = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"];

/// Braille partial ramp: a left-then-right column fill across the 2×4
/// dot grid (`⡀⡄⡆⡇` fill the left column bottom-up, `⣇⣧⣷` add the
/// right), with `⣿` as the full cell and `⠀` (blank braille) as empty.
pub(crate) const BRAILLE_RAMP: [&str; 7] = ["⡀", "⡄", "⡆", "⡇", "⣇", "⣧", "⣷"];

pub(crate) const DEFAULT_FULL: &str = "█";
pub(crate) const DEFAULT_EMPTY: &str = "░";
pub(crate) const DEFAULT_HALF: &str = "▓";
pub(crate) const DEFAULT_OPEN: &str = "[";
pub(crate) const DEFAULT_CLOSE: &str = "]";

/// Threshold percentages for the green→yellow→red color ramp.
/// `pct < green` → [`Role::Success`]; `green <= pct < yellow` →
/// [`Role::Warning`]; `pct >= yellow` → [`Role::Error`]. [`Self::new`]
/// enforces `green <= yellow <= 100`, so the ramp is monotonic by
/// type-system guarantee — no caller can mint an inverted ramp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Thresholds {
    green: u8,
    yellow: u8,
}

impl Thresholds {
    pub(crate) fn new(green: u8, yellow: u8) -> Option<Self> {
        (green <= yellow && yellow <= 100).then_some(Self { green, yellow })
    }

    pub(crate) fn green(self) -> u8 {
        self.green
    }

    pub(crate) fn yellow(self) -> u8 {
        self.yellow
    }

    pub(crate) fn role_for(self, pct: f64) -> Role {
        if pct < f64::from(self.green) {
            Role::Success
        } else if pct < f64::from(self.yellow) {
            Role::Warning
        } else {
            Role::Error
        }
    }
}

/// Sub-cell fill style, selected by `fill = "..."`. `Half` keeps a
/// single `▓`-style partial cell; `Whole` rounds to whole cells;
/// `Eighth`/`Braille` opt into the smooth ramps. Each preset carries
/// its own full/empty glyph defaults (see [`Self::preset_chars`]),
/// overridable via `characters.*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FillMode {
    #[default]
    Half,
    Whole,
    Eighth,
    Braille,
}

impl FillMode {
    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "half" => Some(Self::Half),
            "whole" => Some(Self::Whole),
            "eighth" => Some(Self::Eighth),
            "braille" => Some(Self::Braille),
            _ => None,
        }
    }

    pub(crate) fn preset_chars(self) -> (&'static str, &'static str) {
        match self {
            Self::Braille => ("⣿", "⠀"),
            _ => (DEFAULT_FULL, DEFAULT_EMPTY),
        }
    }

    pub(crate) fn partial(self, half_glyph: &str) -> PartialFill {
        match self {
            Self::Half => PartialFill::Half(half_glyph.to_string()),
            Self::Whole => PartialFill::Round,
            Self::Eighth => {
                PartialFill::Ramp(EIGHTH_RAMP.iter().map(|s| (*s).to_string()).collect())
            }
            Self::Braille => {
                PartialFill::Ramp(BRAILLE_RAMP.iter().map(|s| (*s).to_string()).collect())
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PartialFill {
    /// Round to the nearest whole cell; no partial glyph. The
    /// rate-limit progress default.
    Round,
    /// `floor` + one glyph when the fractional cell is at least half
    /// full. The context_bar default (glyph `▓`).
    Half(String),
    /// `floor` + one ramp glyph chosen by the fractional level, e.g.
    /// the eighth-block or braille ramp. `glyphs[i]` renders level
    /// `i + 1` of a cell split into `glyphs.len() + 1` levels.
    Ramp(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BarChars {
    pub(crate) full: String,
    pub(crate) empty: String,
    pub(crate) open: String,
    pub(crate) close: String,
    pub(crate) partial: PartialFill,
}

/// Everything the shared renderer needs except the fill [`Role`], which
/// the caller supplies per render (it derives from live usage).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BarStyle {
    pub(crate) width: u16,
    pub(crate) chars: BarChars,
    pub(crate) brackets: bool,
    /// Decimal places for the inline ` NN%` suffix; `None` omits it.
    pub(crate) percentage: Option<u8>,
    /// Render the empty trough in [`DIM_ROLE`] instead of the fill role.
    pub(crate) dim_empty: bool,
}

pub(crate) fn bar_cells(
    pct: f64,
    width: u16,
    partial: &PartialFill,
) -> (usize, Option<&str>, usize) {
    // Callers must pass a >= 1 width and a finite pct (both true for the
    // two live callers — `cells`/`progress_width` are validated >= 1, and
    // `pct` comes from a `Percent` or a clamped ratio). The asserts lock
    // those preconditions in the owning module; the `is_finite`
    // reassignment below sanitizes a non-finite `pct` to `0.0` (an empty
    // bar) before the geometry math, so neither the multiply nor the
    // `as usize` cast can see NaN in release builds.
    debug_assert!(width >= 1, "bar_cells requires width >= 1");
    debug_assert!(pct.is_finite(), "bar_cells requires a finite pct");
    let cells = usize::from(width);
    let pct = if pct.is_finite() { pct } else { 0.0 };
    let filled = (f64::from(width) * pct / 100.0).clamp(0.0, f64::from(width));
    match partial {
        PartialFill::Round => {
            let full = (filled.round() as usize).min(cells);
            (full, None, cells - full)
        }
        PartialFill::Half(glyph) => {
            let full = filled.floor() as usize;
            let glyph = (full < cells && filled.fract() >= 0.5).then_some(glyph.as_str());
            let used = full + usize::from(glyph.is_some());
            (full, glyph, cells.saturating_sub(used))
        }
        PartialFill::Ramp(glyphs) => {
            // Split each cell into `levels` sub-steps (8 for a 7-glyph
            // ramp), round the total to the nearest sub-step, then carry
            // whole cells out: `rem` indexes the partial glyph, `rem ==
            // 0` means the fill landed on a cell boundary (no partial).
            let levels = glyphs.len() + 1;
            let max_steps = cells * levels;
            let steps = ((filled * levels as f64).round() as usize).min(max_steps);
            let full = steps / levels;
            let rem = steps % levels;
            let glyph = (rem > 0).then(|| glyphs[rem - 1].as_str());
            (
                full,
                glyph,
                cells
                    .saturating_sub(full)
                    .saturating_sub(usize::from(glyph.is_some())),
            )
        }
    }
}

/// Render the bar as styled spans: optional open bracket ([`FRAME_ROLE`]),
/// filled cells (`fill_role`), the trough ([`DIM_ROLE`] when `dim_empty`,
/// else `fill_role`), optional close bracket, and an optional ` NN%`
/// suffix (`fill_role`). The caller folds these into a `RenderedSegment`
/// via `from_spans`, which coalesces adjacent same-style spans.
pub(crate) fn render_bar(pct: f64, style: &BarStyle, fill_role: Role) -> Vec<StyledRun> {
    let (full, partial, empty) = bar_cells(pct, style.width, &style.chars.partial);
    let mut filled = style.chars.full.repeat(full);
    if let Some(glyph) = partial {
        filled.push_str(glyph);
    }
    let trough = style.chars.empty.repeat(empty);
    let empty_role = if style.dim_empty { DIM_ROLE } else { fill_role };

    let mut spans: Vec<StyledRun> = Vec::with_capacity(5);
    if style.brackets {
        spans.push(StyledRun::new(
            style.chars.open.clone(),
            Style::role(FRAME_ROLE),
        ));
    }
    if !filled.is_empty() {
        spans.push(StyledRun::new(filled, Style::role(fill_role)));
    }
    if !trough.is_empty() {
        spans.push(StyledRun::new(trough, Style::role(empty_role)));
    }
    if style.brackets {
        spans.push(StyledRun::new(
            style.chars.close.clone(),
            Style::role(FRAME_ROLE),
        ));
    }
    if let Some(decimals) = style.percentage {
        let prec = usize::from(decimals);
        spans.push(StyledRun::new(
            format!(" {pct:.prec$}%"),
            Style::role(fill_role),
        ));
    }
    spans
}

/// Flat (uncolored) bar text — the concatenation of [`render_bar`]'s
/// span texts. For callers that share the bar's geometry and glyphs but
/// render it in a single flat role (e.g. the reset-timer progress bar,
/// where threshold color by elapsed time would be meaningless).
pub(crate) fn bar_text(pct: f64, style: &BarStyle) -> String {
    render_bar(pct, style, Role::Info)
        .iter()
        .map(StyledRun::text)
        .collect()
}

/// Validate a user-supplied single-cell glyph. `width == 1` (not `<= 1`)
/// is intentional: ZWJ sequences and combining marks render as one cell
/// on most terminals but `unicode-width` reports 0, which would desync
/// the bar from its configured width.
pub(crate) fn is_single_cell(s: &str) -> bool {
    UnicodeWidthStr::width(s) == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thresholds_role_ramp() {
        let t = Thresholds::new(50, 80).unwrap();
        assert_eq!(t.role_for(49.9), Role::Success);
        assert_eq!(t.role_for(50.0), Role::Warning);
        assert_eq!(t.role_for(79.9), Role::Warning);
        assert_eq!(t.role_for(80.0), Role::Error);
    }

    #[test]
    fn thresholds_new_rejects_inverted_or_out_of_range() {
        assert!(Thresholds::new(80, 50).is_none());
        assert!(Thresholds::new(0, 101).is_none());
        assert!(Thresholds::new(50, 50).is_some());
        assert!(Thresholds::new(0, 100).is_some());
    }

    #[test]
    fn round_partial_has_no_glyph_and_rounds_to_whole_cells() {
        // 45% of 10 = 4.5 → rounds to 5 full, no partial (rate-limit).
        assert_eq!(bar_cells(45.0, 10, &PartialFill::Round), (5, None, 5));
        // 44% of 10 = 4.4 → rounds to 4.
        assert_eq!(bar_cells(44.0, 10, &PartialFill::Round), (4, None, 6));
        assert_eq!(bar_cells(100.0, 10, &PartialFill::Round), (10, None, 0));
        assert_eq!(bar_cells(0.0, 10, &PartialFill::Round), (0, None, 10));
    }

    #[test]
    fn half_partial_matches_context_bar_geometry() {
        let half = PartialFill::Half(DEFAULT_HALF.to_string());
        // 45% → 4.5 → 4 full + ▓ + 5 empty.
        assert_eq!(bar_cells(45.0, 10, &half), (4, Some("▓"), 5));
        // 42% → 4.2 → 4 full, no partial.
        assert_eq!(bar_cells(42.0, 10, &half), (4, None, 6));
        // 44% → 4.4 → frac < 0.5, no partial.
        assert_eq!(bar_cells(44.0, 10, &half), (4, None, 6));
        assert_eq!(bar_cells(100.0, 10, &half), (10, None, 0));
    }

    #[test]
    fn eighth_ramp_picks_glyph_by_fractional_eighth() {
        let ramp = PartialFill::Ramp(EIGHTH_RAMP.iter().map(|s| s.to_string()).collect());
        // 41.25% of 10 = 4.125 cells = 4 full + 1/8 → ▏.
        assert_eq!(bar_cells(41.25, 10, &ramp), (4, Some("▏"), 5));
        // 48.75% = 4.875 = 4 full + 7/8 → ▉.
        assert_eq!(bar_cells(48.75, 10, &ramp), (4, Some("▉"), 5));
        // 50% = 5.0 exactly → 5 full, no partial.
        assert_eq!(bar_cells(50.0, 10, &ramp), (5, None, 5));
        assert_eq!(bar_cells(100.0, 10, &ramp), (10, None, 0));
    }

    #[test]
    fn braille_ramp_is_seven_levels_plus_full() {
        let ramp = PartialFill::Ramp(BRAILLE_RAMP.iter().map(|s| s.to_string()).collect());
        // 4 full + first braille level.
        assert_eq!(bar_cells(41.25, 10, &ramp), (4, Some("⡀"), 5));
        // Mid-ramp: 4 full + 4/8 → braille level 4 (⡇).
        assert_eq!(bar_cells(45.0, 10, &ramp), (4, Some("⡇"), 5));
        assert_eq!(bar_cells(100.0, 10, &ramp), (10, None, 0));
    }

    #[test]
    fn ramp_rounds_near_full_fraction_up_into_a_whole_cell() {
        // 49.5% of 10 = 4.95 cells → round(39.6 eighths) = 40 = 5 cells
        // exactly, so the fraction carries to a whole cell (no partial).
        let eighth = PartialFill::Ramp(EIGHTH_RAMP.iter().map(|s| s.to_string()).collect());
        assert_eq!(bar_cells(49.5, 10, &eighth), (5, None, 5));
        let braille = PartialFill::Ramp(BRAILLE_RAMP.iter().map(|s| s.to_string()).collect());
        assert_eq!(bar_cells(49.5, 10, &braille), (5, None, 5));
    }

    fn ctx_style() -> BarStyle {
        BarStyle {
            width: 10,
            chars: BarChars {
                full: DEFAULT_FULL.to_string(),
                empty: DEFAULT_EMPTY.to_string(),
                open: DEFAULT_OPEN.to_string(),
                close: DEFAULT_CLOSE.to_string(),
                partial: PartialFill::Half(DEFAULT_HALF.to_string()),
            },
            brackets: true,
            percentage: Some(0),
            dim_empty: true,
        }
    }

    fn texts(spans: &[StyledRun]) -> Vec<(&str, Option<Role>)> {
        spans.iter().map(|s| (s.text(), s.style().role)).collect()
    }

    #[test]
    fn render_bar_emits_bracket_fill_trough_percentage_spans() {
        let spans = render_bar(42.0, &ctx_style(), Role::Success);
        assert_eq!(
            texts(&spans),
            vec![
                ("[", Some(FRAME_ROLE)),
                ("████", Some(Role::Success)),
                ("░░░░░░", Some(DIM_ROLE)),
                ("]", Some(FRAME_ROLE)),
                (" 42%", Some(Role::Success)),
            ]
        );
    }

    #[test]
    fn render_bar_rate_limit_shape_round_no_brackets_one_decimal() {
        let style = BarStyle {
            width: 20,
            chars: BarChars {
                full: DEFAULT_FULL.to_string(),
                empty: DEFAULT_EMPTY.to_string(),
                open: DEFAULT_OPEN.to_string(),
                close: DEFAULT_CLOSE.to_string(),
                partial: PartialFill::Round,
            },
            brackets: false,
            percentage: Some(1),
            dim_empty: false,
        };
        // 33% of 20 = 6.6 → rounds to 7 full + 13 empty; flat role (no
        // dim), one-decimal suffix — the pre-s0vw rate-limit shape.
        let spans = render_bar(33.0, &style, Role::Info);
        let joined: String = spans.iter().map(StyledRun::text).collect();
        assert_eq!(joined, "███████░░░░░░░░░░░░░ 33.0%");
        assert!(spans.iter().all(|s| s.style().role == Some(Role::Info)));
    }
}
