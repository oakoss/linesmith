//! linesmith-core: render engine + data context for the linesmith
//! status line. Hosts the segment system, theming, layout, plugin
//! host, config schema, and runtime predicates that both the CLI
//! driver and the doctor consume.
//!
//! `run` reads a JSON payload from a `Read`, renders a status line,
//! and writes the result to a `Write`. The `linesmith-cli` binary
//! crate wires this to stdin/stdout. See `docs/specs/` for the
//! segment, theme, config, and plugin contracts.
//!
//! Workspace-internal in v0.1 per ADR-0018; the public surface is
//! free to refactor without SemVer cost until a publish decision
//! lands (post-v1.0 at the earliest).

pub mod config;
pub mod data_context;
pub mod input;
pub mod layout;
pub mod logging;
pub mod plugins;
pub mod presets;
pub mod runtime;
pub mod segments;
pub mod theme;

pub use segments::builder::{build_default_segments, build_lines, build_segments};

use crate::segments::LineItem;
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
    let items = build_default_segments();
    run_with_segments_and_width(reader, writer, &items, terminal_width)
}

/// Full-control entry: pre-built [`LineItem`] list plus explicit width.
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
    items: &[LineItem],
    terminal_width: u16,
) -> io::Result<()> {
    // `cwd: None` — callers that want gix discovery go through
    // `run_with_context` with a populated RunContext.
    let ctx = RunContext::new(
        theme::default_theme(),
        theme::Capability::None,
        terminal_width,
        None,
        false,
    );
    run_with_context(reader, writer, &mut io::stderr().lock(), items, &ctx)
}

/// CLI run-state bundle: theme + capability + terminal width + cwd.
/// Passed to [`run_with_context`]; the CLI driver builds one from
/// config (theme name), the color-policy precedence chain (CLI flags /
/// env / config), `CliEnv.terminal_width` minus any padding, and the
/// process cwd. Distinct from
/// [`segments::RenderContext`](crate::segments::RenderContext), which
/// is the per-segment-render layout state.
///
/// `cwd` seeds gix repo discovery. `None` skips discovery entirely;
/// `Some(path)` runs `gix::discover(path)` on the first `ctx.git()`
/// read.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunContext<'a> {
    pub theme: &'a theme::Theme,
    pub capability: theme::Capability,
    pub terminal_width: u16,
    pub cwd: Option<std::path::PathBuf>,
    /// Whether the terminal advertises OSC 8 hyperlink support. Drives
    /// emission of `Style.hyperlink` URLs in [`layout::runs_to_ansi`];
    /// orthogonal to color `capability` since hyperlinks and color
    /// support are independent terminal features.
    pub hyperlinks: bool,
}

impl<'a> RunContext<'a> {
    /// Build a `RunContext`. The struct is `#[non_exhaustive]` so
    /// struct-literal construction is blocked downstream; benches,
    /// out-of-tree embedders, and in-crate call sites all go through
    /// this constructor so future field additions touch one site.
    ///
    /// `terminal_width` is forwarded as-is. The layout engine treats
    /// `0` as "no budget" and drops every droppable segment; callers
    /// that want a default should use `detect_terminal_width` or
    /// `DEFAULT_TERMINAL_WIDTH` instead of relying on the sentinel.
    #[must_use]
    pub fn new(
        theme: &'a theme::Theme,
        capability: theme::Capability,
        terminal_width: u16,
        cwd: Option<std::path::PathBuf>,
        hyperlinks: bool,
    ) -> Self {
        Self {
            theme,
            capability,
            terminal_width,
            cwd,
            hyperlinks,
        }
    }
}

/// Full-control entry with injected stderr and explicit run context.
/// Parse failures render a `?` marker to `writer`; only stdin/stdout
/// I/O failures surface as errors.
///
/// # Errors
///
/// Returns an `io::Error` if reading from `reader` or writing to
/// `writer` fails. Stderr write failures are swallowed (a broken
/// stderr pipe must not abort a valid stdout render).
pub fn run_with_context(
    reader: impl Read,
    writer: impl Write,
    stderr: &mut dyn Write,
    items: &[LineItem],
    ctx: &RunContext<'_>,
) -> io::Result<()> {
    // The function predates multi-line and is part of the public API
    // surface (`pub use` in lib.rs), so removing it would be a SemVer
    // break. Delegate to the multi-line path with one line so single-
    // line callers don't need to allocate a `Vec<Vec<...>>` shim.
    run_lines_with_context(reader, writer, stderr, std::slice::from_ref(&items), ctx)
}

/// Multi-line render entry. Each inner slice is one rendered line;
/// the layout algorithm runs independently per line with the full
/// terminal width budget. Stdin is parsed once into a shared
/// [`DataContext`](data_context::DataContext) so every line sees the
/// same data snapshot. Empty inner slices still emit a `writeln!()`
/// — the user explicitly defined the line slot, so it stays in the
/// output even if no segments rendered.
///
/// Parse failures emit a single `?` marker on the first line and
/// stop, matching the single-line failure mode (the marker tells
/// Claude Code "linesmith ran but couldn't parse stdin"; emitting a
/// per-line marker would be visually noisy without conveying more
/// information).
///
/// # Errors
///
/// Returns the first `io::Error` from a `writeln!` to `writer`.
/// Stderr write failures are swallowed.
pub fn run_lines_with_context(
    mut reader: impl Read,
    mut writer: impl Write,
    stderr: &mut dyn Write,
    lines: &[&[LineItem]],
    ctx: &RunContext<'_>,
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

    for items in lines {
        let mut warn = |msg: &str| {
            let _ = writeln!(stderr, "linesmith: {msg}");
        };
        let mut observers = layout::LayoutObservers::new(&mut warn);
        let line = layout::render_with_observers(
            items,
            &data_ctx,
            ctx.terminal_width,
            &mut observers,
            ctx.theme,
            ctx.capability,
            ctx.hyperlinks,
        );
        writeln!(writer, "{line}")?;
    }
    Ok(())
}

/// Width fallback when `terminal_size()` and `COLUMNS` both fail.
/// Matches `docs/specs/segment-system.md` edge-case table.
const DEFAULT_TERMINAL_WIDTH: u16 = 200;

/// Resolve the terminal width in cells. Prefers the OS-reported size, then
/// the `COLUMNS` env var, then `DEFAULT_TERMINAL_WIDTH`. A set-but-invalid
/// `COLUMNS` value routes through [`lsm_warn!`] so the user can correct
/// their config; an unset `COLUMNS` falls through silently (the common
/// case when stdout is piped to Claude Code).
#[must_use]
pub fn detect_terminal_width() -> u16 {
    let os_width = terminal_size::terminal_size().map(|(terminal_size::Width(w), _)| w);
    let columns = std::env::var("COLUMNS").ok();
    resolve_terminal_width(os_width, columns.as_deref(), |msg| {
        crate::lsm_warn!("{msg}")
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
                "project_dir": "/home/dev/linesmith"
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
            "Claude Sonnet 4.6 42% · 200k linesmith\n"
        );
    }
}
