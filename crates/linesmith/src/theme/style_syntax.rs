//! Style-string parser per `docs/specs/theming.md` §Style syntax.
//! Accepts whitespace-separated tokens in any order:
//!   - `role:<name>`  — sets `Style.role`
//!   - `fg:<color>`   — sets `Style.fg` (hex / named / rgb() per [`parse_color`])
//!   - `bold` / `italic` / `underline` / `dim` — decoration flags
//!
//! Empty or whitespace-only input yields `Style::default()`. All tokens
//! are case-insensitive.

use super::user::parse_color;
use super::{Role, Style};

/// Parse failures; each variant carries the offending input for diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StyleParseError {
    UnknownRole(String),
    InvalidFg(String),
    UnknownToken(String),
    MalformedRole(String),
    MalformedFg(String),
    UnclosedParen(String),
    StrayCloseParen(String),
}

impl std::fmt::Display for StyleParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownRole(s) => write!(f, "unknown role '{s}'"),
            Self::InvalidFg(s) => write!(f, "invalid fg color '{s}'"),
            Self::UnknownToken(s) => write!(f, "unknown style token '{s}'"),
            Self::MalformedRole(s) => write!(f, "malformed role directive '{s}'"),
            Self::MalformedFg(s) => write!(f, "malformed fg directive '{s}'"),
            Self::UnclosedParen(s) => write!(f, "unclosed paren in '{s}'"),
            Self::StrayCloseParen(s) => write!(f, "stray ')' in '{s}'"),
        }
    }
}

impl std::error::Error for StyleParseError {}

/// Parse a style string. Empty / whitespace-only yields the default
/// style. Tokens may appear in any order; duplicate tokens overwrite
/// (last-wins within a single string).
pub fn parse_style(s: &str) -> Result<Style, StyleParseError> {
    let mut style = Style::default();
    for token in tokenize(s)? {
        let (head, rest) = token.split_once(':').unzip();
        let head = head.map(str::to_ascii_lowercase);
        match head.as_deref() {
            Some("role") => {
                let name = rest.unwrap_or("");
                if name.is_empty() {
                    return Err(StyleParseError::MalformedRole(token.to_string()));
                }
                style.role = Some(parse_role(name)?);
            }
            Some("fg") => {
                let color = rest.unwrap_or("");
                if color.is_empty() {
                    return Err(StyleParseError::MalformedFg(token.to_string()));
                }
                style.fg = Some(
                    parse_color(color)
                        .map_err(|_| StyleParseError::InvalidFg(color.to_string()))?,
                );
            }
            _ => match token.to_ascii_lowercase().as_str() {
                "bold" => style.bold = true,
                "italic" => style.italic = true,
                "underline" => style.underline = true,
                "dim" => style.dim = true,
                _ => return Err(StyleParseError::UnknownToken(token.to_string())),
            },
        }
    }
    Ok(style)
}

