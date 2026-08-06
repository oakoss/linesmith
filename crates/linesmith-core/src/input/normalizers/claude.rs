//! Claude Code normalizer.
//!
//! Parses a CC statusline JSON payload into a [`StatusContext`]. Per the
//! spec at `docs/specs/input-schema.md`, this normalizer also serves as
//! the **Fallback** for tools whose detection rule is a stub today
//! (QwenCode, CodexCli, CopilotCli, Other). The caller passes the
//! detected [`Tool`] so the resulting context and any [`ParseError`]
//! carry the right discriminator even when this normalizer is used as
//! the Fallback.
//!
//! Sub-field failures degrade per-leaf per ADR-0014: missing/null/
//! malformed leaves become [`Option::None`] with `lsm_warn!` to stderr,
//! never `Err`. Only catastrophic root failures (non-object root,
//! invalid `used_percentage`) surface as [`ParseError`].

use std::path::PathBuf;
use std::sync::Arc;

use crate::input::{
    ContextWindow, CostMetrics, EffortLevel, GitWorktree, JsonType, ModelInfo, OutputStyle,
    ParseError, Percent, StatusContext, Tool, TurnUsage, VimMode, WorkspaceInfo,
};

pub(super) fn normalize(
    raw: Arc<serde_json::Value>,
    tool: Tool,
) -> Result<StatusContext, ParseError> {
    let root = expect_object(&raw, "", &tool)?;

    let model = parse_model(root);
    let workspace = parse_workspace(root);
    let context_window = parse_context_window(root, &tool)?;
    let cost = parse_cost(root)?;
    let effort = parse_effort(root)?;
    let vim = parse_vim(root)?;
    let output_style = parse_output_style(root)?;
    let agent_name = parse_agent_name(root)?;
    let version = parse_version(root)?;

    Ok(StatusContext {
        tool,
        model,
        workspace,
        context_window,
        cost,
        effort,
        vim,
        output_style,
        agent_name,
        version,
        raw,
    })
}

/// ADR-0014: any sub-field failure (missing wrapper, non-object,
/// missing/null/non-string `display_name`) downgrades the field
/// to `None` with an `lsm_warn!` carrying the JSON path. Segments
/// hide; unrelated segments still render.
fn parse_model(root: &serde_json::Map<String, serde_json::Value>) -> Option<ModelInfo> {
    let value = root.get("model")?;
    if value.is_null() {
        return None;
    }
    let model = match value.as_object() {
        Some(o) => o,
        None => {
            crate::lsm_warn!(
                "model: expected object, got {:?}; degrading to None (possible CC schema drift)",
                JsonType::of(value)
            );
            return None;
        }
    };
    let Some(name_value) = model.get("display_name") else {
        crate::lsm_warn!("model.display_name: missing; degrading to None");
        return None;
    };
    if name_value.is_null() {
        return None;
    }
    let Some(display_name) = name_value.as_str() else {
        crate::lsm_warn!(
            "model.display_name: expected string, got {:?}; degrading to None",
            JsonType::of(name_value)
        );
        return None;
    };
    Some(ModelInfo {
        display_name: display_name.to_owned(),
        id: parse_model_id(model),
    })
}

/// `model.id` degrades to `None` independently of `display_name`: an
/// absent or malformed id costs only the family-matching that
/// `rate_limit_7d_model` needs, and hiding the whole model wrapper over
/// it would take unrelated segments down too.
fn parse_model_id(model: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    let value = model.get("id")?;
    if value.is_null() {
        return None;
    }
    let Some(id) = value.as_str() else {
        crate::lsm_warn!(
            "model.id: expected string, got {:?}; degrading to None",
            JsonType::of(value)
        );
        return None;
    };
    (!id.is_empty()).then(|| id.to_owned())
}

