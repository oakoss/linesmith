//! Command-line argument parsing via `lexopt`. Full flag surface and
//! rationale live in `docs/specs/config.md`.

use lexopt::prelude::*;
use std::ffi::OsString;
use std::path::PathBuf;

/// Parsed CLI arguments.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CliArgs {
    pub config: Option<PathBuf>,
    pub check_config: bool,
    pub color_override: Option<ColorOverride>,
}

/// User-supplied color-policy override. `--no-color` and `--force-color`
/// are mutually exclusive in intent; the flag that appears last on the
/// command line wins (lexopt assigns them in order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ColorOverride {
    Never,
    Always,
}

/// What the binary should do after parsing. `Run` is the common case;
/// every other variant is a meta-command that prints / writes and
/// exits.
#[derive(Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    Run(CliArgs),
    Help,
    Version,
    ThemesList,
    PresetsList,
    PresetsApply {
        name: String,
        force: bool,
        config: Option<PathBuf>,
    },
    Init {
        config: Option<PathBuf>,
        /// Skip the post-init `linesmith doctor` invocation.
        /// Per spec §`linesmith init` integration: doctor runs
        /// automatically after init writes the config; users
        /// pass `--no-doctor` to opt out (CI / scripted onboarding,
        /// or a host where doctor's network probe would block).
        no_doctor: bool,
    },
    // Intentionally absent from `HELP`: advertising "diagnose your
    // setup" while only emitting one PASS would be a false promise.
    // Re-list when build_report covers the full eight-category catalog
    // (see docs/specs/doctor.md §Check catalog).
    Doctor {
        plain: bool,
        config: Option<PathBuf>,
    },
    /// Boot the interactive `linesmith config` TUI editor (per
    /// ADR-0015 / ADR-0016). Loads the config at `config` if set,
    /// else the resolved default path; if no file exists, starts
    /// against an in-memory `Config::default()`. `color_override`
    /// carries the top-level `--no-color` / `--force-color` flag so
    /// the TUI's preview honors the same color-resolution chain the
    /// daily render path uses. `Run` and `Config` are the only
    /// variants that consume `color_override`; the parse-time gate
    /// in [`parse`] rejects it on every other subcommand.
    Config {
        config: Option<PathBuf>,
        color_override: Option<ColorOverride>,
    },
    /// Wire linesmith into Claude Code's `~/.claude/settings.json`
    /// statusLine. Atomic write with a `.bak` backup of any prior
    /// contents. `config`, when present, becomes the `--config`
    /// argument of the installed `linesmith` invocation; absent it
    /// installs as bare `linesmith` and relies on the resolved
    /// default at runtime.
    Install {
        config: Option<PathBuf>,
    },
    /// Remove linesmith's statusLine entry from
    /// `~/.claude/settings.json`. Atomic write with a `.bak`
    /// backup. No-op when the file doesn't exist or has no
    /// `statusLine` key.
    Uninstall,
}

/// Help text. Kept short; full docs live at
/// <https://github.com/oakoss/linesmith>.
pub const HELP: &str = "\
linesmith — status line for Claude Code and other AI coding CLIs

USAGE:
    linesmith [OPTIONS]
    linesmith init [--no-doctor]
    linesmith doctor [--plain]
    linesmith config
    linesmith install
    linesmith uninstall
    linesmith themes list
    linesmith presets list
    linesmith presets apply <NAME> [--force]

OPTIONS:
    -c, --config <PATH>    Config file path (overrides default resolution)
        --check-config     Validate config and exit
        --no-color         Strip all color (equivalent to NO_COLOR=1)
        --force-color      Emit color even in non-TTY output
        --force            For `presets apply`: overwrite without confirmation
        --no-doctor        For `init`: skip the post-init doctor run
        --plain            For `doctor`: ASCII output (no Unicode glyphs)
    -h, --help             Print this help text
    -V, --version          Print version

