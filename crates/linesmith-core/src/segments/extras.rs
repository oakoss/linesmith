//! Workspace-level helpers for reading `[segments.<id>]` TOML extras.
//!
//! The render-time config bag is `BTreeMap<String, toml::Value>`; each
//! segment's `from_extras` constructor reads its own keys out of it.
//! Helpers here cover patterns that any segment can use
//! (warn-on-wrong-type, silent-on-absent) so segment families don't
//! reinvent them.
//!
//! Family-specific helpers (e.g. the rate-limit family's
//! `apply_common_extras` and `parse_*_format` parsers) live next to
//! the family they serve, not here.
//!
//! Warn-message format matches the existing convention:
//! `segments.{id}.{key}: expected <type>; ignoring`.

use std::collections::BTreeMap;

/// Read a boolean knob from `[segments.<id>]`. Wrong-type values warn
/// and return `None`; absent keys are silent.
#[must_use]
pub(crate) fn parse_bool(
    extras: &BTreeMap<String, toml::Value>,
    key: &str,
    id: &str,
    warn: &mut impl FnMut(&str),
) -> Option<bool> {
    let v = extras.get(key)?;
    match v.as_bool() {
        Some(b) => Some(b),
        None => {
            warn(&format!("segments.{id}.{key}: expected bool; ignoring"));
            None
        }
    }
}
