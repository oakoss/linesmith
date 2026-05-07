//! Live-preview pane for the `linesmith config` editor per ADR-0016.
//!
//! Renders the in-memory [`config::Config`] through the production
//! layout engine ([`layout::render_to_runs`]) and maps each
//! [`StyledRun`] to a ratatui [`Span`] with theme-resolved color
//! and decoration. The preview reflects exactly what `linesmith`
//! would emit for the same config; accuracy comes from sharing the
//! render path, not from re-implementing it.
//!
//! `DataContext` is built from a hardcoded sample payload so
//! segments that depend on stdin (model, workspace, context_window,
//! cost, etc.) have something to render against. The editor isn't
//! parsing real Claude Code input, so the preview shows a
//! representative rendering for the configured segments rather
//! than mirroring live data.
//!
//! Theme + capability are caller-supplied so the preview honors
//! the user's `config.theme` and any `NO_COLOR`-style overrides
//! the same way the production driver does.

use ratatui::style::{Color as RColor, Modifier, Style as RStyle};
use ratatui::text::{Line, Span};

use crate::config::Config;
use crate::data_context::error::UsageError;
use crate::data_context::DataContext;
use crate::layout::render_to_runs;
use crate::logging::CapturedSink;
use crate::segments::builder::build_lines;
use crate::segments::LineItem;
use crate::theme::{AnsiColor, Capability, Color, StyledRun, Theme};

/// Hardcoded preview payload. Keeps the editor self-contained —
/// no stdin, no env probing, no state leaks across screens. Values
/// are picked to drive every built-in default segment visibly (a
/// known model name, a project path, a partial context window, a
/// non-zero cost figure) without pretending to be live data.
const PREVIEW_STDIN_JSON: &[u8] = br#"{
    "model": { "display_name": "claude-sonnet-4-5" },
    "workspace": { "project_dir": "/home/dev/linesmith" },
    "context_window": {
        "used_percentage": 35.0,
        "context_window_size": 200000,
        "total_input_tokens": 50000,
        "total_output_tokens": 25000
    },
    "cost": {
        "total_cost_usd": 0.42,
        "total_duration_ms": 12345,
        "total_api_duration_ms": 6789,
        "total_lines_added": 12,
        "total_lines_removed": 4
    }
}"#;

/// Render the configured lines into ratatui [`Line`]s plus any
/// warnings the engine emitted. The warnings are everything
/// `build_lines` and `render_to_runs` would surface to stderr in
/// the production path (unknown segment ids, missing plugins,
/// layout-mode mismatches, render errors); the caller chooses how
/// to display them.
///
/// `sink` is the [`CapturedSink`] the TUI installs for the alt-
/// screen lifetime so `lsm_warn!` / `lsm_error!` / `lsm_debug!`
/// emissions from inside `build_lines` and `render_to_runs` (layout
/// contract violations, schema-drift warns inside the input parser,
/// segment render errors) land in the warnings panel instead of
/// painting over the frame. Drained at the end of this call so the
/// returned warnings vec carries the full per-frame diagnostic
/// stream — explicit-callback channel + macro channel — in one
/// list. Pass `None` from non-TUI callers (the unit tests below)
/// where the macros are free to write to stderr.
///
/// Plugin segments don't render: the preview path passes
/// `plugins = None` to keep the editor independent of the rhai
/// runtime. Plugin-authored segments hide silently and the warn
/// channel surfaces an "unknown segment id" message for each.
pub(super) fn render_lines(
    config: &Config,
    theme: &Theme,
    capability: Capability,
    terminal_width: u16,
    sink: Option<&CapturedSink>,
) -> (Vec<Line<'static>>, Vec<String>) {
    let mut warnings: Vec<String> = Vec::new();
    let lines = build_lines(Some(config), None, |msg| warnings.push(msg.to_string()));
    let ctx = preview_context();
    let mut rendered = Vec::with_capacity(lines.len());
    for line in &lines {
        rendered.push(render_line(
            line,
            &ctx,
            terminal_width,
            theme,
            capability,
            |msg| {
                warnings.push(msg.to_string());
            },
        ));
    }
    // Drain after every macro emission has fired so the returned
    // vec captures both channels. Order: explicit-callback
    // warnings first (in segment-iteration order), then macro
    // emissions appended. Macros that fire between draws (none
    // today; possible once v0.2 background plugins land) surface
    // on the next frame's render, not the one they fired during.
    if let Some(sink) = sink {
        warnings.extend(sink.drain());
    }
    (rendered, warnings)
}