/// ADR-0014: any sub-field failure downgrades to `None` + warn.
/// `git_worktree` still degrades independently — a malformed
/// worktree shouldn't hide the project_dir basename.
fn parse_workspace(root: &serde_json::Map<String, serde_json::Value>) -> Option<WorkspaceInfo> {
    let value = root.get("workspace")?;
    if value.is_null() {
        return None;
    }
    let workspace = match value.as_object() {
        Some(o) => o,
        None => {
            crate::lsm_warn!(
                "workspace: expected object, got {:?}; degrading to None (possible CC schema drift)",
                JsonType::of(value)
            );
            return None;
        }
    };
    let Some(dir_value) = workspace.get("project_dir") else {
        crate::lsm_warn!("workspace.project_dir: missing; degrading to None");
        return None;
    };
    if dir_value.is_null() {
        return None;
    }
    let Some(project_dir_str) = dir_value.as_str() else {
        crate::lsm_warn!(
            "workspace.project_dir: expected string, got {:?}; degrading to None",
            JsonType::of(dir_value)
        );
        return None;
    };

    let git_worktree = match workspace.get("git_worktree") {
        Some(serde_json::Value::Null) | None => None,
        Some(serde_json::Value::Object(obj)) => parse_git_worktree(obj),
        Some(other) => {
            crate::lsm_warn!(
                "workspace.git_worktree: expected object, got {:?}; degrading to None (worktree only)",
                JsonType::of(other)
            );
            None
        }
    };

    Some(WorkspaceInfo {
        project_dir: PathBuf::from(project_dir_str),
        git_worktree,
    })
}

fn parse_git_worktree(obj: &serde_json::Map<String, serde_json::Value>) -> Option<GitWorktree> {
    let name = string_leaf(obj, "name", "workspace.git_worktree.name")?;
    let path = string_leaf(obj, "path", "workspace.git_worktree.path")?;
    // Both-empty is the documented "unset" shape (CC emits `""` when
    // not in a worktree); silent. One-empty is asymmetric and almost
    // certainly schema drift — warn so the next operator-debug pass
    // catches it.
    match (name.is_empty(), path.is_empty()) {
        (true, true) => None,
        (false, false) => Some(GitWorktree {
            name: name.to_owned(),
            path: PathBuf::from(path),
        }),
        (n_empty, _) => {
            let (empty_field, populated_field) = if n_empty {
                ("name", "path")
            } else {
                ("path", "name")
            };
            crate::lsm_warn!(
                "workspace.git_worktree: {empty_field} empty but {populated_field} populated; degrading to None (possible CC schema drift)"
            );
            None
        }
    }
}

/// Tolerant string reader. Missing or null → silent `None`
/// (documented "field unset" shape). Non-string → `lsm_warn!` +
/// `None` to surface schema drift.
///
/// `key` is the lookup key into `obj`; `path` is the dotted path used
/// only for diagnostic messages. Splitting these arguments (per
/// lsm-coc) keeps a JSON key with a literal `.` in it from silently
/// missing the lookup.
fn string_leaf<'a>(
    obj: &'a serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    path: &'static str,
) -> Option<&'a str> {
    let value = obj.get(key)?;
    if value.is_null() {
        return None;
    }
    match value.as_str() {
        Some(s) => Some(s),
        None => {
            crate::lsm_warn!(
                "{path}: expected string, got {:?}; degrading to None",
                JsonType::of(value)
            );
            None
        }
    }
}

