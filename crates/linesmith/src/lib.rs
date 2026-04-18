//! linesmith: Rust status line for Claude Code and other AI coding CLIs.
//!
//! `run` reads a JSON payload from a `Read`, renders a status line, and
//! writes the result to a `Write`. The binary in `main.rs` wires this to
//! stdin/stdout. See `docs/specs/` for the segment, theme, config, and
//! plugin contracts.

pub mod cli;
pub mod config;
pub mod input;
pub mod layout;
pub mod segments;

use crate::segments::Segment;
use std::io::{self, Read, Write};

/// Read a JSON payload from `reader`, render a status line, and write it
/// to `writer`. Parse failures render a `?` marker to `writer` and log
/// detail to stderr; only I/O failures surface as errors.
///
/// # Errors
///
/// Returns an `io::Error` if reading from `reader` or writing to `writer`
/// fails. Parse errors are handled internally.
pub fn run(reader: impl Read, writer: impl Write) -> io::Result<()> {
    run_with_width(reader, writer, detect_terminal_width())
}

/// Same as [`run`] but with an explicit terminal width. Exposed so
/// callers with their own width source (tests, a TUI wrapper) can
/// bypass `detect_terminal_width`.
///
/// # Errors
///
/// See [`run_with_segments_and_width`].
pub fn run_with_width(
    reader: impl Read,
    writer: impl Write,
    terminal_width: u16,
) -> io::Result<()> {
    let segments = build_default_segments();
    run_with_segments_and_width(reader, writer, &segments, terminal_width)
}

/// Full-control entry: pre-built segment list plus explicit width.
/// Parse failures render a `?` marker and log to stderr; only I/O
/// failures surface as errors.
///
/// # Errors
///
/// Returns an `io::Error` if reading from `reader` or writing to
/// `writer` fails.
pub fn run_with_segments_and_width(
    mut reader: impl Read,
    mut writer: impl Write,
    segments: &[Box<dyn Segment>],
    terminal_width: u16,
) -> io::Result<()> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;

    let ctx = match input::parse(&buf) {
        Ok(ctx) => ctx,
        Err(err) => {
            // Swallow stderr errors: a broken stderr pipe must not
            // abort the stdout render.
            let _ = writeln!(io::stderr().lock(), "linesmith: parse: {err}");
            return writeln!(writer, "?");
        }
    };

    let line = layout::render(segments, &ctx, terminal_width);
    writeln!(writer, "{line}")
}

/// Build the default segment list: every built-in in canonical order,
/// no overrides applied.
#[must_use]
pub fn build_default_segments() -> Vec<Box<dyn Segment>> {
    segments::DEFAULT_SEGMENT_IDS
        .iter()
        .filter_map(|id| segments::built_in_by_id(id))
        .collect()
}

/// Build a segment list from an optional [`Config`](config::Config).
/// `None` or a config without a `[line]` section uses the default
/// order. `warn` receives a one-line diagnostic for each validation
/// rule triggered (pass `|_| {}` to discard).
///
/// Implements the validation rules in `docs/specs/config.md`
/// §Validation rules: unknown ids skip with a warning, duplicates
/// keep the first, an explicit `segments = []` warns, inverted width
/// bounds drop the override with a warning.
pub fn build_segments(
    config: Option<&config::Config>,
    mut warn: impl FnMut(&str),
) -> Vec<Box<dyn Segment>> {
    let configured_line = config.and_then(|c| c.line.as_ref());
    if let Some(line) = configured_line {
        if line.segments.is_empty() {
            warn("[line].segments is empty; no segments will render");
        }
    }

    let ids: Vec<&str> = match configured_line {
        Some(l) => l.segments.iter().map(String::as_str).collect(),
        None => segments::DEFAULT_SEGMENT_IDS.to_vec(),
    };

    let mut seen = std::collections::HashSet::<&str>::new();
    ids.into_iter()
        .filter_map(|id| {
            if !seen.insert(id) {
                warn(&format!(
                    "segment '{id}' listed more than once; keeping first occurrence"
                ));
                return None;
            }
            let inner = segments::built_in_by_id(id).or_else(|| {
                warn(&format!("unknown segment id '{id}' — skipping"));
                None
            })?;
            let cfg_override = config.and_then(|c| c.segments.get(id));
            Some(apply_override(id, inner, cfg_override, &mut warn))
        })
        .collect()
}

