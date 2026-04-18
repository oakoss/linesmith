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
pub mod theme;

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
    let ctx = RenderContext {
        theme: theme::default_theme(),
        capability: theme::Capability::None,
        terminal_width,
    };
    run_with_context(reader, writer, &mut io::stderr().lock(), segments, &ctx)
}

/// Theme + capability + terminal width bundled for the render path.
/// Passed to [`run_with_context`]; `cli_main` builds one from config
/// (theme name) + `Capability::detect()` + `CliEnv.terminal_width`.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct RenderContext<'a> {
    pub theme: &'a theme::Theme,
    pub capability: theme::Capability,
    pub terminal_width: u16,
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

    let line = layout::render_with_warn(
        segments,
        &status_ctx,
        ctx.terminal_width,
        &mut |msg| {
            let _ = writeln!(stderr, "linesmith: {msg}");
        },
        ctx.theme,
        ctx.capability,
    );
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

/// Process-ambient inputs the CLI reads: env vars consulted by
/// `resolve_config_path`, an optional terminal-width override, and an
/// optional color-capability override. Passed through `cli_main` so
/// tests can drive the whole binary without touching the real process
/// env. `#[non_exhaustive]` leaves room for future env vars
/// (FORCE_COLOR, TERM, ...) without breaking external construction.
///
/// `terminal_width = None` means "detect lazily when the render path
/// needs it." Meta commands (`--help`, `--version`, `--check-config`)
/// never probe the terminal, so stray `COLUMNS` warnings don't leak
/// into clean stderr.
///
/// `color_capability = None` means the same: detect via
/// `supports-color` on the render path only.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CliEnv {
    pub linesmith_config: Option<String>,
    pub xdg_config_home: Option<String>,
    pub home: Option<String>,
    pub terminal_width: Option<u16>,
    pub color_capability: Option<theme::Capability>,
}

impl CliEnv {
    /// Snapshot the real process env vars. Terminal width and color
    /// capability are left unset; `run_cli` probes them only if a
    /// render happens.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            linesmith_config: std::env::var("LINESMITH_CONFIG").ok(),
            xdg_config_home: std::env::var("XDG_CONFIG_HOME").ok(),
            home: std::env::var("HOME").ok(),
            terminal_width: None,
            color_capability: None,
        }
    }
}

/// CLI entry point. `main.rs` wires real stdin/stdout/stderr and a
/// process-env snapshot. Returns a `u8` exit code so callers convert
/// to `ExitCode` only at the outermost layer.
pub fn cli_main<A>(
    args: A,
    stdin: impl Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &CliEnv,
) -> u8
where
    A: IntoIterator,
    A::Item: Into<std::ffi::OsString>,
{
    let action = match cli::parse(args) {
        Ok(a) => a,
        Err(err) => {
            let _ = writeln!(stderr, "linesmith: {err}");
            let _ = writeln!(stderr, "Try --help for usage.");
            return 2;
        }
    };

    match action {
        cli::Action::Help => {
            let _ = write!(stdout, "{}", cli::HELP);
            0
        }
        cli::Action::Version => {
            let _ = writeln!(stdout, "linesmith {}", env!("CARGO_PKG_VERSION"));
            0
        }
        cli::Action::Run(args) => run_cli(args, stdin, stdout, stderr, env),
    }
}

fn run_cli(
    args: cli::CliArgs,
    stdin: impl Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &CliEnv,
) -> u8 {
    let resolved = config::resolve_config_path(
        args.config.clone(),
        env.linesmith_config.as_deref(),
        env.xdg_config_home.as_deref(),
        env.home.as_deref(),
    );
    let (cfg, load_error) = load_config(resolved.as_ref(), stderr);

    if args.check_config {
        return check_config(resolved.as_ref(), cfg.as_ref(), load_error, stderr);
    }

    let segments = build_segments(cfg.as_ref(), |msg| {
        let _ = writeln!(stderr, "linesmith: {msg}");
    });

    let width = env.terminal_width.unwrap_or_else(detect_terminal_width);
    let theme_ref = resolve_theme(cfg.as_ref(), stderr);
    let capability = env
        .color_capability
        .unwrap_or_else(theme::Capability::detect);
    let ctx = RenderContext {
        theme: theme_ref,
        capability,
        terminal_width: width,
    };
    if let Err(err) = run_with_context(stdin, stdout, stderr, &segments, &ctx) {
        let _ = writeln!(stderr, "linesmith: {err}");
        return 1;
    }
    0
}

