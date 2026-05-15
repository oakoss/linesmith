//! Typed events the layout engine emits at its five silent decision
//! sites — priority-drop, `shrink_to_fit` success, truncatable
//! end-ellipsis reflow, `apply_width_bounds` under-min drop, and
//! `apply_width_bounds` over-max truncate — per ADR-0026.
//!
//! Production stdout pays zero observable cost: the engine emits via
//! `LayoutObservers::emit_with(impl FnOnce -> LayoutDecision)` (lsm-rbvv),
//! so `LayoutDecision` construction is deferred behind the
//! observer-presence check. The TUI live preview (lsm-dtdq) collects
//! these events to render per-segment status badges.
//!
//! The enum itself is intentionally NOT `#[non_exhaustive]` —
//! consumers' exhaustive `match` should break at compile time when a
//! sixth variant lands. Per-variant struct bodies ARE
//! `#[non_exhaustive]` so the engine can add a sixth field without
//! breaking pattern-matchers.

use std::borrow::Cow;

/// Event emitted by the layout engine when it takes one of five
/// decisions under width pressure. The `id` field on every variant
/// is borrowed from [`crate::segments::LineItem::Segment.id`] (per
/// ADR-0026); built-in segment ids carry as `Cow::Borrowed` so the
/// production path stays allocation-free.
///
/// Engine emit sites use the `pub(crate)` constructors below to pick
/// up the `debug_assert!`'d width-relation invariants for free.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutDecision {
    /// The reflow loop dropped a segment outright (no shrink form
    /// via `try_shrink`, no truncatable end-ellipsis feasible via
    /// `try_reflow`, or simply not `truncatable`). Fires whenever
    /// the engine removes a segment under width pressure.
    #[non_exhaustive]
    PriorityDrop {
        id: Cow<'static, str>,
        /// Engine `defaults().priority` at drop time. `> 0`
        /// (priority-0 is pinned per `highest_priority_droppable`).
        priority: u8,
        /// Terminal cell budget at the call site
        /// (`apply_layout`'s `terminal_width` parameter).
        terminal_width: u16,
        /// `total_width() - budget` at loop entry; `>= 1`.
        /// `u32` mirrors `total_width()`'s overflow-safe accumulator
        /// so a layout with many wide segments can't wrap `u16`.
        overflow: u32,
        /// Pre-drop `rendered.width` of the segment removed
        /// (not the line delta). `>= 1`.
        dropped_width: u16,
    },
    /// `try_shrink` returned a valid compact render. `from` is the
    /// pre-shrink width, `to` is the post-shrink width, `target` is
    /// the width the reflow loop asked the segment to fit under.
    /// Invariant: `to <= target < from`.
    #[non_exhaustive]
    ShrinkApplied {
        id: Cow<'static, str>,
        from: u16,
        to: u16,
        target: u16,
    },
    /// `try_reflow` succeeded with an end-ellipsis on a `truncatable`
    /// segment. Same `from`/`to`/`target` shape as `ShrinkApplied`.
    #[non_exhaustive]
    ReflowApplied {
        id: Cow<'static, str>,
        from: u16,
        to: u16,
        target: u16,
    },
    /// `apply_width_bounds` returned `None` because the rendered
    /// width fell below the configured `width.min` — the segment is
    /// hidden rather than displayed at a wrong width.
    #[non_exhaustive]
    WidthBoundUnderMinDrop {
        id: Cow<'static, str>,
        rendered_width: u16,
        min: u16,
    },
    /// `apply_width_bounds` clipped a too-wide render via
    /// `truncate_to` (end-ellipsis) because `rendered_width > max`.
    /// Emitted at the `apply_width_bounds` call site, NOT inside
    /// `truncate_to` itself (which is also reached from `try_reflow`,
    /// where the emit is `ReflowApplied`).
    #[non_exhaustive]
    WidthBoundOverMaxTruncate {
        id: Cow<'static, str>,
        rendered_width: u16,
        max: u16,
    },
}