fn apply_override(
    id: &str,
    inner: Box<dyn Segment>,
    ov: Option<&config::SegmentOverride>,
    warn: &mut impl FnMut(&str),
) -> Box<dyn Segment> {
    let Some(ov) = ov else { return inner };
    let base_width = inner.defaults().width;
    let mut wrapped = segments::OverriddenSegment::new(inner);
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
        match segments::WidthBounds::new(min, max) {
            Some(bounds) => wrapped = wrapped.with_width(bounds),
            None => warn(&format!(
                "segments.{id}.width: min ({min}) > max ({max}); ignoring"
            )),
        }
    }
    Box::new(wrapped)
}

/// Width fallback when `terminal_size()` and `COLUMNS` both fail.
/// Matches `docs/specs/segment-system.md` edge-case table.
const DEFAULT_TERMINAL_WIDTH: u16 = 200;

/// Resolve the terminal width in cells. Prefers the OS-reported size, then
/// the `COLUMNS` env var, then `DEFAULT_TERMINAL_WIDTH`. A set-but-invalid
/// `COLUMNS` value logs to stderr so the user can correct their config;
/// an unset `COLUMNS` falls through silently (the common case when stdout
/// is piped to Claude Code).
#[must_use]
pub fn detect_terminal_width() -> u16 {
    let os_width = terminal_size::terminal_size().map(|(terminal_size::Width(w), _)| w);
    let columns = std::env::var("COLUMNS").ok();
    resolve_terminal_width(os_width, columns.as_deref(), |msg| {
        let _ = writeln!(io::stderr().lock(), "linesmith: {msg}");
    })
}