/// Resolve the active theme from config. Unknown names fall back to
/// `default` with a stderr warning; missing or empty `theme` uses
/// the default silently.
fn resolve_theme(cfg: Option<&config::Config>, stderr: &mut dyn Write) -> &'static theme::Theme {
    let Some(name) = cfg
        .and_then(|c| c.theme.as_deref())
        .filter(|n| !n.is_empty())
    else {
        return theme::default_theme();
    };
    match theme::built_in(name) {
        Some(t) => t,
        None => {
            let _ = writeln!(stderr, "linesmith: unknown theme '{name}'; using 'default'");
            theme::default_theme()
        }
    }
}

/// Load the config at `resolved` if present. Missing files are silent
/// for implicit paths (first-run users) but warn for explicit paths
/// (the user asked for a specific file and it wasn't there).
fn load_config(
    resolved: Option<&config::ConfigPath>,
    stderr: &mut dyn Write,
) -> (Option<config::Config>, Option<config::ConfigError>) {
    let Some(cp) = resolved else {
        return (None, None);
    };
    match config::Config::load(&cp.path) {
        Ok(Some(c)) => (Some(c), None),
        Ok(None) => {
            if cp.explicit {
                let _ = writeln!(
                    stderr,
                    "linesmith: config not found at {}",
                    cp.path.display()
                );
            }
            (None, None)
        }
        Err(e) => {
            let _ = writeln!(stderr, "linesmith: {e}");
            (None, Some(e))
        }
    }
}

