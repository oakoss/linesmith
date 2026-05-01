//! Validates the value a plugin's `render(ctx)` function returns and
//! converts it into a [`RenderedSegment`] the layout engine can consume.
//!
//! Return shape per `docs/specs/plugin-api.md` §Plugin return shape:
//!
//! - `()` (rhai unit) → hide this segment for this invocation
//! - A map with `runs: [#{ text, role?, fg?, bold?, italic?, underline?, dim? }, ...]`
//!   plus optional `width` and `right_separator`
//!
//! [`RenderedSegment`] is single-style, so multi-run input surfaces
//! as [`PluginError::MalformedReturn`] with a message pointing at the
//! limitation.
//!
//! `hyperlink` threads through to `Style.hyperlink` so capable
//! terminals render the run as an OSC 8 link.
//!
//! `bg` and `width` are silently accepted but not acted on: `bg`
//! requires a host `Style` field that doesn't exist yet; `width` is
//! recomputed from rendered text. Silence is deliberate — a plugin
//! author who writes these fields for forward compatibility doesn't
//! trigger a load error.

use rhai::{Array, Dynamic, Map};

use linesmith_plugin::PluginError;

use crate::segments::{sanitize_control_chars, RenderedSegment, Separator};
use crate::theme::{Color, Role, Style};

/// Convert a plugin's render-return value into a [`RenderedSegment`].
/// `()` yields `Ok(None)` (segment hidden); any other shape mismatch
/// yields [`PluginError::MalformedReturn`].
pub fn validate_return(value: Dynamic, id: &str) -> Result<Option<RenderedSegment>, PluginError> {
    if value.is_unit() {
        return Ok(None);
    }
    let map = value
        .try_cast::<Map>()
        .ok_or_else(|| malformed(id, "render() must return `()` or a map"))?;

    let runs = parse_runs(&map, id)?;
    let (text, style) = parse_single_run(&runs, id)?;
    let separator = match map.get("right_separator") {
        Some(d) => Some(parse_right_separator(d.clone(), id)?),
        None => None,
    };

    let rendered = match separator {
        Some(sep) => RenderedSegment::with_separator(text, sep),
        None => RenderedSegment::new(text),
    };
    Ok(Some(rendered.with_style(style)))
}

fn parse_runs(map: &Map, id: &str) -> Result<Array, PluginError> {
    let runs_val = map
        .get("runs")
        .ok_or_else(|| malformed(id, "render() return map is missing `runs`"))?;
    let arr = runs_val
        .clone()
        .try_cast::<Array>()
        .ok_or_else(|| malformed(id, "`runs` must be an array"))?;
    if arr.is_empty() {
        return Err(malformed(id, "`runs` array must not be empty"));
    }
    Ok(arr)
}

fn parse_single_run(runs: &Array, id: &str) -> Result<(String, Style), PluginError> {
    if runs.len() > 1 {
        return Err(malformed(
            id,
            "linesmith currently supports exactly one styled run per render; multi-run output is deferred",
        ));
    }
    let run = runs[0]
        .clone()
        .try_cast::<Map>()
        .ok_or_else(|| malformed(id, "each entry in `runs` must be a map"))?;

    let text = run
        .get("text")
        .ok_or_else(|| malformed(id, "run map is missing `text`"))?
        .clone()
        .try_cast::<String>()
        .ok_or_else(|| malformed(id, "`text` must be a string"))?;

    let style = parse_style(&run, id)?;
    Ok((text, style))
}

