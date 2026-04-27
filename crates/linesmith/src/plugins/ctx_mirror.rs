//! Converts a [`DataContext`] into the immutable rhai `Map` a plugin's
//! `render(ctx)` receives.
//!
//! Shape lives in `docs/specs/plugin-api.md` §ctx shape exposed to rhai.
//! Highlights: enum variants render as snake_case `kind` tags,
//! `Option<T>` becomes rhai `()`, lazy `Arc<Result<T, E>>` sources
//! become `#{ kind: "ok", data: ... } | #{ kind: "error", error: ... }`
//! tagged maps, and `ctx.git`'s nested `Option` collapses `Ok(None)`
//! to `#{ kind: "ok", data: () }`.
//!
//! Declared-dep gating: `ctx.settings|claude_json|usage|sessions|git`
//! only appear when the plugin's `@data_deps` header declared them.
//! Undeclared accessors return `()` on the rhai side (not an error).

use std::sync::OnceLock;

use rhai::{Array, Dynamic, Map};
use serde_json::Value as JsonValue;

use super::engine::{MAX_ARRAY_SIZE, MAX_EXPR_DEPTH, MAX_MAP_SIZE, MAX_STRING_SIZE};
use crate::data_context::{
    DataContext, DataDep, DirtyCounts, DirtyState, EndpointUsage, ExtraUsage, FiveHourWindow,
    GitContext, Head, JsonlUsage, RepoKind, SevenDayWindow, TokenCounts, UpstreamState,
    UsageBucket, UsageData,
};
use crate::input::{
    ContextWindow, CostMetrics, GitWorktree, ModelInfo, StatusContext, Tool, TurnUsage,
    WorkspaceInfo,
};
use crate::segments::RenderContext;

const ENV_WHITELIST: &[&str] = &["TERM", "COLORTERM", "NO_COLOR", "FORCE_COLOR", "LANG"];

/// Max nesting depth when converting server- or stdin-supplied JSON
/// into a rhai `Dynamic`. Caps host-side recursion so a nested-object
/// bomb cannot blow the stack — rhai's `MAX_EXPR_DEPTH` gates script
/// parsing, not the mirror walk. Tied to the same ceiling so script
/// and host walks share a consistent policy.
const MAX_JSON_DEPTH: usize = MAX_EXPR_DEPTH;

/// Controls which host-side caps apply during a JSON → rhai
/// conversion. The escape-hatch variant preserves the documented
/// round-trip of `ctx.status.raw` (see `docs/specs/plugin-api.md`);
/// stack safety is non-negotiable so the depth guard fires in both
/// postures, but breadth/string/key caps would silently break
/// plugins reading tool-specific fields through the escape hatch.
#[derive(Clone, Copy)]
enum CapPosture {
    /// Full caps: depth, map breadth, array breadth, string size, key
    /// size. For server-controlled payloads such as
    /// `ctx.usage.unknown_buckets` values where the source has no
    /// reason to emit unbounded content.
    Strict,
    /// Depth cap only. For `ctx.status.raw` — the escape hatch that
    /// must round-trip whatever the upstream tool emits.
    EscapeHatch,
}

impl CapPosture {
    fn is_strict(self) -> bool {
        matches!(self, CapPosture::Strict)
    }
}

/// Accumulated counts of host-side cap hits during one JSON conversion.
/// The walk mutates this in place so a pathological payload emits at
/// most one aggregated `lsm_warn!` per top-level call — a single line
/// instead of one per offending subtree. Counters use `saturating_add`
/// to stay correct on adversarial payloads even if the same category
/// fires more than `usize::MAX` times.
#[derive(Default)]
struct ConversionLimits {
    depth_collapsed: usize,
    map_truncated: usize,
    array_truncated: usize,
    string_truncated: usize,
    map_key_dropped: usize,
}

impl ConversionLimits {
    fn is_empty(&self) -> bool {
        self.depth_collapsed == 0
            && self.map_truncated == 0
            && self.array_truncated == 0
            && self.string_truncated == 0
            && self.map_key_dropped == 0
    }

    fn emit_warn(&self, label: &str) {
        if self.is_empty() {
            return;
        }
        let mut parts: Vec<String> = Vec::new();
        if self.depth_collapsed > 0 {
            parts.push(format!(
                "{} subtree(s) collapsed at depth {MAX_JSON_DEPTH}",
                self.depth_collapsed
            ));
        }
        if self.map_truncated > 0 {
            parts.push(format!(
                "{} map(s) truncated at {MAX_MAP_SIZE} entries",
                self.map_truncated
            ));
        }
        if self.array_truncated > 0 {
            parts.push(format!(
                "{} array(s) truncated at {MAX_ARRAY_SIZE} items",
                self.array_truncated
            ));
        }
        if self.string_truncated > 0 {
            parts.push(format!(
                "{} string(s) truncated at {MAX_STRING_SIZE} bytes",
                self.string_truncated
            ));
        }
        if self.map_key_dropped > 0 {
            parts.push(format!(
                "{} entries dropped for keys longer than {MAX_STRING_SIZE} bytes",
                self.map_key_dropped
            ));
        }
        crate::lsm_warn!("{label}: {}", parts.join("; "));
    }
}

/// Build the `ctx` value a plugin's `render(ctx)` function sees.
///
/// `declared_deps` gates which lazy sources get mirrored. `config` is
/// the plugin's `[segments.<id>]` TOML table already converted to a
/// rhai-compatible `Dynamic` (use `()` when no table is configured).
/// `rc` is the per-render layout state surfaced as `ctx.render` —
/// `ctx.render.terminal_width` today, more fields later.
pub fn build_ctx(
    dc: &DataContext,
    rc: &RenderContext,
    declared_deps: &[DataDep],
    config: Dynamic,
) -> Dynamic {
    let mut map = Map::new();
    map.insert("status".into(), build_status(&dc.status));
    map.insert("config".into(), config);
    map.insert("env".into(), env_snapshot());
    map.insert("render".into(), build_render(rc));

    let declared = |d: DataDep| declared_deps.contains(&d);

    if declared(DataDep::Settings) {
        let arc = dc.settings();
        let value = match &*arc {
            Ok(_) => tagged_ok(Dynamic::from_map(Map::new())),
            Err(e) => tagged_error(e.code()),
        };
        map.insert("settings".into(), value);
    }
    if declared(DataDep::ClaudeJson) {
        let arc = dc.claude_json();
        let value = match &*arc {
            Ok(_) => tagged_ok(Dynamic::from_map(Map::new())),
            Err(e) => tagged_error(e.code()),
        };
        map.insert("claude_json".into(), value);
    }
    if declared(DataDep::Usage) {
        let arc = dc.usage();
        let value = match &*arc {
            Ok(data) => tagged_ok(build_usage_data(data)),
            Err(e) => tagged_error(e.code()),
        };
        map.insert("usage".into(), value);
    }
    if declared(DataDep::Sessions) {
        let arc = dc.sessions();
        let value = match &*arc {
            Ok(_) => tagged_ok(Dynamic::from_map(Map::new())),
            Err(e) => tagged_error(e.code()),
        };
        map.insert("sessions".into(), value);
    }
    if declared(DataDep::Git) {
        let arc = dc.git();
        let value = match &*arc {
            // Ok(Some) → data: <map>; Ok(None) → data: () per
            // plugin-api.md §Special cases (no-git-repo distinct from
            // gix failure).
            Ok(Some(gc)) => tagged_ok(build_git_context(gc)),
            Ok(None) => tagged_ok(Dynamic::UNIT),
            Err(e) => tagged_error(e.code()),
        };
        map.insert("git".into(), value);
    }

    Dynamic::from_map(map)
}

// --- RenderContext mirror ------------------------------------------------

fn build_render(rc: &RenderContext) -> Dynamic {
    let mut m = Map::new();
    m.insert(
        "terminal_width".into(),
        Dynamic::from_int(i64::from(rc.terminal_width)),
    );
    Dynamic::from_map(m)
}

// --- StatusContext mirror ------------------------------------------------