SUBCOMMANDS:
    init                   Interactive onboarding: pick a preset + theme,
                           write config.toml, print Claude Code snippet,
                           and run a setup-verification check (`--no-doctor`
                           to skip)
    doctor                 Run setup diagnostics (env, config, Claude Code
                           integration, credentials, cache, plugins, git);
                           exits 1 on any FAIL
    config                 Boot the interactive TUI editor for `config.toml`.
                           Requires the `config-ui` feature (default-on)
    install                Add the linesmith statusLine block to
                           `~/.claude/settings.json` (atomic write, `.bak`
                           backup of any prior contents)
    uninstall              Remove the linesmith statusLine block from
                           `~/.claude/settings.json` (atomic write, `.bak`
                           backup)
    themes list            List available themes (built-in + user)
    presets list           List available config presets
    presets apply <NAME>   Write a preset's config.toml to the resolved path

Reads a statusline JSON payload on stdin; writes the rendered line to
stdout. See docs/specs/input-schema.md for the payload contract.
";

/// Parse an iterator of raw arguments. Pure: callers pass
/// `std::env::args_os().skip(1)` at startup and tests pass literals.
pub fn parse<I>(raw: I) -> Result<Action, lexopt::Error>
where
    I: IntoIterator,
    I::Item: Into<OsString>,
{
    let mut parser = lexopt::Parser::from_args(raw);
    let mut args = CliArgs::default();
    let mut positional: Vec<OsString> = Vec::new();
    let mut force = false;
    let mut plain = false;
    let mut no_doctor = false;
    while let Some(arg) = parser.next()? {
        match arg {
            Short('c') | Long("config") => {
                let value = parser.value()?;
                if value.is_empty() {
                    return Err(lexopt::Error::MissingValue {
                        option: Some("--config".to_string()),
                    });
                }
                args.config = Some(PathBuf::from(value));
            }
            Long("check-config") => {
                args.check_config = true;
            }
            Long("no-color") => {
                args.color_override = Some(ColorOverride::Never);
            }
            Long("force-color") => {
                args.color_override = Some(ColorOverride::Always);
            }
            Long("force") => {
                // Today `--force` is a `presets apply` modifier hoisted
                // to top-level parsing. If a second subcommand-specific
                // flag lands (e.g. `presets show --plain`), rewrite
                // `dispatch_subcommand` to own its own parser slice
                // rather than growing parallel top-level bools.
                force = true;
            }
            Long("plain") => {
                plain = true;
            }
            Long("no-doctor") => {
                no_doctor = true;
            }
            Short('h') | Long("help") => return Ok(Action::Help),
            Short('V') | Long("version") => return Ok(Action::Version),
            Value(v) => positional.push(v),
            _ => return Err(arg.unexpected()),
        }
    }
    let color_override_snapshot = args.color_override;
    let check_config_snapshot = args.check_config;
    let action = match dispatch_subcommand(
        &positional,
        force,
        plain,
        no_doctor,
        args.config.clone(),
        color_override_snapshot,
    )? {
        Some(action) => action,
        None => Action::Run(args),
    };
    // `--force` only has meaning under `presets apply`; accepting it
    // on any other action would encourage muscle-memory misuse.
    if force && !matches!(action, Action::PresetsApply { .. }) {
        return Err(lexopt::Error::UnexpectedOption("--force".to_string()));
    }
    // `--plain` only has meaning under `doctor`. Reject elsewhere so a
    // typo like `linesmith --plain themes list` doesn't silently no-op.
    if plain && !matches!(action, Action::Doctor { .. }) {
        return Err(lexopt::Error::UnexpectedOption("--plain".to_string()));
    }
    // `--no-doctor` only opts out of the post-`init` doctor run.
    // Accepting it on any other action would let a user pass
    // `--no-doctor doctor` (a doctor invocation that opts out of
    // itself) and other absurdities.
    if no_doctor && !matches!(action, Action::Init { .. }) {
        return Err(lexopt::Error::UnexpectedOption("--no-doctor".to_string()));
    }
    // `--check-config` only has meaning under `Run` (validates config,
    // exits before render). Every other subcommand would silently no-op it.
    if check_config_snapshot && !matches!(action, Action::Run(_)) {
        return Err(lexopt::Error::UnexpectedOption(
            "--check-config".to_string(),
        ));
    }
    // `--no-color` / `--force-color` only have meaning on the
    // subcommands that emit ANSI: the daily render path (`Run`) and
    // the TUI editor (`Config`). On the rest — Doctor renders glyph-
    // only; Init/Install/Uninstall write files; ThemesList/PresetsList
    // are plain-text catalogs — accepting the flag would be a silent
    // no-op a user piping through a colorized log capture might
    // mistake for a successful suppression. Whitelist the variants
    // that consume the override; everything else rejects.
    if let Some(override_) = color_override_snapshot {
        if !matches!(action, Action::Run(_) | Action::Config { .. }) {
            let flag = match override_ {
                ColorOverride::Never => "--no-color",
                ColorOverride::Always => "--force-color",
            };
            return Err(lexopt::Error::UnexpectedOption(flag.to_string()));
        }
    }
    Ok(action)
}