fn parse_style(run: &Map, id: &str) -> Result<Style, PluginError> {
    let mut style = Style::default();

    if let Some(role_dyn) = run.get("role") {
        let role_name = role_dyn
            .clone()
            .try_cast::<String>()
            .ok_or_else(|| malformed(id, "`role` must be a string"))?;
        style.role = Some(parse_role(&role_name, id)?);
    }

    if let Some(fg_dyn) = run.get("fg") {
        let fg_hex = fg_dyn
            .clone()
            .try_cast::<String>()
            .ok_or_else(|| malformed(id, "`fg` must be a hex color string"))?;
        style.fg = Some(parse_hex_color(&fg_hex, id)?);
    }

    for (key, slot) in [
        ("bold", &mut style.bold),
        ("italic", &mut style.italic),
        ("underline", &mut style.underline),
        ("dim", &mut style.dim),
    ] {
        if let Some(dyn_val) = run.get(key) {
            *slot = dyn_val
                .clone()
                .try_cast::<bool>()
                .ok_or_else(|| malformed(id, &format!("`{key}` must be a bool")))?;
        }
    }

    if let Some(link_dyn) = run.get("hyperlink") {
        let url = link_dyn
            .clone()
            .try_cast::<String>()
            .ok_or_else(|| malformed(id, "`hyperlink` must be a string"))?;
        if !url.is_empty() {
            style.hyperlink = Some(url);
        }
    }

    Ok(style)
}

fn parse_role(name: &str, id: &str) -> Result<Role, PluginError> {
    match name {
        "foreground" => Ok(Role::Foreground),
        "background" => Ok(Role::Background),
        "muted" => Ok(Role::Muted),
        "primary" => Ok(Role::Primary),
        "accent" => Ok(Role::Accent),
        "success" => Ok(Role::Success),
        "warning" => Ok(Role::Warning),
        "error" => Ok(Role::Error),
        "info" => Ok(Role::Info),
        "success_dim" => Ok(Role::SuccessDim),
        "warning_dim" => Ok(Role::WarningDim),
        "error_dim" => Ok(Role::ErrorDim),
        "primary_dim" => Ok(Role::PrimaryDim),
        "accent_dim" => Ok(Role::AccentDim),
        "surface" => Ok(Role::Surface),
        "border" => Ok(Role::Border),
        other => Err(malformed(
            id,
            &format!("unknown role `{other}`; see plugin-api.md §Plugin return shape"),
        )),
    }
}

fn parse_hex_color(hex: &str, id: &str) -> Result<Color, PluginError> {
    let body = hex
        .strip_prefix('#')
        .ok_or_else(|| malformed(id, &format!("hex color must start with `#`, got `{hex}`")))?;
    if body.len() != 6 {
        return Err(malformed(
            id,
            &format!("hex color must be `#rrggbb` (6 hex digits), got `{hex}`"),
        ));
    }
    let r = u8::from_str_radix(&body[0..2], 16)
        .map_err(|_| malformed(id, &format!("invalid hex color `{hex}`")))?;
    let g = u8::from_str_radix(&body[2..4], 16)
        .map_err(|_| malformed(id, &format!("invalid hex color `{hex}`")))?;
    let b = u8::from_str_radix(&body[4..6], 16)
        .map_err(|_| malformed(id, &format!("invalid hex color `{hex}`")))?;
    Ok(Color::TrueColor { r, g, b })
}

fn parse_right_separator(sep: Dynamic, id: &str) -> Result<Separator, PluginError> {
    let s = sep
        .try_cast::<String>()
        .ok_or_else(|| malformed(id, "`right_separator` must be a string"))?;
    Ok(match s.as_str() {
        "space" => Separator::Space,
        "theme" => Separator::Theme,
        "none" => Separator::None,
        // Plugin-supplied separators flow straight to the terminal via
        // `Separator::text()`. Without stripping control bytes here, a
        // hostile plugin could smuggle ANSI/OSC sequences through
        // `right_separator` that `RenderedSegment::new` would have
        // caught on `runs[*].text`.
        _ => Separator::Literal(sanitize_control_chars(s).into()),
    })
}