fn build_status(s: &StatusContext) -> Dynamic {
    let mut m = Map::new();
    m.insert("tool".into(), build_tool(&s.tool));
    m.insert("model".into(), build_model(&s.model));
    m.insert("workspace".into(), build_workspace(&s.workspace));
    m.insert(
        "context_window".into(),
        s.context_window
            .as_ref()
            .map_or(Dynamic::UNIT, build_context_window),
    );
    m.insert(
        "cost".into(),
        s.cost.as_ref().map_or(Dynamic::UNIT, build_cost),
    );
    // `rate_limits` is not on StatusContext; plugins read `ctx.usage`
    // via `DataDep::Usage` (see plugin-api.md §ctx.usage shape).
    m.insert(
        "effort".into(),
        s.effort
            .map_or(Dynamic::UNIT, |e| Dynamic::from(e.as_str().to_string())),
    );
    m.insert("raw".into(), json_to_dynamic(&s.raw));
    Dynamic::from_map(m)
}

fn build_tool(t: &Tool) -> Dynamic {
    let mut m = Map::new();
    let (kind, name) = match t {
        Tool::ClaudeCode => ("claude_code", None),
        Tool::QwenCode => ("qwen_code", None),
        Tool::CodexCli => ("codex_cli", None),
        Tool::CopilotCli => ("copilot_cli", None),
        Tool::Other(n) => ("other", Some(n.to_string())),
    };
    m.insert("kind".into(), Dynamic::from(kind.to_string()));
    if let Some(n) = name {
        m.insert("name".into(), Dynamic::from(n));
    }
    Dynamic::from_map(m)
}

fn build_model(m: &ModelInfo) -> Dynamic {
    let mut out = Map::new();
    out.insert("display_name".into(), Dynamic::from(m.display_name.clone()));
    Dynamic::from_map(out)
}

fn build_workspace(w: &WorkspaceInfo) -> Dynamic {
    let mut m = Map::new();
    m.insert(
        "project_dir".into(),
        Dynamic::from(w.project_dir.to_string_lossy().into_owned()),
    );
    m.insert(
        "git_worktree".into(),
        w.git_worktree
            .as_ref()
            .map_or(Dynamic::UNIT, build_worktree),
    );
    Dynamic::from_map(m)
}

fn build_worktree(wt: &GitWorktree) -> Dynamic {
    let mut m = Map::new();
    m.insert("name".into(), Dynamic::from(wt.name.clone()));
    m.insert(
        "path".into(),
        Dynamic::from(wt.path.to_string_lossy().into_owned()),
    );
    Dynamic::from_map(m)
}

fn build_context_window(cw: &ContextWindow) -> Dynamic {
    let mut m = Map::new();
    m.insert("used".into(), Dynamic::from(f64::from(cw.used.value())));
    m.insert(
        "remaining".into(),
        Dynamic::from(f64::from(cw.remaining().value())),
    );
    m.insert("size".into(), int_from_u64(cw.size));
    m.insert(
        "total_input_tokens".into(),
        int_from_u64(cw.total_input_tokens),
    );
    m.insert(
        "total_output_tokens".into(),
        int_from_u64(cw.total_output_tokens),
    );
    m.insert(
        "current_usage".into(),
        cw.current_usage
            .as_ref()
            .map_or(Dynamic::UNIT, build_turn_usage),
    );
    Dynamic::from_map(m)
}

fn build_turn_usage(u: &TurnUsage) -> Dynamic {
    // Destructure so a new field on TurnUsage fails to compile here
    // rather than silently dropping from the rhai mirror.
    let TurnUsage {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
    } = u;
    let mut m = Map::new();
    m.insert("input_tokens".into(), int_from_u64(*input_tokens));
    m.insert("output_tokens".into(), int_from_u64(*output_tokens));
    m.insert(
        "cache_creation_input_tokens".into(),
        int_from_u64(*cache_creation_input_tokens),
    );
    m.insert(
        "cache_read_input_tokens".into(),
        int_from_u64(*cache_read_input_tokens),
    );
    Dynamic::from_map(m)
}

fn build_cost(c: &CostMetrics) -> Dynamic {
    let mut m = Map::new();
    m.insert("total_cost_usd".into(), Dynamic::from(c.total_cost_usd));
    m.insert(
        "total_duration_ms".into(),
        int_from_u64(c.total_duration_ms),
    );
    m.insert(
        "total_api_duration_ms".into(),
        int_from_u64(c.total_api_duration_ms),
    );
    m.insert(
        "total_lines_added".into(),
        int_from_u64(c.total_lines_added),
    );
    m.insert(
        "total_lines_removed".into(),
        int_from_u64(c.total_lines_removed),
    );
    Dynamic::from_map(m)
}

fn build_usage_data(data: &UsageData) -> Dynamic {
    // Tagged-map shape per `plugin-api.md` §ctx shape + ADR-0013:
    // `kind` discriminates the variant; the sibling fields differ
    // between `endpoint` and `jsonl`. Adding a new `UsageData`
    // variant surfaces here as a non-exhaustive match error.
    match data {
        UsageData::Endpoint(e) => build_endpoint_usage(e),
        UsageData::Jsonl(j) => build_jsonl_usage(j),
    }
}

fn build_endpoint_usage(e: &EndpointUsage) -> Dynamic {
    // Destructure so adding a field to EndpointUsage surfaces as a
    // compile error rather than silently dropping from the rhai mirror.
    let EndpointUsage {
        five_hour,
        seven_day,
        seven_day_opus,
        seven_day_sonnet,
        seven_day_oauth_apps,
        extra_usage,
        unknown_buckets,
    } = e;
    let mut m = Map::new();
    m.insert("kind".into(), Dynamic::from("endpoint".to_string()));
    m.insert(
        "five_hour".into(),
        five_hour.as_ref().map_or(Dynamic::UNIT, build_usage_bucket),
    );
    m.insert(
        "seven_day".into(),
        seven_day.as_ref().map_or(Dynamic::UNIT, build_usage_bucket),
    );
    m.insert(
        "seven_day_opus".into(),
        seven_day_opus
            .as_ref()
            .map_or(Dynamic::UNIT, build_usage_bucket),
    );
    m.insert(
        "seven_day_sonnet".into(),
        seven_day_sonnet
            .as_ref()
            .map_or(Dynamic::UNIT, build_usage_bucket),
    );
    m.insert(
        "seven_day_oauth_apps".into(),
        seven_day_oauth_apps
            .as_ref()
            .map_or(Dynamic::UNIT, build_usage_bucket),
    );
    m.insert(
        "extra_usage".into(),
        extra_usage
            .as_ref()
            .map_or(Dynamic::UNIT, build_extra_usage),
    );
    m.insert(
        "unknown_buckets".into(),
        build_unknown_buckets(unknown_buckets),
    );
    Dynamic::from_map(m)
}

fn build_jsonl_usage(j: &JsonlUsage) -> Dynamic {
    let mut m = Map::new();
    m.insert("kind".into(), Dynamic::from("jsonl".to_string()));
    m.insert(
        "five_hour".into(),
        j.five_hour
            .as_ref()
            .map_or(Dynamic::UNIT, build_five_hour_window),
    );
    m.insert("seven_day".into(), build_seven_day_window(&j.seven_day));
    Dynamic::from_map(m)
}

fn build_five_hour_window(w: &FiveHourWindow) -> Dynamic {
    // Destructure so adding a field to FiveHourWindow forces a mirror
    // update. `ends_at` is derived from `start` per the type's invariant
    // — we expose it to plugins alongside `start` since scripts rarely
    // want to recompute the derivation themselves.
    let FiveHourWindow { tokens, start } = w;
    let mut m = Map::new();
    m.insert("tokens".into(), build_token_counts(tokens));
    m.insert("start".into(), Dynamic::from(start.to_rfc3339()));
    m.insert("ends_at".into(), Dynamic::from(w.ends_at().to_rfc3339()));
    Dynamic::from_map(m)
}

fn build_seven_day_window(w: &SevenDayWindow) -> Dynamic {
    let SevenDayWindow { tokens } = w;
    let mut m = Map::new();
    m.insert("tokens".into(), build_token_counts(tokens));
    Dynamic::from_map(m)
}

fn build_token_counts(t: &TokenCounts) -> Dynamic {
    let TokenCounts {
        input,
        output,
        cache_creation,
        cache_read,
    } = t;
    let mut m = Map::new();
    m.insert("input".into(), int_from_u64(*input));
    m.insert("output".into(), int_from_u64(*output));
    m.insert("cache_creation".into(), int_from_u64(*cache_creation));
    m.insert("cache_read".into(), int_from_u64(*cache_read));
    m.insert("total".into(), int_from_u64(t.total()));
    Dynamic::from_map(m)
}

