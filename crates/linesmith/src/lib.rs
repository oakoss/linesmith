//! linesmith: Rust status line for Claude Code and other AI coding CLIs.
//!
//! `run` reads a JSON payload from a `Read`, renders a status line, and
//! writes the result to a `Write`. The binary in `main.rs` wires this to
//! stdin/stdout. See `docs/specs/` for the segment, theme, config, and
//! plugin contracts.

pub mod cli;
pub mod config;
pub mod data_context;
pub(crate) mod driver;
pub mod input;
pub mod layout;
pub mod plugins;
pub mod presets;
pub mod segments;
pub mod theme;

pub use driver::{cli_main, CliEnv};
pub use segments::builder::{build_default_segments, build_segments};

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
/// Parse failures render a `?` marker and log to the real process
/// stderr; output is unstyled. For themed output or injected-stderr
/// testability (used by `cli_main`), call [`run_with_context`] instead.
///
/// # Errors
///
/// Returns an `io::Error` if reading from `reader` or writing to
/// `writer` fails.
pub fn run_with_segments_and_width(
    reader: impl Read,
    writer: impl Write,
    segments: &[Box<dyn Segment>],
    terminal_width: u16,
) -> io::Result<()> {
    // `cwd: None` — callers that want gix discovery go through
    // `run_with_context` with a populated RenderContext.
    let ctx = RenderContext {
        theme: theme::default_theme(),
        capability: theme::Capability::None,
        terminal_width,
        cwd: None,
    };
    run_with_context(reader, writer, &mut io::stderr().lock(), segments, &ctx)
}

/// Theme + capability + terminal width + cwd bundled for the render
/// path. Passed to [`run_with_context`]; the CLI driver builds one
/// from config (theme name), the color-policy precedence chain (CLI
/// flags / env / config), `CliEnv.terminal_width` minus any padding,
/// and the process cwd.
///
/// `cwd` seeds gix repo discovery. `None` skips discovery entirely;
/// `Some(path)` runs `gix::discover(path)` on the first `ctx.git()`
/// read.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RenderContext<'a> {
    pub theme: &'a theme::Theme,
    pub capability: theme::Capability,
    pub terminal_width: u16,
    pub cwd: Option<std::path::PathBuf>,
}

/// Full-control entry with injected stderr and explicit render
/// context. Parse failures render a `?` marker to `writer`; only
/// stdin/stdout I/O failures surface as errors.
///
/// # Errors
///
/// Returns an `io::Error` if reading from `reader` or writing to
/// `writer` fails. Stderr write failures are swallowed (a broken
/// stderr pipe must not abort a valid stdout render).
pub fn run_with_context(
    mut reader: impl Read,
    mut writer: impl Write,
    stderr: &mut dyn Write,
    segments: &[Box<dyn Segment>],
    ctx: &RenderContext<'_>,
) -> io::Result<()> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;

    let status_ctx = match input::parse(&buf) {
        Ok(c) => c,
        Err(err) => {
            let _ = writeln!(stderr, "linesmith: parse: {err}");
            return writeln!(writer, "?");
        }
    };
    let data_ctx = data_context::DataContext::with_cwd(status_ctx, ctx.cwd.clone());

    let line = layout::render_with_warn(
        segments,
        &data_ctx,
        ctx.terminal_width,
        &mut |msg| {
            let _ = writeln!(stderr, "linesmith: {msg}");
        },
        ctx.theme,
        ctx.capability,
    );
    writeln!(writer, "{line}")
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