fn render_line(
    items: &[LineItem],
    ctx: &DataContext,
    width: u16,
    theme: &Theme,
    capability: Capability,
    mut warn: impl FnMut(&str),
) -> Line<'static> {
    let runs = render_to_runs(items, ctx, width, &mut warn);
    let spans: Vec<Span<'static>> = runs
        .iter()
        .map(|r| run_to_span(r, theme, capability))
        .collect();
    Line::from(spans)
}

/// Convert one [`StyledRun`] into a styled ratatui [`Span`].
/// `fg` overrides `role`; decorations layer on top of the
/// resolved color.
fn run_to_span(run: &StyledRun, theme: &Theme, capability: Capability) -> Span<'static> {
    let mut style = RStyle::default();

    let color = run
        .style()
        .fg
        .or_else(|| run.style().role.map(|role| theme.color(role)));
    if let Some(c) = color {
        if let Some(rcolor) = color_to_ratatui(c.downgrade(capability)) {
            style = style.fg(rcolor);
        }
    }

    if run.style().bold {
        style = style.add_modifier(Modifier::BOLD);
    }
    if run.style().italic {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if run.style().underline {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if run.style().dim {
        style = style.add_modifier(Modifier::DIM);
    }

    Span::styled(run.text().to_string(), style)
}

fn color_to_ratatui(c: Color) -> Option<RColor> {
    match c {
        Color::TrueColor { r, g, b } => Some(RColor::Rgb(r, g, b)),
        Color::Palette256(n) => Some(RColor::Indexed(n)),
        Color::Palette16(ansi) => Some(ansi_to_ratatui(ansi)),
        Color::NoColor => None,
        // `Color` is `#[non_exhaustive]`; an unknown future variant
        // falls back to no color rather than a best-guess that
        // might mislead the preview.
        _ => None,
    }
}

/// Map by terminal semantics, not by name: SGR 37 ("white") is the
/// dimmer foreground (ratatui calls it `Gray`), and SGR 97 ("bright
/// white") is the maximum-brightness shade (ratatui calls it
/// `White`). Same swap for "black" / "bright black".
fn ansi_to_ratatui(c: AnsiColor) -> RColor {
    match c {
        AnsiColor::Black => RColor::Black,
        AnsiColor::Red => RColor::Red,
        AnsiColor::Green => RColor::Green,
        AnsiColor::Yellow => RColor::Yellow,
        AnsiColor::Blue => RColor::Blue,
        AnsiColor::Magenta => RColor::Magenta,
        AnsiColor::Cyan => RColor::Cyan,
        AnsiColor::White => RColor::Gray,
        AnsiColor::BrightBlack => RColor::DarkGray,
        AnsiColor::BrightRed => RColor::LightRed,
        AnsiColor::BrightGreen => RColor::LightGreen,
        AnsiColor::BrightYellow => RColor::LightYellow,
        AnsiColor::BrightBlue => RColor::LightBlue,
        AnsiColor::BrightMagenta => RColor::LightMagenta,
        AnsiColor::BrightCyan => RColor::LightCyan,
        AnsiColor::BrightWhite => RColor::White,
    }
}

/// Build the preview's [`DataContext`].
///
/// Goes through `input::parse` because [`StatusContext`] is
/// `#[non_exhaustive]` — outside-crate struct-literal construction
/// is forbidden, and the parser is the only stable construction
/// point. Parser failures degrade to an empty context so a future
/// schema migration that stiffens parse rules doesn't crash the
/// editor on the first frame draw.
///
/// `usage` is preseeded so `rate_limit_*` segments hide cleanly
/// without invoking the live fallback cascade. The cascade would
/// otherwise read the user's real Keychain credentials, JSONL
/// transcripts, and hit the OAuth endpoint on every redraw —
/// undesirable both for performance and for the principle that
/// the editor shouldn't surface real account state.
///
/// `cwd` is set to the process working directory so `git_branch`
/// and `workspace`'s linked-worktree suffix render against the
/// repo the editor is open on. `git()` is `OnceCell`-memoized,
/// so discovery cost is paid once per session.
fn preview_context() -> DataContext {
    preview_context_from(PREVIEW_STDIN_JSON)
}

/// Inner builder taking the bytes as a parameter so tests can
/// drive the parse-rejected fallback path with a payload that
/// actually exercises the `Err =>` arm.
fn preview_context_from(bytes: &[u8]) -> DataContext {
    let status = crate::input::parse(bytes).unwrap_or_else(|_| empty_status());
    let cwd = std::env::current_dir().ok();
    let ctx = DataContext::with_cwd(status, cwd);
    // `NoCredentials` is the "no auth, no data" terminal state of
    // the cascade — picking it routes every `rate_limit_*` segment
    // through the same hide path the production driver hits when
    // the user has no auth, without firing the cascade itself.
    let _ = ctx.preseed_usage(Err(UsageError::NoCredentials));
    ctx
}

/// Last-ditch fallback when [`PREVIEW_STDIN_JSON`] fails to parse.
/// Parses `b"{}"` — every status field collapses to `None`, so
/// most segments hide, but the preview chrome still renders.
fn empty_status() -> crate::input::StatusContext {
    crate::input::parse(b"{}").expect("empty JSON object always parses")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{self, Role, Style};

    fn style_with_fg(fg: Color) -> Style {
        let mut s = Style::default();
        s.fg = Some(fg);
        s
    }

    fn style_with_decorations() -> Style {
        let mut s = Style::default();
        s.bold = true;
        s.italic = true;
        s.underline = true;
        s.dim = true;
        s
    }

    fn render(config: &Config, width: u16) -> (Vec<Line<'static>>, Vec<String>) {
        // `sink = None` keeps these tests independent of the
        // process-wide log sink; they assert the explicit-callback
        // channel only.
        render_lines(
            config,
            theme::default_theme(),
            Capability::TrueColor,
            width,
            None,
        )
    }

    #[test]
    fn preview_context_parses_without_panicking() {
        // Pin: PREVIEW_STDIN_JSON must stay parseable as
        // `input::parse` evolves. If a required field is added,
        // this test fires before any preview frame draws.
        let _ = preview_context();
    }

    #[test]
    fn preview_context_falls_back_when_parse_rejects() {
        // Drive `preview_context_from` with a payload the parser
        // explicitly rejects (negative percentage), pinning that
        // the `Err =>` arm routes to `empty_status` rather than
        // panicking. A regression that swaps this arm for an
        // `expect` would crash here on the first frame draw.
        let rejecting = b"{ \"context_window\": { \"used_percentage\": -1.0 } }";
        assert!(
            crate::input::parse(rejecting).is_err(),
            "test premise — payload must reject",
        );
        let _ = preview_context_from(rejecting);
    }

    #[test]
    fn render_lines_with_default_config_returns_one_line_no_warnings() {
        // Default Config (no `[line]` section) falls back to the
        // built-in default segment set. Pin that the preview emits
        // exactly one line and no warnings — the default config
        // must be silent.
        let cfg = Config::default();
        let (lines, warnings) = render(&cfg, 200);
        assert_eq!(lines.len(), 1);
        assert!(warnings.is_empty(), "default config warned: {warnings:?}");
    }

    #[test]
    fn render_lines_for_two_line_config_returns_two_lines() {
        // Multi-line config: each `[line.N]` slot becomes its own
        // entry in the returned Vec. Without the `layout =
        // "multi-line"` declaration the builder auto-promotes and
        // emits a hint warning; the no-warnings assertion pins
        // that the explicit-layout path is silent.
        let cfg: Config = "layout = \"multi-line\"\n\
                           [line.1]\nsegments = [\"model\"]\n\
                           [line.2]\nsegments = [\"workspace\"]\n"
            .parse()
            .expect("parse");
        let (lines, warnings) = render(&cfg, 200);
        assert_eq!(lines.len(), 2);
        assert!(
            warnings.is_empty(),
            "clean multi-line config warned: {warnings:?}",
        );
    }

    #[test]
    fn render_lines_surfaces_unknown_segment_warning() {
        // Pin the diagnostic-channel contract: `build_lines`
        // emits "unknown segment id" for typos and missing
        // plugin segments; the preview captures and returns
        // them so the caller can display in the UI.
        let cfg: Config = "[line]\nsegments = [\"modle\"]\n".parse().expect("parse");
        let (_lines, warnings) = render(&cfg, 80);
        assert!(
            warnings.iter().any(|w| w.contains("modle")),
            "expected warning about 'modle' typo, got {warnings:?}",
        );
    }

    #[test]
    fn run_to_span_maps_fg_color_to_ratatui_rgb() {
        // Explicit `fg` overrides `role`; pin that the truecolor
        // RGB triple lands in the ratatui span without a downgrade
        // step.
        let run = StyledRun::new(
            "x",
            style_with_fg(Color::TrueColor {
                r: 0xab,
                g: 0xcd,
                b: 0xef,
            }),
        );
        let span = run_to_span(&run, theme::default_theme(), Capability::TrueColor);
        assert_eq!(span.style.fg, Some(RColor::Rgb(0xab, 0xcd, 0xef)));
    }

    #[test]
    fn run_to_span_fg_wins_over_role_when_both_set() {
        // The `or_else` chain says fg overrides role. Pin both
        // fields populated so a future "and"/operand-flip refactor
        // (preferring role) would fail here. The role's resolved
        // color must NOT appear in the span.
        let mut style = Style::default();
        style.fg = Some(Color::TrueColor {
            r: 0x11,
            g: 0x22,
            b: 0x33,
        });
        style.role = Some(Role::Error);
        let run = StyledRun::new("x", style);
        let span = run_to_span(&run, theme::default_theme(), Capability::TrueColor);
        assert_eq!(span.style.fg, Some(RColor::Rgb(0x11, 0x22, 0x33)));
    }

    #[test]
    fn run_to_span_resolves_role_through_theme_when_fg_unset() {
        // With no explicit fg, `role` selects a theme color.
        // Catches a regression where role-resolution is dropped.
        let run = StyledRun::new("x", Style::role(Role::Primary));
        let span = run_to_span(&run, theme::default_theme(), Capability::TrueColor);
        assert!(
            span.style.fg.is_some(),
            "role-only run must resolve to a color via theme",
        );
    }

    #[test]
    fn run_to_span_applies_decorations() {
        // Decorations layer over color. Pin all four in one run
        // so a future "modifiers collapsed into a bitset" refactor
        // catches every flag.
        let run = StyledRun::new("x", style_with_decorations());
        let span = run_to_span(&run, theme::default_theme(), Capability::TrueColor);
        let mods = span.style.add_modifier;
        assert!(mods.contains(Modifier::BOLD));
        assert!(mods.contains(Modifier::ITALIC));
        assert!(mods.contains(Modifier::UNDERLINED));
        assert!(mods.contains(Modifier::DIM));
    }

    #[test]
    fn no_color_capability_strips_fg_but_keeps_decorations() {
        // Pin: color and decoration are independent axes —
        // `Capability::None` strips fg without touching decorations.
        let mut style = style_with_fg(Color::TrueColor { r: 1, g: 2, b: 3 });
        style.bold = true;
        let run = StyledRun::new("x", style);
        let span = run_to_span(&run, theme::default_theme(), Capability::None);
        assert_eq!(span.style.fg, None, "Capability::None must strip color");
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn color_to_ratatui_covers_every_known_variant() {
        // Pin the four-arm match: each `Color` variant produces
        // the documented ratatui mapping. The `_ => None`
        // catch-all is for forward-compat (`Color` is
        // `#[non_exhaustive]`); known variants must stay wired.
        assert_eq!(
            color_to_ratatui(Color::TrueColor { r: 1, g: 2, b: 3 }),
            Some(RColor::Rgb(1, 2, 3)),
        );
        assert_eq!(
            color_to_ratatui(Color::Palette256(42)),
            Some(RColor::Indexed(42)),
        );
        assert_eq!(
            color_to_ratatui(Color::Palette16(AnsiColor::Red)),
            Some(RColor::Red),
        );
        assert_eq!(color_to_ratatui(Color::NoColor), None);
    }

    #[test]
    fn ansi_to_ratatui_maps_every_variant_with_correct_brightness_swap() {
        // Pin the AnsiColor → RColor table. The non-obvious cases
        // are White → Gray and BrightWhite → White (matching xterm
        // semantics: SGR 37 is the dimmer "white", SGR 97 is the
        // brightest). A future "clean up" of the match could
        // silently invert the swap.
        assert_eq!(ansi_to_ratatui(AnsiColor::Black), RColor::Black);
        assert_eq!(ansi_to_ratatui(AnsiColor::Red), RColor::Red);
        assert_eq!(ansi_to_ratatui(AnsiColor::Green), RColor::Green);
        assert_eq!(ansi_to_ratatui(AnsiColor::Yellow), RColor::Yellow);
        assert_eq!(ansi_to_ratatui(AnsiColor::Blue), RColor::Blue);
        assert_eq!(ansi_to_ratatui(AnsiColor::Magenta), RColor::Magenta);
        assert_eq!(ansi_to_ratatui(AnsiColor::Cyan), RColor::Cyan);
        assert_eq!(ansi_to_ratatui(AnsiColor::White), RColor::Gray);
        assert_eq!(ansi_to_ratatui(AnsiColor::BrightBlack), RColor::DarkGray);
        assert_eq!(ansi_to_ratatui(AnsiColor::BrightRed), RColor::LightRed);
        assert_eq!(ansi_to_ratatui(AnsiColor::BrightGreen), RColor::LightGreen);
        assert_eq!(
            ansi_to_ratatui(AnsiColor::BrightYellow),
            RColor::LightYellow
        );
        assert_eq!(ansi_to_ratatui(AnsiColor::BrightBlue), RColor::LightBlue);
        assert_eq!(
            ansi_to_ratatui(AnsiColor::BrightMagenta),
            RColor::LightMagenta,
        );
        assert_eq!(ansi_to_ratatui(AnsiColor::BrightCyan), RColor::LightCyan);
        assert_eq!(ansi_to_ratatui(AnsiColor::BrightWhite), RColor::White);
    }

    #[test]
    fn render_lines_drains_captured_sink_into_warnings() {
        // Pin the macro-channel pickup: `lsm_warn!` emissions that
        // fire during `render_lines` (whether from build_lines,
        // render_to_runs, or any internal helper that bypasses the
        // explicit `warn` callbacks) end up in the returned
        // warnings vec via the captured sink. Without this drain,
        // those emissions would either paint over the alt-screen
        // (if sink is StderrSink) or get silently swallowed.
        use crate::logging::{self, Level};
        use std::sync::Arc;

        // Serialize against any other test in the process that
        // installs a sink or mutates the level — without this, two
        // tests racing each other's `SinkGuard::install` would
        // steal each other's emissions.
        let _serial = logging::_test_serial_lock();
        let cfg = Config::default();
        let captured = Arc::new(CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        // Pre-seed an emission so the drain path has something to
        // pick up regardless of whether any built-in segment in the
        // default config decides to warn this frame.
        logging::set_level(Level::Warn);
        linesmith_core::lsm_warn!("preview-drain-pin: synthetic warn");

        let (_lines, warnings) = render_lines(
            &cfg,
            theme::default_theme(),
            Capability::TrueColor,
            200,
            Some(captured.as_ref()),
        );
        // Strict count + format pin: the drained entry must appear
        // exactly once and carry the `[warn]` prefix. A weaker
        // `.any(contains)` would pass under future refactors that
        // duplicate or strip the captured-sink prefix; both would
        // be silent regressions of the documented format.
        let matches: Vec<&String> = warnings
            .iter()
            .filter(|w| w.contains("preview-drain-pin"))
            .collect();
        assert_eq!(
            matches.len(),
            1,
            "expected exactly one synthetic-warn entry, got {warnings:?}",
        );
        assert!(
            matches[0].starts_with("[warn] "),
            "drained entry must carry the `[warn]` prefix, got {:?}",
            matches[0],
        );
        // Sink consumed by drain — second call must be empty.
        assert!(
            captured.drain().is_empty(),
            "render_lines must consume the captured sink",
        );
    }

    #[test]
    fn render_lines_with_sink_none_does_not_drain_global_sink() {
        // Pin the contract: `sink = None` means render_lines does
        // NOT touch the process-wide sink. A refactor that
        // silently substitutes a default sink (e.g.
        // `sink.unwrap_or(&CapturedSink::default()).drain()`) would
        // both swallow real macro emissions in non-TUI callers and
        // bypass the production stderr destination — the test
        // catches that by installing a captured sink globally,
        // emitting, then asserting the entry is STILL there after
        // a `None` render.
        use crate::logging::{self, Level};
        use std::sync::Arc;

        let _serial = logging::_test_serial_lock();
        let cfg = Config::default();
        let captured = Arc::new(CapturedSink::default());
        let _restore = logging::SinkGuard::install(captured.clone());
        logging::set_level(Level::Warn);
        linesmith_core::lsm_warn!("none-bypass-pin: stays in sink");

        let (_lines, _warnings) = render_lines(
            &cfg,
            theme::default_theme(),
            Capability::TrueColor,
            200,
            None,
        );
        // The render_lines call did not drain — the entry must
        // still be sitting in the captured sink.
        let leftovers = captured.drain();
        assert!(
            leftovers.iter().any(|w| w.contains("none-bypass-pin")),
            "sink=None must leave global sink contents alone, got {leftovers:?}",
        );
    }

    #[test]
    fn render_lines_with_plugin_segment_does_not_crash() {
        // Plugin segments resolve to `unknown segment id` warnings
        // because the preview passes `plugins = None`. Pin that
        // the warning surfaces AND the surrounding non-plugin
        // segments still render — a regression where the unknown
        // id short-circuits the whole line would yield a length-1
        // vec with no spans.
        let cfg: Config = "[line]\nsegments = [\"model\", \"my-plugin\", \"workspace\"]\n"
            .parse()
            .expect("parse");
        let (lines, warnings) = render(&cfg, 200);
        assert_eq!(lines.len(), 1, "plugin segment must not break the line");
        assert!(
            !lines[0].spans.is_empty(),
            "non-plugin segments must still render alongside the unknown id",
        );
        assert!(
            warnings.iter().any(|w| w.contains("my-plugin")),
            "expected plugin warning, got {warnings:?}",
        );
    }
}