fn build_unknown_buckets(map: &std::collections::HashMap<String, JsonValue>) -> Dynamic {
    // Server-controlled payload: cap entry count at MAX_MAP_SIZE and
    // skip keys longer than MAX_STRING_SIZE. rhai's script-side limits
    // don't apply to host-constructed Maps, so a buggy or malicious
    // /api/oauth/usage response has to be bounded here. Sort keys so
    // truncation is deterministic across renders — HashMap iteration
    // order is process-randomized, and without sorting the surviving
    // subset would rotate per render, flickering a specific bucket in
    // and out of a plugin's view.
    let mut sorted: Vec<(&String, &JsonValue)> = map.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));

    let mut m = Map::new();
    let mut dropped_oversize_keys: usize = 0;
    let mut truncated = false;
    let mut limits = ConversionLimits::default();
    for (k, v) in sorted {
        // INVARIANT: the breadth and key-size guards must fire *before*
        // `json_to_dynamic_walk`. Reorder and this scope's `limits`
        // would double-count top-level truncation against the per-value
        // counters the `(values)` warn reports.
        if m.len() >= MAX_MAP_SIZE {
            truncated = true;
            break;
        }
        if k.len() > MAX_STRING_SIZE {
            dropped_oversize_keys = dropped_oversize_keys.saturating_add(1);
            continue;
        }
        m.insert(
            k.as_str().into(),
            json_to_dynamic_walk(v, 0, CapPosture::Strict, &mut limits),
        );
    }
    if truncated {
        crate::lsm_warn!(
            "ctx.usage.unknown_buckets: truncated to {MAX_MAP_SIZE} entries (source had {})",
            map.len(),
        );
    }
    if dropped_oversize_keys > 0 {
        crate::lsm_warn!(
            "ctx.usage.unknown_buckets: dropped {dropped_oversize_keys} entries with keys longer than {MAX_STRING_SIZE} bytes",
        );
    }
    limits.emit_warn("ctx.usage.unknown_buckets (values)");
    Dynamic::from_map(m)
}

fn build_usage_bucket(b: &UsageBucket) -> Dynamic {
    let UsageBucket {
        utilization,
        resets_at,
    } = b;
    let mut m = Map::new();
    m.insert(
        "utilization".into(),
        Dynamic::from(f64::from(utilization.value())),
    );
    m.insert(
        "resets_at".into(),
        resets_at.map_or(Dynamic::UNIT, |t| Dynamic::from(t.to_rfc3339())),
    );
    Dynamic::from_map(m)
}

fn build_extra_usage(x: &ExtraUsage) -> Dynamic {
    let ExtraUsage {
        is_enabled,
        utilization,
        monthly_limit,
        used_credits,
        currency,
    } = x;
    let mut m = Map::new();
    m.insert(
        "is_enabled".into(),
        is_enabled.map_or(Dynamic::UNIT, Dynamic::from),
    );
    m.insert(
        "utilization".into(),
        utilization.map_or(Dynamic::UNIT, |p| Dynamic::from(f64::from(p.value()))),
    );
    m.insert(
        "monthly_limit".into(),
        monthly_limit.map_or(Dynamic::UNIT, Dynamic::from),
    );
    m.insert(
        "used_credits".into(),
        used_credits.map_or(Dynamic::UNIT, Dynamic::from),
    );
    m.insert(
        "currency".into(),
        currency
            .as_deref()
            .map_or(Dynamic::UNIT, |c| Dynamic::from(c.to_string())),
    );
    Dynamic::from_map(m)
}

// --- GitContext mirror ---------------------------------------------------

fn build_git_context(gc: &GitContext) -> Dynamic {
    // Destructure so adding a field to GitContext surfaces as a
    // compile error here rather than silently dropping from the rhai
    // mirror. `..` covers the two private OnceCell fields surfaced
    // through `gc.dirty()` / `gc.upstream()` below.
    let GitContext {
        repo_kind,
        repo_path,
        head,
        ..
    } = gc;
    let mut m = Map::new();
    m.insert("repo_kind".into(), build_repo_kind(repo_kind));
    m.insert(
        "repo_path".into(),
        Dynamic::from(repo_path.to_string_lossy().into_owned()),
    );
    m.insert("head".into(), build_head(head));
    m.insert("dirty".into(), build_dirty(&gc.dirty()));
    m.insert("upstream".into(), build_upstream(&gc.upstream()));
    Dynamic::from_map(m)
}

fn build_repo_kind(kind: &RepoKind) -> Dynamic {
    let mut m = Map::new();
    match kind {
        RepoKind::Main => {
            m.insert("kind".into(), Dynamic::from("main".to_string()));
        }
        RepoKind::Bare => {
            m.insert("kind".into(), Dynamic::from("bare".to_string()));
        }
        RepoKind::Submodule => {
            m.insert("kind".into(), Dynamic::from("submodule".to_string()));
        }
        RepoKind::LinkedWorktree { name } => {
            m.insert("kind".into(), Dynamic::from("linked_worktree".to_string()));
            m.insert("name".into(), Dynamic::from(name.clone()));
        }
    }
    Dynamic::from_map(m)
}

fn build_head(head: &Head) -> Dynamic {
    let mut m = Map::new();
    m.insert("kind".into(), Dynamic::from(head.kind_str().to_string()));
    match head {
        Head::Branch(name) => {
            m.insert("name".into(), Dynamic::from(name.clone()));
        }
        Head::Detached(oid) => {
            m.insert("sha".into(), Dynamic::from(oid.to_string()));
        }
        Head::Unborn { symbolic_ref } => {
            m.insert("symbolic_ref".into(), Dynamic::from(symbolic_ref.clone()));
        }
        Head::OtherRef { full_name } => {
            m.insert("full_name".into(), Dynamic::from(full_name.clone()));
        }
    }
    Dynamic::from_map(m)
}

fn build_dirty(d: &DirtyState) -> Dynamic {
    let mut m = Map::new();
    match d {
        DirtyState::Clean => {
            m.insert("kind".into(), Dynamic::from("clean".to_string()));
        }
        DirtyState::Dirty(None) => {
            m.insert("kind".into(), Dynamic::from("dirty_uncounted".to_string()));
        }
        DirtyState::Dirty(Some(counts)) => {
            let DirtyCounts {
                staged,
                unstaged,
                untracked,
            } = counts;
            m.insert("kind".into(), Dynamic::from("dirty_counted".to_string()));
            m.insert("staged".into(), Dynamic::from(i64::from(*staged)));
            m.insert("unstaged".into(), Dynamic::from(i64::from(*unstaged)));
            m.insert("untracked".into(), Dynamic::from(i64::from(*untracked)));
        }
    }
    Dynamic::from_map(m)
}

fn build_upstream(u: &Option<UpstreamState>) -> Dynamic {
    let Some(u) = u else { return Dynamic::UNIT };
    let UpstreamState {
        ahead,
        behind,
        upstream_branch,
    } = u;
    let mut m = Map::new();
    m.insert("ahead".into(), Dynamic::from(i64::from(*ahead)));
    m.insert("behind".into(), Dynamic::from(i64::from(*behind)));
    m.insert(
        "upstream_branch".into(),
        Dynamic::from(upstream_branch.clone()),
    );
    Dynamic::from_map(m)
}

// --- Tagged Result helpers ------------------------------------------------

fn tagged_ok(data: Dynamic) -> Dynamic {
    let mut m = Map::new();
    m.insert("kind".into(), Dynamic::from("ok".to_string()));
    m.insert("data".into(), data);
    Dynamic::from_map(m)
}

fn tagged_error(code: &str) -> Dynamic {
    let mut m = Map::new();
    m.insert("kind".into(), Dynamic::from("error".to_string()));
    m.insert("error".into(), Dynamic::from(code.to_string()));
    Dynamic::from_map(m)
}

// --- env snapshot ---------------------------------------------------------