fn check_config(
    resolved: Option<&config::ConfigPath>,
    cfg: Option<&config::Config>,
    load_error: Option<config::ConfigError>,
    stderr: &mut dyn Write,
) -> u8 {
    // `--check-config` is the CI / editor contract for strict
    // validation; if we can't even resolve a config path, that's a
    // failure rather than a "use defaults" fallback.
    let Some(cp) = resolved else {
        let _ = writeln!(
            stderr,
            "linesmith: no config path (HOME and XDG_CONFIG_HOME both unset, no --config)"
        );
        return 1;
    };
    if load_error.is_some() {
        let _ = writeln!(stderr, "linesmith: config invalid ({})", cp.path.display());
        return 1;
    }
    let Some(cfg) = cfg else {
        let _ = writeln!(
            stderr,
            "linesmith: no config at {}; using built-in defaults",
            cp.path.display()
        );
        return 0;
    };

    let mut warn_count = 0_usize;
    let _ = build_segments(Some(cfg), |msg| {
        let _ = writeln!(stderr, "linesmith: {msg}");
        warn_count += 1;
    });
    if let Some(name) = cfg.theme.as_deref().filter(|n| !n.is_empty()) {
        if theme::built_in(name).is_none() {
            let _ = writeln!(stderr, "linesmith: unknown theme '{name}'; using 'default'");
            warn_count += 1;
        }
    }
    let _ = writeln!(stderr, "linesmith: config ok ({})", cp.path.display());
    if warn_count > 0 {
        let _ = writeln!(stderr, "linesmith: {warn_count} warning(s)");
    }
    0
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

#[cfg(test)]
mod cli_main_tests {
    //! Drive the whole CLI entry point (`cli_main`) with fake IO and a
    //! hand-built env. These tests lock exit codes and stderr contents
    //! end-to-end. Integration tests in `tests/integration.rs` exercise
    //! the same binary flow via `run_with_width`.

    use super::*;
    use std::io::Cursor;

    /// Shared `CliEnv` for tests that don't care about config resolution.
    /// Width `Some(200)` gives every segment room to render without
    /// probing the real TTY.
    fn empty_env() -> CliEnv {
        CliEnv {
            linesmith_config: None,
            xdg_config_home: None,
            home: None,
            terminal_width: Some(200),
            // Force plain output so tests' stdout assertions don't
            // accidentally include theme ANSI under a truecolor host.
            color_capability: Some(theme::Capability::None),
        }
    }

    /// Run `cli_main` with the given args + stdin; return
    /// `(exit_code, stdout, stderr)` as UTF-8 strings.
    fn run_cli_main(args: &[&str], stdin: &[u8], env: &CliEnv) -> (u8, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let args_owned: Vec<std::ffi::OsString> =
            args.iter().map(std::ffi::OsString::from).collect();
        let code = cli_main(
            args_owned,
            Cursor::new(stdin),
            &mut stdout,
            &mut stderr,
            env,
        );
        (
            code,
            String::from_utf8(stdout).expect("stdout utf8"),
            String::from_utf8(stderr).expect("stderr utf8"),
        )
    }

    // --- meta actions ---

    #[test]
    fn help_flag_prints_help_to_stdout_and_exits_zero() {
        let (code, stdout, stderr) = run_cli_main(&["--help"], b"", &empty_env());
        assert_eq!(code, 0);
        assert_eq!(stdout, cli::HELP);
        assert!(stderr.is_empty());
    }

    #[test]
    fn version_flag_prints_version_to_stdout_and_exits_zero() {
        let (code, stdout, stderr) = run_cli_main(&["--version"], b"", &empty_env());
        assert_eq!(code, 0);
        assert_eq!(stdout, format!("linesmith {}\n", env!("CARGO_PKG_VERSION")));
        assert!(stderr.is_empty());
    }

    #[test]
    fn meta_flags_skip_terminal_width_detection() {
        // With terminal_width: None, the render path probes COLUMNS /
        // the TTY; meta commands must not, so a broken COLUMNS can't
        // leak a width warning into --help / --version / --check-config
        // stderr. Construct an env that would warn *if* detection ran.
        let lazy_env = CliEnv {
            linesmith_config: None,
            xdg_config_home: None,
            home: None,
            terminal_width: None,
            color_capability: None,
        };
        let (code, _stdout, stderr) = run_cli_main(&["--help"], b"", &lazy_env);
        assert_eq!(code, 0);
        assert!(
            stderr.is_empty(),
            "meta flag leaked width-detect output: {stderr}"
        );
    }

    #[test]
    fn unknown_flag_exits_two_and_prints_hint_to_stderr() {
        let (code, stdout, stderr) = run_cli_main(&["--nope"], b"", &empty_env());
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("nope"));
        assert!(stderr.contains("Try --help for usage."));
    }

    #[test]
    fn empty_config_value_exits_two() {
        // Shell-expansion guard: `--config ""` from `--config "$UNSET"`
        // must not silently fall through to defaults.
        let (code, _stdout, stderr) = run_cli_main(&["--config", ""], b"", &empty_env());
        assert_eq!(code, 2);
        assert!(stderr.contains("Try --help"));
    }

    // --- render flow ---

    #[test]
    fn minimal_payload_round_trips_through_cli_main() {
        let json = br#"{
            "model": { "display_name": "Claude Test" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let (code, stdout, stderr) = run_cli_main(&[], json, &empty_env());
        assert_eq!(code, 0);
        assert_eq!(stdout, "Claude Test linesmith\n");
        assert!(stderr.is_empty());
    }

    #[test]
    fn malformed_json_renders_marker_and_routes_parse_error_to_injected_stderr() {
        // Locks the stderr plumbing: parse errors must surface on the
        // caller's stderr sink, not the real process stderr.
        let (code, stdout, stderr) = run_cli_main(&[], b"{not json", &empty_env());
        assert_eq!(code, 0);
        assert_eq!(stdout, "?\n");
        assert!(
            stderr.contains("parse:"),
            "expected parse diag, got: {stderr}"
        );
    }

    #[test]
    fn render_io_error_exits_one() {
        // Hand cli_main a stdout writer that fails, and confirm the
        // render path returns 1 rather than 0 or 2. Without this test
        // the exit code can silently regress to SUCCESS.
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let json = br#"{"model":{"display_name":"Claude"},"workspace":{"project_dir":"/x"}}"#;
        let mut stderr = Vec::new();
        let code = cli_main(
            Vec::<std::ffi::OsString>::new(),
            Cursor::new(json),
            &mut FailingWriter,
            &mut stderr,
            &empty_env(),
        );
        assert_eq!(code, 1);
        let stderr_str = String::from_utf8(stderr).expect("utf8");
        assert!(stderr_str.contains("linesmith:"), "got: {stderr_str}");
    }

    #[test]
    fn explicit_config_path_drives_render_not_just_check() {
        // `check_config_with_valid_file_exits_zero_and_reports_ok`
        // proves the path reaches --check-config; this proves it also
        // drives the render path, so a regression that validates but
        // then discards `resolved` gets caught.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[line]\nsegments = [\"workspace\", \"model\"]\n").unwrap();
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let (code, stdout, _stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &empty_env());
        assert_eq!(code, 0);
        // Config reordered segments: workspace before model.
        assert_eq!(stdout, "linesmith Claude\n");
    }

    // --- --check-config exit-code contract (docs/specs/config.md) ---

    #[test]
    fn check_config_with_no_resolvable_path_exits_one() {
        // HOME, XDG_CONFIG_HOME, and LINESMITH_CONFIG all unset, no
        // --config flag: resolve_config_path returns None and
        // --check-config treats that as a failure rather than "use
        // defaults."
        let (code, stdout, stderr) = run_cli_main(&["--check-config"], b"", &empty_env());
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert!(stderr.contains("no config path"));
    }

    #[test]
    fn check_config_with_valid_file_exits_zero_and_reports_ok() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[line]\nsegments = [\"model\", \"workspace\"]\n").unwrap();
        let (code, stdout, stderr) = run_cli_main(
            &["--check-config", "--config", path.to_str().unwrap()],
            b"",
            &empty_env(),
        );
        assert_eq!(code, 0);
        assert!(stdout.is_empty());
        assert!(stderr.contains("config ok"));
        assert!(stderr.contains(path.to_str().unwrap()));
    }

    #[test]
    fn check_config_with_malformed_toml_exits_one_and_reports_invalid() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[line\nsegments =").unwrap();
        let (code, stdout, stderr) = run_cli_main(
            &["--check-config", "--config", path.to_str().unwrap()],
            b"",
            &empty_env(),
        );
        assert_eq!(code, 1);
        assert!(stdout.is_empty());
        assert!(stderr.contains("config invalid"));
    }

    #[test]
    fn check_config_with_missing_explicit_path_warns_but_exits_zero() {
        // `--check-config` only fails when the path is *unresolvable*
        // (no env anywhere) or the file parses as invalid. A missing
        // explicit path reports "not found" and falls back to defaults
        // with SUCCESS.
        let dir = tempdir();
        let missing = dir.path().join("nonexistent.toml");
        let (code, _stdout, stderr) = run_cli_main(
            &["--check-config", "--config", missing.to_str().unwrap()],
            b"",
            &empty_env(),
        );
        assert_eq!(code, 0);
        assert!(stderr.contains("config not found"));
        assert!(stderr.contains("using built-in defaults"));
    }

    #[test]
    fn check_config_surfaces_validation_warnings() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[line]\nsegments = [\"model\", \"does_not_exist\"]\n",
        )
        .unwrap();
        let (code, _stdout, stderr) = run_cli_main(
            &["--check-config", "--config", path.to_str().unwrap()],
            b"",
            &empty_env(),
        );
        assert_eq!(code, 0);
        assert!(stderr.contains("does_not_exist"));
        assert!(stderr.contains("1 warning(s)"));
    }

    #[test]
    fn check_config_catches_unknown_theme_name() {
        // Without this, a typo like `theme = "defualt"` only surfaces on
        // render. `--check-config` is the CI/editor contract, so it must
        // catch unknown theme names too.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"defualt\"\n").unwrap();
        let (code, _stdout, stderr) = run_cli_main(
            &["--check-config", "--config", path.to_str().unwrap()],
            b"",
            &empty_env(),
        );
        assert_eq!(code, 0);
        assert!(stderr.contains("unknown theme 'defualt'"));
        assert!(stderr.contains("1 warning(s)"));
    }

    // --- CliEnv plumbing ---

    #[test]
    fn cli_env_routes_home_through_to_config_resolution() {
        // Proves env.home actually reaches resolve_config_path rather
        // than getting shadowed by a process env::var read.
        let dir = tempdir();
        let cfg_dir = dir.path().join(".config/linesmith");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("config.toml"),
            "[line]\nsegments = [\"model\"]\n",
        )
        .unwrap();

        let env = CliEnv {
            home: Some(dir.path().to_string_lossy().into_owned()),
            ..empty_env()
        };
        let (code, _stdout, stderr) = run_cli_main(&["--check-config"], b"", &env);
        assert_eq!(code, 0);
        assert!(stderr.contains("config ok"));
    }

    #[test]
    fn cli_env_xdg_takes_precedence_over_home_in_resolution() {
        let dir = tempdir();
        let xdg_cfg = dir.path().join("xdg/linesmith");
        std::fs::create_dir_all(&xdg_cfg).unwrap();
        std::fs::write(xdg_cfg.join("config.toml"), "[line]\nsegments = []\n").unwrap();

        let env = CliEnv {
            xdg_config_home: Some(dir.path().join("xdg").to_string_lossy().into_owned()),
            home: Some("/nowhere/that/exists".to_string()),
            ..empty_env()
        };
        let (code, _stdout, stderr) = run_cli_main(&["--check-config"], b"", &env);
        assert_eq!(code, 0);
        assert!(stderr.contains(dir.path().join("xdg").to_str().unwrap()));
    }

    // --- theme wiring ---

    #[test]
    fn default_theme_under_palette16_wraps_segments_with_sgr() {
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let env = CliEnv {
            color_capability: Some(theme::Capability::Palette16),
            ..empty_env()
        };
        let (code, stdout, _stderr) = run_cli_main(&[], json, &env);
        assert_eq!(code, 0);
        // Model (Primary → BrightMagenta = SGR 95) and workspace (Info →
        // BrightCyan = SGR 96) each get wrapped; plain text between them
        // is a single space separator.
        assert_eq!(stdout, "\x1b[95mClaude\x1b[0m \x1b[96mlinesmith\x1b[0m\n");
    }

    #[test]
    fn minimal_theme_under_palette16_emits_no_color() {
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"minimal\"\n").unwrap();

        let env = CliEnv {
            color_capability: Some(theme::Capability::Palette16),
            ..empty_env()
        };
        let (code, stdout, _stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        // Minimal theme has NoColor for every role; segments don't have
        // bold/italic decorations, so output is plain.
        assert_eq!(stdout, "Claude linesmith\n");
    }

    #[test]
    fn unknown_theme_falls_back_to_default_with_warning() {
        let json = br#"{
            "model": { "display_name": "C" },
            "workspace": { "project_dir": "/x" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"nonexistent\"\n").unwrap();

        let env = CliEnv {
            color_capability: Some(theme::Capability::None),
            ..empty_env()
        };
        let (code, stdout, stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "C x\n");
        assert!(stderr.contains("unknown theme 'nonexistent'"));
        assert!(stderr.contains("using 'default'"));
    }

    #[test]
    fn no_color_capability_strips_theme_under_default() {
        // Even the `default` theme (Palette16 values) emits nothing
        // under Capability::None. This is the NO_COLOR contract: no
        // ANSI bytes, no risk of leaking escape sequences when stdout
        // is piped to non-terminal consumers.
        let json = br#"{
            "model": { "display_name": "C" },
            "workspace": { "project_dir": "/x" }
        }"#;
        let env = CliEnv {
            color_capability: Some(theme::Capability::None),
            ..empty_env()
        };
        let (code, stdout, _stderr) = run_cli_main(&[], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "C x\n");
    }

    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tempdir() -> TempDir {
        let base = std::env::temp_dir().join(format!(
            "linesmith-cli-main-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ));
        std::fs::create_dir_all(&base).expect("mkdir");
        TempDir(base)
    }
}