/// Split on whitespace, but treat parenthesized groups as atomic so
/// `fg:rgb(203, 166, 247)` survives its internal spaces. Unclosed
/// parens surface as `UnclosedParen`; stray `)` outside any group
/// surfaces as `StrayCloseParen` rather than being silently dropped
/// (otherwise `fg:rgb(1,2,3))` would reach `parse_color` as
/// `rgb(1,2,3))` and surface a confusing `InvalidFg` instead).
fn tokenize(s: &str) -> Result<Vec<&str>, StyleParseError> {
    let mut tokens = Vec::new();
    let mut start: Option<usize> = None;
    let mut depth: u32 = 0;
    for (i, c) in s.char_indices() {
        if c.is_whitespace() && depth == 0 {
            if let Some(s0) = start.take() {
                tokens.push(&s[s0..i]);
            }
            continue;
        }
        if start.is_none() {
            start = Some(i);
        }
        match c {
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    let s0 = start.unwrap_or(i);
                    return Err(StyleParseError::StrayCloseParen(s[s0..].to_string()));
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    if depth > 0 {
        let offending = start.map(|s0| &s[s0..]).unwrap_or("");
        return Err(StyleParseError::UnclosedParen(offending.to_string()));
    }
    if let Some(s0) = start {
        tokens.push(&s[s0..]);
    }
    Ok(tokens)
}

fn parse_role(s: &str) -> Result<Role, StyleParseError> {
    let role = match s.to_ascii_lowercase().as_str() {
        "foreground" => Role::Foreground,
        "background" => Role::Background,
        "muted" => Role::Muted,
        "primary" => Role::Primary,
        "accent" => Role::Accent,
        "success" => Role::Success,
        "warning" => Role::Warning,
        "error" => Role::Error,
        "info" => Role::Info,
        "success_dim" | "success-dim" => Role::SuccessDim,
        "warning_dim" | "warning-dim" => Role::WarningDim,
        "error_dim" | "error-dim" => Role::ErrorDim,
        "primary_dim" | "primary-dim" => Role::PrimaryDim,
        "accent_dim" | "accent-dim" => Role::AccentDim,
        "surface" => Role::Surface,
        "border" => Role::Border,
        _ => return Err(StyleParseError::UnknownRole(s.to_string())),
    };
    Ok(role)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{AnsiColor, Color};

    #[test]
    fn empty_string_yields_default_style() {
        assert_eq!(parse_style(""), Ok(Style::default()));
    }

    #[test]
    fn whitespace_only_yields_default_style() {
        assert_eq!(parse_style("   \t \n "), Ok(Style::default()));
    }

    #[test]
    fn role_directive_sets_role() {
        assert_eq!(parse_style("role:primary"), Ok(Style::role(Role::Primary)));
    }

    #[test]
    fn role_plus_decorations_combine() {
        let got = parse_style("role:success bold italic").expect("ok");
        assert_eq!(got.role, Some(Role::Success));
        assert!(got.bold);
        assert!(got.italic);
        assert!(!got.underline);
        assert!(!got.dim);
    }

    #[test]
    fn fg_hex_and_decoration() {
        let got = parse_style("fg:#ff0000 underline").expect("ok");
        assert_eq!(got.fg, Some(Color::TrueColor { r: 255, g: 0, b: 0 }));
        assert_eq!(got.role, None);
        assert!(got.underline);
    }

    #[test]
    fn fg_named_color() {
        let got = parse_style("fg:red").expect("ok");
        assert_eq!(got.fg, Some(Color::Palette16(AnsiColor::Red)));
    }

    #[test]
    fn fg_rgb_function() {
        let got = parse_style("fg:rgb(203,166,247)").expect("ok");
        assert_eq!(
            got.fg,
            Some(Color::TrueColor {
                r: 203,
                g: 166,
                b: 247,
            })
        );
    }

    #[test]
    fn fg_rgb_with_spaces_inside_parens_is_one_token() {
        // Spec `docs/specs/theming.md` accepts `rgb(r, g, b)` with
        // spaces; tokenizer must treat the parenthesized group as atomic.
        let got = parse_style("fg:rgb(203, 166, 247) bold").expect("ok");
        assert_eq!(
            got.fg,
            Some(Color::TrueColor {
                r: 203,
                g: 166,
                b: 247,
            })
        );
        assert!(got.bold);
    }

    #[test]
    fn unclosed_paren_errors() {
        match parse_style("fg:rgb(203,166 bold") {
            Err(StyleParseError::UnclosedParen(s)) => {
                assert!(s.starts_with("fg:rgb("), "got {s:?}");
            }
            other => panic!("expected UnclosedParen, got {other:?}"),
        }
    }

    #[test]
    fn stray_close_paren_errors_before_reaching_parse_color() {
        // Without the eager check, `fg:rgb(1,2,3))` would reach
        // `parse_color` as `rgb(1,2,3))` and surface as `InvalidFg`,
        // masking the real structural defect.
        match parse_style("fg:rgb(1,2,3))") {
            Err(StyleParseError::StrayCloseParen(s)) => {
                assert!(s.starts_with("fg:rgb("), "got {s:?}");
            }
            other => panic!("expected StrayCloseParen, got {other:?}"),
        }
    }

    #[test]
    fn bare_close_paren_errors() {
        match parse_style("bold )") {
            Err(StyleParseError::StrayCloseParen(_)) => {}
            other => panic!("expected StrayCloseParen, got {other:?}"),
        }
    }

    #[test]
    fn nested_parens_are_one_token() {
        // Tokenizer's depth counter must survive `depth > 1` without
        // collapsing to boolean "inside/outside" semantics.
        match parse_style("fg:rgb((1,2,3))") {
            Err(StyleParseError::InvalidFg(_)) => {}
            other => {
                panic!("expected InvalidFg (parse_color rejects double parens), got {other:?}")
            }
        }
    }

    #[test]
    fn case_insensitive_tokens() {
        let got = parse_style("ROLE:PRIMARY BOLD ITALIC").expect("ok");
        assert_eq!(got.role, Some(Role::Primary));
        assert!(got.bold);
        assert!(got.italic);
    }

    #[test]
    fn mixed_case_directive_prefix_parses() {
        // Module doc promises case-insensitive tokens. `Role:` / `Fg:`
        // mixed-case prefixes must parse the same as the lowercase form.
        assert_eq!(
            parse_style("Role:Accent Fg:#ff0000").expect("ok"),
            Style {
                role: Some(Role::Accent),
                fg: Some(Color::TrueColor { r: 255, g: 0, b: 0 }),
                ..Style::default()
            }
        );
    }

    #[test]
    fn order_does_not_matter() {
        let a = parse_style("role:info bold italic").expect("ok");
        let b = parse_style("italic bold role:info").expect("ok");
        let c = parse_style("bold role:info italic").expect("ok");
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn all_four_decorations_compose() {
        let got = parse_style("bold italic underline dim").expect("ok");
        assert!(got.bold);
        assert!(got.italic);
        assert!(got.underline);
        assert!(got.dim);
    }

    #[test]
    fn extended_role_with_underscore_and_hyphen_both_work() {
        assert_eq!(
            parse_style("role:success_dim").unwrap().role,
            Some(Role::SuccessDim)
        );
        assert_eq!(
            parse_style("role:success-dim").unwrap().role,
            Some(Role::SuccessDim)
        );
    }

    #[test]
    fn unknown_role_errors_with_input() {
        match parse_style("role:mauve") {
            Err(StyleParseError::UnknownRole(s)) => assert_eq!(s, "mauve"),
            other => panic!("expected UnknownRole, got {other:?}"),
        }
    }

    #[test]
    fn invalid_fg_errors_with_input() {
        match parse_style("fg:notacolor") {
            Err(StyleParseError::InvalidFg(s)) => assert_eq!(s, "notacolor"),
            other => panic!("expected InvalidFg, got {other:?}"),
        }
    }

    #[test]
    fn unknown_token_errors() {
        match parse_style("role:primary wobbly") {
            Err(StyleParseError::UnknownToken(s)) => assert_eq!(s, "wobbly"),
            other => panic!("expected UnknownToken, got {other:?}"),
        }
    }

    #[test]
    fn malformed_role_directive_errors() {
        match parse_style("role: bold") {
            Err(StyleParseError::MalformedRole(s)) => assert_eq!(s, "role:"),
            other => panic!("expected MalformedRole, got {other:?}"),
        }
    }

    #[test]
    fn malformed_fg_directive_errors() {
        match parse_style("fg:") {
            Err(StyleParseError::MalformedFg(s)) => assert_eq!(s, "fg:"),
            other => panic!("expected MalformedFg, got {other:?}"),
        }
    }

    #[test]
    fn parser_populates_both_fg_and_role_when_both_specified() {
        // Render-time precedence (fg outranks role) is tested in
        // `theme::sgr_open`; here we pin the parser shape only.
        let got = parse_style("role:primary fg:#ff8800 bold").expect("ok");
        assert_eq!(got.role, Some(Role::Primary));
        assert_eq!(
            got.fg,
            Some(Color::TrueColor {
                r: 255,
                g: 136,
                b: 0
            })
        );
        assert!(got.bold);
    }

    #[test]
    fn duplicate_role_token_last_wins() {
        assert_eq!(
            parse_style("role:primary role:accent").unwrap().role,
            Some(Role::Accent)
        );
    }

    #[test]
    fn duplicate_fg_token_last_wins() {
        let got = parse_style("fg:#ff0000 fg:#00ff00").expect("ok");
        assert_eq!(got.fg, Some(Color::TrueColor { r: 0, g: 255, b: 0 }));
    }

    #[test]
    fn duplicate_decoration_token_is_idempotent() {
        let got = parse_style("bold bold").expect("ok");
        assert!(got.bold);
    }

    #[test]
    fn error_display_quotes_offending_input() {
        let err = StyleParseError::UnknownRole("mauve".into());
        assert_eq!(err.to_string(), "unknown role 'mauve'");
        let err = StyleParseError::InvalidFg("xyz".into());
        assert_eq!(err.to_string(), "invalid fg color 'xyz'");
    }
}