fn env_snapshot() -> Dynamic {
    // `var().ok()` collapses `NotPresent` and `NotUnicode` to the
    // same rhai `()`. Acceptable for the 5-key whitelist (TERM,
    // COLORTERM, NO_COLOR, FORCE_COLOR, LANG): a non-UTF-8 value in
    // any of these is already broken upstream, and plugins have no
    // safe way to act on garbage bytes.
    static SNAPSHOT: OnceLock<Dynamic> = OnceLock::new();
    SNAPSHOT
        .get_or_init(|| build_env_map(ENV_WHITELIST, |k| std::env::var(k).ok()))
        .clone()
}

fn build_env_map<F>(keys: &[&str], mut get: F) -> Dynamic
where
    F: FnMut(&str) -> Option<String>,
{
    let mut m = Map::new();
    for key in keys {
        let value = get(key).map_or(Dynamic::UNIT, Dynamic::from);
        m.insert((*key).into(), value);
    }
    Dynamic::from_map(m)
}

// --- serde_json::Value → rhai::Dynamic -----------------------------------

/// `Arc<serde_json::Value>` is the stdin escape hatch exposed as
/// `ctx.status.raw`. Convert recursively so plugins can read tool-
/// specific fields the canonical `StatusContext` doesn't model. The
/// depth guard runs so recursion is stack-safe, but no other cap
/// applies — see `CapPosture::EscapeHatch` and the plugin-api spec.
fn json_to_dynamic(v: &JsonValue) -> Dynamic {
    let mut limits = ConversionLimits::default();
    let out = json_to_dynamic_walk(v, 0, CapPosture::EscapeHatch, &mut limits);
    limits.emit_warn("ctx.status.raw");
    out
}

fn json_to_dynamic_walk(
    v: &JsonValue,
    depth: usize,
    posture: CapPosture,
    limits: &mut ConversionLimits,
) -> Dynamic {
    if depth >= MAX_JSON_DEPTH {
        // A nested-object bomb hits this guard before it blows the
        // Rust stack. Collapse the subtree to `()` so the rest of the
        // mirror survives. Note: `JsonValue::Null` also maps to `()`,
        // so plugins cannot distinguish truncated from genuinely-null
        // without the aggregated warn emitted by `emit_warn`.
        limits.depth_collapsed = limits.depth_collapsed.saturating_add(1);
        return Dynamic::UNIT;
    }
    match v {
        JsonValue::Null => Dynamic::UNIT,
        JsonValue::Bool(b) => Dynamic::from(*b),
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from(i)
            } else if let Some(f) = n.as_f64() {
                Dynamic::from(f)
            } else {
                // serde_json::Number is i64|u64|f64 internally; the
                // first two arms catch i64 and any f64-representable
                // value, leaving only u64 > i64::MAX. Round-trip via
                // f64 (precision loss is acceptable for an escape-
                // hatch field; rhai has no native u64).
                Dynamic::from(n.as_u64().map_or(0.0_f64, |u| u as f64))
            }
        }
        JsonValue::String(s) => {
            if posture.is_strict() && s.len() > MAX_STRING_SIZE {
                limits.string_truncated = limits.string_truncated.saturating_add(1);
                Dynamic::from(truncate_utf8(s, MAX_STRING_SIZE))
            } else {
                Dynamic::from(s.clone())
            }
        }
        JsonValue::Array(arr) => {
            let cap = if posture.is_strict() {
                MAX_ARRAY_SIZE
            } else {
                usize::MAX
            };
            let items: Array = arr
                .iter()
                .take(cap)
                .map(|item| json_to_dynamic_walk(item, depth + 1, posture, limits))
                .collect();
            if posture.is_strict() && arr.len() > MAX_ARRAY_SIZE {
                limits.array_truncated = limits.array_truncated.saturating_add(1);
            }
            Dynamic::from_array(items)
        }
        JsonValue::Object(obj) => {
            let strict = posture.is_strict();
            let mut m = Map::new();
            let mut iter_broke = false;
            for (k, val) in obj {
                if strict && m.len() >= MAX_MAP_SIZE {
                    iter_broke = true;
                    break;
                }
                if strict && k.len() > MAX_STRING_SIZE {
                    limits.map_key_dropped = limits.map_key_dropped.saturating_add(1);
                    continue;
                }
                m.insert(
                    k.as_str().into(),
                    json_to_dynamic_walk(val, depth + 1, posture, limits),
                );
            }
            if iter_broke {
                limits.map_truncated = limits.map_truncated.saturating_add(1);
            }
            Dynamic::from_map(m)
        }
    }
}

fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

