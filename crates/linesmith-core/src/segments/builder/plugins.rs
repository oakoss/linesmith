use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use rhai::{Array, Dynamic, Engine, Map};

use linesmith_plugin::{CompiledPlugin, PluginRegistry};

use super::super::{OverriddenSegment, Segment, WidthBounds};
use crate::config;
use crate::theme;

/// Bundle the lookup table with its engine so the borrow checker
/// (rather than a runtime invariant + `expect`) enforces that
/// plugin renders never reach for a missing engine. Called once per
/// `build_segments` / `build_lines` invocation; the resulting bundle
/// threads through `build_one_line` and is consumed across whichever
/// lines reference plugin ids.
pub(super) fn bundle_plugins(
    plugins: Option<(PluginRegistry, Arc<Engine>)>,
) -> Option<(HashMap<String, CompiledPlugin>, Arc<Engine>)> {
    plugins.map(|(registry, engine)| {
        let lookup: HashMap<String, CompiledPlugin> = registry
            .into_plugins()
            .into_iter()
            .map(|p| (p.id().to_string(), p))
            .collect();
        (lookup, engine)
    })
}
/// Convert the `extra` bag of a `[segments.<plugin-id>]` block into
/// the `rhai::Map` a plugin sees as `ctx.config`.
pub(super) fn toml_table_to_dynamic(table: &BTreeMap<String, toml::Value>) -> Dynamic {
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

pub(super) fn apply_override(
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
