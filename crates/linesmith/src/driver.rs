//! CLI driver. `cli_main` takes args + stdin + env and returns an
//! exit code, handling arg parsing, config loading, `--check-config`,
//! and render dispatch. `main.rs` wires real IO with
//! `CliEnv::from_process`; tests pass `Cursor` / `Vec<u8>` buffers
//! and a hand-built `CliEnv`.

use crate::plugins::PluginRegistry;
use crate::segments::builder::build_lines;
use crate::{
    cli, config, detect_terminal_width, presets, run_lines_with_context, runtime, theme, RunContext,
};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

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
    /// Raw `COLORTERM`, or `None` if unset. Threaded to the
    /// force-color resolver so it can justify TrueColor when the TTY
    /// probe gives up (e.g. piped stdout under Claude Code).
    /// `from_process` snapshots the real env; `for_tests`/`default`
    /// leave it `None` so ambient `COLORTERM=truecolor` can't leak
    /// into captured-output tests.
    pub colorterm: Option<String>,
    /// Raw `TERM`, or `None` if unset. Same snapshot discipline as
    /// `colorterm`.
    pub term: Option<String>,
    pub terminal_width: Option<u16>,
    pub color_capability: Option<theme::Capability>,
    /// cwd used for gix repo discovery. `None` skips discovery
    /// entirely. [`Self::from_process`] sets this to
    /// `std::env::current_dir()`; [`Self::for_tests`] leaves it
    /// `None`.
    pub cwd: Option<std::path::PathBuf>,
    /// Raw `LINESMITH_LOG` value, or `None` if unset. `from_process`
    /// snapshots the real env; `for_tests`/`default` leave it `None`
    /// so a developer's ambient `LINESMITH_LOG=debug` can't pollute
    /// captured-stderr CLI tests.
    pub log_level_env: Option<String>,
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
            colorterm: std::env::var("COLORTERM").ok(),
            term: std::env::var("TERM").ok(),
            terminal_width: None,
            color_capability: None,
            cwd: std::env::current_dir().ok(),
            log_level_env: std::env::var(crate::logging::ENV_VAR).ok(),
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
            colorterm: None,
            term: None,
            terminal_width: Some(200),
            color_capability: Some(theme::Capability::None),
            cwd: None,
            log_level_env: None,
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
    crate::logging::apply(env.log_level_env.as_deref(), stderr);

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
        cli::Action::ThemesList => themes_list(stdout, stderr, env),
        cli::Action::PresetsList => presets_list(stdout),
        cli::Action::PresetsApply {
            name,
            force,
            config,
        } => presets_apply(&name, force, config, stdin, stdout, stderr, env),
        cli::Action::Init { config, no_doctor } => {
            init_action(config, no_doctor, stdin, stdout, stderr, env)
        }
        cli::Action::Doctor { plain, config } => doctor_action(plain, config, stdout, stderr),
        cli::Action::Run(args) => run_cli(args, stdin, stdout, stderr, env),
    }
}

/// Build a doctor report, render it, and return the report's exit
/// code. Stdout I/O failures are surfaced to stderr but do not
/// promote a healthy report to a non-zero exit, matching the
/// `cli_main` convention used by `themes-list`, `presets-list`, etc.:
/// partial / missing stdout is the user-visible signal that the
/// writer broke, not the report.
///
/// `config_override` is threaded into the [`DoctorEnv`] snapshot so
/// the Config category inspects the user-named path directly,
/// rather than the action layer warning about it after the fact.
fn doctor_action(
    plain: bool,
    config_override: Option<PathBuf>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let env = crate::doctor::DoctorEnv::from_process(config_override);
    doctor_action_with_env(env, plain, stdout, stderr)
}

/// Test seam for `doctor_action`. Tests construct a hermetic
/// `DoctorEnv` (via `DoctorEnv::healthy()` plus per-test mutation)
/// and call this directly so the doctor's view of the world is
/// controlled, not inherited from whatever ambient env the test
/// runner happens to provide. Production code never calls this
/// directly; it goes through `doctor_action`.
fn doctor_action_with_env(
    env: crate::doctor::DoctorEnv,
    plain: bool,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> u8 {
    let report = crate::doctor::build_report(&env);
    let mode = if plain {
        crate::doctor::RenderMode::Plain
    } else {
        crate::doctor::RenderMode::Default
    };
    if let Err(err) = crate::doctor::render(stdout, &report, mode) {
        let _ = writeln!(stderr, "linesmith: doctor: {err}");
    }
    report.exit_code()
}

fn themes_list(stdout: &mut dyn Write, stderr: &mut dyn Write, env: &CliEnv) -> u8 {
    let registry = build_theme_registry(env, stderr);
    for rt in registry.iter() {
        let source = match &rt.source {
            theme::ThemeSource::BuiltIn => "built-in".to_string(),
            theme::ThemeSource::UserFile(p) => p.display().to_string(),
            // ThemeSource is `#[non_exhaustive]` per ADR-0018; a
            // future variant in linesmith-core renders generically
            // until the cli adds an explicit label.
            _ => "unknown source".to_string(),
        };
        let _ = writeln!(stdout, "{}\t{}", rt.theme.name(), source);
    }
    0
}

fn presets_list(stdout: &mut dyn Write) -> u8 {
    for name in presets::names() {
        let _ = writeln!(stdout, "{name}");
    }
    0
}

/// Write a preset's body to the resolved config path. Handles
/// backup-on-overwrite, the `--force` short-circuit, and the y/N
/// confirmation prompt. Returns 0 on success, 1 on user-facing errors
/// (unknown preset, aborted overwrite, existing `.bak`, I/O failure,
/// unresolved path).
fn presets_apply(
    name: &str,
    force: bool,
    config_override: Option<PathBuf>,
    stdin: impl Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &CliEnv,
) -> u8 {
    let Some(body) = presets::body(name) else {
        let _ = writeln!(stderr, "linesmith: unknown preset '{name}'");
        let _ = writeln!(stderr, "available presets:");
        for known in presets::names() {
            let _ = writeln!(stderr, "  {known}");
        }
        return 1;
    };

    let Some(resolved) = resolve_writable_config_path(config_override, env, stderr) else {
        return 1;
    };
    let policy = OverwritePolicy::presets(force);
    let with_schema = config::with_schema_directive(body);
    if let Err(code) = write_config_with_backup(&with_schema, &resolved.path, policy, stdin, stderr)
    {
        return code;
    }
    let _ = writeln!(
        stdout,
        "wrote preset '{name}' to {}",
        resolved.path.display()
    );
    0
}

/// Resolve the target config path or emit the "set XDG_CONFIG_HOME or
/// HOME" diagnostic and return `None`. Shared by `presets_apply` and
/// the `init` flow so both treat an undiscoverable home identically.
fn resolve_writable_config_path(
    config_override: Option<PathBuf>,
    env: &CliEnv,
    stderr: &mut dyn Write,
) -> Option<config::ConfigPath> {
    match config::resolve_config_path(
        config_override,
        env.linesmith_config.as_deref(),
        env.xdg_config_home.as_deref(),
        env.home.as_deref(),
    ) {
        Some(p) => Some(p),
        None => {
            let _ = writeln!(
                stderr,
                "linesmith: cannot resolve a config path (set XDG_CONFIG_HOME or HOME)"
            );
            None
        }
    }
}

/// Two-bit policy for [`write_config_with_backup`]. `presets apply`
/// maps `--force` to both bits at once; `init` separates them so a
/// user who confirmed `y` to the overwrite prompt isn't then blocked
/// by a leftover `.bak` from a prior run. Use
/// [`OverwritePolicy::presets`] or [`OverwritePolicy::init`] so the
/// bit-mapping doesn't drift between call sites.
#[derive(Debug, Clone, Copy)]
struct OverwritePolicy {
    /// Skip the y/N prompt entirely. `presets apply --force` sets
    /// this; `init` always leaves it `false` so the user explicitly
    /// confirms.
    skip_prompt: bool,
    /// When a `.bak` already exists alongside `path`, replace it
    /// instead of refusing. `presets apply --force` sets this; `init`
    /// always sets it because the y/N prompt already captured the
    /// user's intent to lose the previous content.
    clobber_backup: bool,
}

impl OverwritePolicy {
    /// `presets apply [--force]` policy: prompt + refuse `.bak` by
    /// default, both elided when `--force`.
    fn presets(force: bool) -> Self {
        Self {
            skip_prompt: force,
            clobber_backup: force,
        }
    }

    /// `init` policy: always prompt, always clobber `.bak` after a
    /// confirmed `y`. The prompt captured the user's intent to lose
    /// the previous content, so a leftover `.bak` from a prior run
    /// shouldn't block the overwrite.
    fn init() -> Self {
        Self {
            skip_prompt: false,
            clobber_backup: true,
        }
    }
}

/// Write `body` to `path`, backing up any prior file. If `path` exists,
/// honor `policy` (prompt or skip; refuse or clobber an existing
/// `.bak`), rename to `path.bak`, then write. If `path` doesn't
/// exist, create parent directories and write. Returns `Ok(true)`
/// when a backup was made, `Ok(false)` otherwise; `Err(1)` on
/// aborted overwrite, refused `.bak`, or I/O failure.
///
/// Shared between `presets_apply` and `init` so both paths use the
/// same prompt phrasing and `.bak` filename. TOCTOU between
/// `exists()` and `rename`/`write` is the downstream call's problem:
/// a vanished file surfaces its own `fs::*` error and we return 1.
fn write_config_with_backup(
    body: &str,
    path: &Path,
    policy: OverwritePolicy,
    stdin: impl Read,
    stderr: &mut dyn Write,
) -> Result<bool, u8> {
    let backup = path.with_extension("toml.bak");
    let mut backup_written = false;

    if path.exists() {
        if !policy.skip_prompt && !confirm_overwrite(path, stdin, stderr) {
            let _ = writeln!(stderr, "linesmith: aborted; config.toml unchanged");
            return Err(1);
        }
        // Refuse to clobber an existing backup unless the policy
        // allows it. `presets apply` defaults to refusing (two
        // generations preserved); `presets apply --force` and `init`
        // (after a confirmed y/N) both clobber.
        let mut clobbered_prior_bak = false;
        if backup.exists() {
            if !policy.clobber_backup {
                let _ = writeln!(
                    stderr,
                    "linesmith: {} already exists; rerun with --force to replace it",
                    backup.display()
                );
                return Err(1);
            }
            // Windows' `fs::rename` fails when the destination exists;
            // pre-remove so the clobber path works the same on every
            // platform.
            if let Err(e) = std::fs::remove_file(&backup) {
                let _ = writeln!(
                    stderr,
                    "linesmith: could not remove existing backup {}: {e}",
                    backup.display()
                );
                return Err(1);
            }
            clobbered_prior_bak = true;
        }
        if let Err(e) = std::fs::rename(path, &backup) {
            let _ = writeln!(
                stderr,
                "linesmith: could not back up {} to {}: {e}",
                path.display(),
                backup.display()
            );
            // Surface that the prior-generation `.bak` was already
            // removed before the rename was attempted. Without this
            // breadcrumb, a user seeing only "could not back up"
            // would assume nothing changed and miss that a
            // recoverable backup was just consumed.
            if clobbered_prior_bak {
                let _ = writeln!(
                    stderr,
                    "linesmith: note: the previous {} was removed just before this failed rename; it is gone",
                    backup.display()
                );
            }
            return Err(1);
        }
        let _ = writeln!(
            stderr,
            "linesmith: backed up previous config to {}",
            backup.display()
        );
        backup_written = true;
    } else if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            let _ = writeln!(
                stderr,
                "linesmith: could not create {}: {e}",
                parent.display()
            );
            return Err(1);
        }
    }

    if let Err(e) = std::fs::write(path, body) {
        let _ = writeln!(stderr, "linesmith: write {}: {e}", path.display());
        if backup_written {
            let _ = writeln!(
                stderr,
                "linesmith: your previous config is preserved at {}",
                backup.display()
            );
        }
        return Err(1);
    }
    Ok(backup_written)
}