/// Shared core of `detect_terminal_width`. Pure: takes the two inputs
/// (OS size, `COLUMNS` value) and a stderr sink, returns the chosen
/// width. Split out so tests don't have to mutate process env.
fn resolve_terminal_width(
    os_width: Option<u16>,
    columns: Option<&str>,
    mut warn: impl FnMut(&str),
) -> u16 {
    if let Some(w) = os_width {
        return w;
    }
    let Some(raw) = columns else {
        return DEFAULT_TERMINAL_WIDTH;
    };
    match raw.parse::<u16>() {
        Ok(parsed) if parsed > 0 => parsed,
        Ok(_) => {
            warn(&format!(
                "COLUMNS='{raw}' is zero; using {DEFAULT_TERMINAL_WIDTH} cells"
            ));
            DEFAULT_TERMINAL_WIDTH
        }
        Err(err) => {
            warn(&format!(
                "COLUMNS='{raw}' unparseable ({err}); using {DEFAULT_TERMINAL_WIDTH} cells"
            ));
            DEFAULT_TERMINAL_WIDTH
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::str::FromStr;

    #[test]
    fn malformed_json_renders_marker_and_succeeds() {
        let mut out = Vec::new();
        run(Cursor::new(b"{not json"), &mut out).expect("IO should not fail");
        assert_eq!(String::from_utf8(out).expect("utf8"), "?\n");
    }

    #[test]
    fn minimal_payload_renders_model_then_workspace() {
        let json = br#"{
            "model": { "display_name": "Claude Test" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let mut out = Vec::new();
        run(Cursor::new(json), &mut out).expect("run ok");
        assert_eq!(
            String::from_utf8(out).expect("utf8"),
            "Claude Test linesmith\n"
        );
    }

    // --- resolve_terminal_width ---

    fn resolve(os_width: Option<u16>, columns: Option<&str>) -> (u16, Vec<String>) {
        let mut warnings = Vec::new();
        let w = resolve_terminal_width(os_width, columns, |m| warnings.push(m.to_string()));
        (w, warnings)
    }

    #[test]
    fn os_width_wins_over_columns_env() {
        let (w, warns) = resolve(Some(120), Some("80"));
        assert_eq!(w, 120);
        assert!(warns.is_empty());
    }

    #[test]
    fn columns_env_used_when_os_width_missing() {
        let (w, warns) = resolve(None, Some("80"));
        assert_eq!(w, 80);
        assert!(warns.is_empty());
    }

    #[test]
    fn missing_columns_falls_back_silently() {
        let (w, warns) = resolve(None, None);
        assert_eq!(w, DEFAULT_TERMINAL_WIDTH);
        assert!(warns.is_empty());
    }

    #[test]
    fn zero_columns_falls_back_and_warns() {
        let (w, warns) = resolve(None, Some("0"));
        assert_eq!(w, DEFAULT_TERMINAL_WIDTH);
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("COLUMNS='0'"));
    }

    #[test]
    fn unparseable_columns_falls_back_and_warns() {
        let (w, warns) = resolve(None, Some("wide"));
        assert_eq!(w, DEFAULT_TERMINAL_WIDTH);
        assert_eq!(warns.len(), 1);
        assert!(warns[0].contains("unparseable"));
    }

    #[test]
    fn columns_beyond_u16_range_warns() {
        // "99999" is > u16::MAX (65535), so parse::<u16>() fails.
        let (w, warns) = resolve(None, Some("99999"));
        assert_eq!(w, DEFAULT_TERMINAL_WIDTH);
        assert_eq!(warns.len(), 1);
    }

    // --- build_segments ---

    fn built(cfg: Option<&config::Config>) -> Vec<Box<dyn Segment>> {
        build_segments(cfg, |_| {})
    }

    #[test]
    fn build_segments_uses_default_order_when_config_missing() {
        assert_eq!(built(None).len(), segments::DEFAULT_SEGMENT_IDS.len());
    }

    #[test]
    fn build_segments_empty_config_falls_back_to_defaults() {
        let cfg = config::Config::default();
        assert_eq!(built(Some(&cfg)).len(), segments::DEFAULT_SEGMENT_IDS.len());
    }

    #[test]
    fn build_segments_uses_configured_line_order() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["workspace", "model"]
            "#,
        )
        .expect("parse");
        let got = built(Some(&cfg));
        // Compare by default priority since we can't name-check dyn
        // Segments directly.
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].defaults().priority, 16); // workspace
        assert_eq!(got[1].defaults().priority, 64); // model
    }

    #[test]
    fn build_segments_applies_priority_override() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model"]
                [segments.model]
                priority = 0
            "#,
        )
        .expect("parse");
        let got = built(Some(&cfg));
        assert_eq!(got[0].defaults().priority, 0);
    }

    #[test]
    fn build_segments_applies_width_override() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["workspace"]
                [segments.workspace.width]
                min = 5
                max = 30
            "#,
        )
        .expect("parse");
        let got = built(Some(&cfg));
        let bounds = got[0].defaults().width.expect("width set");
        assert_eq!(bounds.min(), 5);
        assert_eq!(bounds.max(), 30);
    }

    #[test]
    fn build_segments_skips_unknown_ids_and_warns() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model", "does_not_exist", "workspace"]
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let got = build_segments(Some(&cfg), |msg| warnings.push(msg.to_string()));
        assert_eq!(got.len(), 2);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("does_not_exist"));
    }

    #[test]
    fn build_segments_dedupes_duplicates_with_warning() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["model", "model", "workspace"]
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let got = build_segments(Some(&cfg), |msg| warnings.push(msg.to_string()));
        assert_eq!(got.len(), 2); // one model, one workspace
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("model"));
        assert!(warnings[0].contains("more than once"));
    }

    #[test]
    fn build_segments_warns_on_explicitly_empty_segment_list() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = []
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let got = build_segments(Some(&cfg), |msg| warnings.push(msg.to_string()));
        assert!(got.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("empty"));
    }

    #[test]
    fn build_segments_warns_on_inverted_width_bounds() {
        let cfg = config::Config::from_str(
            r#"
                [line]
                segments = ["workspace"]
                [segments.workspace.width]
                min = 40
                max = 10
            "#,
        )
        .expect("parse");
        let mut warnings = Vec::new();
        let got = build_segments(Some(&cfg), |msg| warnings.push(msg.to_string()));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].defaults().width, None);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("min"));
        assert!(warnings[0].contains("max"));
    }

    // Stub with a baseline `width` default pins `apply_override`'s
    // inherit-from-inner branches. No built-in segment exercises
    // these today, so the contract needs an explicit guard.
    struct StubWithWidth;

    impl Segment for StubWithWidth {
        fn render(&self, _: &input::StatusContext) -> segments::RenderResult {
            Ok(Some(segments::RenderedSegment::new("x")))
        }
        fn defaults(&self) -> segments::SegmentDefaults {
            segments::SegmentDefaults::with_priority(128)
                .with_width(segments::WidthBounds::new(10, 50).expect("valid"))
        }
    }

    fn merge_width(min: Option<u16>, max: Option<u16>) -> segments::WidthBounds {
        let ov = config::SegmentOverride {
            priority: None,
            width: Some(config::WidthBoundsConfig { min, max }),
        };
        let wrapped = apply_override("stub", Box::new(StubWithWidth), Some(&ov), &mut |_| {});
        wrapped.defaults().width.expect("width preserved")
    }

    #[test]
    fn width_merge_min_only_inherits_max_from_inner_default() {
        let got = merge_width(Some(5), None);
        assert_eq!(got.min(), 5);
        assert_eq!(got.max(), 50);
    }

    #[test]
    fn width_merge_max_only_inherits_min_from_inner_default() {
        let got = merge_width(None, Some(80));
        assert_eq!(got.min(), 10);
        assert_eq!(got.max(), 80);
    }

    #[test]
    fn width_merge_both_sides_override_inner_default() {
        let got = merge_width(Some(3), Some(40));
        assert_eq!(got.min(), 3);
        assert_eq!(got.max(), 40);
    }

    #[test]
    fn width_merge_empty_override_keeps_inner_default() {
        // An empty [segments.<id>.width] table still appears as
        // `Some(WidthBoundsConfig { min: None, max: None })`; the
        // merged bounds must equal the inner's default.
        let got = merge_width(None, None);
        assert_eq!(got.min(), 10);
        assert_eq!(got.max(), 50);
    }

    #[test]
    fn build_segments_forward_compat_keys_dont_break_parsing() {
        let cfg = config::Config::from_str(
            r#"
                theme = "catppuccin-mocha"
                preset = "developer"
                layout = "single-line"
                [line]
                segments = ["model"]
                [layout_options]
                separator = "powerline"
            "#,
        )
        .expect("parse");
        assert_eq!(built(Some(&cfg)).len(), 1);
    }

    #[test]
    fn full_payload_renders_model_context_workspace() {
        let json = br#"{
            "model": { "display_name": "Claude Sonnet 4.6" },
            "workspace": {
                "project_dir": "/home/dev/linesmith",
                "git_worktree": { "name": "feat-auth", "path": "/wt/feat-auth" }
            },
            "context_window": {
                "used_percentage": 42.5,
                "context_window_size": 200000,
                "total_input_tokens": 12345,
                "total_output_tokens": 6789
            }
        }"#;
        let mut out = Vec::new();
        run(Cursor::new(json), &mut out).expect("run ok");
        assert_eq!(
            String::from_utf8(out).expect("utf8"),
            "Claude Sonnet 4.6 42% · 200k linesmith/feat-auth\n"
        );
    }
}
