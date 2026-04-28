//! Per-turn token segments: `tokens_input`, `tokens_output`,
//! `tokens_cached`, `tokens_total`. Each reads the corresponding field
//! from `ctx.status.context_window.current_usage` (the per-turn
//! breakdown from the most recent API call, see `TurnUsage`). All hide
//! when `current_usage` is None — either before the first API call in
//! a session, or when the schema didn't supply the key.
//!
//! Rather than one `tokens` segment with show/hide config toggles, we
//! split into four segments so users compose via `[line.segments]` the
//! same way they order other built-ins. The split matches ccstatusline
//! (see `docs/research/jsonl-data-source.md` §7 "ccstatusline widget
//! catalog").

use super::rate_limit_format::format_tokens;
use super::{RenderContext, RenderResult, RenderedSegment, Segment, SegmentDefaults};
use crate::data_context::DataContext;
use crate::input::TurnUsage;
use crate::theme::Role;

/// Same bucket as `cost` (192): per-turn token counts are useful
/// context but not time-critical, so they yield first under width
/// pressure.
const PRIORITY: u8 = 192;

pub struct TokensInputSegment;
pub struct TokensOutputSegment;
pub struct TokensCachedSegment;
pub struct TokensTotalSegment;

impl Segment for TokensInputSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(usage) = current_usage(ctx) else {
            crate::lsm_debug!("tokens_input: current_usage absent; hiding");
            return Ok(None);
        };
        Ok(Some(render("in", usage.input_tokens)))
    }
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

impl Segment for TokensOutputSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(usage) = current_usage(ctx) else {
            crate::lsm_debug!("tokens_output: current_usage absent; hiding");
            return Ok(None);
        };
        Ok(Some(render("out", usage.output_tokens)))
    }
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

impl Segment for TokensCachedSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(usage) = current_usage(ctx) else {
            crate::lsm_debug!("tokens_cached: current_usage absent; hiding");
            return Ok(None);
        };
        // Cache-related tokens for the turn: creation + read. Both are
        // input-side, so a single "cache" number tracks "how much did
        // caching do for us this turn" without the user having to sum
        // two fields themselves.
        let cached = usage
            .cache_creation_input_tokens
            .saturating_add(usage.cache_read_input_tokens);
        Ok(Some(render("cache", cached)))
    }
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

impl Segment for TokensTotalSegment {
    fn render(&self, ctx: &DataContext, _rc: &RenderContext) -> RenderResult {
        let Some(usage) = current_usage(ctx) else {
            crate::lsm_debug!("tokens_total: current_usage absent; hiding");
            return Ok(None);
        };
        let total = usage
            .input_tokens
            .saturating_add(usage.output_tokens)
            .saturating_add(usage.cache_creation_input_tokens)
            .saturating_add(usage.cache_read_input_tokens);
        Ok(Some(render("total", total)))
    }
    fn defaults(&self) -> SegmentDefaults {
        SegmentDefaults::with_priority(PRIORITY)
    }
}

fn current_usage(ctx: &DataContext) -> Option<&TurnUsage> {
    ctx.status.context_window.as_ref()?.current_usage.as_ref()
}