/// Prompt once on stderr; accept `y` / `yes` (case-insensitive) as yes,
/// everything else as no. EOF is treated as "no" so non-interactive
/// callers abort rather than overwrite.
fn confirm_overwrite(path: &Path, stdin: impl Read, stderr: &mut dyn Write) -> bool {
    let _ = write!(stderr, "overwrite {}? [y/N] ", path.display());
    let _ = stderr.flush();
    let mut line = String::new();
    let mut reader = BufReader::new(stdin);
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    parse_confirmation(&line)
}

/// Treat `y` / `yes` (case-insensitive, surrounding whitespace allowed)
/// as confirmation. Everything else, including empty input, is rejected.
fn parse_confirmation(input: &str) -> bool {
    matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// User-supplied choices for `linesmith init`. The dialoguer wrapper
/// builds these at the CLI; tests construct them directly so the
/// file-writing core can run without a TTY.
#[derive(Debug, Clone)]
struct InitChoices {
    preset: String,
    theme: String,
}

/// Gather choices via dialoguer, then dispatch to [`init_with_choices`].
/// The split keeps the file-writing + snippet logic testable without a
/// fake TTY. Also translates dialoguer's I/O errors (TTY missing,
/// stdin closed mid-prompt, broken pipe, etc.) into a printed
/// diagnostic plus a hint pointing at `presets apply` for
/// non-interactive use.
fn init_action(
    config_override: Option<PathBuf>,
    no_doctor: bool,
    stdin: impl Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &CliEnv,
) -> u8 {
    let preset_names: Vec<&'static str> = presets::names().collect();
    let registry = build_theme_registry(env, stderr);
    let theme_names: Vec<String> = registry
        .iter()
        .map(|t| t.theme.name().to_string())
        .collect();

    let choices = match prompt_for_init_choices(&preset_names, &theme_names) {
        Ok(c) => c,
        Err(err) => {
            // dialoguer collapses TTY-missing, EOF-mid-prompt, broken
            // pipe, and EINTR into a single `Error::IO` variant, so
            // we can't reliably distinguish them. Print the
            // underlying error verbatim and offer the no-tty fallback
            // as conditional advice rather than a diagnosis.
            let _ = writeln!(stderr, "linesmith: init: {err}");
            let _ = writeln!(
                stderr,
                "linesmith: if a terminal isn't attached, use 'linesmith presets apply <name>' instead",
            );
            return 1;
        }
    };

    init_with_choices(
        &choices,
        no_doctor,
        config_override,
        stdin,
        stdout,
        stderr,
        env,
    )
}

/// Drive the two interactive Select prompts. Defaults to index 0 in
/// each list so pressing Enter through both prompts produces a usable
/// config; the caller controls what index 0 means (today: preset =
/// `minimal`, theme = `default`).
///
/// `docs/specs/config.md:285` and lsm-5yd both list a third "tool"
/// prompt (claude-code / qwen-code / auto). Skipped here for v0.1:
/// `Config` has no `tool` field, only Claude Code has a real install
/// path, and prompting for an option that does nothing is worse UX
/// than not prompting at all. Revisit when qwen-code support gives
/// the choice a real consequence.
fn prompt_for_init_choices(
    presets: &[&str],
    themes: &[String],
) -> Result<InitChoices, dialoguer::Error> {
    use dialoguer::{theme::ColorfulTheme, Select};
    let theme = ColorfulTheme::default();
    let preset_idx = Select::with_theme(&theme)
        .with_prompt("preset")
        .items(presets)
        .default(0)
        .interact()?;
    let theme_idx = Select::with_theme(&theme)
        .with_prompt("theme")
        .items(themes)
        .default(0)
        .interact()?;
    Ok(InitChoices {
        preset: presets[preset_idx].to_string(),
        theme: themes[theme_idx].clone(),
    })
}

/// Testable core of `linesmith init`: write the chosen preset (with the
/// chosen theme injected) to the resolved config path, then print the
/// Claude Code `settings.json` snippet for copy-paste. Returns 0 on
/// success, 1 on user-facing errors.
fn init_with_choices(
    choices: &InitChoices,
    no_doctor: bool,
    config_override: Option<PathBuf>,
    stdin: impl Read,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
    env: &CliEnv,
) -> u8 {
    let Some(body) = presets::body(&choices.preset) else {
        // Defense-in-depth: prompts only offer registered presets, but a
        // hand-built `InitChoices` from a test could pass an unknown
        // name.
        let _ = writeln!(stderr, "linesmith: unknown preset '{}'", choices.preset);
        return 1;
    };
    let composed = config::with_schema_directive(&compose_init_body(body, &choices.theme));

    let Some(resolved) = resolve_writable_config_path(config_override, env, stderr) else {
        return 1;
    };
    let policy = OverwritePolicy::init();
    if let Err(code) = write_config_with_backup(&composed, &resolved.path, policy, stdin, stderr) {
        return code;
    }

    // Snippet must point at the file we just wrote. If the user
    // resolved the path explicitly (via `--config` OR `LINESMITH_CONFIG`),
    // bare `linesmith` would re-resolve to the XDG default and miss
    // it. The `explicit` bit lets us emit the right `--config <path>`
    // for both branches.
    let snippet_path = resolved.explicit.then(|| resolved.path.clone());
    let _ = writeln!(stdout, "wrote config.toml to {}", resolved.path.display());
    warn_if_path_needs_user_edit(snippet_path.as_deref(), stderr);
    let _ = writeln!(stdout);
    let _ = writeln!(stdout, "Add this to your Claude Code settings.json:");
    let _ = writeln!(stdout, "(merge into the existing top-level object)");
    let _ = writeln!(stdout);
    let _ = writeln!(stdout, "  \"statusLine\": {{");
    let _ = writeln!(stdout, "    \"type\": \"command\",");
    let _ = writeln!(stdout, "    \"command\": {},", json_command(&snippet_path));
    let _ = writeln!(stdout, "    \"padding\": 0");
    let _ = writeln!(stdout, "  }}");

    if no_doctor {
        return 0;
    }

    // Spec §`linesmith init` integration: post-init doctor
    // invocation. Init's exit code propagates from doctor —
    // a doctor FAIL after a clean init means the env around
    // the new config is still broken (Claude Code missing,
    // credentials unresolvable, etc.) and the user should
    // know before they try `linesmith` for real. Pass the
    // resolved path through so doctor inspects exactly the
    // file we just wrote, not whatever its own cascade might
    // re-resolve to (which can differ when --config or
    // $LINESMITH_CONFIG were involved).
    let _ = writeln!(stdout);
    let _ = writeln!(stdout, "Running doctor to verify your setup...");
    let _ = writeln!(stdout);
    doctor_action(false, Some(resolved.path), stdout, stderr)
}

/// JSON-encode the `command` string for the Claude Code snippet emitted
/// by `init_with_choices`. When the user resolved an explicit path (via
/// `--config` or `LINESMITH_CONFIG`), Claude Code must call
/// `linesmith --config <PATH>` to read the file we just wrote;
/// otherwise plain `linesmith` finds the XDG default. Only JSON
/// escaping happens here (backslashes + double quotes); shell quoting
/// is the caller's problem because POSIX and Windows shells disagree
/// on the rules and any wrapping we do would be wrong somewhere.
/// [`warn_if_path_needs_user_edit`] flags the cases that need
/// hand-quoting before this is invoked.
fn json_command(explicit_path: &Option<PathBuf>) -> String {
    let escape = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    match explicit_path {
        Some(p) => format!("\"linesmith --config {}\"", escape(&p.to_string_lossy())),
        None => "\"linesmith\"".to_string(),
    }
}

/// Warn on stderr when the `--config` path will produce a snippet that
/// can't be pasted as-is. Two failure modes:
///
/// - Non-UTF-8 bytes: `to_string_lossy` substitutes `U+FFFD`, so the
///   emitted path is garbled and Claude Code's exec fails at runtime
///   with no link back to init.
/// - Shell metacharacters or whitespace: Claude Code tokenizes
///   `command` as a shell argv, so `linesmith --config /a b/c.toml`
///   becomes four tokens. The snippet is valid JSON but exec won't
///   find the config.
///
/// Init's exit code stays 0 either way (the file was written); the
/// warning tells the user to hand-edit before pasting.
fn warn_if_path_needs_user_edit(explicit_path: Option<&Path>, stderr: &mut dyn Write) {
    let Some(p) = explicit_path else { return };
    let s = p.to_str();
    let non_utf8 = s.is_none();
    let leading_dash = s.is_some_and(|s| s.starts_with('-'));
    let needs_quoting = s.is_some_and(|s| s.chars().any(needs_shell_quoting));
    if non_utf8 {
        let _ = writeln!(
            stderr,
            "linesmith: warning: --config path contains non-UTF-8 bytes; the emitted snippet uses Unicode replacement characters and won't work as-is — hand-edit before pasting"
        );
    } else if leading_dash {
        // Shell tokenizes the snippet's `command` as argv, so a path
        // starting with `-` lands as a flag at re-invocation. lexopt
        // rejects the unknown flag and the statusline silently fails
        // every Claude Code tick.
        let _ = writeln!(
            stderr,
            "linesmith: warning: --config path starts with `-` ({}); the emitted snippet would be parsed as a flag at re-invocation. Hand-edit the snippet's `command` field before pasting: prefix the path with `./`, use an absolute path, or insert `--` before it",
            p.display()
        );
    } else if needs_quoting {
        let _ = writeln!(
            stderr,
            "linesmith: warning: --config path contains characters that need shell quoting ({}); add quotes around the path in the snippet's `command` field before pasting",
            p.display()
        );
    }
}

/// Characters that change shell tokenization away from "one
/// whitespace-free token." Conservative: the goal is flagging paths
/// the user would otherwise paste and find broken at exec time, not
/// reproducing every shell's full quoting grammar. The arm below
/// covers common POSIX metacharacters plus a useful subset of
/// cmd.exe / PowerShell specials (including cmd.exe's `^` escape and
/// `%` variable expansion). Position-sensitive cases like a leading
/// `-` are checked separately in [`warn_if_path_needs_user_edit`].
///
/// `\` and `~` are POSIX-only: `\` is the shell escape and `~` is
/// home-directory expansion. On Windows they're literal (`\` is the
/// path separator, `~` shows up in 8.3 short names like `RUNNER~1`),
/// so flagging them there would warn on every Windows tempdir path.
fn needs_shell_quoting(c: char) -> bool {
    if matches!(
        c,
        ' ' | '\t'
            | '\''
            | '"'
            | '`'
            | '$'
            | '*'
            | '?'
            | '['
            | ']'
            | '&'
            | ';'
            | '|'
            | '<'
            | '>'
            | '('
            | ')'
            | '{'
            | '}'
            | '#'
            | '!'
            | '='
            | '^'
            | '%'
    ) {
        return true;
    }
    #[cfg(not(windows))]
    {
        matches!(c, '\\' | '~')
    }
    #[cfg(windows)]
    {
        false
    }
}

/// Inject `theme = "<name>"` into a preset body before the first table
/// header. `default` is the implicit choice and skipped, so `init`
/// with the default theme writes the same bytes as `presets apply
/// <name>`. See
/// `init_default_theme_omits_theme_field_to_match_presets_apply` for
/// the regression guard: pinning byte equality on the common path
/// makes the rare-path escape bug obvious.
fn compose_init_body(preset_body: &str, theme: &str) -> String {
    if theme == "default" {
        return preset_body.to_string();
    }
    let escaped = theme.replace('\\', "\\\\").replace('"', "\\\"");
    let theme_line = format!("theme = \"{escaped}\"\n\n");
    // A preset starting with `[` at byte 0 has no preceding newline,
    // so `find("\n[")` would miss it. Treat both shapes as "table
    // header at the top" and prepend before it.
    if preset_body.starts_with('[') {
        return format!("{theme_line}{preset_body}");
    }
    if let Some(idx) = preset_body.find("\n[") {
        // Keep the preset's leading comment block above the injected
        // `theme = ...` line.
        let split = idx + 1;
        let mut out = String::with_capacity(preset_body.len() + theme_line.len());
        out.push_str(&preset_body[..split]);
        out.push_str(&theme_line);
        out.push_str(&preset_body[split..]);
        out
    } else {
        // Degenerate preset with no tables: prepend so `theme` still
        // appears at top-level position.
        format!("{theme_line}{preset_body}")
    }
}

/// CLI-side wrapper around [`runtime::plugins::load_plugins`]:
/// builds the runtime `XdgEnv` from `CliEnv`, calls the predicate,
/// streams plugin-load errors to stderr, and returns the registry
/// plus the error count so `check_config` can fold the count into
/// its summary without re-parsing the stderr stream.
fn load_plugins(
    cfg: Option<&config::Config>,
    env: &CliEnv,
    stderr: &mut dyn Write,
) -> (
    Option<(PluginRegistry, std::sync::Arc<rhai::Engine>)>,
    usize,
) {
    let xdg_env = cli_env_to_xdg(env);
    let Some((registry, engine)) = runtime::plugins::load_plugins(cfg, &xdg_env) else {
        return (None, 0);
    };
    let errors = registry.load_errors();
    let error_count = errors.len();
    for err in errors {
        let _ = writeln!(stderr, "linesmith: plugin: {err}");
    }
    (Some((registry, engine)), error_count)
}

/// Build the runtime's `XdgEnv` snapshot from this caller's
/// [`CliEnv`]. Centralized so every runtime predicate that needs
/// XDG inputs gets the same conversion, and so `linesmith-cli`
/// keeps owning the `CliEnv → XdgEnv` map after the workspace
/// split (per ADR-0018).
fn cli_env_to_xdg(env: &CliEnv) -> crate::data_context::xdg::XdgEnv {
    crate::data_context::xdg::XdgEnv::from_os_options(
        None,
        env.xdg_config_home.clone().map(std::ffi::OsString::from),
        env.home.clone().map(std::ffi::OsString::from),
    )
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

    let registry = build_theme_registry(env, stderr);

    if args.check_config {
        return check_config(
            resolved.as_ref(),
            cfg.as_ref(),
            load_error,
            config_warnings,
            &registry,
            env,
            stderr,
        );
    }

    // Surface unknown-key warnings before rendering so they ride on the
    // same stderr stream as segment-build warnings and parse errors.
    for msg in &config_warnings {
        let _ = writeln!(stderr, "linesmith: {msg}");
    }

    let (plugins, _plugin_load_errors) = load_plugins(cfg.as_ref(), env, stderr);
    let lines = build_lines(cfg.as_ref(), plugins, |msg| {
        let _ = writeln!(stderr, "linesmith: {msg}");
    });
    // `run_lines_with_context` takes `&[&[Box<dyn Segment>]]`; map
    // each owned `Vec` to a borrowed slice once.
    let line_refs: Vec<&[Box<dyn crate::segments::Segment>]> =
        lines.iter().map(Vec::as_slice).collect();

    let raw_width = env.terminal_width.unwrap_or_else(detect_terminal_width);
    let padding = layout_options(cfg.as_ref()).map_or(0, |l| l.claude_padding);
    let width = raw_width.saturating_sub(padding);
    let theme_ref = resolve_theme(cfg.as_ref(), &registry, stderr);
    let capability = resolve_color_capability(args.color_override, env, cfg.as_ref());
    let hyperlinks = supports_hyperlinks::on(supports_hyperlinks::Stream::Stdout);
    let ctx = RunContext::new(theme_ref, capability, width, env.cwd.clone(), hyperlinks);
    if let Err(err) = run_lines_with_context(stdin, stdout, stderr, &line_refs, &ctx) {
        let _ = writeln!(stderr, "linesmith: {err}");
        return 1;
    }
    0
}

fn build_theme_registry(env: &CliEnv, stderr: &mut dyn Write) -> theme::ThemeRegistry {
    let xdg_env = cli_env_to_xdg(env);
    let dir = runtime::themes::user_themes_dir(&xdg_env);
    runtime::themes::build_theme_registry(dir.as_deref(), |msg| {
        let _ = writeln!(stderr, "linesmith: {msg}");
    })
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
        Some(cli::ColorOverride::Always) => return force_color_detect(env),
        None => {}
    }
    if env.no_color {
        return theme::Capability::None;
    }
    if env.force_color {
        return force_color_detect(env);
    }
    match layout_options(cfg).map(|l| l.color).unwrap_or_default() {
        config::ColorPolicy::Never => theme::Capability::None,
        config::ColorPolicy::Always => force_color_detect(env),
        // Auto plus any future variant routes to terminal detection.
        // ColorPolicy is `#[non_exhaustive]` per ADR-0018, so the
        // catch-all also handles unknown variants — auto-detect is
        // the safe degradation while the cli wires explicit handling.
        _ => theme::Capability::from_terminal(),
    }
}

