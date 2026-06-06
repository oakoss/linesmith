use super::super::{PowerlineWidth, Separator, DEFAULT_SEGMENT_IDS};
use crate::config;

/// Resolve the single-line entry list and warn on an explicitly empty
/// `[line].segments`. Returns one vec wrapped in an outer vec for
/// uniform treatment by [`build_lines`].
pub(super) fn single_line_entries(
    line_cfg: Option<&config::LineConfig>,
    warn: &mut impl FnMut(&str),
) -> Vec<Vec<config::LineEntry>> {
    if let Some(line) = line_cfg {
        if line.segments.is_empty() {
            warn("[line].segments is empty; no segments will render");
        }
    }
    let entries: Vec<config::LineEntry> = match line_cfg {
        Some(l) => l.segments.clone(),
        None => DEFAULT_SEGMENT_IDS
            .iter()
            .map(|&s| config::LineEntry::Id(s.to_string()))
            .collect(),
    };
    vec![entries]
}

/// Validate `[line.N]` sub-tables for multi-line mode. The flatten
/// map carries raw [`toml::Value`]s so the parser can preserve the
/// "unknown keys are warnings" forward-compat contract; this
/// function does the per-key validation: numeric key, positive,
/// pointing at a table with a `segments` array of strings.
/// Anything else gets a warning and is dropped. Returns `None` if
/// no usable lines remain — caller falls back to single-line. Sorts
/// by parsed integer so `[line.10]` follows `[line.2]` rather than
/// coming before it lexicographically.
pub(super) fn validated_numbered_lines(
    line_cfg: Option<&config::LineConfig>,
    warn: &mut impl FnMut(&str),
) -> Option<Vec<Vec<config::LineEntry>>> {
    let line = line_cfg?;
    if line.numbered.is_empty() {
        return None;
    }
    let mut valid: Vec<(u32, Vec<config::LineEntry>)> = line
        .numbered
        .iter()
        .filter_map(|(key, value)| {
            // Distinguish "non-table value under [line]" (a typo'd or
            // forward-compat scalar like `[line] segmnts = [...]`)
            // from "table-shaped sub-table with a non-integer key"
            // (`[line.foo]`). Both warn and drop, but the wording
            // points the user at the right fix.
            if !matches!(value, toml::Value::Table(_)) {
                warn(&format!(
                    "[line] has unknown key '{key}' ({}); expected `[line.N]` sub-tables only. Skipping.",
                    describe_toml_value(value)
                ));
                return None;
            }
            let n = match key.parse::<u32>() {
                Ok(n) if n > 0 => n,
                _ => {
                    warn(&format!(
                        "[line.{key}] is not a positive integer key; skipping"
                    ));
                    return None;
                }
            };
            extract_line_segments(key, value, warn).map(|segs| (n, segs))
        })
        .collect();
    if valid.is_empty() {
        return None;
    }
    valid.sort_by_key(|(n, _)| *n);
    for (n, segs) in &valid {
        if segs.is_empty() {
            warn(&format!(
                "[line.{n}].segments is empty; that line will render nothing"
            ));
        }
    }
    Some(valid.into_iter().map(|(_, segs)| segs).collect())
}

/// Pull the `segments` array out of one `[line.N]` value as a
/// `Vec<LineEntry>`. Per ADR-0024, the array accepts a mixed shape:
/// bare strings round-trip as [`config::LineEntry::Id`]; inline
/// tables round-trip as [`config::LineEntry::Item`]. Returns `None`
/// on shape mismatches (non-table, missing/wrong-typed `segments`)
/// with a targeted diagnostic; non-string/non-table entries inside
/// the array warn and drop per-item.
fn extract_line_segments(
    key: &str,
    value: &toml::Value,
    warn: &mut impl FnMut(&str),
) -> Option<Vec<config::LineEntry>> {
    let table = match value {
        toml::Value::Table(t) => t,
        other => {
            warn(&format!(
                "[line] key '{key}' is a {} (expected a sub-table with `segments = [...]`); skipping",
                describe_toml_value(other)
            ));
            return None;
        }
    };
    let segments_value = match table.get("segments") {
        Some(v) => v,
        None => {
            warn(&format!("[line.{key}] has no `segments` array; skipping"));
            return None;
        }
    };
    let array = match segments_value {
        toml::Value::Array(a) => a,
        other => {
            warn(&format!(
                "[line.{key}].segments is a {} (expected an array of strings or inline tables); skipping",
                describe_toml_value(other)
            ));
            return None;
        }
    };
    let mut segs = Vec::with_capacity(array.len());
    for (i, item) in array.iter().enumerate() {
        match item {
            toml::Value::String(s) => segs.push(config::LineEntry::Id(s.clone())),
            toml::Value::Table(t) => {
                match toml::Value::Table(t.clone()).try_into::<config::LineEntryItem>() {
                    Ok(parsed) => segs.push(config::LineEntry::Item(parsed)),
                    Err(e) => warn(&format!(
                        "[line.{key}].segments[{i}] inline table didn't parse ({e}); skipping that item",
                    )),
                }
            }
            other => {
                warn(&format!(
                    "[line.{key}].segments[{i}] is a {} (expected a string or inline table); skipping that item",
                    describe_toml_value(other)
                ));
            }
        }
    }
    Some(segs)
}