fn int_from_u64(n: u64) -> Dynamic {
    // rhai integers are i64. A u64 > i64::MAX reaching this function
    // signals an upstream parser bug (no real Claude statusline field
    // is even close); clamp to i64::MAX so the render never panics.
    // Forensics survive on the raw stdin via `ctx.status.raw`.
    Dynamic::from(i64::try_from(n).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data_context::DataContext;
    use crate::input::{EffortLevel, Percent};
    use std::path::PathBuf;
    use std::sync::Arc;

    fn minimal_status() -> StatusContext {
        StatusContext {
            tool: Tool::ClaudeCode,
            model: ModelInfo {
                display_name: "Sonnet".to_string(),
            },
            workspace: WorkspaceInfo {
                project_dir: PathBuf::from("/repo"),
                git_worktree: None,
            },
            context_window: None,
            cost: None,
            effort: None,
            raw: Arc::new(serde_json::json!({"custom": "field"})),
        }
    }

    fn build_and_unwrap_map(dc: &DataContext, deps: &[DataDep]) -> Map {
        let rc = RenderContext::new(80);
        let dyn_ctx = build_ctx(dc, &rc, deps, Dynamic::UNIT);
        dyn_ctx.try_cast::<Map>().expect("ctx is a map")
    }

    fn status_map(ctx: &Map) -> Map {
        ctx.get("status")
            .expect("status key")
            .clone()
            .try_cast::<Map>()
            .expect("status is a map")
    }

    #[test]
    fn top_level_has_status_config_env() {
        let dc = DataContext::new(minimal_status());
        let ctx = build_and_unwrap_map(&dc, &[]);
        assert!(ctx.contains_key("status"));
        assert!(ctx.contains_key("config"));
        assert!(ctx.contains_key("env"));
    }

    #[test]
    fn undeclared_sources_absent() {
        let dc = DataContext::new(minimal_status());
        let ctx = build_and_unwrap_map(&dc, &[]);
        for key in ["settings", "claude_json", "usage", "sessions", "git"] {
            assert!(!ctx.contains_key(key), "{key} should not appear");
        }
    }

    #[test]
    fn usage_endpoint_mirror_preserves_every_field_plugins_depend_on() {
        // Plugin-facing contract: `ctx.usage.data.*` field names and
        // their scalar types are load-bearing. A rename in
        // `EndpointUsage` / `UsageBucket` / `ExtraUsage` must either
        // break this test or update it so spec drift surfaces in CI.
        use crate::data_context::{EndpointUsage, ExtraUsage, UsageBucket, UsageData};
        use chrono::{TimeZone, Utc};

        let mut unknown_buckets = std::collections::HashMap::new();
        unknown_buckets.insert("iguana_necktie".to_string(), serde_json::Value::Null);
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: Some(UsageBucket {
                utilization: Percent::new(42.0).unwrap(),
                resets_at: Some(Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).unwrap()),
            }),
            seven_day: Some(UsageBucket {
                utilization: Percent::new(33.0).unwrap(),
                resets_at: None,
            }),
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: Some(ExtraUsage {
                is_enabled: Some(true),
                utilization: Some(Percent::new(17.5).unwrap()),
                monthly_limit: Some(100.0),
                used_credits: Some(40.0),
                currency: Some("EUR".into()),
            }),
            unknown_buckets,
        });

        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);

        let wrapper: Map = ctx
            .get("usage")
            .expect("usage key")
            .clone()
            .try_cast()
            .expect("usage is a map");
        assert_eq!(
            wrapper
                .get("kind")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("ok".to_string()),
        );
        let payload: Map = wrapper
            .get("data")
            .expect("data payload")
            .clone()
            .try_cast()
            .expect("data is a map");

        assert_eq!(
            payload
                .get("kind")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("endpoint".to_string()),
        );
        let five: Map = payload
            .get("five_hour")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            five.get("utilization")
                .and_then(|d| d.clone().try_cast::<f64>()),
            Some(42.0),
        );
        assert!(five.get("resets_at").unwrap().is_string());
        let seven: Map = payload
            .get("seven_day")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert!(seven.get("resets_at").unwrap().is_unit());
        assert!(payload.get("seven_day_opus").unwrap().is_unit());
        let extra: Map = payload
            .get("extra_usage")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            extra
                .get("is_enabled")
                .and_then(|d| d.clone().try_cast::<bool>()),
            Some(true),
        );
        assert_eq!(
            extra
                .get("monthly_limit")
                .and_then(|d| d.clone().try_cast::<f64>()),
            Some(100.0),
        );
        assert_eq!(
            extra
                .get("currency")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("EUR".to_string()),
        );
        let unknown: Map = payload
            .get("unknown_buckets")
            .expect("unknown_buckets present")
            .clone()
            .try_cast()
            .unwrap();
        assert!(unknown.contains_key("iguana_necktie"));
    }

    #[test]
    fn usage_jsonl_variant_mirrors_tokens_and_ends_at() {
        // ADR-0013 + plugin-api.md §ctx shape: jsonl variant exposes
        // `kind: "jsonl"` with raw token counts and the 5h window's
        // `ends_at`. Plugins read this to render the JSONL fallback
        // — changes must break this test.
        use crate::data_context::{
            FiveHourWindow, JsonlUsage, SevenDayWindow, TokenCounts, UsageData,
        };
        use chrono::{TimeZone, Utc};
        let tokens = TokenCounts::from_parts(400_000, 20_000, 0, 0);
        // `start + 5h` = ends_at; encode as `start` per the invariant.
        let start = Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).unwrap();
        let ends_at = Utc.with_ymd_and_hms(2099, 1, 1, 5, 0, 0).unwrap();
        let data = UsageData::Jsonl(JsonlUsage::new(
            Some(FiveHourWindow::new(tokens, start)),
            SevenDayWindow::new(TokenCounts::from_parts(1_000_000, 0, 0, 0)),
        ));
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let payload: Map = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            payload
                .get("kind")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("jsonl".to_string()),
        );
        let five: Map = payload
            .get("five_hour")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            five.get("ends_at")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some(ends_at.to_rfc3339()),
        );
        let token_map: Map = five.get("tokens").unwrap().clone().try_cast().unwrap();
        assert_eq!(
            token_map
                .get("total")
                .and_then(|d| d.clone().try_cast::<i64>()),
            Some(420_000),
        );
        let seven: Map = payload
            .get("seven_day")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        let seven_tokens: Map = seven.get("tokens").unwrap().clone().try_cast().unwrap();
        assert_eq!(
            seven_tokens
                .get("input")
                .and_then(|d| d.clone().try_cast::<i64>()),
            Some(1_000_000),
        );
        // Every token category must round-trip so plugin scripts that
        // read `tokens.cache_read` etc. don't silently get `()`.
        for key in ["output", "cache_creation", "cache_read"] {
            assert!(
                seven_tokens.contains_key(key),
                "expected tokens.{key} on jsonl mirror",
            );
        }
        // `unknown_buckets` is Endpoint-only per ADR-0013. A plugin
        // that reads `ctx.usage.data.unknown_buckets` unconditionally
        // against JSONL data must see it missing, not an empty map.
        assert!(
            !payload.contains_key("unknown_buckets"),
            "jsonl variant must not expose unknown_buckets",
        );
    }

    #[test]
    fn usage_jsonl_variant_with_no_active_block_exposes_unit_five_hour() {
        // JSONL with `five_hour: None` (inactive block) must mirror as
        // `()` so rhai scripts can short-circuit with `if ctx.usage.data.five_hour != () { ... }`.
        use crate::data_context::{JsonlUsage, SevenDayWindow, TokenCounts, UsageData};
        let data = UsageData::Jsonl(JsonlUsage::new(
            None,
            SevenDayWindow::new(TokenCounts::default()),
        ));
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let payload: Map = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert!(
            payload.get("five_hour").unwrap().is_unit(),
            "jsonl five_hour=None must mirror as rhai ()",
        );
        // seven_day is always present even on an empty transcript.
        assert!(!payload.get("seven_day").unwrap().is_unit());
    }

    #[test]
    fn declared_source_shows_up_as_tagged_error_when_stub() {
        // Seeded to decouple from host-machine Keychain/network state;
        // the real cascade would otherwise hit the OAuth endpoint.
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Err(crate::data_context::UsageError::Jsonl(
            crate::data_context::JsonlError::NoEntries,
        )))
        .expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let usage: Map = ctx
            .get("usage")
            .expect("usage key")
            .clone()
            .try_cast()
            .expect("usage is a map");
        assert_eq!(
            usage
                .get("kind")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("error".to_string())
        );
        assert_eq!(
            usage
                .get("error")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("NoEntries".to_string())
        );
    }

    #[test]
    fn git_dep_maps_ok_none_to_unit_data() {
        // "Not in a git repo" surface: kind: "ok", data: () per
        // plugin-api.md §Special cases.
        let dc = DataContext::new(minimal_status());
        dc.preseed_git(Ok(None)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Git]);
        let git: Map = ctx.get("git").unwrap().clone().try_cast().unwrap();
        assert_eq!(
            git.get("kind").and_then(|d| d.clone().try_cast::<String>()),
            Some("ok".to_string())
        );
        assert!(git.get("data").expect("data present").is_unit());
    }

    #[test]
    fn git_dep_reports_error_variant_when_gix_failed() {
        use crate::data_context::GitError;
        let dc = DataContext::new(minimal_status());
        dc.preseed_git(Err(GitError::CorruptRepo {
            path: std::path::PathBuf::from("/tmp/bad"),
            message: "synthetic".into(),
        }))
        .expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Git]);
        let git: Map = ctx.get("git").unwrap().clone().try_cast().unwrap();
        assert_eq!(
            git.get("kind").and_then(|d| d.clone().try_cast::<String>()),
            Some("error".to_string())
        );
        assert_eq!(
            git.get("error")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("CorruptRepo".to_string())
        );
    }

    #[test]
    fn git_dep_maps_ok_some_to_populated_map() {
        use crate::data_context::{GitContext, Head, RepoKind};
        let dc = DataContext::new(minimal_status());
        dc.preseed_git(Ok(Some(GitContext::new(
            RepoKind::Main,
            std::path::PathBuf::from("/repo/.git"),
            Head::Branch("feature/auth".into()),
        ))))
        .expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Git]);
        let git: Map = ctx.get("git").unwrap().clone().try_cast().unwrap();
        assert_eq!(
            git.get("kind").and_then(|d| d.clone().try_cast::<String>()),
            Some("ok".to_string())
        );
        let data: Map = git.get("data").unwrap().clone().try_cast().unwrap();
        let kind: Map = data.get("repo_kind").unwrap().clone().try_cast().unwrap();
        assert_eq!(
            kind.get("kind")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("main".to_string())
        );
        let head: Map = data.get("head").unwrap().clone().try_cast().unwrap();
        assert_eq!(
            head.get("kind")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("branch".to_string())
        );
        assert_eq!(
            head.get("name")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("feature/auth".to_string())
        );
    }

    #[test]
    fn tool_claude_code_has_only_kind() {
        let dc = DataContext::new(minimal_status());
        let ctx = build_and_unwrap_map(&dc, &[]);
        let tool: Map = status_map(&ctx)
            .get("tool")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            tool.get("kind")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("claude_code".to_string())
        );
        assert!(!tool.contains_key("name"));
    }

    #[test]
    fn all_tool_variants_map_to_snake_case_kind() {
        // `build_tool`'s match is exhaustive, so a new `Tool` variant
        // fails to compile. This test pins the snake_case label each
        // variant maps to so an accidental label rename still trips.
        let cases: &[(Tool, &str)] = &[
            (Tool::ClaudeCode, "claude_code"),
            (Tool::QwenCode, "qwen_code"),
            (Tool::CodexCli, "codex_cli"),
            (Tool::CopilotCli, "copilot_cli"),
        ];
        for (tool, expected) in cases {
            let mut s = minimal_status();
            s.tool = tool.clone();
            let dc = DataContext::new(s);
            let ctx = build_and_unwrap_map(&dc, &[]);
            let map: Map = status_map(&ctx)
                .get("tool")
                .unwrap()
                .clone()
                .try_cast()
                .unwrap();
            assert_eq!(
                map.get("kind").and_then(|d| d.clone().try_cast::<String>()),
                Some((*expected).to_string()),
                "tool variant {tool:?}",
            );
            assert!(
                !map.contains_key("name"),
                "non-Other variant {tool:?} should not carry a name field"
            );
        }
    }

    #[test]
    fn tool_other_carries_forensic_name() {
        let mut status = minimal_status();
        status.tool = Tool::Other("gemini".into());
        let dc = DataContext::new(status);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let tool: Map = status_map(&ctx)
            .get("tool")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            tool.get("kind")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("other".to_string())
        );
        assert_eq!(
            tool.get("name")
                .and_then(|d| d.clone().try_cast::<String>()),
            Some("gemini".to_string())
        );
    }

    #[test]
    fn option_fields_become_unit_when_none() {
        let dc = DataContext::new(minimal_status());
        let ctx = build_and_unwrap_map(&dc, &[]);
        let status = status_map(&ctx);
        assert!(status.get("context_window").unwrap().is_unit());
        assert!(status.get("cost").unwrap().is_unit());
        assert!(status.get("effort").unwrap().is_unit());
        assert!(
            !status.contains_key("rate_limits"),
            "rate_limits is no longer mirrored; plugins read ctx.usage",
        );
    }

    #[test]
    fn effort_surfaces_as_snake_case_string() {
        let mut s = minimal_status();
        s.effort = Some(EffortLevel::XHigh);
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let effort = status_map(&ctx)
            .get("effort")
            .unwrap()
            .clone()
            .try_cast::<String>()
            .unwrap();
        assert_eq!(effort, "xhigh");
    }

    #[test]
    fn each_lazy_dep_surfaces_as_tagged_error_when_stub() {
        // Every non-Git lazy source independently builds its tagged
        // map. Test all four arms so a copy-paste bug (wrong source,
        // wrong key) is caught here rather than in a downstream plugin
        // that suddenly sees `()` instead of an error. Expected code
        // varies per source — stubs emit "NotImplemented"; Usage is
        // seeded with the JSONL sentinel so the test doesn't hit the
        // real cascade (Keychain + network) on dev machines.
        let cases: &[(DataDep, &str, &str)] = &[
            (DataDep::Settings, "settings", "NotImplemented"),
            (DataDep::ClaudeJson, "claude_json", "NotImplemented"),
            (DataDep::Sessions, "sessions", "NotImplemented"),
            (DataDep::Usage, "usage", "NoEntries"),
        ];
        for (dep, key, expected_code) in cases {
            let dc = DataContext::new(minimal_status());
            if matches!(dep, DataDep::Usage) {
                dc.preseed_usage(Err(crate::data_context::UsageError::Jsonl(
                    crate::data_context::JsonlError::NoEntries,
                )))
                .expect("seed");
            }
            let ctx = build_and_unwrap_map(&dc, &[*dep]);
            let entry: Map = ctx
                .get(*key)
                .unwrap_or_else(|| panic!("dep {dep:?} should populate `{key}`"))
                .clone()
                .try_cast()
                .expect("source map");
            assert_eq!(
                entry
                    .get("kind")
                    .and_then(|d| d.clone().try_cast::<String>()),
                Some("error".to_string()),
                "dep {dep:?} should surface a tagged error",
            );
            assert_eq!(
                entry
                    .get("error")
                    .and_then(|d| d.clone().try_cast::<String>()),
                Some((*expected_code).to_string()),
                "dep {dep:?} expected code {expected_code}",
            );
        }
    }

    #[test]
    fn context_window_exposes_used_and_remaining_as_floats() {
        let mut s = minimal_status();
        s.context_window = Some(ContextWindow {
            used: Percent::new(42.5).unwrap(),
            size: 200_000,
            total_input_tokens: 1_000,
            total_output_tokens: 2_000,
            current_usage: None,
        });
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let cw: Map = status_map(&ctx)
            .get("context_window")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            cw.get("used").unwrap().clone().try_cast::<f64>().unwrap(),
            42.5
        );
        assert_eq!(
            cw.get("remaining")
                .unwrap()
                .clone()
                .try_cast::<f64>()
                .unwrap(),
            57.5
        );
        assert_eq!(
            cw.get("size").unwrap().clone().try_cast::<i64>().unwrap(),
            200_000
        );
        // None current_usage mirrors as rhai `()` so plugins can check
        // `if ctx.status.context_window.current_usage != () { ... }`.
        assert!(cw.get("current_usage").unwrap().is_unit());
    }

    #[test]
    fn context_window_current_usage_mirrors_all_four_fields() {
        let mut s = minimal_status();
        s.context_window = Some(ContextWindow {
            used: Percent::new(12.4).unwrap(),
            size: 200_000,
            total_input_tokens: 24_800,
            total_output_tokens: 3_200,
            current_usage: Some(TurnUsage {
                input_tokens: 2_000,
                output_tokens: 500,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 500,
            }),
        });
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let usage: Map = status_map(&ctx)
            .get("context_window")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("current_usage")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            usage
                .get("input_tokens")
                .unwrap()
                .clone()
                .try_cast::<i64>()
                .unwrap(),
            2_000
        );
        assert_eq!(
            usage
                .get("output_tokens")
                .unwrap()
                .clone()
                .try_cast::<i64>()
                .unwrap(),
            500
        );
        assert_eq!(
            usage
                .get("cache_creation_input_tokens")
                .unwrap()
                .clone()
                .try_cast::<i64>()
                .unwrap(),
            0
        );
        assert_eq!(
            usage
                .get("cache_read_input_tokens")
                .unwrap()
                .clone()
                .try_cast::<i64>()
                .unwrap(),
            500
        );
    }

    #[test]
    fn cost_lines_fields_round_trip_as_i64() {
        let mut s = minimal_status();
        s.cost = Some(CostMetrics {
            total_cost_usd: 1.23,
            total_duration_ms: 60_000,
            total_api_duration_ms: 30_000,
            total_lines_added: 500,
            total_lines_removed: 10,
        });
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let cost: Map = status_map(&ctx)
            .get("cost")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            cost.get("total_lines_added")
                .unwrap()
                .clone()
                .try_cast::<i64>()
                .unwrap(),
            500
        );
        assert_eq!(
            cost.get("total_cost_usd")
                .unwrap()
                .clone()
                .try_cast::<f64>()
                .unwrap(),
            1.23
        );
    }

    #[test]
    fn raw_json_object_round_trips_recursively() {
        let raw = serde_json::json!({
            "nested": {
                "list": [1, "two", true, null],
                "flag": false
            }
        });
        let mut s = minimal_status();
        s.raw = Arc::new(raw);
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let raw_map: Map = status_map(&ctx)
            .get("raw")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        let nested: Map = raw_map.get("nested").unwrap().clone().try_cast().unwrap();
        let list: Array = nested.get("list").unwrap().clone().try_cast().unwrap();
        assert_eq!(list[0].clone().try_cast::<i64>().unwrap(), 1);
        assert_eq!(
            list[1].clone().try_cast::<String>().unwrap(),
            "two".to_string()
        );
        assert!(list[2].clone().try_cast::<bool>().unwrap());
        assert!(list[3].is_unit());
    }

    #[test]
    fn raw_empty_array_and_object_round_trip() {
        let raw = serde_json::json!({ "arr": [], "obj": {} });
        let mut s = minimal_status();
        s.raw = Arc::new(raw);
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let raw_map: Map = status_map(&ctx)
            .get("raw")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        let arr: Array = raw_map.get("arr").unwrap().clone().try_cast().unwrap();
        assert!(arr.is_empty());
        let obj: Map = raw_map.get("obj").unwrap().clone().try_cast().unwrap();
        assert!(obj.is_empty());
    }

    #[test]
    fn unknown_buckets_drop_oversize_keys() {
        // Keys longer than MAX_STRING_SIZE are skipped — rhai's engine
        // limit is enforced script-side, not on host-built maps, so
        // the mirror is the only place a key bomb can be caught.
        use crate::data_context::{EndpointUsage, UsageData};
        let mut unknown = std::collections::HashMap::new();
        let huge_key = "x".repeat(MAX_STRING_SIZE + 1);
        unknown.insert(huge_key.clone(), serde_json::Value::Null);
        unknown.insert("ok_key".to_string(), serde_json::Value::Bool(true));
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: unknown,
        });
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let payload: Map = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        let mirrored: Map = payload
            .get("unknown_buckets")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert!(
            mirrored.contains_key("ok_key"),
            "normal-sized key must survive",
        );
        assert!(
            !mirrored.contains_key(huge_key.as_str()),
            "oversize key must be dropped",
        );
    }

    fn build_nested_object_chain(depth_links: usize, leaf: JsonValue) -> JsonValue {
        // Wrap a leaf value in `depth_links` layers of `{"nest": ...}`,
        // so the root is a Map and the value at depth `depth_links` is
        // `leaf`. Caller chooses the exact position relative to the cap.
        let mut v = leaf;
        for _ in 0..depth_links {
            v = serde_json::json!({ "nest": v });
        }
        v
    }

    #[test]
    fn raw_json_at_exact_max_depth_survives_one_deeper_collapses() {
        // Pin the `>=` boundary: the value at depth MAX_JSON_DEPTH - 1
        // must still be a real Dynamic, and the value at depth
        // MAX_JSON_DEPTH must collapse to `()`. A future refactor to
        // `>` would survive the "beyond the cap" test but break this
        // one.
        let leaf = serde_json::json!({ "leaf": "bottom" });
        // Root is depth 0. `build_nested_object_chain(N, leaf)` puts
        // `leaf` at depth N. Build exactly MAX_JSON_DEPTH links so the
        // deepest Map in the input sits at depth MAX_JSON_DEPTH.
        let nested = build_nested_object_chain(MAX_JSON_DEPTH, leaf);
        let mut s = minimal_status();
        s.raw = Arc::new(nested);
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let mut cursor = status_map(&ctx)
            .get("raw")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap();
        // Walk MAX_JSON_DEPTH - 1 links. After each `.get("nest")` +
        // cast, cursor sits one level deeper. After MAX_JSON_DEPTH - 1
        // iterations we are at depth MAX_JSON_DEPTH - 1 (still a Map).
        for _ in 0..(MAX_JSON_DEPTH - 1) {
            let next = cursor.get("nest").expect("nest key below cap").clone();
            cursor = next.try_cast::<Map>().expect("map below cap");
        }
        // Depth MAX_JSON_DEPTH - 1 is still below the cap: `.get("nest")`
        // returns the value at depth MAX_JSON_DEPTH, which is where the
        // guard fires.
        let capped = cursor.get("nest").expect("nest at cap").clone();
        assert!(
            capped.is_unit(),
            "value at depth MAX_JSON_DEPTH must collapse to ()",
        );
    }

    #[test]
    fn raw_json_nested_arrays_beyond_max_depth_collapse_to_unit() {
        // The depth guard runs in the Array arm too; pin it with a
        // pure-array chain so a refactor that drops the array-side
        // increment is caught.
        let mut nested = serde_json::json!("leaf");
        for _ in 0..(MAX_JSON_DEPTH + 2) {
            nested = serde_json::json!([nested]);
        }
        let mut s = minimal_status();
        s.raw = Arc::new(nested);
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let mut cursor: Array = status_map(&ctx)
            .get("raw")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        for _ in 0..(MAX_JSON_DEPTH - 1) {
            let next = cursor[0].clone();
            cursor = next.try_cast::<Array>().expect("array below cap");
        }
        assert!(
            cursor[0].is_unit(),
            "array element at depth MAX_JSON_DEPTH must collapse to ()",
        );
    }

    #[test]
    fn raw_json_nested_object_preserves_all_entries_as_escape_hatch() {
        // `ctx.status.raw` is the documented escape hatch for tool-
        // specific fields; breadth caps would silently break plugins
        // that read through it. Under EscapeHatch posture, every
        // entry of a nested object must round-trip.
        let mut obj = serde_json::Map::new();
        for i in 0..(MAX_MAP_SIZE + 50) {
            obj.insert(format!("key_{i:04}"), serde_json::Value::Bool(true));
        }
        let expected = obj.len();
        let mut s = minimal_status();
        s.raw = Arc::new(serde_json::Value::Object(obj));
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let raw_map: Map = status_map(&ctx)
            .get("raw")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(raw_map.len(), expected);
    }

    #[test]
    fn raw_json_nested_array_preserves_all_items_as_escape_hatch() {
        // Symmetric with the nested-object EscapeHatch test above:
        // array breadth must round-trip on `ctx.status.raw` so plugins
        // reading tool-specific lists (e.g. diagnostic traces) see the
        // full payload.
        let arr: Vec<JsonValue> = (0..(MAX_ARRAY_SIZE + 50))
            .map(|i| serde_json::Value::from(i as i64))
            .collect();
        let expected = arr.len();
        let mut s = minimal_status();
        s.raw = Arc::new(serde_json::Value::Array(arr));
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let raw_arr: Array = status_map(&ctx)
            .get("raw")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(raw_arr.len(), expected);
    }

    #[test]
    fn raw_json_oversize_string_preserves_full_content_as_escape_hatch() {
        // Strings on the raw path must round-trip regardless of size
        // — see the `ctx.status.raw` resource-posture note in
        // docs/specs/plugin-api.md.
        let oversized = "a".repeat(MAX_STRING_SIZE * 2);
        let mut s = minimal_status();
        s.raw = Arc::new(serde_json::json!({ "big": oversized.clone() }));
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let raw_map: Map = status_map(&ctx)
            .get("raw")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        let big = raw_map
            .get("big")
            .unwrap()
            .clone()
            .try_cast::<String>()
            .unwrap();
        assert_eq!(big.len(), oversized.len());
    }

    #[test]
    fn unknown_buckets_value_nested_object_truncates_under_strict() {
        // Strict posture (bucket values): nested objects are breadth-
        // capped. Server-controlled data has no escape-hatch exemption.
        use crate::data_context::{EndpointUsage, UsageData};
        let mut obj = serde_json::Map::new();
        for i in 0..(MAX_MAP_SIZE + 50) {
            obj.insert(format!("key_{i:04}"), serde_json::Value::Bool(true));
        }
        let mut unknown = std::collections::HashMap::new();
        unknown.insert("wide_value".to_string(), serde_json::Value::Object(obj));
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: unknown,
        });
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let value: Map = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("unknown_buckets")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("wide_value")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(value.len(), MAX_MAP_SIZE);
    }

    #[test]
    fn unknown_buckets_value_nested_array_truncates_under_strict() {
        use crate::data_context::{EndpointUsage, UsageData};
        let arr: Vec<JsonValue> = (0..(MAX_ARRAY_SIZE + 50))
            .map(|i| serde_json::Value::from(i as i64))
            .collect();
        let mut unknown = std::collections::HashMap::new();
        unknown.insert("wide_array".to_string(), serde_json::Value::Array(arr));
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: unknown,
        });
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let value: Array = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("unknown_buckets")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("wide_array")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(value.len(), MAX_ARRAY_SIZE);
    }

    #[test]
    fn unknown_buckets_value_oversize_string_is_truncated_under_strict() {
        use crate::data_context::{EndpointUsage, UsageData};
        let oversized = "a".repeat(MAX_STRING_SIZE * 2);
        let mut unknown = std::collections::HashMap::new();
        unknown.insert(
            "big".to_string(),
            serde_json::Value::String(oversized.clone()),
        );
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: unknown,
        });
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let big: String = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("unknown_buckets")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("big")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(big.len(), MAX_STRING_SIZE);
    }

    #[test]
    fn unknown_buckets_value_multibyte_string_truncates_at_utf8_boundary() {
        // 3-byte char ('€' = 0xE2 0x82 0xAC). MAX_STRING_SIZE = 1024
        // does not align with 3 bytes (1024 = 341*3 + 1), so a byte-
        // index-1024 truncation lands mid-codepoint. `truncate_utf8`
        // must back up to the nearest char boundary — byte index 1023,
        // which retains 341 full characters. A `s[..max_bytes]` naïve
        // slice would panic; `from_utf8_lossy` would contaminate with
        // replacement chars.
        use crate::data_context::{EndpointUsage, UsageData};
        let char_bytes = "€".len();
        let char_count = MAX_STRING_SIZE / char_bytes + 1;
        let oversized: String = "€".repeat(char_count);
        let mut unknown = std::collections::HashMap::new();
        unknown.insert(
            "euros".to_string(),
            serde_json::Value::String(oversized.clone()),
        );
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: unknown,
        });
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let euros: String = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("unknown_buckets")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("euros")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert!(
            euros.len() <= MAX_STRING_SIZE,
            "truncation must not exceed MAX_STRING_SIZE",
        );
        assert!(
            euros.chars().all(|c| c == '€'),
            "truncation must land on a UTF-8 char boundary",
        );
        assert_eq!(
            euros.len() % char_bytes,
            0,
            "byte length divisible by char size"
        );
    }

    #[test]
    fn unknown_buckets_at_exact_max_map_size_are_not_truncated() {
        // Pin the `>=` off-by-one: exactly MAX_MAP_SIZE entries must
        // all survive. One fewer than this count was the bug vector
        // for the upstream `>` vs `>=` corner.
        use crate::data_context::{EndpointUsage, UsageData};
        let mut unknown = std::collections::HashMap::new();
        for i in 0..MAX_MAP_SIZE {
            unknown.insert(format!("bucket_{i:04}"), serde_json::Value::Null);
        }
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: unknown,
        });
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let mirrored: Map = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("unknown_buckets")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(mirrored.len(), MAX_MAP_SIZE);
    }

    #[test]
    fn unknown_buckets_key_at_exact_max_string_size_survives() {
        // The key check uses `>`, so a key of exactly MAX_STRING_SIZE
        // must pass through. This pins that boundary against a future
        // `>=` refactor.
        use crate::data_context::{EndpointUsage, UsageData};
        let boundary_key = "x".repeat(MAX_STRING_SIZE);
        let mut unknown = std::collections::HashMap::new();
        unknown.insert(boundary_key.clone(), serde_json::Value::Bool(true));
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: unknown,
        });
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let mirrored: Map = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("unknown_buckets")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert!(mirrored.contains_key(boundary_key.as_str()));
    }

    #[test]
    fn unknown_buckets_truncation_survives_deterministically() {
        // HashMap iteration is process-randomized; without the sort in
        // `build_unknown_buckets` the survivor set would rotate across
        // renders and plugins would see specific buckets flicker in
        // and out. This test freezes the expected survivor set.
        use crate::data_context::{EndpointUsage, UsageData};
        let mut unknown = std::collections::HashMap::new();
        for i in 0..(MAX_MAP_SIZE + 10) {
            unknown.insert(format!("bucket_{i:04}"), serde_json::Value::Null);
        }
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: unknown,
        });
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let mirrored: Map = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("unknown_buckets")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        // Sorted lexicographically; MAX_MAP_SIZE survivors are the
        // first MAX_MAP_SIZE lex-ordered keys.
        for i in 0..MAX_MAP_SIZE {
            let key = format!("bucket_{i:04}");
            assert!(
                mirrored.contains_key(key.as_str()),
                "deterministic-sort survivor {key} missing",
            );
        }
        for i in MAX_MAP_SIZE..(MAX_MAP_SIZE + 10) {
            let key = format!("bucket_{i:04}");
            assert!(
                !mirrored.contains_key(key.as_str()),
                "lex-larger key {key} should have been truncated",
            );
        }
    }

    #[test]
    fn unknown_buckets_value_depth_resets_per_entry() {
        // Each bucket value gets its own MAX_JSON_DEPTH budget — the
        // walk is called with depth=0 per value so siblings don't
        // share recursion budget. A bucket value nested to exactly
        // MAX_JSON_DEPTH - 1 levels must fully survive.
        use crate::data_context::{EndpointUsage, UsageData};
        let leaf = serde_json::json!("leaf");
        // Place `leaf` at exactly depth MAX_JSON_DEPTH - 1 so the
        // walk's depth counter reaches MAX_JSON_DEPTH - 1 on the leaf
        // and does not fire the guard.
        let deep_value = build_nested_object_chain(MAX_JSON_DEPTH - 1, leaf);
        let mut unknown = std::collections::HashMap::new();
        unknown.insert("deep".to_string(), deep_value);
        let data = UsageData::Endpoint(EndpointUsage {
            five_hour: None,
            seven_day: None,
            seven_day_opus: None,
            seven_day_sonnet: None,
            seven_day_oauth_apps: None,
            extra_usage: None,
            unknown_buckets: unknown,
        });
        let dc = DataContext::new(minimal_status());
        dc.preseed_usage(Ok(data)).expect("seed");
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Usage]);
        let mirrored: Map = ctx
            .get("usage")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("data")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("unknown_buckets")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        let mut cursor: Map = mirrored.get("deep").unwrap().clone().try_cast().unwrap();
        for _ in 0..(MAX_JSON_DEPTH - 2) {
            let next = cursor.get("nest").expect("nest key").clone();
            cursor = next.try_cast::<Map>().expect("map below cap");
        }
        let leaf_value = cursor.get("nest").expect("leaf node").clone();
        assert_eq!(
            leaf_value.try_cast::<String>().as_deref(),
            Some("leaf"),
            "value nested to depth MAX_JSON_DEPTH - 1 must fully survive because each bucket gets its own depth budget",
        );
    }

    #[test]
    fn raw_u64_above_i64_max_falls_through_to_f64() {
        // serde_json::Number can hold u64 > i64::MAX; rhai has no
        // native u64 so we round-trip via f64 (some precision loss
        // expected for the very large values, but no panic / drop).
        let raw = serde_json::json!({ "huge": u64::MAX });
        let mut s = minimal_status();
        s.raw = Arc::new(raw);
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let raw_map: Map = status_map(&ctx)
            .get("raw")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        let huge = raw_map.get("huge").unwrap().clone().try_cast::<f64>();
        assert!(huge.is_some(), "u64 > i64::MAX must surface as a number");
    }

    #[test]
    fn env_whitelist_keys_present_even_when_env_is_empty() {
        let ctx = build_env_map(ENV_WHITELIST, |_| None)
            .try_cast::<Map>()
            .unwrap();
        for key in ENV_WHITELIST {
            assert!(ctx.contains_key(*key), "{key} should be present as ()");
            assert!(ctx.get(*key).unwrap().is_unit());
        }
    }

    #[test]
    fn env_non_whitelisted_key_absent() {
        let ctx = build_env_map(ENV_WHITELIST, |k| match k {
            "TERM" => Some("xterm".to_string()),
            _ => None,
        })
        .try_cast::<Map>()
        .unwrap();
        assert_eq!(
            ctx.get("TERM")
                .unwrap()
                .clone()
                .try_cast::<String>()
                .unwrap(),
            "xterm"
        );
        assert!(!ctx.contains_key("HOME"));
        assert!(!ctx.contains_key("PATH"));
    }

    #[test]
    fn workspace_without_worktree_emits_unit() {
        let dc = DataContext::new(minimal_status());
        let ctx = build_and_unwrap_map(&dc, &[]);
        let ws: Map = status_map(&ctx)
            .get("workspace")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert!(ws.get("git_worktree").unwrap().is_unit());
    }

    #[test]
    fn workspace_worktree_preserves_name_and_path() {
        let mut s = minimal_status();
        s.workspace.git_worktree = Some(GitWorktree {
            name: "feature".to_string(),
            path: PathBuf::from("/wt/feature"),
        });
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let wt: Map = status_map(&ctx)
            .get("workspace")
            .unwrap()
            .clone()
            .try_cast::<Map>()
            .unwrap()
            .get("git_worktree")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            wt.get("name")
                .unwrap()
                .clone()
                .try_cast::<String>()
                .unwrap(),
            "feature"
        );
        assert_eq!(
            wt.get("path")
                .unwrap()
                .clone()
                .try_cast::<String>()
                .unwrap(),
            "/wt/feature"
        );
    }

    #[test]
    fn config_is_passed_through_as_provided() {
        let dc = DataContext::new(minimal_status());
        let mut config_map = Map::new();
        config_map.insert("threshold".into(), Dynamic::from(42_i64));
        let rc = RenderContext::new(80);
        let ctx: Map = build_ctx(&dc, &rc, &[], Dynamic::from_map(config_map))
            .try_cast()
            .unwrap();
        let config: Map = ctx.get("config").unwrap().clone().try_cast().unwrap();
        assert_eq!(
            config
                .get("threshold")
                .unwrap()
                .clone()
                .try_cast::<i64>()
                .unwrap(),
            42
        );
    }
}
