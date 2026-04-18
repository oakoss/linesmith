//! CLI driver. `cli_main` takes args + stdin + env and returns an
//! exit code, handling arg parsing, config loading, `--check-config`,
//! and render dispatch. `main.rs` wires real IO with
//! `CliEnv::from_process`; tests pass `Cursor` / `Vec<u8>` buffers
//! and a hand-built `CliEnv`.

use crate::segments::builder::build_segments;
use crate::{cli, config, detect_terminal_width, run_with_context, theme, RenderContext};
use std::io::{Read, Write};

/// `NO_COLOR`-style flag: any non-empty value means "on." Per
/// no-color.org.
fn no_color_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// `FORCE_COLOR`-style flag: treat `"0"`, `"false"`, `"off"`, unset,
/// and empty as "absent" (no force). Any other value enables force.
/// Matches npm / supports-color / chalk conventions so `FORCE_COLOR=0`
/// doesn't accidentally force color on users who set it to disable.
fn force_color_env(name: &str) -> bool {
    let Ok(v) = std::env::var(name) else {
        return false;
    };
    !matches!(v.as_str(), "" | "0" | "false" | "off")
}

/// Process-ambient inputs the CLI reads: env vars consulted by
/// `resolve_config_path`, the color-policy env flags, an optional
/// terminal-width override, and an optional color-capability
/// override. Passed through `cli_main` so tests can drive the whole
/// binary without touching the real process env. `#[non_exhaustive]`
/// leaves room for future env vars (TERM, ...) without breaking
/// external construction.
///
/// Env snapshotting is the exclusive job of [`CliEnv::from_process`].
/// [`CliEnv::default`] and [`CliEnv::for_tests`] do not read any
/// ambient state — the resolver honors only what the struct carries.
/// Production binaries must use `from_process`; callers passing
/// `default()` opt out of env awareness entirely (including
/// `NO_COLOR` / `FORCE_COLOR` / `COLUMNS`).
///
/// `terminal_width = None` means "detect lazily when the render path
/// needs it." Meta commands (`--help`, `--version`, `--check-config`)
/// never probe the terminal, so stray `COLUMNS` warnings don't leak
/// into clean stderr.
///
/// `color_capability = Some(cap)` bypasses the entire color-policy
/// precedence chain — reserved for test determinism. Production uses
/// `None` and lets `no_color` / `force_color` / config resolve it.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct CliEnv {
    pub linesmith_config: Option<String>,
    pub xdg_config_home: Option<String>,
    pub home: Option<String>,
    pub no_color: bool,
    pub force_color: bool,
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
            no_color: no_color_env("NO_COLOR"),
            force_color: force_color_env("FORCE_COLOR"),
            terminal_width: None,
            color_capability: None,
        }
    }

    /// Test-suite baseline: no env paths, color flags off,
    /// `terminal_width = Some(200)`, `color_capability = Some(None)`.
    /// Forces the capability override so stdout stays plain under a
    /// truecolor host; tests that exercise the color-policy resolver
    /// directly use `CliEnv::default()` instead.
    #[must_use]
    pub fn for_tests() -> Self {
        Self {
            linesmith_config: None,
            xdg_config_home: None,
            home: None,
            no_color: false,
            force_color: false,
            terminal_width: Some(200),
            color_capability: Some(theme::Capability::None),
        }
    }
}

/// CLI entry point. Returns a `u8` exit code so callers convert to
/// `ExitCode` only at the outermost layer.
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
    let (cfg, load_error, config_warnings) = load_config(resolved.as_ref(), stderr);

    if args.check_config {
        return check_config(
            resolved.as_ref(),
            cfg.as_ref(),
            load_error,
            config_warnings,
            stderr,
        );
    }

    // Surface unknown-key warnings before rendering so they ride on the
    // same stderr stream as segment-build warnings and parse errors.
    for msg in &config_warnings {
        let _ = writeln!(stderr, "linesmith: {msg}");
    }

    let segments = build_segments(cfg.as_ref(), |msg| {
        let _ = writeln!(stderr, "linesmith: {msg}");
    });

    let raw_width = env.terminal_width.unwrap_or_else(detect_terminal_width);
    let padding = layout_options(cfg.as_ref()).map_or(0, |l| l.claude_padding);
    let width = raw_width.saturating_sub(padding);
    let theme_ref = resolve_theme(cfg.as_ref(), stderr);
    let capability = resolve_color_capability(args.color_override, env, cfg.as_ref());
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

