//! Parses the optional `@data_deps = [...]` declaration from the first
//! contiguous block of `//` line comments at the top of a plugin
//! script. See `docs/specs/plugin-api.md` §@data_deps header syntax for
//! the full contract.
//!
//! Resolved dep list is always a superset of
//! `&[DataDep::Status]` — every plugin implicitly has access to the
//! stdin payload — even if the author lists other deps explicitly or
//! declares no header at all.

use crate::data_context::DataDep;

/// Error surface for header parsing. The registry layer wraps these
/// into [`PluginError`](super::errors::PluginError) variants with
/// the plugin id attached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    /// The `@data_deps = ...` RHS didn't parse as a JSON-style array
    /// of bare-string dep names.
    Malformed(String),
    /// A listed dep name is not in the plugin-accessible set.
    /// Per `plugin-api.md`, `credentials` and `jsonl` are reserved
    /// and surface here even though they're real `DataDep` variants.
    UnknownDep(String),
}

/// Map a header token to its [`DataDep`]. Rejects `credentials` and
/// `jsonl` (reserved-from-plugin per `plugin-api.md` §@data_deps
/// header syntax) by returning `None`, so the parser can report them
/// as [`HeaderError::UnknownDep`].
fn dep_from_token(token: &str) -> Option<DataDep> {
    match token {
        "status" => Some(DataDep::Status),
        "settings" => Some(DataDep::Settings),
        "claude_json" => Some(DataDep::ClaudeJson),
        "usage" => Some(DataDep::Usage),
        "sessions" => Some(DataDep::Sessions),
        "git" => Some(DataDep::Git),
        _ => None,
    }
}

/// Parse a plugin script's `@data_deps` header. Returns the resolved
/// dep list (always including [`DataDep::Status`]), `HeaderError` on
/// malformed syntax or an unknown / reserved dep name.
///
/// Accepts:
/// - No header at all (defaults to `[Status]`)
/// - Empty array (`@data_deps = []`) — same as no header
/// - Single-line (`@data_deps = ["status", "usage"]`)
/// - Multi-line across multiple `//` comment lines
/// - Trailing commas
/// - Single or double quotes around each name
pub fn parse_data_deps_header(src: &str) -> Result<Vec<DataDep>, HeaderError> {
    let header_block = collect_header_block(src);
    let Some(rhs) = find_data_deps_rhs(&header_block)? else {
        return Ok(vec![DataDep::Status]);
    };
    let tokens = split_array_body(rhs)?;
    let mut deps = vec![DataDep::Status];
    for token in tokens {
        match dep_from_token(&token) {
            Some(dep) => {
                if !deps.contains(&dep) {
                    deps.push(dep);
                }
            }
            None => return Err(HeaderError::UnknownDep(token)),
        }
    }
    Ok(deps)
}

/// Concatenate the first contiguous block of `//` comment lines,
/// stripping the `//` prefix and optional single following space from
/// each. A blank line or any non-`//` line ends the block (per spec).
fn collect_header_block(src: &str) -> String {
    let mut buf = String::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            break;
        }
        let Some(rest) = trimmed.strip_prefix("//") else {
            break;
        };
        // Drop a single leading space after `//` for ergonomic
        // multi-line indentation; anything else is kept verbatim.
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        buf.push_str(rest);
        buf.push('\n');
    }
    buf
}

/// Locate `@data_deps = [ ... ]` in the header block and return the
/// text starting right after the opening `[`. `Ok(None)` when no
/// `@data_deps` declaration exists at all (a valid "no header" state
/// resolved to the default `[Status]` by the caller). If the key is
/// present but the `= [` shape is missing (e.g. `@data_deps ["x"]`
/// or `@data_deps = "x"`), returns [`HeaderError::Malformed`] rather
/// than silently degrading to the default — writing `@data_deps`
/// signals intent, so a malformed RHS should surface as an error.
/// Missing closing `]` is detected downstream by [`split_array_body`].
fn find_data_deps_rhs(header: &str) -> Result<Option<&str>, HeaderError> {
    let Some(start) = header.find("@data_deps") else {
        return Ok(None);
    };
    let after_key = &header[start + "@data_deps".len()..];
    let Some(eq_pos) = after_key.find('=') else {
        return Err(HeaderError::Malformed(
            "@data_deps declaration missing `=`".to_string(),
        ));
    };
    let after_eq = after_key[eq_pos + 1..].trim_start();
    let Some(open) = after_eq.strip_prefix('[') else {
        return Err(HeaderError::Malformed(
            "@data_deps RHS must be an array literal starting with `[`".to_string(),
        ));
    };
    Ok(Some(open))
}

