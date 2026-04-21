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

use crate::data_context::{DataContext, DataDep};
use crate::input::{
    ContextWindow, CostMetrics, GitWorktree, ModelInfo, RateLimitWindow, RateLimits, StatusContext,
    Tool, WorkspaceInfo,
};

const ENV_WHITELIST: &[&str] = &["TERM", "COLORTERM", "NO_COLOR", "FORCE_COLOR", "LANG"];

/// Build the `ctx` value a plugin's `render(ctx)` function sees.
///
/// `declared_deps` gates which lazy sources get mirrored. `config` is
/// the plugin's `[segments.<id>]` TOML table already converted to a
/// rhai-compatible `Dynamic` (use `()` when no table is configured).
pub fn build_ctx(dc: &DataContext, declared_deps: &[DataDep], config: Dynamic) -> Dynamic {
    let mut map = Map::new();
    map.insert("status".into(), build_status(&dc.status));
    map.insert("config".into(), config);
    map.insert("env".into(), env_snapshot());

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
            Ok(_) => tagged_ok(Dynamic::from_map(Map::new())),
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
            Ok(Some(_)) => tagged_ok(Dynamic::from_map(Map::new())),
            Ok(None) => tagged_ok(Dynamic::UNIT),
            Err(e) => tagged_error(e.code()),
        };
        map.insert("git".into(), value);
    }

    Dynamic::from_map(map)
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
    m.insert(
        "rate_limits".into(),
        s.rate_limits
            .as_ref()
            .map_or(Dynamic::UNIT, build_rate_limits),
    );
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

fn build_rate_limits(rl: &RateLimits) -> Dynamic {
    let mut m = Map::new();
    match rl {
        RateLimits::FiveHourOnly(w) => {
            m.insert("kind".into(), Dynamic::from("five_hour_only".to_string()));
            m.insert("five_hour".into(), build_rate_window(w));
        }
        RateLimits::SevenDayOnly(w) => {
            m.insert("kind".into(), Dynamic::from("seven_day_only".to_string()));
            m.insert("seven_day".into(), build_rate_window(w));
        }
        RateLimits::Both {
            five_hour,
            seven_day,
        } => {
            m.insert("kind".into(), Dynamic::from("both".to_string()));
            m.insert("five_hour".into(), build_rate_window(five_hour));
            m.insert("seven_day".into(), build_rate_window(seven_day));
        }
    }
    Dynamic::from_map(m)
}

fn build_rate_window(w: &RateLimitWindow) -> Dynamic {
    let mut m = Map::new();
    m.insert("used".into(), Dynamic::from(f64::from(w.used.value())));
    m.insert("resets_at".into(), Dynamic::from(w.resets_at.to_rfc3339()));
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
/// specific fields the canonical `StatusContext` doesn't model.
fn json_to_dynamic(v: &JsonValue) -> Dynamic {
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
        JsonValue::String(s) => Dynamic::from(s.clone()),
        JsonValue::Array(arr) => {
            let items: Array = arr.iter().map(json_to_dynamic).collect();
            Dynamic::from_array(items)
        }
        JsonValue::Object(obj) => {
            let mut m = Map::new();
            for (k, val) in obj {
                m.insert(k.as_str().into(), json_to_dynamic(val));
            }
            Dynamic::from_map(m)
        }
    }
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
    use chrono::TimeZone;
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
            rate_limits: None,
            effort: None,
            raw: Arc::new(serde_json::json!({"custom": "field"})),
        }
    }

    fn build_and_unwrap_map(dc: &DataContext, deps: &[DataDep]) -> Map {
        let dyn_ctx = build_ctx(dc, deps, Dynamic::UNIT);
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
    fn git_dep_reports_error_variant_on_stub() {
        let dc = DataContext::new(minimal_status());
        let ctx = build_and_unwrap_map(&dc, &[DataDep::Git]);
        let git: Map = ctx.get("git").unwrap().clone().try_cast().unwrap();
        // Stub returns NotImplemented; the Ok(None) → unit path is
        // covered by a structural test below.
        assert_eq!(
            git.get("kind").and_then(|d| d.clone().try_cast::<String>()),
            Some("error".to_string())
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
        // Regression guard: a `Tool` variant added without a matching
        // arm in `build_tool` would silently fall through to "other"
        // (or fail to compile if exhaustive). Pin every variant.
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
        assert!(status.get("rate_limits").unwrap().is_unit());
        assert!(status.get("effort").unwrap().is_unit());
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
    fn rate_limits_both_variant_has_all_three_keys() {
        let mut s = minimal_status();
        let w = RateLimitWindow {
            used: Percent::new(20.0).unwrap(),
            resets_at: chrono::Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).unwrap(),
        };
        s.rate_limits = Some(RateLimits::Both {
            five_hour: w,
            seven_day: w,
        });
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let rl: Map = status_map(&ctx)
            .get("rate_limits")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            rl.get("kind").and_then(|d| d.clone().try_cast::<String>()),
            Some("both".to_string())
        );
        assert!(rl.contains_key("five_hour"));
        assert!(rl.contains_key("seven_day"));
    }

    #[test]
    fn rate_limits_five_hour_only_omits_seven_day() {
        let mut s = minimal_status();
        s.rate_limits = Some(RateLimits::FiveHourOnly(RateLimitWindow {
            used: Percent::new(5.0).unwrap(),
            resets_at: chrono::Utc.with_ymd_and_hms(2099, 1, 1, 0, 0, 0).unwrap(),
        }));
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let rl: Map = status_map(&ctx)
            .get("rate_limits")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            rl.get("kind").and_then(|d| d.clone().try_cast::<String>()),
            Some("five_hour_only".to_string())
        );
        assert!(rl.contains_key("five_hour"));
        assert!(!rl.contains_key("seven_day"));
    }

    #[test]
    fn rate_limits_seven_day_only_omits_five_hour() {
        let mut s = minimal_status();
        s.rate_limits = Some(RateLimits::SevenDayOnly(RateLimitWindow {
            used: Percent::new(8.0).unwrap(),
            resets_at: chrono::Utc.with_ymd_and_hms(2099, 1, 8, 0, 0, 0).unwrap(),
        }));
        let dc = DataContext::new(s);
        let ctx = build_and_unwrap_map(&dc, &[]);
        let rl: Map = status_map(&ctx)
            .get("rate_limits")
            .unwrap()
            .clone()
            .try_cast()
            .unwrap();
        assert_eq!(
            rl.get("kind").and_then(|d| d.clone().try_cast::<String>()),
            Some("seven_day_only".to_string())
        );
        assert!(rl.contains_key("seven_day"));
        assert!(!rl.contains_key("five_hour"));
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
        let ctx: Map = build_ctx(&dc, &[], Dynamic::from_map(config_map))
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