fn layout_options(cfg: Option<&config::Config>) -> Option<&config::LayoutOptions> {
    cfg.and_then(|c| c.layout_options.as_ref())
}

/// Color-policy precedence chain, first match wins:
///   1. `CliEnv.color_capability` override (test escape hatch)
///   2. `--no-color` / `--force-color` CLI flag
///   3. `NO_COLOR` env var
///   4. `FORCE_COLOR` env var
///   5. `[layout_options].color` in config
///   6. default `auto` — detect via `supports-color`
fn resolve_color_capability(
    cli_override: Option<cli::ColorOverride>,
    env: &CliEnv,
    cfg: Option<&config::Config>,
) -> theme::Capability {
    if let Some(cap) = env.color_capability {
        return cap;
    }
    match cli_override {
        Some(cli::ColorOverride::Never) => return theme::Capability::None,
        Some(cli::ColorOverride::Always) => return force_color_detect(),
        None => {}
    }
    if env.no_color {
        return theme::Capability::None;
    }
    if env.force_color {
        return force_color_detect();
    }
    match layout_options(cfg).map(|l| l.color).unwrap_or_default() {
        config::ColorPolicy::Never => theme::Capability::None,
        config::ColorPolicy::Always => force_color_detect(),
        config::ColorPolicy::Auto => theme::Capability::from_terminal(),
    }
}