// The `pub(crate)` constructors below have no production callers
// until lsm-b00q wires them at the five emit sites in `apply_layout`.
// Tests cover them, but `cargo clippy --lib` excludes #[cfg(test)] from
// dead-code analysis. The allow lifts once lsm-b00q lands.
#[allow(dead_code)]
impl LayoutDecision {
    /// Engine-recommended remediation phrasing for the decision, or
    /// `None` when the decision doesn't have a user-actionable fix.
    /// Returns `&'static str` because every remediation is a literal;
    /// reach for `format!` and the signature stops compiling, which
    /// keeps the table in one place and testable.
    ///
    /// Per-variant match (no `_` arm) so a future sixth variant
    /// forces the author to declare its remediation intent at
    /// compile time rather than silently inheriting `None`.
    #[must_use]
    pub fn remediation(&self) -> Option<&'static str> {
        match self {
            Self::ShrinkApplied { .. } => Some("Set `width.max` to clamp earlier"),
            Self::WidthBoundOverMaxTruncate { .. } => {
                Some("Increase `width.max` or lower `priority`")
            }
            Self::PriorityDrop { .. } => None,
            Self::ReflowApplied { .. } => None,
            Self::WidthBoundUnderMinDrop { .. } => None,
        }
    }

    /// `priority > 0`: `apply_layout`'s `highest_priority_droppable` filter
    /// excludes priority-0 segments; the assertion catches a future bypass.
    /// `overflow >= 1` and `dropped_width >= 1` mirror the engine invariants:
    /// the loop only enters this branch when `total > budget`, and a zero-width
    /// drop would be semantically empty.
    #[must_use]
    pub(crate) fn priority_drop(
        id: Cow<'static, str>,
        priority: u8,
        terminal_width: u16,
        overflow: u32,
        dropped_width: u16,
    ) -> Self {
        debug_assert!(
            priority > 0 && overflow >= 1 && dropped_width >= 1,
            "PriorityDrop invariants: priority>0, overflow>=1, dropped_width>=1 (got priority={priority}, overflow={overflow}, dropped_width={dropped_width})"
        );
        Self::PriorityDrop {
            id,
            priority,
            terminal_width,
            overflow,
            dropped_width,
        }
    }

    /// `apply_layout` enforces `overflow >= 1` so `target < from` holds;
    /// the assertion catches a future refactor that drops that precondition.
    #[must_use]
    pub(crate) fn shrink_applied(id: Cow<'static, str>, from: u16, to: u16, target: u16) -> Self {
        debug_assert!(
            to <= target && target < from,
            "ShrinkApplied requires to <= target < from (got from={from}, to={to}, target={target})"
        );
        Self::ShrinkApplied {
            id,
            from,
            to,
            target,
        }
    }

    /// Same width-relation invariant as [`shrink_applied`](Self::shrink_applied).
    #[must_use]
    pub(crate) fn reflow_applied(id: Cow<'static, str>, from: u16, to: u16, target: u16) -> Self {
        debug_assert!(
            to <= target && target < from,
            "ReflowApplied requires to <= target < from (got from={from}, to={to}, target={target})"
        );
        Self::ReflowApplied {
            id,
            from,
            to,
            target,
        }
    }

    #[must_use]
    pub(crate) fn width_bound_under_min_drop(
        id: Cow<'static, str>,
        rendered_width: u16,
        min: u16,
    ) -> Self {
        debug_assert!(
            rendered_width < min,
            "WidthBoundUnderMinDrop requires rendered_width < min (got rendered_width={rendered_width}, min={min})"
        );
        Self::WidthBoundUnderMinDrop {
            id,
            rendered_width,
            min,
        }
    }

    #[must_use]
    pub(crate) fn width_bound_over_max_truncate(
        id: Cow<'static, str>,
        rendered_width: u16,
        max: u16,
    ) -> Self {
        debug_assert!(
            rendered_width > max,
            "WidthBoundOverMaxTruncate requires rendered_width > max (got rendered_width={rendered_width}, max={max})"
        );
        Self::WidthBoundOverMaxTruncate {
            id,
            rendered_width,
            max,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remediation_pins_per_variant_phrasing() {
        // Per-variant exhaustive: both `Some` arms quote the literal
        // verbatim (a swap of the two remediation strings would trip
        // here), and all three `None` arms are confirmed.
        let shrink = LayoutDecision::shrink_applied(Cow::Borrowed("git"), 20, 17, 17);
        let over_max = LayoutDecision::width_bound_over_max_truncate(Cow::Borrowed("git"), 50, 30);
        let drop = LayoutDecision::priority_drop(Cow::Borrowed("git"), 200, 80, 12, 6);
        let reflow = LayoutDecision::reflow_applied(Cow::Borrowed("git"), 20, 17, 17);
        let under_min = LayoutDecision::width_bound_under_min_drop(Cow::Borrowed("git"), 3, 5);

        assert_eq!(
            shrink.remediation(),
            Some("Set `width.max` to clamp earlier")
        );
        assert_eq!(
            over_max.remediation(),
            Some("Increase `width.max` or lower `priority`")
        );
        assert!(drop.remediation().is_none());
        assert!(reflow.remediation().is_none());
        assert!(under_min.remediation().is_none());
    }

    #[test]
    fn constructors_accept_invariant_boundaries() {
        // Boundary-acceptance table: pin the `==` and adjacent edges
        // of each invariant so a future refactor that tightens `<=`
        // to `<` (or vice versa) is caught.

        // Shrink/Reflow: `to == target` (the `<=` boundary) accepted.
        let _ = LayoutDecision::shrink_applied(Cow::Borrowed("a"), 20, 17, 17);
        let _ = LayoutDecision::reflow_applied(Cow::Borrowed("a"), 20, 17, 17);
        // Shrink/Reflow: `to < target` (other side of `<=`) accepted.
        let _ = LayoutDecision::shrink_applied(Cow::Borrowed("a"), 20, 16, 17);
        let _ = LayoutDecision::reflow_applied(Cow::Borrowed("a"), 20, 16, 17);

        // PriorityDrop: priority=1 (minimum non-zero) accepted, with
        // overflow=1 + dropped_width=1 at their minimum non-zero values.
        let _ = LayoutDecision::priority_drop(Cow::Borrowed("a"), 1, 80, 1, 1);

        // Width bounds: rendered = bound ± 1 accepted (the strict
        // `<`/`>` boundary).
        let _ = LayoutDecision::width_bound_under_min_drop(Cow::Borrowed("a"), 4, 5);
        let _ = LayoutDecision::width_bound_over_max_truncate(Cow::Borrowed("a"), 6, 5);
    }

    // Compile-time witness that `LayoutDecision: Eq`. ADR-0026 §C
    // requires the derive; `PartialEq` is exercised at runtime in
    // `derives_clone_debug_partial_eq` below, but `Eq` is a marker
    // trait with no runtime surface — so pin it at compile time.
    const _ASSERT_EQ_DERIVED: fn() = || {
        fn assert_eq<T: Eq>() {}
        assert_eq::<LayoutDecision>();
    };

    #[test]
    fn derives_clone_debug_partial_eq() {
        let d = LayoutDecision::shrink_applied(Cow::Borrowed("git"), 20, 17, 17);
        let cloned = d.clone();
        assert_eq!(d, cloned);
        let dbg = format!("{d:?}");
        assert!(dbg.contains("ShrinkApplied"), "got {dbg:?}");
        // Variant name is the load-bearing piece; pin the id-field
        // shape too so a debug-format regression that drops fields
        // surfaces.
        assert!(dbg.contains("id: \"git\""), "got {dbg:?}");
    }

    // The eleven #[should_panic] tests below depend on `debug_assert!`
    // firing, which doesn't happen under `cargo test --release`. Gate
    // each on `cfg(debug_assertions)` so release-profile runs stay green.

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "PriorityDrop invariants: priority>0, overflow>=1, dropped_width>=1")]
    fn priority_drop_panics_in_debug_when_priority_is_zero() {
        let _ = LayoutDecision::priority_drop(Cow::Borrowed("git"), 0, 80, 12, 6);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "PriorityDrop invariants: priority>0, overflow>=1, dropped_width>=1")]
    fn priority_drop_panics_when_overflow_is_zero() {
        let _ = LayoutDecision::priority_drop(Cow::Borrowed("git"), 200, 80, 0, 6);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "PriorityDrop invariants: priority>0, overflow>=1, dropped_width>=1")]
    fn priority_drop_panics_when_dropped_width_is_zero() {
        let _ = LayoutDecision::priority_drop(Cow::Borrowed("git"), 200, 80, 12, 0);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "ShrinkApplied requires to <= target < from")]
    fn shrink_applied_panics_when_target_not_below_from() {
        // target == from violates target < from.
        let _ = LayoutDecision::shrink_applied(Cow::Borrowed("git"), 20, 17, 20);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "ShrinkApplied requires to <= target < from")]
    fn shrink_applied_panics_when_to_above_target() {
        let _ = LayoutDecision::shrink_applied(Cow::Borrowed("git"), 20, 18, 17);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "ReflowApplied requires to <= target < from")]
    fn reflow_applied_panics_when_target_not_below_from() {
        let _ = LayoutDecision::reflow_applied(Cow::Borrowed("git"), 20, 17, 20);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "ReflowApplied requires to <= target < from")]
    fn reflow_applied_panics_when_to_above_target() {
        let _ = LayoutDecision::reflow_applied(Cow::Borrowed("git"), 20, 18, 17);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "ShrinkApplied requires to <= target < from")]
    fn shrink_applied_panics_when_target_above_from() {
        // target > from (target=21, from=20) — distinct failure mode
        // from target == from. A refactor that wrote `target <= from`
        // would catch case 2 (== from) but accept this case.
        let _ = LayoutDecision::shrink_applied(Cow::Borrowed("git"), 20, 17, 21);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "ReflowApplied requires to <= target < from")]
    fn reflow_applied_panics_when_target_above_from() {
        let _ = LayoutDecision::reflow_applied(Cow::Borrowed("git"), 20, 17, 21);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "WidthBoundUnderMinDrop requires rendered_width < min")]
    fn under_min_drop_panics_when_rendered_at_or_above_min() {
        let _ = LayoutDecision::width_bound_under_min_drop(Cow::Borrowed("git"), 5, 5);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "WidthBoundOverMaxTruncate requires rendered_width > max")]
    fn over_max_truncate_panics_when_rendered_at_or_below_max() {
        let _ = LayoutDecision::width_bound_over_max_truncate(Cow::Borrowed("git"), 30, 30);
    }
}