fn parse_context_window(
    root: &serde_json::Map<String, serde_json::Value>,
    tool: &Tool,
) -> Result<Option<ContextWindow>, ParseError> {
    let Some(value) = root.get("context_window") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(cw) = value.as_object() else {
        crate::lsm_warn!(
            "context_window: expected object, got {:?}; degrading to None",
            JsonType::of(value)
        );
        return Ok(None);
    };

    // ADR-0014: each leaf degrades independently. A null
    // `used_percentage` (the documented pre-first-API-call shape)
    // no longer hides peers like `current_usage` or `size`.
    let used = parse_used_percentage(cw, tool)?;
    let size = parse_size(cw);
    let total_input_tokens = try_u64_required(
        cw,
        "total_input_tokens",
        "context_window.total_input_tokens",
    );
    let total_output_tokens = try_u64_required(
        cw,
        "total_output_tokens",
        "context_window.total_output_tokens",
    );
    let current_usage = parse_current_usage(cw)?;

    let window = ContextWindow {
        used,
        size,
        total_input_tokens,
        total_output_tokens,
        current_usage,
    };
    // Collapse to `None` when every leaf failed so the plugin
    // contract `ctx.status.context_window != ()` round-trips: a
    // non-`()` map must always have at least one readable leaf.
    if context_window_is_empty(&window) {
        return Ok(None);
    }
    Ok(Some(window))
}

fn context_window_is_empty(cw: &ContextWindow) -> bool {
    cw.used.is_none()
        && cw.size.is_none()
        && cw.total_input_tokens.is_none()
        && cw.total_output_tokens.is_none()
        && cw.current_usage.is_none()
}

/// Parse `context_window.used_percentage` into `Option<Percent>`.
/// - missing / null → `Ok(None)`, silent (documented pre-first-
///   API-call shape; warning would spam at every fresh session)
/// - non-number → `Ok(None)` + warn (schema drift)
/// - in-range value → `Ok(Some(_))`
/// - >100 → clamp to 100 + warn (claude-code#37163)
/// - <0 or NaN → `Err(InvalidValue)`. Carve-out from ADR-0014's
///   warn-and-degrade default: a negative is undocumented and
///   most likely a corrupted payload; surfacing loud catches
///   real upstream breakage that "Some(0)" or warn+None would mask.
fn parse_used_percentage(
    cw: &serde_json::Map<String, serde_json::Value>,
    tool: &Tool,
) -> Result<Option<Percent>, ParseError> {
    let Some(value) = cw.get("used_percentage") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(used_raw) = value.as_f64() else {
        crate::lsm_warn!(
            "context_window.used_percentage: expected number, got {:?}; degrading leaf to None",
            JsonType::of(value)
        );
        return Ok(None);
    };
    if used_raw > 100.0 {
        crate::lsm_warn!("context_window.used_percentage = {used_raw} > 100; clamping to 100");
        return Ok(Some(
            Percent::from_f64_clamped(used_raw).expect("non-NaN value > 100 clamps successfully"),
        ));
    }
    match Percent::from_f64(used_raw) {
        Some(p) => Ok(Some(p)),
        None => Err(invalid_value(
            "context_window.used_percentage",
            "percentage must be a number in [0, 100]",
            tool,
        )),
    }
}

/// Parse `context_window_size` into `Option<u32>`. Narrows `u64`
/// to ADR-0014's `u32`; warns and degrades on overflow rather
/// than wrapping. No real CC context window comes anywhere close.
fn parse_size(cw: &serde_json::Map<String, serde_json::Value>) -> Option<u32> {
    let raw = try_u64_required(
        cw,
        "context_window_size",
        "context_window.context_window_size",
    )?;
    match u32::try_from(raw) {
        Ok(n) => Some(n),
        Err(_) => {
            crate::lsm_warn!(
                "context_window.context_window_size = {raw} exceeds u32::MAX; degrading leaf to None"
            );
            None
        }
    }
}

/// Tolerant `u64` reader for *contracted* leaves (CC contract
/// guarantees the key is present and non-null). Missing or null
/// here is schema drift; warn so the channel surfaces upstream
/// changes. Use `try_u64_optional` for documented "may be absent"
/// fields like `current_usage.*`.
///
/// `key` is the lookup key into `obj`; `path` is the dotted path used
/// only for diagnostics. See `string_leaf` for the rationale behind
/// splitting these args.
fn try_u64_required(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    path: &'static str,
) -> Option<u64> {
    let Some(value) = obj.get(key) else {
        crate::lsm_warn!("{path}: missing; degrading leaf to None (possible CC schema drift)");
        return None;
    };
    if value.is_null() {
        crate::lsm_warn!("{path}: null; degrading leaf to None (possible CC schema drift)");
        return None;
    }
    match value.as_u64() {
        Some(n) => Some(n),
        None => {
            crate::lsm_warn!(
                "{path}: expected unsigned integer, got {:?}; degrading leaf to None",
                JsonType::of(value)
            );
            None
        }
    }
}