/// Under "force color" intent, pick the richest tier either the
/// `supports-color` TTY probe or the `COLORTERM` / `TERM` snapshot
/// on `CliEnv` can justify. See `theme::Capability::force_from` for
/// the full semantics (including the `Palette16` floor that overrides
/// `TERM=dumb` when the user explicitly asks for color).
///
/// The env branch is load-bearing for Claude Code: CC spawns linesmith
/// with piped stdout, so `supports-color` returns `None`; without the
/// env rescue, `color = "always"` would collapse to `Palette16` and
/// every TrueColor theme would render as its 16-color downgrade.
///
/// Reads env only from `CliEnv`, never from the live process, so
/// tests and embedders stay hermetic.
fn force_color_detect(env: &CliEnv) -> theme::Capability {
    theme::Capability::force_from(
        theme::Capability::from_terminal(),
        theme::Capability::from_env_vars(env.colorterm.as_deref(), env.term.as_deref()),
    )
}

/// Resolve the active theme from config. Unknown names fall back to
/// `default` with a stderr warning; missing or empty `theme` uses
/// the default silently.
fn resolve_theme<'a>(
    cfg: Option<&config::Config>,
    registry: &'a theme::ThemeRegistry,
    stderr: &mut dyn Write,
) -> &'a theme::Theme {
    let Some(name) = cfg
        .and_then(|c| c.theme.as_deref())
        .filter(|n| !n.is_empty())
    else {
        return registry
            .lookup("default")
            .expect("default theme is always in the registry");
    };
    match registry.lookup(name) {
        Some(t) => t,
        None => {
            let _ = writeln!(stderr, "linesmith: unknown theme '{name}'; using 'default'");
            registry
                .lookup("default")
                .expect("default theme is always in the registry")
        }
    }
}

/// `--check-config`-friendly wrapper around
/// [`runtime::config::load_config`]. Writes user-facing diagnostics
/// (missing-file for explicit paths, parse / I/O errors) to stderr
/// and returns the existing 3-tuple shape `run_cli` and
/// `check_config` consume.
fn load_config(
    resolved: Option<&config::ConfigPath>,
    stderr: &mut dyn Write,
) -> (
    Option<config::Config>,
    Option<config::ConfigError>,
    Vec<String>,
) {
    use runtime::config::ConfigLoadOutcome;
    match runtime::config::load_config(resolved) {
        ConfigLoadOutcome::Unresolved => (None, None, Vec::new()),
        ConfigLoadOutcome::Loaded {
            config, warnings, ..
        } => (Some(*config), None, warnings),
        ConfigLoadOutcome::NotFound { path, explicit } => {
            if explicit {
                let _ = writeln!(stderr, "linesmith: config not found at {}", path.display());
            }
            (None, None, Vec::new())
        }
        ConfigLoadOutcome::IoError {
            source, warnings, ..
        }
        | ConfigLoadOutcome::ParseError {
            source, warnings, ..
        } => {
            let _ = writeln!(stderr, "linesmith: {source}");
            (None, Some(source), warnings)
        }
        // ConfigLoadOutcome is `#[non_exhaustive]` per ADR-0018. A
        // new variant in linesmith-core without an updated mapping
        // here falls back to defaults so the CLI still renders a
        // status line — but write a stderr line so the cli/core
        // version skew is visible instead of silently producing
        // default-themed output the user can't explain.
        _ => {
            let _ = writeln!(
                stderr,
                "linesmith: unrecognized config load outcome (cli/core version skew); using defaults",
            );
            (None, None, Vec::new())
        }
    }
}