fn describe_toml_value(v: &toml::Value) -> &'static str {
    match v {
        toml::Value::String(_) => "string",
        toml::Value::Integer(_) => "integer",
        toml::Value::Float(_) => "float",
        toml::Value::Boolean(_) => "boolean",
        toml::Value::Datetime(_) => "datetime",
        toml::Value::Array(_) => "array",
        toml::Value::Table(_) => "table",
    }
}
/// Resolve the `[layout_options].separator` once for the whole
/// config. Shared by both single- and multi-line builds so per-line
/// caches don't repeat the same warning.
pub(super) fn resolve_layout_separator(
    config: Option<&config::Config>,
    warn: &mut impl FnMut(&str),
) -> Separator {
    let powerline_width = config
        .and_then(|c| c.layout_options.as_ref())
        .and_then(|lo| lo.powerline_width)
        .map(|w| validate_powerline_width(w, warn))
        .unwrap_or_default();
    config
        .and_then(|c| c.layout_options.as_ref())
        .and_then(|lo| lo.separator.as_deref())
        .map(|s| parse_layout_separator(s, powerline_width, warn))
        .unwrap_or(Separator::Space)
}

/// Resolve the `[layout_options].icons` mode once for the whole
/// config. Absence follows `LayoutOptions`' default.
pub(super) fn resolve_icon_mode(config: Option<&config::Config>) -> config::IconMode {
    config
        .and_then(|c| c.layout_options.as_ref())
        .map(|lo| lo.icons)
        .unwrap_or_default()
}

/// Parse a `[layout_options].separator` string into a [`Separator`].
///
/// - `"space"` → [`Separator::Space`]
/// - `"powerline"` → [`Separator::Powerline`] (`powerline_width`
///   controls the chevron cell count, 1 or 2)
/// - `"capsule"` / `"flex"` — reserved for v0.2+; warn + fall back to
///   `Space` so today's configs migrate cleanly when the v0.2
///   renderers land
/// - `""` (truly empty) → [`Separator::None`]
/// - anything else → [`Separator::Literal`] verbatim, e.g.
///   `separator = " | "` for ccstatusline-parity
///
/// Reserved-keyword matching is case- and whitespace-insensitive
/// against the trimmed input. Typos do not warn — `"powereline"`
/// renders as the literal word, since "anything not a reserved
/// keyword is a literal" is the contract.
pub(super) fn parse_layout_separator(
    value: &str,
    powerline_width: PowerlineWidth,
    warn: &mut impl FnMut(&str),
) -> Separator {
    // Empty (truly zero-length) is "no separator." Whitespace-only is
    // a deliberate literal — `separator = "   "` means "three spaces
    // between segments," not "no separator." Distinguishing these
    // here prevents the `value.trim()` keyword pre-pass from eating
    // user-meaningful whitespace literals.
    if value.is_empty() {
        return Separator::None;
    }
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "space" => Separator::Space,
        "powerline" => Separator::Powerline {
            width: powerline_width,
        },
        "capsule" | "flex" => {
            warn(&format!(
                "[layout_options].separator '{value}' is reserved for v0.2+; rendering as 'space'"
            ));
            Separator::Space
        }
        _ => Separator::Literal(std::borrow::Cow::Owned(value.to_string())),
    }
}

/// Validate `[layout_options].powerline_width`. Only `1` and `2` are
/// meaningful — most Nerd Fonts render U+E0B0 as 1 cell, some
/// fonts/sizes render it as 2. Any other value warns and falls back
/// to `1` so a typo doesn't silently desync the layout math.
pub(super) fn validate_powerline_width(width: u16, warn: &mut impl FnMut(&str)) -> PowerlineWidth {
    match width {
        1 => PowerlineWidth::One,
        2 => PowerlineWidth::Two,
        other => {
            warn(&format!(
                "[layout_options].powerline_width = {other} is not 1 or 2; using 1"
            ));
            PowerlineWidth::One
        }
    }
}