/// Recognize subcommands from positional args. Anything not matched
/// returns a clear error rather than silently falling through to `Run`.
fn dispatch_subcommand(
    positional: &[OsString],
    force: bool,
    plain: bool,
    no_doctor: bool,
    config: Option<PathBuf>,
    color_override: Option<ColorOverride>,
) -> Result<Option<Action>, lexopt::Error> {
    if positional.is_empty() {
        return Ok(None);
    }
    let first = positional[0].to_string_lossy();
    match first.as_ref() {
        "init" => {
            if positional.len() > 1 {
                return Err(lexopt::Error::UnexpectedValue {
                    option: "init".to_string(),
                    value: positional[1].to_string_lossy().to_string().into(),
                });
            }
            Ok(Some(Action::Init { config, no_doctor }))
        }
        "doctor" => {
            if positional.len() > 1 {
                return Err(lexopt::Error::UnexpectedValue {
                    option: "doctor".to_string(),
                    value: positional[1].to_string_lossy().to_string().into(),
                });
            }
            Ok(Some(Action::Doctor { plain, config }))
        }
        "config" => {
            if positional.len() > 1 {
                return Err(lexopt::Error::UnexpectedValue {
                    option: "config".to_string(),
                    value: positional[1].to_string_lossy().to_string().into(),
                });
            }
            Ok(Some(Action::Config {
                config,
                color_override,
            }))
        }
        "install" => {
            if positional.len() > 1 {
                return Err(lexopt::Error::UnexpectedValue {
                    option: "install".to_string(),
                    value: positional[1].to_string_lossy().to_string().into(),
                });
            }
            Ok(Some(Action::Install { config }))
        }
        "uninstall" => {
            if positional.len() > 1 {
                return Err(lexopt::Error::UnexpectedValue {
                    option: "uninstall".to_string(),
                    value: positional[1].to_string_lossy().to_string().into(),
                });
            }
            Ok(Some(Action::Uninstall))
        }
        "themes" => {
            let sub = positional.get(1).map(|s| s.to_string_lossy().into_owned());
            match sub.as_deref() {
                Some("list") if positional.len() == 2 => Ok(Some(Action::ThemesList)),
                Some(other) => Err(lexopt::Error::UnexpectedValue {
                    option: "themes".to_string(),
                    value: other.to_string().into(),
                }),
                None => Err(lexopt::Error::MissingValue {
                    option: Some("themes <subcommand>".to_string()),
                }),
            }
        }
        "presets" => {
            let sub = positional.get(1).map(|s| s.to_string_lossy().into_owned());
            match sub.as_deref() {
                Some("list") if positional.len() == 2 => Ok(Some(Action::PresetsList)),
                Some("apply") => {
                    let name = positional.get(2).ok_or(lexopt::Error::MissingValue {
                        option: Some("presets apply <NAME>".to_string()),
                    })?;
                    if positional.len() > 3 {
                        return Err(lexopt::Error::UnexpectedValue {
                            option: "presets apply".to_string(),
                            value: positional[3].to_string_lossy().to_string().into(),
                        });
                    }
                    Ok(Some(Action::PresetsApply {
                        name: name.to_string_lossy().into_owned(),
                        force,
                        config,
                    }))
                }
                Some(other) => Err(lexopt::Error::UnexpectedValue {
                    option: "presets".to_string(),
                    value: other.to_string().into(),
                }),
                None => Err(lexopt::Error::MissingValue {
                    option: Some("presets <subcommand>".to_string()),
                }),
            }
        }
        _ => Err(lexopt::Error::UnexpectedValue {
            option: "<subcommand>".to_string(),
            value: first.to_string().into(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Action, lexopt::Error> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn empty_args_returns_run_with_defaults() {
        let got = parse_args(&[]).expect("ok");
        assert_eq!(got, Action::Run(CliArgs::default()));
    }

    #[test]
    fn help_short_and_long_return_help_action() {
        assert_eq!(parse_args(&["-h"]).expect("ok"), Action::Help);
        assert_eq!(parse_args(&["--help"]).expect("ok"), Action::Help);
    }

    #[test]
    fn version_short_and_long_return_version_action() {
        assert_eq!(parse_args(&["-V"]).expect("ok"), Action::Version);
        assert_eq!(parse_args(&["--version"]).expect("ok"), Action::Version);
    }

    #[test]
    fn config_flag_captures_path() {
        let got = parse_args(&["--config", "/etc/linesmith.toml"]).expect("ok");
        assert_eq!(
            got,
            Action::Run(CliArgs {
                config: Some(PathBuf::from("/etc/linesmith.toml")),
                check_config: false,
                color_override: None,
            })
        );
    }

    #[test]
    fn config_short_flag_captures_path() {
        let got = parse_args(&["-c", "/etc/linesmith.toml"]).expect("ok");
        assert_eq!(
            got,
            Action::Run(CliArgs {
                config: Some(PathBuf::from("/etc/linesmith.toml")),
                check_config: false,
                color_override: None,
            })
        );
    }

    #[test]
    fn check_config_flag_sets_bool() {
        let got = parse_args(&["--check-config"]).expect("ok");
        assert_eq!(
            got,
            Action::Run(CliArgs {
                config: None,
                check_config: true,
                color_override: None,
            })
        );
    }

    #[test]
    fn check_config_composes_with_config_path() {
        let got = parse_args(&["--config", "custom.toml", "--check-config"]).expect("ok");
        assert_eq!(
            got,
            Action::Run(CliArgs {
                config: Some(PathBuf::from("custom.toml")),
                check_config: true,
                color_override: None,
            })
        );
    }

    #[test]
    fn unknown_flag_returns_error() {
        let err = parse_args(&["--nope"]).unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn config_without_value_returns_error() {
        let err = parse_args(&["--config"]).unwrap_err();
        // Expected: lexopt surfaces a "missing value" flavor of error;
        // lock the variant rather than the specific wording.
        assert!(matches!(err, lexopt::Error::MissingValue { .. }));
    }

    #[test]
    fn empty_config_value_is_rejected() {
        // `--config ""` can happen from shell expansions like
        // `--config "$MAYBE_UNSET"`; reject rather than silently
        // falling through to defaults with no explanation.
        let err = parse_args(&["--config", ""]).unwrap_err();
        assert!(matches!(err, lexopt::Error::MissingValue { .. }));
    }

    #[test]
    fn no_color_flag_sets_never_override() {
        let got = parse_args(&["--no-color"]).expect("ok");
        assert_eq!(
            got,
            Action::Run(CliArgs {
                config: None,
                check_config: false,
                color_override: Some(ColorOverride::Never),
            })
        );
    }

    #[test]
    fn force_color_flag_sets_always_override() {
        let got = parse_args(&["--force-color"]).expect("ok");
        assert_eq!(
            got,
            Action::Run(CliArgs {
                config: None,
                check_config: false,
                color_override: Some(ColorOverride::Always),
            })
        );
    }

    #[test]
    fn conflicting_color_flags_last_wins() {
        // lexopt assigns in order; last flag on the command line wins.
        // Users don't get an error when both flags appear — they get
        // the most recently specified intent. Pin both Action::Run
        // (CliArgs.color_override) and Action::Config (variant field)
        // since they capture the override at different sites.
        let got = parse_args(&["--no-color", "--force-color"]).expect("ok");
        match got {
            Action::Run(args) => assert_eq!(args.color_override, Some(ColorOverride::Always)),
            _ => panic!("expected Run action"),
        }
        let got = parse_args(&["--force-color", "--no-color"]).expect("ok");
        match got {
            Action::Run(args) => assert_eq!(args.color_override, Some(ColorOverride::Never)),
            _ => panic!("expected Run action"),
        }
        let got = parse_args(&["--no-color", "--force-color", "config"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: None,
                color_override: Some(ColorOverride::Always),
            }
        );
        let got = parse_args(&["--force-color", "--no-color", "config"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: None,
                color_override: Some(ColorOverride::Never),
            }
        );
        // Interleaved ordering: flag → subcommand → flag is the most
        // common shell-history shape. lexopt accepts positionals and
        // flags in any order, but a parser that switched to two-pass
        // parsing (flags first, positionals second) could regress
        // this. Pin both flags-before vs flags-around explicitly.
        let got = parse_args(&["--no-color", "config", "--force-color"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: None,
                color_override: Some(ColorOverride::Always),
            }
        );
    }

    #[test]
    fn themes_list_subcommand_parses() {
        assert_eq!(
            parse_args(&["themes", "list"]).expect("ok"),
            Action::ThemesList
        );
    }

    #[test]
    fn themes_without_subcommand_errors() {
        let err = parse_args(&["themes"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::MissingValue { .. }));
    }

    #[test]
    fn themes_with_unknown_subcommand_errors() {
        let err = parse_args(&["themes", "remove"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedValue { .. }));
    }

    #[test]
    fn unknown_top_level_subcommand_errors() {
        let err = parse_args(&["bogus"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedValue { .. }));
    }

    #[test]
    fn presets_list_subcommand_parses() {
        assert_eq!(
            parse_args(&["presets", "list"]).expect("ok"),
            Action::PresetsList
        );
    }

    #[test]
    fn presets_apply_subcommand_parses_name() {
        assert_eq!(
            parse_args(&["presets", "apply", "developer"]).expect("ok"),
            Action::PresetsApply {
                name: "developer".to_string(),
                force: false,
                config: None,
            }
        );
    }

    #[test]
    fn presets_apply_with_force_flag_sets_force() {
        let got = parse_args(&["presets", "apply", "developer", "--force"]).expect("ok");
        assert_eq!(
            got,
            Action::PresetsApply {
                name: "developer".to_string(),
                force: true,
                config: None,
            }
        );
    }

    #[test]
    fn presets_apply_without_name_errors() {
        let err = parse_args(&["presets", "apply"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::MissingValue { .. }));
    }

    #[test]
    fn presets_apply_with_extra_positional_errors() {
        let err = parse_args(&["presets", "apply", "developer", "extra"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedValue { .. }));
    }

    #[test]
    fn presets_without_subcommand_errors() {
        let err = parse_args(&["presets"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::MissingValue { .. }));
    }

    #[test]
    fn presets_with_unknown_subcommand_errors() {
        let err = parse_args(&["presets", "delete", "minimal"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedValue { .. }));
    }

    #[test]
    fn presets_apply_force_before_subcommand_also_parses() {
        // lexopt interleaves flags and positionals; pinning both
        // orderings prevents a parser regression that accepts only one.
        let got = parse_args(&["--force", "presets", "apply", "developer"]).expect("ok");
        assert_eq!(
            got,
            Action::PresetsApply {
                name: "developer".to_string(),
                force: true,
                config: None,
            }
        );
    }

    #[test]
    fn force_flag_rejected_outside_presets_apply() {
        // `--force` only has meaning for `presets apply`; using it with
        // any other action should error rather than silently no-op.
        for args in [
            vec!["--force"],
            vec!["--force", "themes", "list"],
            vec!["--force", "presets", "list"],
            vec!["--force", "--check-config"],
        ] {
            let err = parse_args(&args).unwrap_err();
            assert!(
                matches!(err, lexopt::Error::UnexpectedOption(ref s) if s == "--force"),
                "args {args:?} should reject --force, got {err:?}"
            );
        }
    }

    #[test]
    fn presets_apply_threads_config_flag_into_action() {
        let got = parse_args(&[
            "--config",
            "/tmp/custom.toml",
            "presets",
            "apply",
            "minimal",
        ])
        .expect("ok");
        assert_eq!(
            got,
            Action::PresetsApply {
                name: "minimal".to_string(),
                force: false,
                config: Some(PathBuf::from("/tmp/custom.toml")),
            }
        );
    }

    #[test]
    fn presets_apply_empty_string_name_still_parses_as_apply() {
        // Driver will reject empty name via the registry lookup; CLI
        // only validates shape here.
        assert_eq!(
            parse_args(&["presets", "apply", ""]).expect("ok"),
            Action::PresetsApply {
                name: String::new(),
                force: false,
                config: None,
            }
        );
    }

    #[test]
    fn init_subcommand_parses_with_no_config_override() {
        assert_eq!(
            parse_args(&["init"]).expect("ok"),
            Action::Init {
                config: None,
                no_doctor: false,
            }
        );
    }

    #[test]
    fn init_threads_config_flag_into_action() {
        let got = parse_args(&["--config", "/tmp/init.toml", "init"]).expect("ok");
        assert_eq!(
            got,
            Action::Init {
                config: Some(PathBuf::from("/tmp/init.toml")),
                no_doctor: false,
            }
        );
    }

    #[test]
    fn init_no_doctor_flag_sets_field() {
        assert_eq!(
            parse_args(&["init", "--no-doctor"]).expect("ok"),
            Action::Init {
                config: None,
                no_doctor: true,
            }
        );
    }

    #[test]
    fn init_no_doctor_before_subcommand_also_parses() {
        // lexopt interleaves flags and positionals; pin both
        // orderings so a parser regression that rejects one
        // ordering trips here.
        assert_eq!(
            parse_args(&["--no-doctor", "init"]).expect("ok"),
            Action::Init {
                config: None,
                no_doctor: true,
            }
        );
    }

    #[test]
    fn no_doctor_flag_rejected_outside_init() {
        // `--no-doctor` only opts out of init's post-write doctor
        // run; using it elsewhere (`linesmith --no-doctor doctor`,
        // bare `--no-doctor`) should error rather than silently
        // no-op.
        for args in [
            vec!["--no-doctor"],
            vec!["--no-doctor", "doctor"],
            vec!["--no-doctor", "themes", "list"],
            vec!["--no-doctor", "presets", "list"],
        ] {
            let err = parse_args(&args).unwrap_err();
            assert!(
                matches!(err, lexopt::Error::UnexpectedOption(ref s) if s == "--no-doctor"),
                "args {args:?} should reject --no-doctor, got {err:?}",
            );
        }
    }

    #[test]
    fn init_with_extra_positional_errors() {
        let err = parse_args(&["init", "minimal"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedValue { .. }));
    }

    #[test]
    fn init_rejects_force_flag() {
        // `--force` is `presets apply`-only; init's overwrite path goes
        // through the same y/N prompt without a force escape hatch.
        let err = parse_args(&["--force", "init"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedOption(ref s) if s == "--force"));
    }

    #[test]
    fn doctor_subcommand_parses_with_no_flags() {
        assert_eq!(
            parse_args(&["doctor"]).expect("ok"),
            Action::Doctor {
                plain: false,
                config: None,
            }
        );
    }

    #[test]
    fn doctor_subcommand_parses_plain_flag() {
        assert_eq!(
            parse_args(&["doctor", "--plain"]).expect("ok"),
            Action::Doctor {
                plain: true,
                config: None,
            }
        );
    }

    #[test]
    fn doctor_plain_before_subcommand_also_parses() {
        // lexopt interleaves flags and positionals; pinning both
        // orderings prevents a parser regression that accepts only one.
        assert_eq!(
            parse_args(&["--plain", "doctor"]).expect("ok"),
            Action::Doctor {
                plain: true,
                config: None,
            }
        );
    }

    #[test]
    fn doctor_threads_config_flag_into_action() {
        // Without this, `linesmith doctor --config /tmp/alt.toml`
        // silently discarded the path and reported on the default
        // config instead of the file the user explicitly asked
        // about.
        let got = parse_args(&["--config", "/tmp/alt.toml", "doctor"]).expect("ok");
        assert_eq!(
            got,
            Action::Doctor {
                plain: false,
                config: Some(PathBuf::from("/tmp/alt.toml")),
            }
        );
    }

    #[test]
    fn check_config_flag_rejected_outside_run() {
        // `--check-config` must error outside `Run` rather than silently
        // no-op: driver.rs::run_with_env only branches on check_config
        // for the render path.
        let cases: &[&[&str]] = &[
            &["--check-config", "init"],
            &["--check-config", "doctor"],
            &["--check-config", "config"],
            &["--check-config", "install"],
            &["--check-config", "uninstall"],
            &["--check-config", "themes", "list"],
            &["--check-config", "presets", "list"],
            &["--check-config", "presets", "apply", "minimal"],
        ];
        for argv in cases {
            let err = parse_args(argv).unwrap_err();
            assert!(
                matches!(err, lexopt::Error::UnexpectedOption(ref s) if s == "--check-config"),
                "args {argv:?} should reject --check-config, got {err:?}"
            );
        }
        let err = parse_args(&["doctor", "--check-config"]).unwrap_err();
        assert!(
            matches!(err, lexopt::Error::UnexpectedOption(ref s) if s == "--check-config"),
            "flag should reject after subcommand too, got {err:?}"
        );
        let err = parse_args(&["--check-config", "presets", "apply"]).unwrap_err();
        assert!(
            matches!(err, lexopt::Error::MissingValue { .. }),
            "missing-name error must win over check-config-rejection, got {err:?}"
        );
    }

    #[test]
    fn color_overrides_rejected_on_subcommands_without_color_output() {
        // Only `Run` (daily render path) and `Config` (TUI preview)
        // emit ANSI; the rest would silently no-op a color flag.
        // Reject so a user piping output through a colorized log
        // capture doesn't think they suppressed something that was
        // never there.
        let cases: &[&[&str]] = &[
            &["doctor"],
            &["init"],
            &["install"],
            &["uninstall"],
            &["themes", "list"],
            &["presets", "list"],
            &["presets", "apply", "minimal"],
        ];
        for argv in cases {
            for flag in ["--no-color", "--force-color"] {
                let mut full = vec![flag];
                full.extend_from_slice(argv);
                let err = parse_args(&full).unwrap_err();
                assert!(
                    matches!(err, lexopt::Error::UnexpectedOption(ref s) if s == flag),
                    "flag {flag} should be rejected on {argv:?}, got {err:?}"
                );
            }
        }
        // Spot-check: the gate fires regardless of where the flag
        // sits relative to the subcommand. The 14 cases above all
        // pass the flag before; pin one after to prove
        // ordering-invariance.
        let err = parse_args(&["doctor", "--no-color"]).unwrap_err();
        assert!(
            matches!(err, lexopt::Error::UnexpectedOption(ref s) if s == "--no-color"),
            "flag should reject after subcommand too, got {err:?}"
        );
        // Spot-check: `presets apply` without a NAME must fail at
        // dispatch (MissingValue) before the color-rejection gate
        // fires. A future refactor that flips the ordering would
        // surface "--no-color" instead of the real "missing NAME"
        // problem; this pins the precedence.
        let err = parse_args(&["--no-color", "presets", "apply"]).unwrap_err();
        assert!(
            matches!(err, lexopt::Error::MissingValue { .. }),
            "missing-name error must win over color-rejection, got {err:?}"
        );
    }

    #[test]
    fn doctor_combines_config_and_plain() {
        let got = parse_args(&["--config", "/tmp/alt.toml", "doctor", "--plain"]).expect("ok");
        assert_eq!(
            got,
            Action::Doctor {
                plain: true,
                config: Some(PathBuf::from("/tmp/alt.toml")),
            }
        );
    }

    #[test]
    fn doctor_with_extra_positional_errors() {
        let err = parse_args(&["doctor", "extra"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedValue { .. }));
    }

    #[test]
    fn plain_flag_rejected_outside_doctor() {
        // Mirror the surface coverage that `--force` gets so a typo
        // like `linesmith --plain themes list` doesn't silently no-op.
        for args in [
            vec!["--plain"],
            vec!["--plain", "themes", "list"],
            vec!["--plain", "presets", "list"],
            vec!["--plain", "presets", "apply", "minimal"],
            vec!["--plain", "init"],
            vec!["--plain", "--check-config"],
        ] {
            let err = parse_args(&args).unwrap_err();
            assert!(
                matches!(err, lexopt::Error::UnexpectedOption(ref s) if s == "--plain"),
                "args {args:?} should reject --plain, got {err:?}"
            );
        }
    }

    #[test]
    fn config_subcommand_parses_with_no_flags() {
        assert_eq!(
            parse_args(&["config"]).expect("ok"),
            Action::Config {
                config: None,
                color_override: None,
            }
        );
    }

    #[test]
    fn config_subcommand_threads_config_flag_into_action() {
        // `linesmith --config /tmp/alt.toml config` uses the supplied
        // path as the file the TUI loads / writes back to. Pin both
        // orderings so a parser regression that drops one trips here.
        let got = parse_args(&["--config", "/tmp/alt.toml", "config"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: Some(PathBuf::from("/tmp/alt.toml")),
                color_override: None,
            }
        );
        let got = parse_args(&["config", "--config", "/tmp/alt.toml"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: Some(PathBuf::from("/tmp/alt.toml")),
                color_override: None,
            }
        );
    }

    #[test]
    fn config_subcommand_threads_no_color_flag_into_action() {
        let got = parse_args(&["--no-color", "config"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: None,
                color_override: Some(ColorOverride::Never),
            }
        );
        let got = parse_args(&["config", "--no-color"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: None,
                color_override: Some(ColorOverride::Never),
            }
        );
    }

    #[test]
    fn config_subcommand_threads_force_color_flag_into_action() {
        let got = parse_args(&["--force-color", "config"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: None,
                color_override: Some(ColorOverride::Always),
            }
        );
        let got = parse_args(&["config", "--force-color"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: None,
                color_override: Some(ColorOverride::Always),
            }
        );
    }

    #[test]
    fn config_subcommand_threads_color_flag_and_config_flag_together() {
        let got = parse_args(&["--no-color", "--config", "/tmp/alt.toml", "config"]).expect("ok");
        assert_eq!(
            got,
            Action::Config {
                config: Some(PathBuf::from("/tmp/alt.toml")),
                color_override: Some(ColorOverride::Never),
            }
        );
    }

    #[test]
    fn config_subcommand_with_extra_positional_errors() {
        let err = parse_args(&["config", "extra"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedValue { .. }));
    }

    #[test]
    fn install_subcommand_parses_without_flags() {
        assert_eq!(
            parse_args(&["install"]).expect("ok"),
            Action::Install { config: None },
        );
    }

    #[test]
    fn install_subcommand_threads_config_flag_into_action() {
        // `linesmith --config /tmp/x.toml install` installs a
        // statusLine pointing at `linesmith --config /tmp/x.toml`,
        // not bare `linesmith`. Pin both orderings.
        let got = parse_args(&["--config", "/tmp/x.toml", "install"]).expect("ok");
        assert_eq!(
            got,
            Action::Install {
                config: Some(PathBuf::from("/tmp/x.toml")),
            },
        );
        let got = parse_args(&["install", "--config", "/tmp/x.toml"]).expect("ok");
        assert_eq!(
            got,
            Action::Install {
                config: Some(PathBuf::from("/tmp/x.toml")),
            },
        );
    }

    #[test]
    fn install_subcommand_with_extra_positional_errors() {
        let err = parse_args(&["install", "extra"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedValue { .. }));
    }

    #[test]
    fn uninstall_subcommand_parses_without_flags() {
        assert_eq!(parse_args(&["uninstall"]).expect("ok"), Action::Uninstall);
    }

    #[test]
    fn uninstall_subcommand_with_extra_positional_errors() {
        let err = parse_args(&["uninstall", "extra"]).unwrap_err();
        assert!(matches!(err, lexopt::Error::UnexpectedValue { .. }));
    }

    #[test]
    fn equals_style_config_value_parses() {
        // lexopt supports `--config=PATH`; pin so a parser swap
        // doesn't silently drop the shape users will try.
        let got = parse_args(&["--config=/custom.toml"]).expect("ok");
        assert_eq!(
            got,
            Action::Run(CliArgs {
                config: Some(PathBuf::from("/custom.toml")),
                check_config: false,
                color_override: None,
            })
        );
    }
}