fn check_config(
    resolved: Option<&config::ConfigPath>,
    cfg: Option<&config::Config>,
    load_error: Option<config::ConfigError>,
    config_warnings: Vec<String>,
    registry: &theme::ThemeRegistry,
    env: &CliEnv,
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
    let (plugins, plugin_load_errors) = load_plugins(Some(cfg), env, stderr);
    warn_count += plugin_load_errors;
    // `build_lines` (not `build_segments`) so multi-line edge-case
    // warnings — `[line.foo]` non-numeric keys, single-line-with-
    // numbered-tables mode-mismatch, etc. — surface through
    // `--check-config` the same way single-line validation does.
    let _ = build_lines(Some(cfg), plugins, |msg| {
        let _ = writeln!(stderr, "linesmith: {msg}");
        warn_count += 1;
    });
    if let Some(name) = cfg.theme.as_deref().filter(|n| !n.is_empty()) {
        if registry.lookup(name).is_none() {
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
    fn multi_line_config_renders_one_writeln_per_line() {
        // End-to-end multi-line: config declares two [line.N]
        // sub-tables; stdout must carry two newline-separated rows
        // with each line's segments resolved independently. Pin the
        // exact bytes (not just newline count) so a regression that
        // crosses the per-line boundary surfaces.
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/proj" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
                layout = "multi-line"
                [line.1]
                segments = ["model"]
                [line.2]
                segments = ["workspace"]
            "#,
        )
        .unwrap();
        let env = CliEnv::for_tests();
        let (code, stdout, stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0, "stderr:\n{stderr}");
        assert_eq!(stdout, "Claude\nproj\n");
    }

    #[test]
    fn multi_line_config_renders_lines_in_parsed_integer_order() {
        // BTreeMap key order is lexicographic on strings; the builder
        // sorts numerically. Pin the through-the-driver behavior so
        // a regression in either layer surfaces here.
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/proj" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
                layout = "multi-line"
                [line.10]
                segments = ["workspace"]
                [line.2]
                segments = ["model"]
            "#,
        )
        .unwrap();
        let env = CliEnv::for_tests();
        let (code, stdout, _stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "Claude\nproj\n");
    }

    #[test]
    fn multi_line_with_no_numbered_tables_falls_back_to_single_line_render() {
        // Spec edge case: the fallback warning surfaces on stderr
        // and rendering uses [line].segments, producing one stdout
        // line (not zero, not two).
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/x" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
                layout = "multi-line"
                [line]
                segments = ["model"]
            "#,
        )
        .unwrap();
        let env = CliEnv::for_tests();
        let (code, stdout, stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "Claude\n", "expected exactly one line");
        assert!(
            stderr.contains("no usable [line.N]"),
            "fallback warning should reach stderr, got:\n{stderr}"
        );
    }

    #[test]
    fn check_config_surfaces_multi_line_validation_warnings() {
        // `--check-config` consumes `build_lines` (not just
        // `build_segments`) so multi-line edge-case warnings
        // ([line.foo], single-line+numbered, etc.) flow through the
        // editor-facing validator, not just the render path.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
                layout = "multi-line"
                [line.1]
                segments = ["model"]
                [line.foo]
                segments = ["bogus"]
            "#,
        )
        .unwrap();
        let env = CliEnv::for_tests();
        let (_code, _stdout, stderr) = run_cli_main(
            &["--config", path.to_str().unwrap(), "--check-config"],
            b"",
            &env,
        );
        assert!(
            stderr.contains("[line.foo]") && stderr.contains("not a positive integer"),
            "non-numeric-key warning should reach --check-config, got:\n{stderr}"
        );
    }

    #[test]
    fn multi_line_parse_failure_emits_one_question_marker_not_per_line() {
        // The render loop iterates per line, but parse failure
        // returns before the loop runs and emits a single `?\n`.
        // Without this pin, a refactor that moves the parse into the
        // loop would silently spam `?\n?\n?\n` for an N-line config.
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
                layout = "multi-line"
                [line.1]
                segments = ["model"]
                [line.2]
                segments = ["workspace"]
                [line.3]
                segments = ["context_window"]
            "#,
        )
        .unwrap();
        let env = CliEnv::for_tests();
        let (code, stdout, stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], b"{not json", &env);
        assert_eq!(code, 0, "parse failure renders the marker, exits 0");
        assert_eq!(stdout, "?\n", "exactly one marker, not one per line");
        assert!(
            stderr.contains("parse:"),
            "parse error should breadcrumb to stderr, got:\n{stderr}"
        );
    }

    #[test]
    fn multi_line_empty_segments_in_a_line_still_emits_trailing_newline() {
        // The doc on `run_lines_with_context` says explicitly-defined
        // line slots stay in the output even if no segments rendered.
        // Pin the byte count so a refactor that "optimizes" away the
        // empty-line writeln doesn't silently change vertical
        // footprint.
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/x" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            r#"
                layout = "multi-line"
                [line.1]
                segments = ["model"]
                [line.2]
                segments = []
            "#,
        )
        .unwrap();
        let env = CliEnv::for_tests();
        let (code, stdout, _stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        assert_eq!(
            stdout, "Claude\n\n",
            "empty line slot must still emit `\\n`"
        );
    }

    #[test]
    fn power_user_preset_renders_two_lines_end_to_end() {
        // The preset-shape test in presets/mod.rs pins layout +
        // per-line ids; this one pins the full pipeline (preset →
        // builder → renderer) actually produces two lines. Catches
        // a regression where a refactor breaks any layer in the
        // chain.
        let json = br#"{
            "model": { "display_name": "Claude" },
            "workspace": { "project_dir": "/home/dev/proj" }
        }"#;
        let dir = tempdir();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            presets::body("power-user").expect("preset registered"),
        )
        .unwrap();
        let env = CliEnv::for_tests();
        let (code, stdout, _stderr) =
            run_cli_main(&["--config", path.to_str().unwrap()], json, &env);
        assert_eq!(code, 0);
        let lines: Vec<&str> = stdout.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "power-user must render exactly two lines, got: {stdout:?}"
        );
        assert!(
            lines[0].contains("Claude"),
            "line 1 should carry model name, got: {:?}",
            lines[0]
        );
        assert!(
            lines[0].contains("proj"),
            "line 1 should carry workspace name, got: {:?}",
            lines[0]
        );
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

    // --- user theme loading ---

    #[test]
    fn user_theme_from_disk_renders_with_configured_palette() {
        let dir = tempdir();
        let themes_dir = dir.path().join(".config/linesmith/themes");
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(
            themes_dir.join("neon.toml"),
            r##"
                name = "neon"
                [roles]
                foreground = "#ffffff"
                background = "#000000"
                muted      = "#888888"
                primary    = "#ff00ff"
                accent     = "#00ffff"
                success    = "#00ff00"
                warning    = "#ffff00"
                error      = "#ff0000"
                info       = "#0080ff"
            "##,
        )
        .unwrap();

        let cfg_dir = dir.path().join(".config/linesmith");
        std::fs::write(cfg_dir.join("config.toml"), "theme = \"neon\"\n").unwrap();

        let json = br#"{
            "model": { "display_name": "C" },
            "workspace": { "project_dir": "/x" }
        }"#;
        let env = CliEnv {
            home: Some(dir.path().to_string_lossy().into_owned()),
            color_capability: Some(theme::Capability::TrueColor),
            ..CliEnv::for_tests()
        };
        let (code, stdout, _stderr) = run_cli_main(&[], json, &env);
        assert_eq!(code, 0);
        // Primary (#ff00ff = 255,0,255) wraps model; Info (#0080ff =
        // 0,128,255) wraps workspace.
        assert_eq!(
            stdout,
            "\x1b[38;2;255;0;255mC\x1b[0m \x1b[38;2;0;128;255mx\x1b[0m\n"
        );
    }

    #[test]
    fn unknown_user_theme_name_falls_back_to_default_with_warning() {
        let dir = tempdir();
        std::fs::create_dir_all(dir.path().join(".config/linesmith/themes")).unwrap();
        let cfg_dir = dir.path().join(".config/linesmith");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(cfg_dir.join("config.toml"), "theme = \"nonexistent\"\n").unwrap();
        let env = CliEnv {
            home: Some(dir.path().to_string_lossy().into_owned()),
            ..CliEnv::for_tests()
        };
        let json = br#"{
            "model": { "display_name": "C" },
            "workspace": { "project_dir": "/x" }
        }"#;
        let (code, stdout, stderr) = run_cli_main(&[], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "C x\n");
        assert!(stderr.contains("unknown theme 'nonexistent'"));
    }

    #[test]
    fn broken_user_theme_file_warns_but_does_not_abort_startup() {
        let dir = tempdir();
        let themes_dir = dir.path().join(".config/linesmith/themes");
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(themes_dir.join("broken.toml"), "not valid toml [[").unwrap();
        let env = CliEnv {
            home: Some(dir.path().to_string_lossy().into_owned()),
            ..CliEnv::for_tests()
        };
        let json = br#"{
            "model": { "display_name": "C" },
            "workspace": { "project_dir": "/x" }
        }"#;
        let (code, stdout, stderr) = run_cli_main(&[], json, &env);
        assert_eq!(code, 0);
        assert_eq!(stdout, "C x\n");
        assert!(stderr.contains("broken.toml"));
    }

    #[test]
    fn check_config_accepts_user_theme_name() {
        // Regression guard: validation used to consult only built-ins,
        // so a `theme = "myuser"` that exists on disk was flagged.
        let dir = tempdir();
        let themes_dir = dir.path().join(".config/linesmith/themes");
        std::fs::create_dir_all(&themes_dir).unwrap();
        std::fs::write(
            themes_dir.join("myuser.toml"),
            r##"
                name = "myuser"
                [roles]
                foreground = "#ffffff"
                background = "#000000"
                muted      = "#888888"
                primary    = "#ff00ff"
                accent     = "#00ffff"
                success    = "#00ff00"
                warning    = "#ffff00"
                error      = "#ff0000"
                info       = "#0080ff"
            "##,
        )
        .unwrap();
        let cfg_dir = dir.path().join(".config/linesmith");
        std::fs::write(cfg_dir.join("config.toml"), "theme = \"myuser\"\n").unwrap();
        let env = CliEnv {
            home: Some(dir.path().to_string_lossy().into_owned()),
            ..CliEnv::for_tests()
        };
        let (code, _stdout, stderr) = run_cli_main(&["--check-config"], b"", &env);
        assert_eq!(code, 0);
        assert!(stderr.contains("config ok"));
        assert!(!stderr.contains("unknown theme"));
    }

    // --- themes list subcommand ---

    #[test]
    fn themes_list_prints_every_built_in_to_stdout() {
        let (code, stdout, _stderr) = run_cli_main(&["themes", "list"], b"", &CliEnv::for_tests());
        assert_eq!(code, 0);
        for name in [
            "default",
            "minimal",
            "catppuccin-latte",
            "catppuccin-frappe",
            "catppuccin-macchiato",
            "catppuccin-mocha",
        ] {
            assert!(
                stdout.contains(&format!("{name}\tbuilt-in")),
                "missing '{name}\\tbuilt-in' in:\n{stdout}"
            );
        }
    }

    #[test]
    fn themes_list_includes_user_themes_with_source_path() {
        let dir = tempdir();
        // Separator-normalized join to match the XDG cascade's
        // chained `.join` form on Windows — substring match against
        // cascade-rendered output requires identical separators.
        let themes_dir = dir.path().join(".config").join("linesmith").join("themes");
        std::fs::create_dir_all(&themes_dir).unwrap();
        let user_theme = themes_dir.join("neon.toml");
        std::fs::write(
            &user_theme,
            r##"
                name = "neon"
                [roles]
                foreground = "#ffffff"
                background = "#000000"
                muted      = "#888888"
                primary    = "#ff00ff"
                accent     = "#00ffff"
                success    = "#00ff00"
                warning    = "#ffff00"
                error      = "#ff0000"
                info       = "#0080ff"
            "##,
        )
        .unwrap();
        let env = CliEnv {
            home: Some(dir.path().to_string_lossy().into_owned()),
            ..CliEnv::for_tests()
        };
        let (code, stdout, _stderr) = run_cli_main(&["themes", "list"], b"", &env);
        assert_eq!(code, 0);
        assert!(
            stdout.contains("neon\t"),
            "missing user theme line:\n{stdout}"
        );
        assert!(
            stdout.contains(user_theme.to_str().unwrap()),
            "missing source path in:\n{stdout}"
        );
    }

    #[test]
    fn user_themes_dir_prefers_xdg_over_home() {
        // Both env vars set, different themes in each dir: the one
        // under $XDG_CONFIG_HOME/linesmith/themes wins. Without this
        // test, a swap of the precedence (or a bug in the empty-filter
        // on xdg_config_home) slips through silently.
        let dir = tempdir();
        let xdg_themes = dir.path().join("xdg/linesmith/themes");
        let home_themes = dir.path().join("home/.config/linesmith/themes");
        std::fs::create_dir_all(&xdg_themes).unwrap();
        std::fs::create_dir_all(&home_themes).unwrap();
        std::fs::write(
            xdg_themes.join("xdg_theme.toml"),
            r##"
                name = "xdgonly"
                [roles]
                foreground = "#aaaaaa"
                background = "#000000"
                muted = "#888888"
                primary = "#ff00ff"
                accent = "#00ffff"
                success = "#00ff00"
                warning = "#ffff00"
                error = "#ff0000"
                info = "#0080ff"
            "##,
        )
        .unwrap();
        std::fs::write(
            home_themes.join("home_theme.toml"),
            r##"
                name = "homeonly"
                [roles]
                foreground = "#bbbbbb"
                background = "#000000"
                muted = "#888888"
                primary = "#ff00ff"
                accent = "#00ffff"
                success = "#00ff00"
                warning = "#ffff00"
                error = "#ff0000"
                info = "#0080ff"
            "##,
        )
        .unwrap();
        let env = CliEnv {
            xdg_config_home: Some(dir.path().join("xdg").to_string_lossy().into_owned()),
            home: Some(dir.path().join("home").to_string_lossy().into_owned()),
            ..CliEnv::for_tests()
        };
        let (code, stdout, _stderr) = run_cli_main(&["themes", "list"], b"", &env);
        assert_eq!(code, 0);
        assert!(
            stdout.contains("xdgonly"),
            "XDG theme missing from list:\n{stdout}",
        );
        assert!(
            !stdout.contains("homeonly"),
            "HOME theme leaked in when XDG was set:\n{stdout}",
        );
    }

    #[test]
    fn unknown_subcommand_exits_two() {
        let (code, _stdout, stderr) =
            run_cli_main(&["bogus", "command"], b"", &CliEnv::for_tests());
        assert_eq!(code, 2);
        assert!(stderr.contains("Try --help"));
    }

    #[test]
    fn themes_without_subcommand_exits_two() {
        let (code, _stdout, stderr) = run_cli_main(&["themes"], b"", &CliEnv::for_tests());
        assert_eq!(code, 2);
        assert!(stderr.contains("Try --help"));
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

    // `LayoutOptions` is `#[non_exhaustive]` in linesmith-core,
    // which blocks both literal AND struct-update construction
    // across the crate boundary (E0639). Tests build through
    // Default + field assignment instead.
    fn layout_options_with_color(color: config::ColorPolicy) -> config::LayoutOptions {
        let mut opts = config::LayoutOptions::default();
        opts.color = color;
        opts
    }

    #[test]
    fn color_policy_no_color_env_wins_over_config_always() {
        let cfg = config::Config {
            layout_options: Some(layout_options_with_color(config::ColorPolicy::Always)),
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
            layout_options: Some(layout_options_with_color(config::ColorPolicy::Never)),
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
            layout_options: Some(layout_options_with_color(config::ColorPolicy::Always)),
            ..config::Config::default()
        };
        let got = resolve_color_capability(None, &policy_env(), Some(&cfg));
        assert!(got >= theme::Capability::Palette16, "got {got:?}");
    }

    #[test]
    fn color_policy_config_auto_falls_through_to_terminal_detection() {
        let cfg = config::Config {
            layout_options: Some(layout_options_with_color(config::ColorPolicy::Auto)),
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
        // Pass an empty CliEnv so the result is deterministic regardless
        // of the host terminal.
        assert_ne!(
            force_color_detect(&CliEnv::default()),
            theme::Capability::None
        );
    }

    #[test]
    fn force_color_detect_reads_colorterm_from_cli_env_not_ambient_process() {
        // Hermeticity guard. force_color_detect must read only from
        // CliEnv; a direct std::env::var or a module-level ambient
        // reader would bypass the snapshot and make the resolver
        // non-deterministic for tests and embedders.
        let env = CliEnv {
            colorterm: Some("truecolor".to_string()),
            term: Some("xterm-ghostty".to_string()),
            ..CliEnv::default()
        };
        assert_eq!(force_color_detect(&env), theme::Capability::TrueColor);
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

    fn env_with_home(dir: &Path) -> CliEnv {
        CliEnv {
            home: Some(dir.to_string_lossy().into_owned()),
            ..CliEnv::for_tests()
        }
    }

    #[test]
    fn presets_list_prints_every_preset_name() {
        let (code, stdout, _stderr) = run_cli_main(&["presets", "list"], b"", &CliEnv::for_tests());
        assert_eq!(code, 0);
        for name in crate::presets::names() {
            assert!(stdout.contains(name), "missing '{name}' in:\n{stdout}");
        }
    }

    #[test]
    fn presets_apply_writes_parsed_config_to_resolved_path() {
        use std::str::FromStr;
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, stdout, stderr) = run_cli_main(&["presets", "apply", "minimal"], b"", &env);
        assert_eq!(code, 0, "stderr:\n{stderr}");
        let expected = dir.path().join(".config/linesmith/config.toml");
        assert!(expected.exists(), "config.toml not written");
        let written = std::fs::read_to_string(&expected).unwrap();
        let cfg = config::Config::from_str(&written).expect("round-trips");
        assert_eq!(
            cfg.line.expect("has [line]").segments,
            vec!["model".to_string(), "context_window".to_string()]
        );
        assert!(stdout.contains("wrote preset 'minimal'"));
    }

    #[test]
    fn presets_apply_unknown_name_errors_and_lists_valid() {
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_cli_main(&["presets", "apply", "bogus"], b"", &env);
        assert_eq!(code, 1);
        assert!(stderr.contains("unknown preset 'bogus'"));
        assert!(stderr.contains("developer"));
    }

    #[test]
    fn presets_apply_prompts_on_existing_config_and_accepts_y() {
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# prior content\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_cli_main(&["presets", "apply", "minimal"], b"y\n", &env);
        assert_eq!(code, 0);
        let backup = dir.path().join(".config/linesmith/config.toml.bak");
        assert!(backup.exists(), "expected .bak");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "# prior content\n"
        );
        assert!(std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("preset: minimal"));
        // Prompt + backup-success line both land on stderr so stdout
        // stays clean for pipes.
        assert!(stderr.contains("overwrite"));
        assert!(stderr.contains("backed up previous config to"));
    }

    #[test]
    fn presets_apply_prompt_rejects_on_n_and_leaves_config_untouched() {
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# prior content\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_cli_main(&["presets", "apply", "minimal"], b"n\n", &env);
        assert_eq!(code, 1);
        assert!(stderr.contains("aborted"));
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "# prior content\n");
    }

    #[test]
    fn presets_apply_force_skips_prompt_and_backs_up() {
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# prior content\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, _stderr) =
            run_cli_main(&["presets", "apply", "developer", "--force"], b"", &env);
        assert_eq!(code, 0);
        let backup = dir.path().join(".config/linesmith/config.toml.bak");
        assert!(backup.exists());
        assert!(std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("preset: developer"));
    }

    #[test]
    fn presets_apply_eof_without_force_aborts() {
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# prior\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_cli_main(&["presets", "apply", "minimal"], b"", &env);
        assert_eq!(code, 1);
        assert!(stderr.contains("aborted"));
    }

    #[test]
    fn presets_apply_refuses_to_clobber_existing_backup_without_force() {
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        let bak = dir.path().join(".config/linesmith/config.toml.bak");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# current\n").unwrap();
        std::fs::write(&bak, "# older generation\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_cli_main(&["presets", "apply", "minimal"], b"y\n", &env);
        assert_eq!(code, 1);
        assert!(stderr.contains("already exists"));
        assert!(stderr.contains("--force"));
        // Both files are untouched.
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "# current\n");
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "# older generation\n"
        );
    }

    #[test]
    fn presets_apply_force_replaces_existing_backup() {
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        let bak = dir.path().join(".config/linesmith/config.toml.bak");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# current\n").unwrap();
        std::fs::write(&bak, "# older generation\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, _stderr) =
            run_cli_main(&["presets", "apply", "minimal", "--force"], b"", &env);
        assert_eq!(code, 0);
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "# current\n");
        assert!(std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("preset: minimal"));
    }

    #[test]
    fn presets_apply_honors_explicit_config_flag_over_xdg_path() {
        let dir = tempdir();
        let custom = dir.path().join("custom-preset.toml");
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_cli_main(
            &[
                "--config",
                custom.to_str().unwrap(),
                "presets",
                "apply",
                "minimal",
            ],
            b"",
            &env,
        );
        assert_eq!(code, 0, "stderr:\n{stderr}");
        assert!(custom.exists(), "expected preset written to --config path");
        // XDG fallback must NOT have been written.
        assert!(!dir.path().join(".config/linesmith/config.toml").exists());
    }

    #[test]
    fn presets_apply_creates_missing_parent_dirs() {
        // Fresh HOME with nothing under `.config/`: the resolver
        // produces `<HOME>/.config/linesmith/config.toml` and
        // presets_apply must `create_dir_all` both intermediate dirs.
        let dir = tempdir();
        assert!(!dir.path().join(".config").exists());
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_cli_main(&["presets", "apply", "minimal"], b"", &env);
        assert_eq!(code, 0, "stderr:\n{stderr}");
        assert!(dir.path().join(".config/linesmith").is_dir());
        assert!(dir.path().join(".config/linesmith/config.toml").exists());
    }

    #[test]
    fn presets_apply_empty_name_fails_with_unknown_preset() {
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_cli_main(&["presets", "apply", ""], b"", &env);
        assert_eq!(code, 1);
        assert!(stderr.contains("unknown preset ''"));
    }

    #[test]
    fn presets_apply_write_failure_reports_stderr_and_exits_one() {
        // Parent is a regular file, so `create_dir_all` fails and the
        // write never starts. Pins the stderr-plus-exit-1 contract on
        // the I/O-failure branch without depending on filesystem
        // permissions (which vary across CI).
        let dir = tempdir();
        let not_a_dir = dir.path().join(".config/linesmith");
        std::fs::create_dir_all(not_a_dir.parent().unwrap()).unwrap();
        std::fs::write(&not_a_dir, "I am a file, not a directory").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_cli_main(&["presets", "apply", "minimal"], b"", &env);
        assert_eq!(code, 1);
        assert!(
            stderr.contains("could not create"),
            "expected 'could not create' diagnostic, got: {stderr}"
        );
    }

    #[test]
    fn presets_list_ignores_force_flag_by_rejecting_it() {
        // Pins the "force outside apply errors" contract from the CLI
        // layer through to driver behavior.
        let (code, _stdout, stderr) =
            run_cli_main(&["--force", "presets", "list"], b"", &CliEnv::for_tests());
        assert_eq!(code, 2, "CLI parse error should exit 2");
        assert!(stderr.contains("--force"));
    }

    #[test]
    fn parse_confirmation_accepts_y_yes_case_insensitive_and_trims_whitespace() {
        for ok in [
            "y", "Y", "yes", "YES", "Yes", "  y  \n", "yes\r\n", " YES \t",
        ] {
            assert!(super::parse_confirmation(ok), "expected yes for {ok:?}");
        }
        for no in ["", "\n", " ", "yeah", "ye", "no", "n", "maybe", "yess"] {
            assert!(!super::parse_confirmation(no), "expected no for {no:?}");
        }
    }

    // --- init ---

    /// Drive the testable core directly (bypassing dialoguer) with a
    /// hand-built `InitChoices`. Mirrors `run_cli_main`'s tuple shape so
    /// init tests read like the surrounding `presets_apply_*` tests.
    fn run_init(
        choices: InitChoices,
        config_override: Option<PathBuf>,
        stdin: &[u8],
        env: &CliEnv,
    ) -> (u8, String, String) {
        // Existing tests don't assert on the post-init doctor
        // output, so default to `--no-doctor`. New tests that
        // pin the doctor wiring call `run_init_with_doctor` below.
        run_init_with_doctor(
            choices,
            /* no_doctor= */ true,
            config_override,
            stdin,
            env,
        )
    }

    fn run_init_with_doctor(
        choices: InitChoices,
        no_doctor: bool,
        config_override: Option<PathBuf>,
        stdin: &[u8],
        env: &CliEnv,
    ) -> (u8, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = super::init_with_choices(
            &choices,
            no_doctor,
            config_override,
            io::Cursor::new(stdin),
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

    fn init_choices(preset: &str, theme: &str) -> InitChoices {
        InitChoices {
            preset: preset.to_string(),
            theme: theme.to_string(),
        }
    }

    #[test]
    fn init_creates_valid_config_round_trips_parse() {
        use std::str::FromStr;
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, stdout, stderr) =
            run_init(init_choices("minimal", "catppuccin-mocha"), None, b"", &env);
        assert_eq!(code, 0, "stderr:\n{stderr}");
        let path = dir.path().join(".config/linesmith/config.toml");
        let written = std::fs::read_to_string(&path).expect("file exists");
        let cfg = config::Config::from_str(&written).expect("round-trips");
        assert_eq!(cfg.theme.as_deref(), Some("catppuccin-mocha"));
        assert_eq!(
            cfg.line.expect("has [line]").segments,
            vec!["model".to_string(), "context_window".to_string()]
        );
        assert!(stdout.contains("wrote config.toml to"));
    }

    #[test]
    fn init_no_doctor_writes_config_without_running_doctor() {
        // Spec §`linesmith init` integration: `--no-doctor`
        // bypasses the post-init doctor invocation. CI / scripted
        // onboarding paths use this so the network probe doesn't
        // block, and so init's exit code reflects only init.
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, stdout, stderr) = run_init_with_doctor(
            init_choices("minimal", "catppuccin-mocha"),
            /* no_doctor= */ true,
            None,
            b"",
            &env,
        );
        assert_eq!(code, 0, "stderr:\n{stderr}");
        assert!(
            stdout.contains("wrote config.toml to"),
            "init still writes the config:\n{stdout}",
        );
        assert!(
            !stdout.contains("Running doctor"),
            "doctor must NOT run with --no-doctor; got stdout:\n{stdout}",
        );
        assert!(
            !stdout.contains("linesmith doctor (v"),
            "doctor report must NOT appear with --no-doctor; got stdout:\n{stdout}",
        );
    }

    #[test]
    fn init_runs_doctor_inline_after_writing_config() {
        // Spec §`linesmith init` integration: doctor runs after
        // init writes the config, with the "Running doctor..."
        // separator line and the report appearing inline. The
        // resolved-config path threads through to doctor so it
        // inspects exactly the file we just wrote.
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (_code, stdout, _stderr) = run_init_with_doctor(
            init_choices("minimal", "catppuccin-mocha"),
            /* no_doctor= */ false,
            None,
            b"",
            &env,
        );
        assert!(
            stdout.contains("wrote config.toml to"),
            "init must write the config before invoking doctor:\n{stdout}",
        );
        assert!(
            stdout.contains("Running doctor to verify your setup"),
            "init must announce the doctor run:\n{stdout}",
        );
        assert!(
            stdout.contains("linesmith doctor (v"),
            "doctor report header must appear inline:\n{stdout}",
        );
        // Doctor reads the file we just wrote — confirm by
        // looking for the Config section's PASS line for it.
        assert!(
            stdout.contains("Config file:") && stdout.contains("config.toml"),
            "doctor should report on the freshly-written config:\n{stdout}",
        );
    }

    #[test]
    fn init_propagates_doctor_exit_code() {
        // Spec line 161: "If doctor exits non-zero, init's own
        // exit code propagates it." The healthy fixture path on a
        // tempdir-rooted $HOME doesn't have a real claude binary
        // or credentials, so the auto-doctor will FAIL — perfect
        // for pinning the propagation contract. (If doctor PASSed
        // here, init would exit 0 and we'd still cover the
        // happy-path propagation; the failure path is the one
        // that matters for the contract.)
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, _stdout, _stderr) = run_init_with_doctor(
            init_choices("minimal", "catppuccin-mocha"),
            /* no_doctor= */ false,
            None,
            b"",
            &env,
        );
        // The actual doctor outcome depends on the host's
        // PATH/keychain/etc., so accept either propagation
        // direction. What we're pinning: init's exit code is
        // *whatever doctor returned*, not hardcoded to 0.
        assert!(
            code == 0 || code == 1,
            "init should propagate doctor's exit code (0 or 1); got {code}",
        );
    }

    #[test]
    fn init_writes_to_resolved_xdg_path() {
        // Two tempdirs so XDG-vs-HOME precedence is decisive: if the
        // resolver wrongly fell through to HOME, we'd see a file in
        // `home_dir`, not `xdg_dir`.
        let xdg_dir = tempdir();
        let home_dir = tempdir();
        let env = CliEnv {
            xdg_config_home: Some(xdg_dir.path().to_string_lossy().into_owned()),
            home: Some(home_dir.path().to_string_lossy().into_owned()),
            ..CliEnv::for_tests()
        };
        let (code, _stdout, stderr) = run_init(init_choices("minimal", "default"), None, b"", &env);
        assert_eq!(code, 0, "stderr:\n{stderr}");
        assert!(
            xdg_dir.path().join("linesmith/config.toml").exists(),
            "expected config at XDG path"
        );
        assert!(
            !home_dir
                .path()
                .join(".config/linesmith/config.toml")
                .exists(),
            "HOME path should not be touched when XDG resolves"
        );
    }

    /// Pull the indented `"statusLine": { ... }` JSON-fragment block
    /// out of the init stdout and wrap it as a complete object so
    /// `serde_json` can parse it. Asserting on the parsed shape (not
    /// substring matches) catches regressions like a stray trailing
    /// comma or unbalanced brace that would silently ship as bad UX.
    fn parse_snippet(stdout: &str) -> serde_json::Value {
        let lines: Vec<&str> = stdout.lines().collect();
        let start = lines
            .iter()
            .position(|l| l.contains("\"statusLine\""))
            .expect("snippet header present");
        let end = lines[start..]
            .iter()
            .position(|l| l.trim_end() == "  }")
            .map(|i| start + i)
            .expect("snippet closing brace present");
        let body: String = lines[start..=end].join("\n");
        let wrapped = format!("{{\n{body}\n}}");
        serde_json::from_str(&wrapped)
            .unwrap_or_else(|e| panic!("snippet not valid JSON ({e}):\n{wrapped}"))
    }

    #[test]
    fn init_emits_claude_code_settings_snippet() {
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, stdout, _stderr) = run_init(init_choices("minimal", "default"), None, b"", &env);
        assert_eq!(code, 0);
        let parsed = parse_snippet(&stdout);
        let status_line = &parsed["statusLine"];
        assert_eq!(status_line["type"], "command");
        assert_eq!(status_line["command"], "linesmith");
        assert_eq!(status_line["padding"], 0);
        assert!(
            stdout.contains("settings.json"),
            "snippet should name the destination file"
        );
        assert!(
            stdout.contains("merge into the existing top-level object"),
            "merge hint missing — without it, users pasting into an empty file get invalid JSON",
        );
    }

    #[test]
    fn init_snippet_preserves_explicit_config_flag() {
        // When --config <PATH> was used, the snippet must tell Claude
        // Code to call `linesmith --config <PATH>`. A bare
        // `linesmith` would point at the XDG default and the file
        // `init` just wrote would never be read.
        let dir = tempdir();
        let custom = dir.path().join("custom-init.toml");
        let env = env_with_home(dir.path());
        let (code, stdout, _stderr) = run_init(
            init_choices("minimal", "default"),
            Some(custom.clone()),
            b"",
            &env,
        );
        assert_eq!(code, 0);
        let parsed = parse_snippet(&stdout);
        let cmd = parsed["statusLine"]["command"]
            .as_str()
            .expect("command is a string");
        assert!(
            cmd.starts_with("linesmith --config "),
            "expected `--config` in command, got: {cmd}"
        );
        assert!(
            cmd.contains(custom.to_string_lossy().as_ref()),
            "command should reference the actual --config path: {cmd}"
        );
    }

    #[test]
    fn init_snippet_preserves_env_resolved_config_path() {
        // When the user's explicit path comes from `LINESMITH_CONFIG`,
        // the snippet must include `--config <path>` just as it does
        // for the CLI flag. A bare `linesmith` snippet would
        // re-resolve to the XDG default and miss the file we just
        // wrote.
        let dir = tempdir();
        let custom = dir.path().join("env-init.toml");
        let env = CliEnv {
            linesmith_config: Some(custom.to_string_lossy().into_owned()),
            ..env_with_home(dir.path())
        };
        let (code, stdout, _stderr) = run_init(init_choices("minimal", "default"), None, b"", &env);
        assert_eq!(code, 0);
        let parsed = parse_snippet(&stdout);
        let cmd = parsed["statusLine"]["command"]
            .as_str()
            .expect("command is a string");
        assert!(
            cmd.starts_with("linesmith --config "),
            "env-resolved path should still produce --config snippet, got: {cmd}"
        );
        assert!(
            cmd.contains(custom.to_string_lossy().as_ref()),
            "command should reference the env-resolved path: {cmd}"
        );
    }

    #[test]
    fn init_snippet_uses_bare_command_for_implicit_xdg_path() {
        // The other side of the contract: when neither --config nor
        // LINESMITH_CONFIG is set, the resolved path is the XDG
        // default. Plain `linesmith` finds it without --config, so
        // the snippet stays bare (no spurious --config flag).
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, stdout, _stderr) = run_init(init_choices("minimal", "default"), None, b"", &env);
        assert_eq!(code, 0);
        let parsed = parse_snippet(&stdout);
        assert_eq!(parsed["statusLine"]["command"], "linesmith");
    }

    #[test]
    fn init_warns_about_env_resolved_path_with_spaces() {
        // The user-edit warning must also fire on env-resolved paths,
        // not just --config. Same broken-snippet failure mode
        // either way.
        let dir = tempdir();
        let custom = dir.path().join("env path with spaces.toml");
        let env = CliEnv {
            linesmith_config: Some(custom.to_string_lossy().into_owned()),
            ..env_with_home(dir.path())
        };
        let (code, _stdout, stderr) = run_init(init_choices("minimal", "default"), None, b"", &env);
        assert_eq!(code, 0);
        assert!(
            stderr.contains("characters that need shell quoting"),
            "missing shell-quoting warning for env-resolved path:\n{stderr}"
        );
    }

    // NTFS forbids `"` in filenames, so this case can't be exercised
    // on Windows; backslash escaping there is exercised by
    // `init_snippet_preserves_explicit_config_flag` (Windows tempdir
    // paths always contain `\`, and `parse_snippet` panics on invalid
    // JSON).
    #[cfg(not(windows))]
    #[test]
    fn init_snippet_escapes_quotes_and_backslashes_in_config_path() {
        // The path is interpolated into a JSON string, so backslashes
        // (Windows paths) and double quotes must be escaped or the
        // emitted snippet is invalid JSON.
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let weird = dir.path().join("weird\"path\\ok.toml");
        let (code, stdout, _stderr) = run_init(
            init_choices("minimal", "default"),
            Some(weird.clone()),
            b"",
            &env,
        );
        assert_eq!(code, 0);
        // The decisive assertion: parse_snippet panics on invalid JSON.
        let parsed = parse_snippet(&stdout);
        let cmd = parsed["statusLine"]["command"]
            .as_str()
            .expect("command is a string");
        assert!(cmd.contains(weird.to_string_lossy().as_ref()));
    }

    #[test]
    fn init_succeeds_when_both_config_and_backup_already_exist() {
        // A user who init'd once (creating config.toml.bak), then
        // init'd a second time and confirmed `y`, must not be
        // stranded on a third `init` because the .bak from run #1
        // still exists. The `y` confirmation already captured intent
        // to lose prior content, so init clobbers .bak unconditionally.
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        let bak = dir.path().join(".config/linesmith/config.toml.bak");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# current\n").unwrap();
        std::fs::write(&bak, "# older generation\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) =
            run_init(init_choices("minimal", "default"), None, b"y\n", &env);
        assert_eq!(
            code, 0,
            "init must not deadlock when .bak already exists\nstderr:\n{stderr}"
        );
        // The previously-current config moved to .bak; the older .bak
        // generation was clobbered.
        assert_eq!(std::fs::read_to_string(&bak).unwrap(), "# current\n");
        assert_eq!(
            std::fs::read_to_string(&cfg).unwrap(),
            config::with_schema_directive(presets::body("minimal").unwrap())
        );
    }

    #[test]
    fn init_user_says_no_to_overwrite_does_not_clobber_backup() {
        // Init only clobbers .bak after a confirmed `y`. An `n` must
        // leave both files untouched, or every aborted init would
        // destroy the .bak.
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        let bak = dir.path().join(".config/linesmith/config.toml.bak");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# current\n").unwrap();
        std::fs::write(&bak, "# older generation\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, _stderr) =
            run_init(init_choices("minimal", "default"), None, b"n\n", &env);
        assert_eq!(code, 1);
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "# current\n");
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "# older generation\n"
        );
    }

    #[test]
    fn init_writes_schema_directive_at_top_of_config() {
        // taplo / VS Code / Zed pick up the published JSON Schema via
        // the `#:schema URL` directive at the top of the file. Pin
        // the position (very first bytes) so a future refactor that
        // shifts the directive below comments or theme lines breaks
        // editor schema attachment.
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_init(init_choices("minimal", "default"), None, b"", &env);
        assert_eq!(code, 0, "stderr:\n{stderr}");
        let written = std::fs::read_to_string(dir.path().join(".config/linesmith/config.toml"))
            .expect("file exists");
        assert!(
            written.starts_with("#:schema https://"),
            "expected schema directive at top, got:\n{written}"
        );
        assert!(
            written.contains("config.schema.json"),
            "schema URL must point at config.schema.json"
        );
    }

    #[test]
    fn presets_apply_writes_schema_directive_at_top_of_config() {
        // Same contract as init: presets apply must also emit the
        // `#:schema` directive at the top.
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, _stdout, _stderr) = run_cli_main(&["presets", "apply", "minimal"], b"", &env);
        assert_eq!(code, 0);
        let written = std::fs::read_to_string(dir.path().join(".config/linesmith/config.toml"))
            .expect("file exists");
        assert!(
            written.starts_with("#:schema https://"),
            "expected schema directive at top, got:\n{written}"
        );
    }

    #[test]
    fn init_default_theme_omits_theme_field_to_match_presets_apply() {
        // When the user picks `default`, the written file should be
        // byte-identical to `presets apply minimal` so diff tools and
        // future migrations don't see a redundant `theme = "default"`.
        // Both paths prepend the same `#:schema` directive, so
        // byte-equality holds against the schema-wrapped preset.
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_init(init_choices("minimal", "default"), None, b"", &env);
        assert_eq!(code, 0, "stderr:\n{stderr}");
        let written = std::fs::read_to_string(dir.path().join(".config/linesmith/config.toml"))
            .expect("file exists");
        assert_eq!(
            written,
            config::with_schema_directive(presets::body("minimal").expect("preset registered"))
        );
    }

    #[test]
    fn init_overwrite_prompts_and_backs_up_existing_config() {
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# prior content\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) =
            run_init(init_choices("minimal", "default"), None, b"y\n", &env);
        assert_eq!(code, 0, "stderr:\n{stderr}");
        let backup = dir.path().join(".config/linesmith/config.toml.bak");
        assert!(backup.exists(), "expected .bak");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "# prior content\n"
        );
        // `confirm_overwrite`'s prompt wording is owned by that helper;
        // assert only the post-prompt diagnostic that's specific to
        // this code path. The backup's existence + contents above
        // already prove the prompt was honored.
        assert!(stderr.contains("backed up previous config to"));
    }

    #[test]
    fn init_eof_on_overwrite_aborts_without_clobbering() {
        let dir = tempdir();
        let cfg = dir.path().join(".config/linesmith/config.toml");
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "# prior\n").unwrap();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_init(init_choices("minimal", "default"), None, b"", &env);
        assert_eq!(code, 1);
        assert!(stderr.contains("aborted"));
        // Original content untouched.
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), "# prior\n");
    }

    #[test]
    fn init_honors_explicit_config_flag_over_xdg_path() {
        let dir = tempdir();
        let custom = dir.path().join("custom-init.toml");
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_init(
            init_choices("minimal", "default"),
            Some(custom.clone()),
            b"",
            &env,
        );
        assert_eq!(code, 0, "stderr:\n{stderr}");
        assert!(custom.exists(), "expected init at --config path");
        assert!(!dir.path().join(".config/linesmith/config.toml").exists());
    }

    #[test]
    fn init_unknown_preset_in_choices_errors() {
        // The dialoguer wrapper only ever emits registered names, but a
        // hand-built `InitChoices` with an unknown preset should still
        // get a clear error rather than silently writing nothing.
        let dir = tempdir();
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) =
            run_init(init_choices("not-a-preset", "default"), None, b"", &env);
        assert_eq!(code, 1);
        assert!(stderr.contains("unknown preset 'not-a-preset'"));
    }

    #[test]
    fn init_without_resolvable_path_errors() {
        let env = CliEnv {
            home: None,
            ..CliEnv::for_tests()
        };
        let (code, _stdout, stderr) = run_init(init_choices("minimal", "default"), None, b"", &env);
        assert_eq!(code, 1);
        assert!(stderr.contains("cannot resolve"));
    }

    #[test]
    fn compose_init_body_default_theme_passes_body_through() {
        let body = "# hdr\n\n[line]\nsegments = [\"model\"]\n";
        assert_eq!(super::compose_init_body(body, "default"), body);
    }

    #[test]
    fn compose_init_body_inserts_theme_before_first_table() {
        let body = "# hdr\n# more\n\n[line]\nsegments = [\"model\"]\n";
        let out = super::compose_init_body(body, "catppuccin-mocha");
        assert_eq!(
            out,
            "# hdr\n# more\n\ntheme = \"catppuccin-mocha\"\n\n[line]\nsegments = [\"model\"]\n"
        );
    }

    #[test]
    fn compose_init_body_escapes_quotes_and_backslashes_in_theme_name() {
        // Theme names from user files may contain unusual characters;
        // the TOML string literal must remain valid.
        let body = "[line]\nsegments = []\n";
        let out = super::compose_init_body(body, "weird\"name\\foo");
        assert!(
            out.starts_with("theme = \"weird\\\"name\\\\foo\"\n\n"),
            "unexpected escape: {out:?}"
        );
        // Confirm round-trip parse.
        use std::str::FromStr;
        let cfg = config::Config::from_str(&out).expect("escaped TOML parses");
        assert_eq!(cfg.theme.as_deref(), Some("weird\"name\\foo"));
    }

    #[test]
    fn compose_init_body_handles_table_header_at_byte_zero() {
        // No leading comment, no leading newline: `find("\n[")`
        // wouldn't see this header, so we route through
        // `starts_with('[')` to inject `theme` at the top instead of
        // burying it after the segments.
        use std::str::FromStr;
        let body = "[line]\nsegments = [\"model\"]\n";
        let out = super::compose_init_body(body, "catppuccin-mocha");
        assert_eq!(
            out,
            "theme = \"catppuccin-mocha\"\n\n[line]\nsegments = [\"model\"]\n"
        );
        let cfg = config::Config::from_str(&out).expect("parses");
        assert_eq!(cfg.theme.as_deref(), Some("catppuccin-mocha"));
    }

    #[test]
    fn compose_init_body_falls_back_when_preset_has_no_tables() {
        // Truly-degenerate preset (comments only, no `[table]` ever):
        // the function still produces parseable TOML rather than
        // returning the body unchanged.
        use std::str::FromStr;
        let body = "# only a comment\n";
        let out = super::compose_init_body(body, "catppuccin-mocha");
        assert!(
            out.starts_with("theme = \"catppuccin-mocha\"\n\n"),
            "unexpected output: {out:?}"
        );
        assert!(out.ends_with("# only a comment\n"));
        let cfg = config::Config::from_str(&out).expect("parses");
        assert_eq!(cfg.theme.as_deref(), Some("catppuccin-mocha"));
    }

    #[test]
    fn init_warns_when_config_path_contains_spaces() {
        // Snippet stays valid JSON, but Claude Code tokenizes the
        // `command` field as a shell argv — the user has to add quotes
        // before pasting. Without the warning the snippet would look
        // fine and break silently at exec time.
        let dir = tempdir();
        let custom = dir.path().join("path with spaces.toml");
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) = run_init(
            init_choices("minimal", "default"),
            Some(custom.clone()),
            b"",
            &env,
        );
        assert_eq!(code, 0);
        assert!(
            stderr.contains("characters that need shell quoting"),
            "missing shell-quoting warning, stderr:\n{stderr}"
        );
        assert!(
            stderr.contains("path with spaces.toml"),
            "warning should name the path"
        );
    }

    #[test]
    fn init_does_not_warn_for_plain_ascii_config_path() {
        // Plain paths shouldn't get a warning — false positives would
        // train users to ignore the channel.
        let dir = tempdir();
        let custom = dir.path().join("plain.toml");
        let env = env_with_home(dir.path());
        let (_code, _stdout, stderr) =
            run_init(init_choices("minimal", "default"), Some(custom), b"", &env);
        assert!(
            !stderr.contains("characters that need shell quoting"),
            "false-positive warning, stderr:\n{stderr}"
        );
        assert!(
            !stderr.contains("non-UTF-8"),
            "false-positive warning, stderr:\n{stderr}"
        );
    }

    #[test]
    fn init_warns_when_config_path_contains_shell_metacharacters() {
        // Single-quote, dollar, ampersand, etc. all change shell
        // tokenization. One of them is enough to trigger the warning.
        let dir = tempdir();
        let custom = dir.path().join("weird$path.toml");
        let env = env_with_home(dir.path());
        let (code, _stdout, stderr) =
            run_init(init_choices("minimal", "default"), Some(custom), b"", &env);
        assert_eq!(code, 0);
        assert!(
            stderr.contains("characters that need shell quoting"),
            "missing warning for `$`, stderr:\n{stderr}"
        );
    }

    #[test]
    fn needs_shell_quoting_flags_metacharacters_and_whitespace() {
        // Direct unit test of the predicate so a future addition of a
        // new metachar to the match arm doesn't silently regress.
        // `^` and `%` are cmd.exe specials; the rest are POSIX
        // metacharacters most shells respect.
        for c in [' ', '\t', '\'', '"', '$', '&', ';', '|', '*', '?', '^', '%'] {
            assert!(
                super::needs_shell_quoting(c),
                "expected {c:?} to need quoting"
            );
        }
        for c in ['a', 'Z', '0', '_', '-', '.', '/'] {
            assert!(
                !super::needs_shell_quoting(c),
                "expected {c:?} to NOT need quoting"
            );
        }
        // `\` and `~` pin the platform split: POSIX shell
        // escape / home expansion vs. Windows path separator and 8.3
        // short-name literal. Without these arms a future refactor
        // that collapses the cfg split slips past CI on every host.
        for c in ['\\', '~'] {
            #[cfg(not(windows))]
            assert!(
                super::needs_shell_quoting(c),
                "POSIX: {c:?} must need quoting"
            );
            #[cfg(windows)]
            assert!(
                !super::needs_shell_quoting(c),
                "Windows: {c:?} must not need quoting (path separator / 8.3 short name)"
            );
        }
    }

    #[test]
    fn warn_if_path_needs_user_edit_flags_leading_dash() {
        // Position-sensitive case: a path string starting with `-`
        // (e.g. `LINESMITH_CONFIG=-relative.toml` or a relative
        // `--config -foo`) becomes a flag at the snippet's
        // re-invocation. lexopt rejects the unknown flag and the
        // statusline silently fails on every Claude Code tick.
        // Tested directly because `tempdir().join("-foo")` produces
        // an absolute path that starts with `/`, not `-`.
        let mut stderr = Vec::new();
        super::warn_if_path_needs_user_edit(Some(Path::new("-relative.toml")), &mut stderr);
        let stderr = String::from_utf8(stderr).unwrap();
        assert!(
            stderr.contains("starts with `-`"),
            "missing leading-dash warning:\n{stderr}"
        );
    }

    #[test]
    fn warn_if_path_needs_user_edit_quiet_for_clean_paths() {
        // The leading-dash, shell-quoting, and non-UTF-8 branches
        // are mutually exclusive guards. A plain absolute path with
        // no metacharacters trips none of them.
        let mut stderr = Vec::new();
        super::warn_if_path_needs_user_edit(
            Some(Path::new("/etc/linesmith/config.toml")),
            &mut stderr,
        );
        assert!(
            stderr.is_empty(),
            "false-positive warning:\n{}",
            String::from_utf8_lossy(&stderr)
        );
    }

    #[test]
    fn overwrite_policy_constructors_lock_the_bit_mapping() {
        // The doc on `OverwritePolicy` promises that the named
        // constructors keep the bit-mapping from drifting between
        // call sites. Pin the contract directly so a future refactor
        // that "simplifies" `presets(force)` to `{ skip_prompt: false,
        // clobber_backup: force }` (or similar) breaks here, not at
        // a call site that no longer exists.
        let presets_off = super::OverwritePolicy::presets(false);
        assert!(!presets_off.skip_prompt);
        assert!(!presets_off.clobber_backup);

        let presets_on = super::OverwritePolicy::presets(true);
        assert!(presets_on.skip_prompt);
        assert!(presets_on.clobber_backup);

        let init = super::OverwritePolicy::init();
        assert!(!init.skip_prompt, "init must always prompt");
        assert!(init.clobber_backup, "init must always clobber .bak");
    }

    // The rename-after-clobber breadcrumb path (where
    // `write_config_with_backup` removes an existing .bak and the
    // subsequent rename then fails) isn't unit-tested. Triggering
    // rename failure between a successful `remove_file` and `rename`
    // requires interposition (race) or cross-mount setup. The branch
    // is short and visually inspectable in `write_config_with_backup`;
    // revisit if the helper grows.

    #[test]
    fn presets_apply_without_resolvable_path_errors() {
        let env = CliEnv {
            home: None,
            ..CliEnv::for_tests()
        };
        let (code, _stdout, stderr) = run_cli_main(&["presets", "apply", "minimal"], b"", &env);
        assert_eq!(code, 1);
        assert!(stderr.contains("cannot resolve"));
    }

    #[test]
    fn doctor_subcommand_dispatches_via_cli_main() {
        // CLI-dispatch coverage: parsing reaches doctor_action and
        // emits a report header. We don't assert exit code here
        // because doctor_action calls DoctorEnv::from_process() —
        // exit code depends on the host's $HOME / $TERM / TTY state,
        // which test runners can leave in any shape (`env -i` etc.).
        // The hermetic exit-code contracts live in the
        // `doctor_action_with_env_*` tests below.
        let (_code, stdout, _stderr) = run_cli_main(&["doctor"], b"", &CliEnv::for_tests());
        assert!(stdout.contains("linesmith doctor"), "{stdout}");
        assert!(stdout.contains("Self"), "{stdout}");
    }

    /// Build a doctor-style stdout/stderr pair driven from a hermetic
    /// `DoctorEnv`. Avoids the `from_process()` call that
    /// `doctor_action` itself makes, so tests can assert exit codes
    /// without depending on the host's env.
    fn run_doctor(env: crate::doctor::DoctorEnv, plain: bool) -> (u8, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = doctor_action_with_env(env, plain, &mut stdout, &mut stderr);
        (
            code,
            String::from_utf8(stdout).expect("utf8 stdout"),
            String::from_utf8(stderr).expect("utf8 stderr"),
        )
    }

    #[test]
    fn doctor_action_with_healthy_env_renders_and_exits_zero() {
        let (code, stdout, stderr) = run_doctor(crate::doctor::DoctorEnv::healthy(), false);
        assert_eq!(code, 0, "healthy env must exit 0; stderr was: {stderr}");
        assert!(stdout.contains("linesmith doctor"), "{stdout}");
        assert!(stdout.contains("Environment"), "{stdout}");
        assert!(stdout.contains("Config"), "{stdout}");
        assert!(stdout.contains("Self"), "{stdout}");
        assert!(stdout.contains("Exit: 0"), "{stdout}");
        assert!(stderr.is_empty(), "{stderr}");
    }

    #[test]
    fn doctor_action_with_home_unset_exits_one() {
        // The hermetic counterpart to the CLI-dispatch smoke: when
        // env.home FAILs, doctor_action surfaces it as exit 1
        // end-to-end, not just at the build_report layer.
        let mut env = crate::doctor::DoctorEnv::healthy();
        env.home_env = crate::doctor::EnvVarState::Unset;
        let (code, _stdout, _stderr) = run_doctor(env, false);
        assert_eq!(code, 1);
    }

    #[test]
    fn doctor_plain_output_is_ascii_only() {
        let (code, stdout, _stderr) = run_doctor(crate::doctor::DoctorEnv::healthy(), true);
        assert_eq!(code, 0);
        assert!(stdout.is_ascii(), "plain output had non-ASCII:\n{stdout}");
        assert!(stdout.contains("OK"), "{stdout}");
    }

    #[test]
    fn doctor_default_output_includes_unicode_glyph() {
        let (code, stdout, _stderr) = run_doctor(crate::doctor::DoctorEnv::healthy(), false);
        assert_eq!(code, 0);
        assert!(stdout.contains('✓'), "{stdout}");
    }

    #[test]
    fn doctor_unknown_flag_exits_two() {
        let (code, _stdout, stderr) =
            run_cli_main(&["doctor", "--bogus"], b"", &CliEnv::for_tests());
        assert_eq!(code, 2);
        assert!(
            stderr.contains("bogus") || stderr.contains("Try --help"),
            "{stderr}"
        );
    }

    #[test]
    fn doctor_config_override_to_missing_explicit_path_fails() {
        // An explicit `--config <path>` that doesn't exist is a FAIL,
        // not a WARN: the user named *this file*. Falling back to
        // defaults silently (same WARN as the implicit-cascade case)
        // would hide a typo. Guards against a regression where the
        // override gets silently dropped before reaching `from_process`.
        let mut env = crate::doctor::DoctorEnv::healthy();
        env.config = crate::doctor::DoctorConfigSnapshot {
            cli_override: Some(PathBuf::from("/nonexistent.toml")),
            resolved: Some(crate::config::ConfigPath {
                path: PathBuf::from("/nonexistent.toml"),
                explicit: true,
            }),
            read: crate::doctor::ConfigReadOutcome::NotFound {
                path: PathBuf::from("/nonexistent.toml"),
                explicit: true,
            },
            plugin_dirs: Vec::new(),
            known_segment_ids: crate::doctor::DoctorConfigSnapshot::built_in_segment_ids(),
            known_theme_names: crate::doctor::DoctorConfigSnapshot::built_in_theme_names(),
        };
        let (code, stdout, _stderr) = run_doctor(env, false);
        assert_eq!(code, 1, "explicit missing config must FAIL → exit 1");
        assert!(
            stdout.contains("/nonexistent.toml"),
            "report should name the missing path:\n{stdout}"
        );
    }

    #[test]
    fn doctor_action_threads_cli_override_through_from_process() {
        // Locks the production wire-up that the snapshot-based
        // `doctor_config_override_to_missing_explicit_path_fails`
        // test can't see: a regression that drops `cli_override`
        // before reaching `DoctorEnv::from_process` would still
        // pass that test (because the snapshot is built directly).
        // This goes through `doctor_action` — the path the binary
        // actually takes — and asserts the rendered report names
        // the path the user supplied. If the override is silently
        // dropped, the report instead describes the host's
        // $HOME-derived path (or "no config path resolved"), which
        // wouldn't contain `unique-marker.toml`.
        let dir = tempdir();
        let cfg_path = dir.path().join("unique-marker.toml");
        std::fs::write(&cfg_path, r#"theme = "default""#).expect("write tempfile");
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let _code = doctor_action(true, Some(cfg_path.clone()), &mut stdout, &mut stderr);
        let stdout_str = String::from_utf8(stdout).expect("utf8 stdout");
        assert!(
            stdout_str.contains(&cfg_path.display().to_string()),
            "report must name the supplied --config path; \
             got stdout (override probably dropped before from_process):\n{stdout_str}",
        );
        // Discovery must have produced a PASS line for our file —
        // catches a refactor that threads the path but reads the
        // wrong file.
        assert!(
            stdout_str.contains("Config file:"),
            "expected `Config file:` PASS line:\n{stdout_str}",
        );
    }

    #[test]
    fn doctor_stdout_failure_logs_to_stderr_and_returns_report_exit_code() {
        // Pin the documented divergence from `Run`'s render path:
        // doctor surfaces stdout I/O errors via stderr but lets the
        // report's exit code through. A future "fix" that promotes
        // I/O errors to exit 1 would be a contract change.
        struct FailingWriter;
        impl Write for FailingWriter {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut stderr = Vec::new();
        let code = doctor_action_with_env(
            crate::doctor::DoctorEnv::healthy(),
            false,
            &mut FailingWriter,
            &mut stderr,
        );
        assert_eq!(
            code, 0,
            "all-PASS report exits 0 even when stdout is broken"
        );
        let stderr_str = String::from_utf8(stderr).expect("utf8");
        assert!(
            stderr_str.contains("linesmith: doctor:"),
            "expected doctor diagnostic on stderr, got: {stderr_str}"
        );
    }

    fn tempdir() -> TempDir {
        // Timestamp + monotonic counter: parallel tests can hit the
        // same nanosecond under cargo test's thread pool, so the
        // counter is the last line of defense against collisions.
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "linesmith-driver-test-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&base).expect("mkdir");
        TempDir(base)
    }
}