/// Tolerant `u64` reader for *optional* leaves where missing/null
/// is documented (e.g. `current_usage.*` before the first API
/// call). Silent on absence; warn on type drift.
fn try_u64_optional(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    path: &'static str,
) -> Option<u64> {
    let value = obj.get(key)?;
    if value.is_null() {
        return None;
    }
    match value.as_u64() {
        Some(n) => Some(n),
        None => {
            crate::lsm_warn!(
                "{path}: expected unsigned integer, got {:?}; degrading leaf to None",
                JsonType::of(value)
            );
            None
        }
    }
}

/// Tolerant `f64` reader for *contracted* leaves. Mirrors
/// `try_u64_required`. JSON syntax can't represent NaN or ±Inf
/// (`serde_json` rejects them as `InvalidJson` at parse time), so a
/// non-finite check is unreachable through the [`parse`](crate::input::parse)
/// path. Callers constructing a `serde_json::Value` programmatically
/// must enforce finiteness themselves; this helper trusts the JSON
/// parser's invariant.
fn try_f64_required(
    obj: &serde_json::Map<String, serde_json::Value>,
    key: &'static str,
    path: &'static str,
) -> Option<f64> {
    let Some(value) = obj.get(key) else {
        crate::lsm_warn!("{path}: missing; degrading leaf to None (possible CC schema drift)");
        return None;
    };
    if value.is_null() {
        crate::lsm_warn!("{path}: null; degrading leaf to None (possible CC schema drift)");
        return None;
    }
    let Some(n) = value.as_f64() else {
        crate::lsm_warn!(
            "{path}: expected number, got {:?}; degrading leaf to None",
            JsonType::of(value)
        );
        return None;
    };
    debug_assert!(
        n.is_finite(),
        "{path}: non-finite f64 leaked past serde_json"
    );
    Some(n)
}