/// Split the body between `[` and `]` into trimmed, unquoted tokens.
/// Whitespace, newlines, trailing commas, and inline `//` comments
/// (per spec §@data_deps header syntax "comments inside the array")
/// are tolerated. Missing closing `]` or unbalanced quoting surfaces
/// as [`HeaderError::Malformed`].
fn split_array_body(body: &str) -> Result<Vec<String>, HeaderError> {
    let Some(end) = body.find(']') else {
        return Err(HeaderError::Malformed(
            "missing closing `]` in @data_deps array".to_string(),
        ));
    };
    let inside = &body[..end];
    // Strip `//` inline comments line-by-line before comma-splitting.
    // `//` extends to end-of-line, not end-of-fragment; a fragment
    // can span multiple lines (a dep on one line, a justification
    // comment on the next), so we can't just find the first `//`.
    let stripped: String = inside
        .lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join(" ");
    let mut tokens = Vec::new();
    for raw in stripped.split(',') {
        let s = raw.trim();
        if s.is_empty() {
            continue;
        }
        let unquoted = unquote(s)?;
        tokens.push(unquoted);
    }
    Ok(tokens)
}

fn unquote(s: &str) -> Result<String, HeaderError> {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        Ok(s[1..s.len() - 1].to_string())
    } else {
        Err(HeaderError::Malformed(format!(
            "expected quoted string, got `{s}`"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_header_defaults_to_status_only() {
        let src = "fn render(ctx) { () }";
        assert_eq!(parse_data_deps_header(src), Ok(vec![DataDep::Status]));
    }

    #[test]
    fn empty_array_defaults_to_status_only() {
        let src = "// @data_deps = []\nfn render(ctx) {}";
        assert_eq!(parse_data_deps_header(src), Ok(vec![DataDep::Status]));
    }

    #[test]
    fn single_line_single_entry_unions_with_status() {
        let src = r#"// @data_deps = ["usage"]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Ok(vec![DataDep::Status, DataDep::Usage])
        );
    }

    #[test]
    fn single_line_multi_entry() {
        let src = r#"// @data_deps = ["settings", "usage", "git"]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Ok(vec![
                DataDep::Status,
                DataDep::Settings,
                DataDep::Usage,
                DataDep::Git
            ])
        );
    }

    #[test]
    fn explicit_status_is_accepted_without_duplication() {
        let src = r#"// @data_deps = ["status", "usage"]
fn render(ctx) {}"#;
        let deps = parse_data_deps_header(src).unwrap();
        assert_eq!(deps, vec![DataDep::Status, DataDep::Usage]);
        assert_eq!(
            deps.iter().filter(|d| **d == DataDep::Status).count(),
            1,
            "Status must not be duplicated when listed explicitly"
        );
    }

    #[test]
    fn multi_line_array_accepted() {
        let src = r#"// @data_deps = [
//   "settings",
//   "usage",
//   "git",
// ]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Ok(vec![
                DataDep::Status,
                DataDep::Settings,
                DataDep::Usage,
                DataDep::Git
            ])
        );
    }

    #[test]
    fn trailing_comma_in_single_line_ok() {
        let src = r#"// @data_deps = ["usage",]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Ok(vec![DataDep::Status, DataDep::Usage])
        );
    }

    #[test]
    fn single_quotes_accepted() {
        let src = "// @data_deps = ['usage']\nfn render(ctx) {}";
        assert_eq!(
            parse_data_deps_header(src),
            Ok(vec![DataDep::Status, DataDep::Usage])
        );
    }

    #[test]
    fn unknown_dep_name_rejected() {
        let src = r#"// @data_deps = ["usage", "mystery"]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Err(HeaderError::UnknownDep("mystery".to_string()))
        );
    }

    #[test]
    fn reserved_credentials_dep_rejected_as_unknown() {
        // `credentials` is a real DataDep variant but not plugin-
        // accessible per spec §@data_deps header syntax. Header
        // parser must reject it with UnknownDep, matching the
        // error surface for truly unknown names.
        let src = r#"// @data_deps = ["credentials"]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Err(HeaderError::UnknownDep("credentials".to_string()))
        );
    }

    #[test]
    fn reserved_jsonl_dep_rejected_as_unknown() {
        let src = r#"// @data_deps = ["jsonl"]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Err(HeaderError::UnknownDep("jsonl".to_string()))
        );
    }

    #[test]
    fn blank_line_ends_header_block() {
        // The header is the first block of `//` lines. A blank line
        // after it means `@data_deps` below is in a different block
        // and must not be parsed.
        let src = r#"// top comment

// @data_deps = ["usage"]
fn render(ctx) {}"#;
        // The `@data_deps` line is in a second block, so the first
        // block's resolution defaults to [Status] only.
        assert_eq!(parse_data_deps_header(src), Ok(vec![DataDep::Status]));
    }

    #[test]
    fn non_comment_line_ends_header_block() {
        // Anything that doesn't start with `//` (after trimming
        // whitespace) ends the block — including rhai statements.
        let src = r#"// top comment
fn render(ctx) {}
// @data_deps = ["usage"]"#;
        assert_eq!(parse_data_deps_header(src), Ok(vec![DataDep::Status]));
    }

    #[test]
    fn header_appearing_after_other_comments_still_parses() {
        // Multi-line `//` comments before `@data_deps` are part of
        // the same header block; the parser finds the declaration
        // regardless of its position within the block.
        let src = r#"// Some plugin description
// Authored by me
// @data_deps = ["usage"]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Ok(vec![DataDep::Status, DataDep::Usage])
        );
    }

    #[test]
    fn malformed_missing_equals_rejected() {
        // Spec intent: writing `@data_deps` declares a header, so
        // malformed RHS must surface as an error — not silently
        // downgrade to the default `[Status]`.
        let src = r#"// @data_deps ["usage"]
fn render(ctx) {}"#;
        assert!(matches!(
            parse_data_deps_header(src),
            Err(HeaderError::Malformed(_))
        ));
    }

    #[test]
    fn malformed_scalar_rhs_rejected() {
        let src = r#"// @data_deps = "usage"
fn render(ctx) {}"#;
        assert!(matches!(
            parse_data_deps_header(src),
            Err(HeaderError::Malformed(_))
        ));
    }

    #[test]
    fn malformed_missing_closing_bracket() {
        let src = r#"// @data_deps = ["usage"
fn render(ctx) {}"#;
        assert!(matches!(
            parse_data_deps_header(src),
            Err(HeaderError::Malformed(_))
        ));
    }

    #[test]
    fn malformed_unquoted_token() {
        let src = r#"// @data_deps = [usage]
fn render(ctx) {}"#;
        assert!(matches!(
            parse_data_deps_header(src),
            Err(HeaderError::Malformed(_))
        ));
    }

    #[test]
    fn block_comment_syntax_is_not_scanned() {
        // Per spec: `/* @data_deps = [...] */` is NOT parsed.
        let src = r#"/* @data_deps = ["usage"] */
fn render(ctx) {}"#;
        assert_eq!(parse_data_deps_header(src), Ok(vec![DataDep::Status]));
    }

    #[test]
    fn inline_comment_on_array_line_accepted() {
        // Spec §@data_deps header syntax: "Trailing commas, comments
        // inside the array, and multi-line forms are all accepted."
        let src = r#"// @data_deps = [
//   "usage",       // why we need it
//   "git",         // trailing comment too
// ]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Ok(vec![DataDep::Status, DataDep::Usage, DataDep::Git])
        );
    }

    #[test]
    fn inline_comment_after_last_entry_accepted() {
        // Spec only requires line-comment support inside the array;
        // block comments are not scanned. Exercise the single-line
        // `//` case after a quoted entry.
        let src = r#"// @data_deps = [
//   "usage", // ok
//   "git"
// ]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Ok(vec![DataDep::Status, DataDep::Usage, DataDep::Git])
        );
    }

    #[test]
    fn whitespace_before_double_slash_is_tolerated() {
        let src = r#"    // @data_deps = ["usage"]
fn render(ctx) {}"#;
        assert_eq!(
            parse_data_deps_header(src),
            Ok(vec![DataDep::Status, DataDep::Usage])
        );
    }
}
