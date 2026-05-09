use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use rhai::{Dynamic, Engine, Map};

use linesmith_plugin::{CompiledPlugin, PluginRegistry};

use super::super::{built_in_by_id, LineItem, Segment, Separator, DEFAULT_SEGMENT_IDS};
use super::layout::{resolve_layout_separator, single_line_entries, validated_numbered_lines};
use super::plugins::{apply_override, bundle_plugins, toml_table_to_dynamic};
use crate::config;
use crate::plugins::RhaiSegment;

/// Build the default line: every built-in in canonical order with
/// `Separator::Space` between each pair, no overrides applied.
#[must_use]
pub fn build_default_segments() -> Vec<LineItem> {
    let segs: Vec<Box<dyn Segment>> = DEFAULT_SEGMENT_IDS
        .iter()
        .filter_map(|id| built_in_by_id(id, None, &mut |_| {}))
        .collect();
    interleave_separators(segs, &Separator::Space)
}

/// Walk a built segment list and interleave `sep` between adjacent
/// segments, producing the [`LineItem`] sequence the renderer
/// consumes. No leading or trailing separator.
fn interleave_separators(segs: Vec<Box<dyn Segment>>, sep: &Separator) -> Vec<LineItem> {
    let n = segs.len();
    // n=0 saturates to 0; n>=1 gives 2n-1 slots (n segments + n-1 separators).
    let mut items = Vec::with_capacity(n.saturating_mul(2).saturating_sub(1));
    for (i, seg) in segs.into_iter().enumerate() {
        items.push(LineItem::Segment(seg));
        if i + 1 < n {
            items.push(LineItem::Separator(sep.clone()));
        }
    }
    items
}

/// Build a segment list from an optional [`Config`](config::Config).
/// `None` or a config without a `[line]` section uses the default
/// order. `warn` receives a one-line diagnostic for each validation
/// rule triggered (pass `|_| {}` to discard).
///
/// `plugins` carries the discovered [`PluginRegistry`] plus its
/// shared engine. Built-in ids win on collision (the registry already
/// rejects plugins shadowing built-ins at load time, so a plugin
/// reaching this function can only collide with another plugin or
/// stand alone).
///
/// Implements the validation rules in `docs/specs/config.md`
/// §Validation rules: unknown ids skip with a warning, duplicates
/// keep the first, an explicit `segments = []` warns, inverted width
/// bounds drop the override with a warning.
///
/// Single-line entry; multi-line callers should use [`build_lines`].
/// When this function is invoked against a `layout = "multi-line"`
/// config, it returns line 1 (sorted by numbered key) and warns,
/// rather than reading the empty `[line].segments` field and
/// returning nothing. Without that fallback, embedders loading the
/// multi-line `power-user` preset would silently render a blank
/// status line.
pub fn build_segments(
    config: Option<&config::Config>,
    plugins: Option<(PluginRegistry, Arc<Engine>)>,
    mut warn: impl FnMut(&str),
) -> Vec<LineItem> {
    let configured_line = config.and_then(|c| c.line.as_ref());
    let layout_mode = config.map(|c| c.layout).unwrap_or_default();

    // If the caller is on the legacy single-line API but the config
    // declares multi-line, hand them line 1 with a warning. Without
    // the fallback, `[line].segments` would be empty and the
    // embedder would silently render a blank status line.
    if matches!(layout_mode, config::LayoutMode::MultiLine) {
        if let Some(first) = validated_numbered_lines(configured_line, &mut warn)
            .and_then(|mut v| (!v.is_empty()).then(|| v.remove(0)))
        {
            warn("layout = \"multi-line\" passed to build_segments (the single-line API); rendering line 1 only. Call build_lines to render every [line.N] sub-table.");
            let layout_separator = resolve_layout_separator(config, &mut warn);
            let mut plugin_bundle = bundle_plugins(plugins);
            let mut consumed = std::collections::HashSet::new();
            return build_one_line(
                &first,
                config,
                &mut plugin_bundle,
                &mut consumed,
                &layout_separator,
                &mut warn,
            );
        }
        // Multi-line declared but no usable [line.N]; fall through to
        // the single-line path which already warns on empty segments.
    }

    if let Some(line) = configured_line {
        if line.segments.is_empty() {
            warn("[line].segments is empty; no segments will render");
        }
    }

    let layout_separator = resolve_layout_separator(config, &mut warn);

    let entries: Vec<config::LineEntry> = match configured_line {
        Some(l) => l.segments.clone(),
        None => DEFAULT_SEGMENT_IDS
            .iter()
            .map(|&s| config::LineEntry::Id(s.to_string()))
            .collect(),
    };

    let mut plugin_bundle = bundle_plugins(plugins);
    let mut consumed = std::collections::HashSet::new();
    build_one_line(
        &entries,
        config,
        &mut plugin_bundle,
        &mut consumed,
        &layout_separator,
        &mut warn,
    )
}