fn render(label: &'static str, count: u64) -> RenderedSegment {
    // Reuse the project-wide compact formatter so `1_900` renders as
    // `1.9k` (not `1k`) and a session's per-turn count stays consistent
    // with the rate-limit / JSONL token formatting elsewhere.
    RenderedSegment::new(format!("{label} {}", format_tokens(count))).with_role(Role::Muted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        ContextWindow, ModelInfo, Percent, StatusContext, Tool, TurnUsage, WorkspaceInfo,
    };
    use std::path::PathBuf;
    use std::sync::Arc;

    fn rc() -> RenderContext {
        RenderContext::new(80)
    }

    fn ctx(usage: Option<TurnUsage>) -> DataContext {
        DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "X".into(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            },
            context_window: Some(ContextWindow {
                used: Percent::new(0.0).unwrap(),
                size: 200_000,
                total_input_tokens: 0,
                total_output_tokens: 0,
                current_usage: usage,
            }),
            cost: None,
            effort: None,
            vim: None,
            output_style: None,
            agent_name: None,
            version: None,
            raw: Arc::new(serde_json::Value::Null),
        })
    }

    fn usage(input: u64, output: u64, cache_creation: u64, cache_read: u64) -> TurnUsage {
        TurnUsage {
            input_tokens: input,
            output_tokens: output,
            cache_creation_input_tokens: cache_creation,
            cache_read_input_tokens: cache_read,
        }
    }

    fn ctx_without_context_window() -> DataContext {
        DataContext::new(StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "X".into(),
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

    // format_tokens itself is covered in `rate_limit_format` tests;
    // here we only assert the segment-level integration.

    // --- tokens_input ---

    #[test]
    fn tokens_input_renders_current_usage_input_with_muted_role() {
        assert_eq!(
            TokensInputSegment
                .render(&ctx(Some(usage(2_000, 500, 0, 500))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("in 2.0k").with_role(Role::Muted))
        );
    }

    #[test]
    fn tokens_input_hidden_when_current_usage_absent() {
        assert_eq!(TokensInputSegment.render(&ctx(None), &rc()).unwrap(), None);
    }

    #[test]
    fn tokens_input_hidden_when_context_window_absent() {
        // Double-None path: no context_window at all short-circuits
        // before we even reach current_usage.
        assert_eq!(
            TokensInputSegment
                .render(&ctx_without_context_window(), &rc())
                .unwrap(),
            None
        );
    }

    // --- tokens_output ---

    #[test]
    fn tokens_output_renders_current_usage_output_with_muted_role() {
        assert_eq!(
            TokensOutputSegment
                .render(&ctx(Some(usage(2_000, 500, 0, 500))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("out 500").with_role(Role::Muted))
        );
    }

    #[test]
    fn tokens_output_hidden_when_current_usage_absent() {
        assert_eq!(TokensOutputSegment.render(&ctx(None), &rc()).unwrap(), None);
    }

    #[test]
    fn tokens_output_hidden_when_context_window_absent() {
        assert_eq!(
            TokensOutputSegment
                .render(&ctx_without_context_window(), &rc())
                .unwrap(),
            None
        );
    }

    // --- tokens_cached ---

    #[test]
    fn tokens_cached_sums_cache_creation_and_read_with_muted_role() {
        assert_eq!(
            TokensCachedSegment
                .render(&ctx(Some(usage(2_000, 500, 300, 700))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("cache 1.0k").with_role(Role::Muted))
        );
    }

    #[test]
    fn tokens_cached_renders_zero_when_both_fields_zero() {
        assert_eq!(
            TokensCachedSegment
                .render(&ctx(Some(usage(2_000, 500, 0, 0))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("cache 0").with_role(Role::Muted))
        );
    }

    #[test]
    fn tokens_cached_hidden_when_current_usage_absent() {
        assert_eq!(TokensCachedSegment.render(&ctx(None), &rc()).unwrap(), None);
    }

    #[test]
    fn tokens_cached_hidden_when_context_window_absent() {
        assert_eq!(
            TokensCachedSegment
                .render(&ctx_without_context_window(), &rc())
                .unwrap(),
            None
        );
    }

    // --- tokens_total ---

    #[test]
    fn tokens_total_sums_all_four_fields() {
        assert_eq!(
            TokensTotalSegment
                .render(&ctx(Some(usage(2_000, 500, 300, 700))), &rc())
                .unwrap(),
            Some(RenderedSegment::new("total 3.5k").with_role(Role::Muted))
        );
    }

    #[test]
    fn tokens_total_saturates_on_overflow() {
        // Four u64::MAX values can't co-exist in a real payload, but
        // the saturating chain must not panic on the adversarial input.
        assert_eq!(
            TokensTotalSegment
                .render(
                    &ctx(Some(usage(u64::MAX, u64::MAX, u64::MAX, u64::MAX))),
                    &rc()
                )
                .unwrap(),
            Some(
                RenderedSegment::new(format!("total {}", format_tokens(u64::MAX)))
                    .with_role(Role::Muted)
            )
        );
    }

    #[test]
    fn tokens_total_hidden_when_current_usage_absent() {
        assert_eq!(TokensTotalSegment.render(&ctx(None), &rc()).unwrap(), None);
    }

    #[test]
    fn tokens_total_hidden_when_context_window_absent() {
        assert_eq!(
            TokensTotalSegment
                .render(&ctx_without_context_window(), &rc())
                .unwrap(),
            None
        );
    }

    // --- defaults ---

    #[test]
    fn all_four_segments_share_priority() {
        assert_eq!(TokensInputSegment.defaults().priority, PRIORITY);
        assert_eq!(TokensOutputSegment.defaults().priority, PRIORITY);
        assert_eq!(TokensCachedSegment.defaults().priority, PRIORITY);
        assert_eq!(TokensTotalSegment.defaults().priority, PRIORITY);
    }
}