fn malformed(id: &str, message: &str) -> PluginError {
    PluginError::MalformedReturn {
        id: id.to_string(),
        message: message.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::build_engine;
    use rhai::Engine;

    fn eval_and_validate(script: &str, id: &str) -> Result<Option<RenderedSegment>, PluginError> {
        let engine: std::sync::Arc<Engine> = build_engine();
        let value: Dynamic = engine.eval(script).expect("rhai eval ok");
        validate_return(value, id)
    }

    #[test]
    fn unit_return_hides_segment() {
        assert_eq!(eval_and_validate("()", "t"), Ok(None));
    }

    #[test]
    fn single_run_text_only() {
        let rendered = eval_and_validate(r#"#{ runs: [#{ text: "hello" }] }"#, "t")
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "hello");
        assert_eq!(rendered.style(), &Style::default());
    }

    #[test]
    fn single_run_with_role() {
        let rendered = eval_and_validate(r#"#{ runs: [#{ text: "ok", role: "success" }] }"#, "t")
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.style().role, Some(Role::Success));
    }

    #[test]
    fn single_run_with_decorations() {
        let rendered = eval_and_validate(
            r#"#{ runs: [#{ text: "x", bold: true, italic: true, underline: true, dim: true }] }"#,
            "t",
        )
        .unwrap()
        .expect("rendered");
        let s = rendered.style();
        assert!(s.bold);
        assert!(s.italic);
        assert!(s.underline);
        assert!(s.dim);
    }

    #[test]
    fn single_run_with_hex_fg() {
        let rendered = eval_and_validate(r##"#{ runs: [#{ text: "x", fg: "#ff8040" }] }"##, "t")
            .unwrap()
            .expect("rendered");
        assert_eq!(
            rendered.style().fg,
            Some(Color::TrueColor {
                r: 0xff,
                g: 0x80,
                b: 0x40
            })
        );
    }

    #[test]
    fn right_separator_space() {
        let rendered = eval_and_validate(
            r#"#{ runs: [#{ text: "x" }], right_separator: "space" }"#,
            "t",
        )
        .unwrap()
        .expect("rendered");
        assert_eq!(rendered.right_separator(), Some(&Separator::Space));
    }

    #[test]
    fn right_separator_literal_preserves_user_string() {
        let rendered = eval_and_validate(
            r#"#{ runs: [#{ text: "x" }], right_separator: " | " }"#,
            "t",
        )
        .unwrap()
        .expect("rendered");
        match rendered.right_separator() {
            Some(Separator::Literal(s)) => assert_eq!(s, " | "),
            other => panic!("expected literal separator, got {other:?}"),
        }
    }

    #[test]
    fn right_separator_empty_string_is_literal() {
        let rendered =
            eval_and_validate(r#"#{ runs: [#{ text: "x" }], right_separator: "" }"#, "t")
                .unwrap()
                .expect("rendered");
        match rendered.right_separator() {
            Some(Separator::Literal(s)) => assert_eq!(s, ""),
            other => panic!("expected literal separator, got {other:?}"),
        }
    }

    #[test]
    fn non_map_non_unit_return_rejected() {
        let err = eval_and_validate(r#""just a string""#, "t").unwrap_err();
        assert!(matches!(err, PluginError::MalformedReturn { .. }));
    }

    #[test]
    fn missing_runs_key_rejected() {
        let err = eval_and_validate(r#"#{ width: 5 }"#, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("runs"), "message: {message}");
    }

    #[test]
    fn empty_runs_rejected() {
        let err = eval_and_validate(r#"#{ runs: [] }"#, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("empty"));
    }

    #[test]
    fn multi_run_rejected_with_deferred_note() {
        let err =
            eval_and_validate(r#"#{ runs: [#{ text: "a" }, #{ text: "b" }] }"#, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(
            message.contains("one") || message.contains("multi-run"),
            "message should flag the single-run restriction: {message}"
        );
    }

    #[test]
    fn run_without_text_rejected() {
        let err = eval_and_validate(r#"#{ runs: [#{ role: "primary" }] }"#, "t").unwrap_err();
        assert!(matches!(err, PluginError::MalformedReturn { .. }));
    }

    #[test]
    fn run_text_wrong_type_rejected() {
        let err = eval_and_validate(r#"#{ runs: [#{ text: 42 }] }"#, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("text"));
    }

    #[test]
    fn unknown_role_rejected() {
        let err =
            eval_and_validate(r#"#{ runs: [#{ text: "x", role: "mystery" }] }"#, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("mystery"));
    }

    #[test]
    fn hex_color_missing_hash_rejected() {
        let err =
            eval_and_validate(r#"#{ runs: [#{ text: "x", fg: "ff0000" }] }"#, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("start with"), "message: {message}");
    }

    #[test]
    fn hex_color_wrong_length_rejected() {
        let err =
            eval_and_validate(r##"#{ runs: [#{ text: "x", fg: "#abc" }] }"##, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("6 hex digits"), "message: {message}");
    }

    #[test]
    fn hex_color_alpha_form_rejected() {
        // `#rrggbbaa` is a plausible plugin-author mistake (web colors
        // often carry an alpha byte). Pin it to the 6-digit branch.
        let err = eval_and_validate(r##"#{ runs: [#{ text: "x", fg: "#ff804080" }] }"##, "t")
            .unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("6 hex digits"), "message: {message}");
    }

    #[test]
    fn hex_color_empty_body_rejected() {
        let err = eval_and_validate(r##"#{ runs: [#{ text: "x", fg: "#" }] }"##, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("6 hex digits"), "message: {message}");
    }

    #[test]
    fn hex_color_uppercase_accepted() {
        let rendered = eval_and_validate(r##"#{ runs: [#{ text: "x", fg: "#FF8040" }] }"##, "t")
            .unwrap()
            .expect("rendered");
        assert_eq!(
            rendered.style().fg,
            Some(Color::TrueColor {
                r: 0xff,
                g: 0x80,
                b: 0x40
            })
        );
    }

    #[test]
    fn hex_color_non_hex_digits_rejected() {
        let err =
            eval_and_validate(r##"#{ runs: [#{ text: "x", fg: "#zzzzzz" }] }"##, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("invalid hex"), "message: {message}");
    }

    #[test]
    fn decoration_wrong_type_rejected() {
        let err =
            eval_and_validate(r#"#{ runs: [#{ text: "x", bold: "yes" }] }"#, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("bold"));
    }

    #[test]
    fn all_16_roles_parse_via_snake_case_token() {
        // Regression guard: if a Role variant is added and
        // `parse_role` isn't updated, this table test fails.
        let cases: &[(&str, Role)] = &[
            ("foreground", Role::Foreground),
            ("background", Role::Background),
            ("muted", Role::Muted),
            ("primary", Role::Primary),
            ("accent", Role::Accent),
            ("success", Role::Success),
            ("warning", Role::Warning),
            ("error", Role::Error),
            ("info", Role::Info),
            ("success_dim", Role::SuccessDim),
            ("warning_dim", Role::WarningDim),
            ("error_dim", Role::ErrorDim),
            ("primary_dim", Role::PrimaryDim),
            ("accent_dim", Role::AccentDim),
            ("surface", Role::Surface),
            ("border", Role::Border),
        ];
        for (token, expected) in cases {
            let script = format!(r#"#{{ runs: [#{{ text: "x", role: "{token}" }}] }}"#);
            let rendered = eval_and_validate(&script, "t")
                .unwrap_or_else(|e| panic!("role `{token}` failed: {e}"))
                .expect("rendered");
            assert_eq!(
                rendered.style().role,
                Some(*expected),
                "role token `{token}` should parse to {expected:?}"
            );
        }
    }

    #[test]
    fn all_separator_strings_map_correctly() {
        let cases: &[(&str, Separator)] = &[
            ("space", Separator::Space),
            ("theme", Separator::Theme),
            ("none", Separator::None),
        ];
        for (token, expected) in cases {
            let script = format!(r#"#{{ runs: [#{{ text: "x" }}], right_separator: "{token}" }}"#);
            let rendered = eval_and_validate(&script, "t")
                .unwrap_or_else(|e| panic!("separator `{token}` failed: {e}"))
                .expect("rendered");
            assert_eq!(rendered.right_separator(), Some(expected));
        }
    }

    #[test]
    fn control_chars_in_plugin_text_are_stripped() {
        let rendered = eval_and_validate(r#"#{ runs: [#{ text: "evil\u001B[2Jafter" }] }"#, "t")
            .unwrap()
            .expect("rendered");
        assert!(!rendered.text().contains('\x1b'), "ESC must be stripped");
        assert!(rendered.text().contains("evil"));
        assert!(rendered.text().contains("after"));
    }

    #[test]
    fn control_chars_in_plugin_separator_are_stripped() {
        // Only control chars get dropped; printable residue (`[2J`)
        // stays, but without the leading ESC it's inert.
        let rendered = eval_and_validate(
            r#"#{ runs: [#{ text: "x" }], right_separator: "\u001B[2J|" }"#,
            "t",
        )
        .unwrap()
        .expect("rendered");
        match rendered.right_separator() {
            Some(Separator::Literal(s)) => {
                assert!(!s.contains('\x1b'), "ESC must be stripped, got {s:?}");
                assert!(s.contains('|'), "surviving printable bytes kept: {s:?}");
            }
            other => panic!("expected literal separator, got {other:?}"),
        }
    }

    #[test]
    fn runs_field_non_array_rejected() {
        let err = eval_and_validate(r#"#{ runs: "not an array" }"#, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("runs"), "message: {message}");
        assert!(message.to_lowercase().contains("array"));
    }

    #[test]
    fn runs_element_non_map_rejected() {
        let err = eval_and_validate(r#"#{ runs: [42] }"#, "t").unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.to_lowercase().contains("map"));
    }

    #[test]
    fn right_separator_non_string_rejected() {
        let err = eval_and_validate(r#"#{ runs: [#{ text: "x" }], right_separator: 42 }"#, "t")
            .unwrap_err();
        let PluginError::MalformedReturn { message, .. } = err else {
            panic!("expected MalformedReturn");
        };
        assert!(message.contains("right_separator") || message.contains("string"));
    }

    #[test]
    fn unsupported_bg_field_silently_ignored() {
        let rendered = eval_and_validate(r##"#{ runs: [#{ text: "x", bg: "#000000" }] }"##, "t")
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.text(), "x");
    }

    #[test]
    fn hyperlink_field_threads_to_style() {
        let rendered = eval_and_validate(
            r#"#{ runs: [#{ text: "x", hyperlink: "https://example.com" }] }"#,
            "t",
        )
        .unwrap()
        .expect("rendered");
        assert_eq!(rendered.text(), "x");
        assert_eq!(
            rendered.style().hyperlink.as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn empty_hyperlink_string_does_not_set_link() {
        // `hyperlink: ""` folds to None so the emitter doesn't wrap
        // text in a link-to-nothing OSC 8 pair. Empty URL `\x1b]8;;\x1b\\`
        // is the canonical OSC 8 close sequence per ECMA-48 — using it
        // as a link target would semantically tell the terminal "no
        // link," which is just absence with extra bytes.
        let rendered = eval_and_validate(r#"#{ runs: [#{ text: "x", hyperlink: "" }] }"#, "t")
            .unwrap()
            .expect("rendered");
        assert_eq!(rendered.style().hyperlink, None);
    }

    #[test]
    fn non_string_hyperlink_rejected() {
        let err =
            eval_and_validate(r#"#{ runs: [#{ text: "x", hyperlink: 42 }] }"#, "t").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("hyperlink"), "got: {msg}");
    }
}