fn parse_current_usage(
    cw: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<TurnUsage>, ParseError> {
    // Claude Code emits `current_usage: null` before the first API
    // call in a session (see docs/research/claude-code-statusline-api.md).
    // The key's presence isn't guaranteed by the schema either, so
    // tolerate outright omission as defense-in-depth; both map to
    // Option::None.
    let Some(value) = cw.get("current_usage") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(obj) = value.as_object() else {
        crate::lsm_warn!(
            "context_window.current_usage: expected object, got {:?}; degrading to None",
            JsonType::of(value)
        );
        return Ok(None);
    };
    // TurnUsage's leaves are non-Option, so any leaf failure collapses
    // the whole struct. Type drift already warned per-leaf via
    // `try_u64_optional`; the collapse-warn below fires only when at
    // least one absent leaf was NOT covered by a type-drift warn (a
    // mixed shape would otherwise produce no top-level diagnostic).
    let keys: [(&'static str, &'static str); 4] = [
        ("input_tokens", "context_window.current_usage.input_tokens"),
        (
            "output_tokens",
            "context_window.current_usage.output_tokens",
        ),
        (
            "cache_creation_input_tokens",
            "context_window.current_usage.cache_creation_input_tokens",
        ),
        (
            "cache_read_input_tokens",
            "context_window.current_usage.cache_read_input_tokens",
        ),
    ];
    let leaves = keys.map(|(key, path)| try_u64_optional(obj, key, path));
    let populated = leaves.iter().filter(|v| v.is_some()).count();
    if populated == 0 {
        return Ok(None);
    }
    let every_absence_already_warned = keys.iter().zip(leaves.iter()).all(|((key, _), leaf)| {
        leaf.is_some() || matches!(obj.get(*key), Some(v) if !v.is_null() && !v.is_u64())
    });
    if populated < 4 {
        if !every_absence_already_warned {
            crate::lsm_warn!(
                "context_window.current_usage: only {populated}/4 token fields populated; collapsing to None per ADR-0014 all-or-nothing rule (possible CC schema drift)"
            );
        }
        return Ok(None);
    }
    let [input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens] =
        leaves.map(|v| v.expect("populated==4"));
    Ok(Some(TurnUsage {
        input_tokens,
        output_tokens,
        cache_creation_input_tokens,
        cache_read_input_tokens,
    }))
}

fn parse_cost(
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<CostMetrics>, ParseError> {
    let Some(value) = root.get("cost") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(cost) = value.as_object() else {
        crate::lsm_warn!(
            "cost: expected object, got {:?}; degrading to None",
            JsonType::of(value)
        );
        return Ok(None);
    };

    // Per ADR-0014, leaves degrade independently. CC contract
    // guarantees these keys when `cost` is present, so missing/null
    // is schema drift and warns at the leaf.
    let metrics = CostMetrics {
        total_cost_usd: try_f64_required(cost, "total_cost_usd", "cost.total_cost_usd"),
        total_duration_ms: try_u64_required(cost, "total_duration_ms", "cost.total_duration_ms"),
        total_api_duration_ms: try_u64_required(
            cost,
            "total_api_duration_ms",
            "cost.total_api_duration_ms",
        ),
        total_lines_added: try_u64_required(cost, "total_lines_added", "cost.total_lines_added"),
        total_lines_removed: try_u64_required(
            cost,
            "total_lines_removed",
            "cost.total_lines_removed",
        ),
    };
    // If every leaf failed, collapse to `None` so the plugin
    // contract `ctx.status.cost != ()` (per plugin-api.md) round-
    // trips correctly: a non-`()` cost map must always have at
    // least one readable leaf.
    if cost_is_empty(&metrics) {
        return Ok(None);
    }
    Ok(Some(metrics))
}

fn cost_is_empty(c: &CostMetrics) -> bool {
    c.total_cost_usd.is_none()
        && c.total_duration_ms.is_none()
        && c.total_api_duration_ms.is_none()
        && c.total_lines_added.is_none()
        && c.total_lines_removed.is_none()
}

fn parse_effort(
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<EffortLevel>, ParseError> {
    let Some(value) = root.get("effort") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    // Canonical CC 2.1.x emits `effort: { level: "xhigh" }`. A bare
    // string is tolerated for forward/backward compat. Per ADR-0014,
    // every failure path warn-and-degrades — symmetric with parse_vim.
    let (raw, path): (&str, &'static str) = match value {
        serde_json::Value::Object(obj) => {
            let Some(level) = obj.get("level") else {
                crate::lsm_warn!(
                    "effort: wrapper present but `level` missing; degrading to None (possible CC schema drift)"
                );
                return Ok(None);
            };
            if level.is_null() {
                return Ok(None);
            }
            let Some(s) = level.as_str() else {
                crate::lsm_warn!(
                    "effort.level: expected string, got {:?}; degrading to None",
                    JsonType::of(level)
                );
                return Ok(None);
            };
            (s, "effort.level")
        }
        serde_json::Value::String(s) => (s.as_str(), "effort"),
        other => {
            crate::lsm_warn!(
                "effort: expected object or string, got {:?}; degrading to None",
                JsonType::of(other)
            );
            return Ok(None);
        }
    };
    match raw.parse::<EffortLevel>() {
        Ok(level) => Ok(Some(level)),
        Err(()) => {
            crate::lsm_warn!(
                "effort: unknown level {raw:?} at {path}; degrading to None (possible CC schema drift — known: low, medium, high, max, xhigh)"
            );
            Ok(None)
        }
    }
}

fn parse_vim(
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<VimMode>, ParseError> {
    let Some(value) = root.get("vim") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    // Canonical CC shape is `vim: { mode: "<name>" }` per
    // research/claude-code-statusline-api.md. A bare string is
    // tolerated for forward/backward compat. Per ADR-0014, every
    // failure path warn-and-degrades.
    let (raw, path): (&str, &'static str) = match value {
        serde_json::Value::Object(obj) => {
            let Some(mode) = obj.get("mode") else {
                crate::lsm_warn!(
                    "vim: wrapper present but `mode` missing; degrading to None (possible CC schema drift)"
                );
                return Ok(None);
            };
            if mode.is_null() {
                return Ok(None);
            }
            let Some(s) = mode.as_str() else {
                crate::lsm_warn!(
                    "vim.mode: expected string, got {:?}; degrading to None",
                    JsonType::of(mode)
                );
                return Ok(None);
            };
            (s, "vim.mode")
        }
        serde_json::Value::String(s) => {
            // Bare-string tolerated for compat; log so schema drift
            // from canonical `vim: { mode: "..." }` leaves a trail.
            crate::lsm_debug!(
                "vim: accepted bare-string compat shape {:?}; canonical is {{ mode }}",
                s
            );
            (s.as_str(), "vim")
        }
        other => {
            crate::lsm_warn!(
                "vim: expected object or string, got {:?}; degrading to None",
                JsonType::of(other)
            );
            return Ok(None);
        }
    };
    // Unknown vim modes degrade the segment, not the whole render:
    // `vim` is opt-in and informational, so a future CC mode (e.g.
    // `select`, `terminal`) shouldn't blank the statusline.
    match raw.parse::<VimMode>() {
        Ok(mode) => Ok(Some(mode)),
        Err(()) => {
            crate::lsm_warn!(
                "vim: unknown mode {raw:?} at {path}; degrading to None (possible CC schema drift — known: normal, insert, visual, command, replace)"
            );
            Ok(None)
        }
    }
}

fn parse_output_style(
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<OutputStyle>, ParseError> {
    let Some(value) = root.get("output_style") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(obj) = value.as_object() else {
        crate::lsm_warn!(
            "output_style: expected object, got {:?}; degrading to None",
            JsonType::of(value)
        );
        return Ok(None);
    };
    // Tolerate a missing `name` as None so the schema can grow,
    // but warn: the wrapper is present, so a future CC rename of
    // `name` would otherwise dark-ship this segment without a
    // diagnostic trail.
    let Some(name_value) = obj.get("name") else {
        crate::lsm_warn!(
            "output_style: wrapper present but `name` field missing; degrading to None (possible CC schema drift)"
        );
        return Ok(None);
    };
    if name_value.is_null() {
        return Ok(None);
    }
    let Some(name) = name_value.as_str() else {
        crate::lsm_warn!(
            "output_style.name: expected string, got {:?}; degrading to None",
            JsonType::of(name_value)
        );
        return Ok(None);
    };
    // Empty `name` is the documented "no active style" shape (CC emits
    // `""` when the user hasn't selected one); silent fold to None.
    if name.is_empty() {
        return Ok(None);
    }
    Ok(Some(OutputStyle {
        name: name.to_owned(),
    }))
}

fn parse_version(
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<String>, ParseError> {
    let Some(value) = root.get("version") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(raw) = value.as_str() else {
        crate::lsm_warn!(
            "version: expected string, got {:?}; degrading to None",
            JsonType::of(value)
        );
        return Ok(None);
    };
    // Trim and fold whitespace-only / empty to None — the
    // empty-payload contract should treat `"  "` the same as `""`
    // and `null` rather than rendering a blank-looking version.
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed.to_owned()))
}

fn parse_agent_name(
    root: &serde_json::Map<String, serde_json::Value>,
) -> Result<Option<String>, ParseError> {
    let Some(value) = root.get("agent") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let Some(obj) = value.as_object() else {
        crate::lsm_warn!(
            "agent: expected object, got {:?}; degrading to None",
            JsonType::of(value)
        );
        return Ok(None);
    };
    // Same drift-detection rationale as `parse_output_style`: warn
    // when the wrapper is present but `name` is absent.
    let Some(name_value) = obj.get("name") else {
        crate::lsm_warn!(
            "agent: wrapper present but `name` field missing; degrading to None (possible CC schema drift)"
        );
        return Ok(None);
    };
    if name_value.is_null() {
        return Ok(None);
    }
    let Some(name) = name_value.as_str() else {
        crate::lsm_warn!(
            "agent.name: expected string, got {:?}; degrading to None",
            JsonType::of(name_value)
        );
        return Ok(None);
    };
    // Empty `name` is the documented "no active sub-agent" shape;
    // silent fold to None.
    if name.is_empty() {
        return Ok(None);
    }
    Ok(Some(name.to_owned()))
}

fn expect_object<'a>(
    value: &'a serde_json::Value,
    path: &'static str,
    tool: &Tool,
) -> Result<&'a serde_json::Map<String, serde_json::Value>, ParseError> {
    value
        .as_object()
        .ok_or_else(|| type_mismatch(path, JsonType::Object, JsonType::of(value), tool))
}

fn type_mismatch(
    path: impl Into<String>,
    expected: JsonType,
    got: JsonType,
    tool: &Tool,
) -> ParseError {
    ParseError::TypeMismatch {
        tool: tool.clone(),
        path: path.into(),
        expected,
        got,
    }
}

fn invalid_value(path: impl Into<String>, reason: &'static str, tool: &Tool) -> ParseError {
    ParseError::InvalidValue {
        tool: tool.clone(),
        path: path.into(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::_test_capture_warns;

    fn obj_of(value: serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
        value
            .as_object()
            .cloned()
            .expect("test fixture is a JSON object")
    }

    // --- lsm-coc anchor: dotted-key lookups stay correct -----------------
    //
    // The two-arg helper form (`key`, `path`) replaced an earlier
    // `path_tail`-based design that silently dropped any lookup whose key
    // contained a literal `.`. CC payloads don't have such keys today,
    // but Qwen / Codex / Copilot payloads might, and a future refactor
    // that "simplifies" back to a single arg must trip these tests.

    #[test]
    fn string_leaf_finds_key_with_literal_dot() {
        let obj = obj_of(serde_json::json!({ "a.b": "value" }));
        assert_eq!(string_leaf(&obj, "a.b", "diagnostic.path"), Some("value"));
    }

    #[test]
    fn try_u64_required_finds_key_with_literal_dot() {
        let obj = obj_of(serde_json::json!({ "x.y": 42 }));
        assert_eq!(try_u64_required(&obj, "x.y", "diagnostic.path"), Some(42));
    }

    #[test]
    fn string_leaf_warn_uses_path_not_key() {
        // Pins the contract: `path` is for diagnostics, `key` is for
        // lookup. A refactor that swaps them would still find the
        // value (when the key happened to be the full path) but the
        // warn would echo the wrong identifier.
        let obj = obj_of(serde_json::json!({ "name": 42 })); // wrong-typed
        let (got, warns) = _test_capture_warns(|| {
            string_leaf(&obj, "name", "outer.wrapper.name").map(str::to_owned)
        });
        assert_eq!(got, None);
        assert_eq!(warns.len(), 1);
        assert!(
            warns[0].contains("outer.wrapper.name"),
            "warn should echo the diagnostic path, got: {:?}",
            warns[0]
        );
    }

    // --- parse_git_worktree asymmetric-empty warning --------------------

    #[test]
    fn parse_git_worktree_both_empty_is_silent_none() {
        let obj = obj_of(serde_json::json!({ "name": "", "path": "" }));
        let (got, warns) = _test_capture_warns(|| parse_git_worktree(&obj));
        assert!(got.is_none());
        assert!(warns.is_empty(), "all-empty is documented; should not warn");
    }

    #[test]
    fn parse_git_worktree_asymmetric_empty_warns() {
        let obj = obj_of(serde_json::json!({ "name": "", "path": "/wt/main" }));
        let (got, warns) = _test_capture_warns(|| parse_git_worktree(&obj));
        assert!(got.is_none());
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("name empty"));
        assert!(warns[0].contains("path populated"));
    }

    #[test]
    fn parse_git_worktree_path_empty_name_populated_warns() {
        // Mirror of the previous test — pins both arms of the
        // n_empty / !n_empty branch so a swap bug surfaces.
        let obj = obj_of(serde_json::json!({ "name": "main", "path": "" }));
        let (got, warns) = _test_capture_warns(|| parse_git_worktree(&obj));
        assert!(got.is_none());
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("path empty"));
        assert!(warns[0].contains("name populated"));
    }

    // --- parse_current_usage partial-null warning -----------------------

    #[test]
    fn parse_current_usage_all_null_is_silent_none() {
        let cw = obj_of(serde_json::json!({
            "current_usage": {
                "input_tokens": serde_json::Value::Null,
                "output_tokens": serde_json::Value::Null,
                "cache_creation_input_tokens": serde_json::Value::Null,
                "cache_read_input_tokens": serde_json::Value::Null,
            }
        }));
        let (got, warns) = _test_capture_warns(|| parse_current_usage(&cw));
        assert!(matches!(got, Ok(None)));
        assert!(
            warns.is_empty(),
            "all-null is documented pre-first-call shape; should not warn"
        );
    }

    #[test]
    fn parse_current_usage_partial_population_warns_and_collapses() {
        let cw = obj_of(serde_json::json!({
            "current_usage": {
                "input_tokens": 1,
                "output_tokens": 2,
                "cache_creation_input_tokens": serde_json::Value::Null,
                "cache_read_input_tokens": serde_json::Value::Null,
            }
        }));
        let (got, warns) = _test_capture_warns(|| parse_current_usage(&cw));
        assert!(matches!(got, Ok(None)));
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("2/4"));
    }

    #[test]
    fn parse_current_usage_pure_type_drift_suppresses_collapse_warn() {
        // Every absent leaf is accounted for by a type-drift warn → no
        // collapse-warn fires (avoids double-narrating the same event).
        let cw = obj_of(serde_json::json!({
            "current_usage": {
                "input_tokens": "string",
                "output_tokens": 1,
                "cache_creation_input_tokens": 1,
                "cache_read_input_tokens": 1,
            }
        }));
        let (got, warns) = _test_capture_warns(|| parse_current_usage(&cw));
        assert!(matches!(got, Ok(None)));
        assert_eq!(
            warns.len(),
            1,
            "expected only the per-leaf type-drift warn, got {warns:?}"
        );
        assert!(warns[0].contains("input_tokens"));
        assert!(
            !warns[0].contains("3/4"),
            "collapse-warn must not double-narrate, got {:?}",
            warns[0]
        );
    }

    #[test]
    fn parse_current_usage_mixed_type_drift_and_silent_absence_still_warns_collapse() {
        // Type drift on one leaf + silent absence on another: the
        // per-leaf warn covers only the typed-wrong leaf, so the
        // collapse-warn must still fire to name the asymmetric shape.
        let cw = obj_of(serde_json::json!({
            "current_usage": {
                "input_tokens": "abc",
                "cache_creation_input_tokens": 5,
                "cache_read_input_tokens": serde_json::Value::Null,
            }
        }));
        let (got, warns) = _test_capture_warns(|| parse_current_usage(&cw));
        assert!(matches!(got, Ok(None)));
        assert!(warns.iter().any(|w| w.contains("input_tokens")));
        assert!(
            warns.iter().any(|w| w.contains("1/4")),
            "collapse-warn should still fire when a sibling is silently absent, got {warns:?}"
        );
    }
}