/// Under "force color" intent, pick the richest supported tier; if the
/// terminal reports no color support (typical when stdout isn't a TTY),
/// fall back to `Palette16` so the user sees something rather than
/// nothing.
fn force_color_detect() -> theme::Capability {
    match theme::Capability::from_terminal() {
        theme::Capability::None => theme::Capability::Palette16,
        other => other,
    }
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
/// (the user asked for a specific file and it wasn't there). Unknown
/// keys inside the file are collected as warnings so callers (render
/// path vs `--check-config`) can decide how to surface them.
fn load_config(
    resolved: Option<&config::ConfigPath>,
    stderr: &mut dyn Write,
) -> (
    Option<config::Config>,
    Option<config::ConfigError>,
    Vec<String>,
) {
    let Some(cp) = resolved else {
        return (None, None, Vec::new());
    };
    let mut warnings = Vec::new();
    let load_result =
        config::Config::load_validated(&cp.path, |msg| warnings.push(msg.to_string()));
    match load_result {
        Ok(Some(c)) => (Some(c), None, warnings),
        Ok(None) => {
            if cp.explicit {
                let _ = writeln!(
                    stderr,
                    "linesmith: config not found at {}",
                    cp.path.display()
                );
            }
            (None, None, warnings)
        }
        Err(e) => {
            let _ = writeln!(stderr, "linesmith: {e}");
            (None, Some(e), warnings)
        }
    }
}

fn check_config(
    resolved: Option<&config::ConfigPath>,
    cfg: Option<&config::Config>,
    load_error: Option<config::ConfigError>,
    config_warnings: Vec<String>,
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
    for msg in &config_warnings {
        let _ = writeln!(stderr, "linesmith: {msg}");
        warn_count += 1;
    }
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
    //! Drive the whole CLI entry point (`cli_main`) with fake IO and a
    //! hand-built env. These tests lock exit codes and stderr contents
    //! end-to-end. Integration tests in `tests/integration.rs` exercise
    //! the same binary flow via `run_with_width`.

    use super::*;
    use std::io;
    use std::io::Cursor;

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
        let (code, stdout, stderr) = run_cli_main(&["--help"], b"", &CliEnv::for_tests());
        assert_eq!(code, 0);
        assert_eq!(stdout, cli::HELP);
        assert!(stderr.is_empty());
    }

    #[test]
    fn version_flag_prints_version_to_stdout_and_exits_zero() {
        let (code, stdout, stderr) = run_cli_main(&["--version"], b"", &CliEnv::for_tests());
        assert_eq!(code, 0);
        assert_eq!(stdout, format!("linesmith {}\n", env!("CARGO_PKG_VERSION")));
        assert!(stderr.is_empty());
    }

    #[test]
    fn meta_flags_skip_terminal_width_detection() {
        // With terminal_width: None, the render path probes COLUMNS /
        // the TTY; meta commands must not, so a broken COLUMNS can't
        // leak a width warning into --help / --version / --check-config
        // stderr. `CliEnv::default()` would warn *if* detection ran.
        let (code, _stdout, stderr) = run_cli_main(&["--help"], b"", &CliEnv::default());
        assert_eq!(code, 0);
        assert!(
            stderr.is_empty(),
            "meta flag leaked width-detect output: {stderr}"
        );
    }

    #[test]
    fn unknown_flag_exits_two_and_prints_hint_to_stderr() {
        let (code, stdout, stderr) = run_cli_main(&["--nope"], b"", &CliEnv::for_tests());
        assert_eq!(code, 2);
        assert!(stdout.is_empty());
        assert!(stderr.contains("nope"));
        assert!(stderr.contains("Try --help for usage."));
    }

    #[test]
    fn empty_config_value_exits_two() {
        // Shell-expansion guard: `--config ""` from `--config "$UNSET"`
        // must not silently fall through to defaults.
        let (code, _stdout, stderr) = run_cli_main(&["--config", ""], b"", &CliEnv::for_tests());
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
        let (code, stdout, stderr) = run_cli_main(&[], json, &CliEnv::for_tests());
        assert_eq!(code, 0);
        assert_eq!(stdout, "Claude Test linesmith\n");
        assert!(stderr.is_empty());
    }

    #[test]
    fn malformed_json_renders_marker_and_routes_parse_error_to_injected_stderr() {
        // Locks the stderr plumbing: parse errors must surface on the
        // caller's stderr sink, not the real process stderr.
        let (code, stdout, stderr) = run_cli_main(&[], b"{not json", &CliEnv::for_tests());
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
        let env = CliEnv::for_tests();
        let code = cli_main(
            Vec::<std::ffi::OsString>::new(),
            Cursor::new(json),
            &mut FailingWriter,
            &mut stderr,
            &env,
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
        let (code, stdout, _stderr) = run_cli_main(
            &["--config", path.to_str().unwrap()],
            json,
            &CliEnv::for_tests(),
        );
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
        let (code, stdout, stderr) = run_cli_main(&["--check-config"], b"", &CliEnv::for_tests());
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
            &CliEnv::for_tests(),
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
            &CliEnv::for_tests(),
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
            &CliEnv::for_tests(),
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
            &CliEnv::for_tests(),
        );
        assert_eq!(code, 0);
        assert!(stderr.contains("does_not_exist"));
        assert!(stderr.contains("1 warning(s)"));
    }

    #[test]
    fn check_config_catches_unknown_top_level_key() {
        // Typo at top level: `thme` should warn and count toward the
        // summary so CI gates catch it.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "thme = \"default\"\n").unwrap();
        let (code, _stdout, stderr) = run_cli_main(
            &["--check-config", "--config", path.to_str().unwrap()],
            b"",
            &CliEnv::for_tests(),
        );
        assert_eq!(code, 0);
        assert!(stderr.contains("thme"));
        assert!(stderr.contains("1 warning(s)"));
    }

    #[test]
    fn check_config_catches_unknown_segment_override_key() {
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[segments.model]\npriorty = 16\n").unwrap();
        let (code, _stdout, stderr) = run_cli_main(
            &["--check-config", "--config", path.to_str().unwrap()],
            b"",
            &CliEnv::for_tests(),
        );
        assert_eq!(code, 0);
        assert!(stderr.contains("priorty"));
        assert!(stderr.contains("[segments.model]"));
        assert!(stderr.contains("1 warning(s)"));
    }

    #[test]
    fn check_config_counts_warnings_across_all_three_scopes() {
        // One typo each at top-level, [layout_options], and
        // [segments.<id>]: the summary must tally all three so a CI
        // gate grepping the count catches the full set.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "thme = \"oops\"\n[layout_options]\nseparatr = \"x\"\n[segments.model]\npriorty = 1\n",
        )
        .unwrap();
        let (code, _stdout, stderr) = run_cli_main(
            &["--check-config", "--config", path.to_str().unwrap()],
            b"",
            &CliEnv::for_tests(),
        );
        assert_eq!(code, 0);
        assert!(stderr.contains("thme"));
        assert!(stderr.contains("separatr"));
        assert!(stderr.contains("priorty"));
        assert!(stderr.contains("3 warning(s)"));
    }

    #[test]
    fn unknown_key_warnings_emit_once_per_typo_on_render_path() {
        // Pins the early-return at `if args.check_config { return ... }`:
        // the render path's pre-render loop and check_config's loop
        // must not double-emit for the same typo.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "thme = \"oops\"\n").unwrap();
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let (code, _stdout, stderr) = run_cli_main(
            &["--config", path.to_str().unwrap()],
            json,
            &CliEnv::for_tests(),
        );
        assert_eq!(code, 0);
        assert_eq!(
            stderr.matches("thme").count(),
            1,
            "unknown-key warning double-emitted: {stderr}"
        );
    }

    #[test]
    fn render_path_surfaces_unknown_key_warnings_on_stderr() {
        // Render flow must still see unknown-key warnings even though
        // no `--check-config` summary runs.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "thme = \"oops\"\n").unwrap();
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let (code, stdout, stderr) = run_cli_main(
            &["--config", path.to_str().unwrap()],
            json,
            &CliEnv::for_tests(),
        );
        assert_eq!(code, 0);
        assert_eq!(stdout, "Claude linesmith\n");
        assert!(stderr.contains("thme"));
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
            &CliEnv::for_tests(),
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
            ..CliEnv::for_tests()
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
            ..CliEnv::for_tests()
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
            ..CliEnv::for_tests()
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
            ..CliEnv::for_tests()
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
            ..CliEnv::for_tests()
        };
        let (code, stdout, stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "C x\n");
        assert!(stderr.contains("unknown theme 'nonexistent'"));
        assert!(stderr.contains("using 'default'"));
    }

    #[test]
    fn catppuccin_mocha_renders_with_mocha_palette_under_truecolor() {
        // End-to-end contract: `theme = "catppuccin-mocha"` in config +
        // TrueColor capability emits the Mocha palette's exact RGB
        // values. Model (Primary → mauve: 203,166,247); workspace
        // (Info → teal: 148,226,213). If this snapshot fails after a
        // `catppuccin` crate bump, the upstream palette drifted and the
        // theme file deserves a deliberate review.
        let json = br#"{
            "model": { "display_name": "C" },
            "workspace": { "project_dir": "/x" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "theme = \"catppuccin-mocha\"\n").unwrap();
        let env = CliEnv {
            color_capability: Some(theme::Capability::TrueColor),
            ..CliEnv::for_tests()
        };
        let (code, stdout, _stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        assert_eq!(
            stdout,
            "\x1b[38;2;203;166;247mC\x1b[0m \x1b[38;2;148;226;213mx\x1b[0m\n"
        );
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
            ..CliEnv::for_tests()
        };
        let (code, stdout, _stderr) = run_cli_main(&[], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "C x\n");
    }

    // --- color-policy precedence ---

    /// Build a `CliEnv` suitable for driving the resolver directly —
    /// the test-only capability override is cleared so the chain
    /// actually executes.
    fn policy_env() -> CliEnv {
        CliEnv {
            color_capability: None,
            ..CliEnv::for_tests()
        }
    }

    #[test]
    fn color_policy_cli_never_wins_over_force_env() {
        let env = CliEnv {
            force_color: true,
            ..policy_env()
        };
        assert_eq!(
            resolve_color_capability(Some(cli::ColorOverride::Never), &env, None),
            theme::Capability::None,
        );
    }

    #[test]
    fn color_policy_cli_always_wins_over_no_color_env() {
        let env = CliEnv {
            no_color: true,
            ..policy_env()
        };
        let got = resolve_color_capability(Some(cli::ColorOverride::Always), &env, None);
        // Falls back to Palette16 when the terminal reports None (tests
        // run without a TTY); anything ≥ Palette16 proves the force
        // path ran and didn't land on Capability::None.
        assert!(got >= theme::Capability::Palette16, "got {got:?}");
    }

    #[test]
    fn color_policy_no_color_env_wins_over_force_env() {
        // When both NO_COLOR and FORCE_COLOR are set, no-color.org's
        // rule is that NO_COLOR wins (order in the chain).
        let env = CliEnv {
            no_color: true,
            force_color: true,
            ..policy_env()
        };
        assert_eq!(
            resolve_color_capability(None, &env, None),
            theme::Capability::None,
        );
    }

    #[test]
    fn color_policy_no_color_env_wins_over_config_always() {
        let cfg = config::Config {
            layout_options: Some(config::LayoutOptions {
                color: config::ColorPolicy::Always,
                ..config::LayoutOptions::default()
            }),
            ..config::Config::default()
        };
        let env = CliEnv {
            no_color: true,
            ..policy_env()
        };
        assert_eq!(
            resolve_color_capability(None, &env, Some(&cfg)),
            theme::Capability::None,
        );
    }

    #[test]
    fn color_policy_config_never_strips_color() {
        let cfg = config::Config {
            layout_options: Some(config::LayoutOptions {
                color: config::ColorPolicy::Never,
                ..config::LayoutOptions::default()
            }),
            ..config::Config::default()
        };
        assert_eq!(
            resolve_color_capability(None, &policy_env(), Some(&cfg)),
            theme::Capability::None,
        );
    }

    #[test]
    fn color_policy_config_always_forces_color() {
        // Mirror of the `Never` test for the other explicit branch.
        let cfg = config::Config {
            layout_options: Some(config::LayoutOptions {
                color: config::ColorPolicy::Always,
                ..config::LayoutOptions::default()
            }),
            ..config::Config::default()
        };
        let got = resolve_color_capability(None, &policy_env(), Some(&cfg));
        assert!(got >= theme::Capability::Palette16, "got {got:?}");
    }

    #[test]
    fn color_policy_config_auto_falls_through_to_terminal_detection() {
        let cfg = config::Config {
            layout_options: Some(config::LayoutOptions {
                color: config::ColorPolicy::Auto,
                ..config::LayoutOptions::default()
            }),
            ..config::Config::default()
        };
        // Without a TTY under `cargo test`, `from_terminal` returns None;
        // the assertion is just that the resolver didn't short-circuit.
        let got = resolve_color_capability(None, &policy_env(), Some(&cfg));
        assert_eq!(got, theme::Capability::from_terminal());
    }

    #[test]
    fn force_color_detect_never_returns_none() {
        // The Palette16 floor is the whole point of force_color_detect;
        // pin it directly so a regression dropping the fallback match
        // arm is visible without chasing through resolver assertions.
        assert_ne!(force_color_detect(), theme::Capability::None);
    }

    #[test]
    fn color_policy_force_color_env_zero_is_treated_as_absent() {
        // npm / chalk / supports-color all treat FORCE_COLOR=0 as
        // "explicitly disabled", not as force-on. Our `force_color_env`
        // parse maps it to false, so `env.force_color = false` and the
        // chain falls through to config / auto rather than forcing.
        // Verify the parser itself:
        std::env::set_var("LINESMITH_FORCE_COLOR_TEST_ZERO", "0");
        assert!(!force_color_env("LINESMITH_FORCE_COLOR_TEST_ZERO"));
        std::env::set_var("LINESMITH_FORCE_COLOR_TEST_ONE", "1");
        assert!(force_color_env("LINESMITH_FORCE_COLOR_TEST_ONE"));
        std::env::set_var("LINESMITH_FORCE_COLOR_TEST_EMPTY", "");
        assert!(!force_color_env("LINESMITH_FORCE_COLOR_TEST_EMPTY"));
        std::env::remove_var("LINESMITH_FORCE_COLOR_TEST_ZERO");
        std::env::remove_var("LINESMITH_FORCE_COLOR_TEST_ONE");
        std::env::remove_var("LINESMITH_FORCE_COLOR_TEST_EMPTY");
    }

    #[test]
    fn color_policy_test_capability_override_short_circuits_everything() {
        let env = CliEnv {
            no_color: true,
            force_color: true,
            color_capability: Some(theme::Capability::Palette256),
            ..policy_env()
        };
        assert_eq!(
            resolve_color_capability(Some(cli::ColorOverride::Never), &env, None),
            theme::Capability::Palette256,
        );
    }

    // --- claude_padding ---

    #[test]
    fn claude_padding_shrinks_render_budget_and_drops_segment() {
        // Full render: "Claude linesmith" = 16 cells. Budget 20 alone
        // fits everything; padding=10 shrinks usable to 10 cells, which
        // forces the higher-priority segment (model=64) to drop so only
        // workspace (priority 16, 9 cells) survives.
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[layout_options]\nclaude_padding = 10\n").unwrap();
        let env = CliEnv {
            terminal_width: Some(20),
            ..CliEnv::for_tests()
        };
        let (code, stdout, _stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "linesmith\n");
    }

    #[test]
    fn claude_padding_exceeds_width_clamps_to_zero_and_drops_everything() {
        // Pathological misconfiguration: padding larger than the
        // terminal width saturates to 0 usable cells, so every
        // positive-priority segment drops. Silent clamp is the right
        // semantic — validating this at config-load would require
        // width at parse time, which we don't have.
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[layout_options]\nclaude_padding = 500\n").unwrap();
        let env = CliEnv {
            terminal_width: Some(80),
            ..CliEnv::for_tests()
        };
        let (code, stdout, _stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        // All positive-priority segments drop; only the trailing
        // newline remains.
        assert_eq!(stdout, "\n");
    }

    #[test]
    fn claude_padding_zero_matches_no_padding() {
        // Explicit 0 should be indistinguishable from omitting the key.
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[layout_options]\nclaude_padding = 0\n").unwrap();
        let (code, stdout, _stderr) = run_cli_main(
            &["--config", path.to_str().unwrap()],
            json,
            &CliEnv::for_tests(),
        );
        assert_eq!(code, 0);
        assert_eq!(stdout, "Claude linesmith\n");
    }

    // --- CLI flag end-to-end ---

    #[test]
    fn no_color_flag_outranks_force_color_env_end_to_end() {
        // force_color=true would yield colored output via the resolver's
        // FORCE_COLOR branch; --no-color must outrank it, proving the
        // CLI flag sits at the top of the precedence chain through the
        // full render pipeline.
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/linesmith" }
        }"#;
        let env = CliEnv {
            force_color: true,
            color_capability: None,
            ..CliEnv::for_tests()
        };
        let (code, stdout, _stderr) = run_cli_main(&["--no-color"], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "Claude linesmith\n");
        assert!(!stdout.contains('\x1b'));
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
            "linesmith-driver-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
        ));
        std::fs::create_dir_all(&base).expect("mkdir");
        TempDir(base)
    }
}