/// Build a list of segment lists, one per rendered line. Single-line
/// configs return a vec of length 1; multi-line configs return one
/// inner vec per `[line.N]` sub-table sorted by the parsed integer
/// key. Edge cases per `docs/specs/config.md` §Edge cases:
///
/// - `layout = "multi-line"` without usable `[line.N]` sub-tables
///   warns and falls back to single-line using `[line].segments`.
/// - `layout = "single-line"` (or unset) with `[line.N]` tables
///   present warns and ignores the numbered tables.
/// - Numbered keys that don't parse as positive integers (e.g.
///   `[line.foo]`) warn and drop.
/// - In multi-line mode with both `[line].segments` and `[line.N]`
///   present, the numbered tables win and `[line].segments` is
///   ignored. The spec's edge-case table doesn't enumerate this
///   combination; this is a builder-level precedence choice
///   matching the principle that single-line callers should go
///   through [`build_segments`].
///
/// Plugin segments can appear in at most one line per render: the
/// shared plugin lookup is consumed on first use. A plugin id
/// referenced again in a later line surfaces as "plugin '<id>' was
/// rendered on an earlier line" so the user knows the cause is
/// reuse, not a typo. v0.1 limitation; lifting it (Arc-shared or
/// cloneable `CompiledPlugin`) is tracked separately.
pub fn build_lines(
    config: Option<&config::Config>,
    plugins: Option<(PluginRegistry, Arc<Engine>)>,
    mut warn: impl FnMut(&str),
) -> Vec<Vec<LineItem>> {
    let mode = config.map(|c| c.layout).unwrap_or_default();
    let line_cfg = config.and_then(|c| c.line.as_ref());

    let line_entry_lists: Vec<Vec<config::LineEntry>> = match mode {
        config::LayoutMode::SingleLine => {
            // Two single-line + numbered combinations: if `segments`
            // is populated, the user picked single-line on purpose
            // and the numbered tables are noise — warn and ignore.
            // If `segments` is empty AND numbered is populated, the
            // user almost certainly meant multi-line and forgot the
            // `layout = "multi-line"` line; auto-promote with a hint
            // rather than silently rendering nothing.
            let has_numbered = line_cfg.is_some_and(|l| !l.numbered.is_empty());
            let has_segments = line_cfg.is_some_and(|l| !l.segments.is_empty());
            if has_numbered && !has_segments {
                if let Some(promoted) = validated_numbered_lines(line_cfg, &mut warn) {
                    warn("[line.N] sub-tables present but no top-level `layout` field; treating as multi-line. Add `layout = \"multi-line\"` to silence this warning.");
                    promoted
                } else {
                    warn("[line.N] sub-tables present but none are usable, and [line].segments is empty; nothing will render");
                    single_line_entries(line_cfg, &mut warn)
                }
            } else {
                if has_numbered {
                    warn("layout is single-line but [line.N] sub-tables are present; ignoring numbered tables and rendering [line].segments");
                }
                single_line_entries(line_cfg, &mut warn)
            }
        }
        config::LayoutMode::MultiLine => match validated_numbered_lines(line_cfg, &mut warn) {
            Some(lines) => lines,
            None => {
                warn("layout = \"multi-line\" but no usable [line.N] sub-tables; falling back to single-line using [line].segments");
                single_line_entries(line_cfg, &mut warn)
            }
        },
    };

    let layout_separator = resolve_layout_separator(config, &mut warn);
    let mut plugin_bundle = bundle_plugins(plugins);
    let mut consumed_plugins = std::collections::HashSet::<String>::new();

    line_entry_lists
        .into_iter()
        .map(|entries| {
            build_one_line(
                &entries,
                config,
                &mut plugin_bundle,
                &mut consumed_plugins,
                &layout_separator,
                &mut warn,
            )
        })
        .collect()
}
/// Inner segment-building loop, shared by single-line `build_segments`
/// and per-line `build_lines`. Walks `entries` and emits a
/// [`LineItem`] sequence per ADR-0024:
///
/// - Bare-string / `type = "<segment-id>"` entries materialize as
///   [`LineItem::Segment`].
/// - `type = "separator"` entries materialize as [`LineItem::Separator`]
///   using the entry's `character` override or the global
///   `[layout_options].separator` fallback.
/// - Adjacent segments with no explicit separator between them get
///   the global `layout_separator` interleaved (preserves pre-ADR
///   behavior for string-only configs).
/// - A segment with `merge = true` suppresses the boundary at its
///   right edge: the implicit interleave is skipped AND any
///   immediately-following explicit separator is dropped silently.
///
/// Dedupes segment ids within a single call (so duplicates within
/// one line warn) but not across calls — multi-line configs that
/// list the same built-in id in two different lines produce two
/// independent segment instances, which is the right behavior for
/// stateless built-ins. Separator entries are NOT deduped (each
/// separator entry is positionally distinct).
///
/// `consumed_plugins` tracks plugin ids removed from the shared
/// lookup by earlier `build_one_line` calls. The lookup itself can't
/// distinguish "never existed" from "consumed" after a `remove`, so
/// we shadow consumption here. A lookup miss combined with a
/// consumed-set hit produces the specific "rendered on an earlier
/// line" warning; otherwise the generic "unknown segment id"
/// diagnostic fires.
fn build_one_line(
    entries: &[config::LineEntry],
    config: Option<&config::Config>,
    plugin_bundle: &mut Option<(HashMap<String, CompiledPlugin>, Arc<Engine>)>,
    consumed_plugins: &mut std::collections::HashSet<String>,
    layout_separator: &Separator,
    warn: &mut impl FnMut(&str),
) -> Vec<LineItem> {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut items: Vec<LineItem> = Vec::with_capacity(entries.len() * 2);
    // True when the most-recently-pushed segment had `merge = true`.
    // Cleared when the next segment lands; persists across an
    // explicit separator entry (so `seg(merge), |, seg` drops the
    // explicit separator AND the implicit interleave).
    let mut merge_pending = false;

    for entry in entries {
        if matches!(entry, config::LineEntry::Item(item) if item.kind.as_deref() == Some("separator") && item.merge.is_some())
        {
            warn("[line].segments separator entry has `merge = ...`; ignoring (merge is for segment entries)");
        }
        if matches!(entry, config::LineEntry::Item(item) if item.kind.as_deref() != Some("separator") && item.character.is_some())
        {
            warn("[line].segments segment entry has `character = ...`; ignoring (character is for separator entries)");
        }

        match entry.kind() {
            None => {
                warn("[line].segments inline-table entry is missing `type`; skipping");
                continue;
            }
            Some("separator") => {
                if merge_pending {
                    // Suppress: the preceding segment opted into
                    // merge with its right neighbor. The merge flag
                    // stays armed until the next segment lands.
                    continue;
                }
                match items.last() {
                    Some(LineItem::Segment(_)) => {} // proceed below
                    Some(LineItem::Separator(_)) => {
                        warn(
                            "[line].segments has consecutive separator entries; keeping the first",
                        );
                        continue;
                    }
                    None => {
                        warn("[line].segments leads with a separator entry; skipping");
                        continue;
                    }
                }
                let sep = entry.separator_character().map_or_else(
                    || layout_separator.clone(),
                    |c| Separator::Literal(Cow::Owned(c.to_string())),
                );
                items.push(LineItem::Separator(sep));
            }
            Some(id) => {
                if !seen.insert(id.to_string()) {
                    warn(&format!(
                        "segment '{id}' listed more than once; keeping first occurrence"
                    ));
                    continue;
                }
                let cfg_override = config.and_then(|c| c.segments.get(id));
                let extras = cfg_override.map(|ov| &ov.extra);
                let inner = if let Some(b) = built_in_by_id(id, extras, warn) {
                    Some(b)
                } else if let Some((lookup, engine)) = plugin_bundle.as_mut() {
                    lookup.remove(id).map(|plugin| {
                        consumed_plugins.insert(id.to_string());
                        let plugin_config = cfg_override.map_or_else(
                            || Dynamic::from_map(Map::new()),
                            |ov| toml_table_to_dynamic(&ov.extra),
                        );
                        Box::new(RhaiSegment::from_compiled(
                            plugin,
                            engine.clone(),
                            plugin_config,
                        )) as Box<dyn Segment>
                    })
                } else {
                    None
                };
                let Some(inner) = inner else {
                    if consumed_plugins.contains(id) {
                        warn(&format!(
                            "plugin '{id}' was rendered on an earlier line; v0.1 supports each plugin on at most one line per render — skipping"
                        ));
                    } else {
                        warn(&format!("unknown segment id '{id}' — skipping"));
                    }
                    continue;
                };
                let seg = apply_override(id, inner, cfg_override, warn);

                // Implicit interleave: when the previous item is a
                // segment (no explicit separator between them) and
                // the previous segment didn't request merge, insert
                // the global default separator. When the previous
                // item is already a Separator (explicit), don't
                // double-up.
                if matches!(items.last(), Some(LineItem::Segment(_))) && !merge_pending {
                    items.push(LineItem::Separator(layout_separator.clone()));
                }
                items.push(LineItem::Segment(seg));
                merge_pending = entry.merge();
            }
        }
    }
    items
}
