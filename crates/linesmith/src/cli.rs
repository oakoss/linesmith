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
    },
}

/// Help text. Kept short; full docs live at
/// <https://github.com/oakoss/linesmith>.
pub const HELP: &str = "\
linesmith — status line for Claude Code and other AI coding CLIs

USAGE:
    linesmith [OPTIONS]
    linesmith init
    linesmith themes list
    linesmith presets list
    linesmith presets apply <NAME> [--force]

OPTIONS:
    -c, --config <PATH>    Config file path (overrides default resolution)
        --check-config     Validate config and exit
        --no-color         Strip all color (equivalent to NO_COLOR=1)
        --force-color      Emit color even in non-TTY output
        --force            For `presets apply`: overwrite without confirmation
    -h, --help             Print this help text
    -V, --version          Print version

SUBCOMMANDS:
    init                   Interactive onboarding: pick a preset + theme,
                           write config.toml, print Claude Code snippet
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
            Short('h') | Long("help") => return Ok(Action::Help),
            Short('V') | Long("version") => return Ok(Action::Version),
            Value(v) => positional.push(v),
            _ => return Err(arg.unexpected()),
        }
    }
    let action = match dispatch_subcommand(&positional, force, args.config.clone())? {
        Some(action) => action,
        None => Action::Run(args),
    };
    // `--force` is a `presets apply` modifier; accepting it on any other
    // action would encourage muscle-memory misuse (e.g. `linesmith
    // --force themes list` looks plausible and would otherwise be a
    // no-op).
    if force && !matches!(action, Action::PresetsApply { .. }) {
        return Err(lexopt::Error::UnexpectedOption("--force".to_string()));
    }
    Ok(action)
}

/// Recognize subcommands from positional args. Anything not matched
/// returns a clear error rather than silently falling through to `Run`.
fn dispatch_subcommand(
    positional: &[OsString],
    force: bool,
    config: Option<PathBuf>,
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
            Ok(Some(Action::Init { config }))
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
        // the most recently specified intent.
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
            Action::Init { config: None }
        );
    }

    #[test]
    fn init_threads_config_flag_into_action() {
        let got = parse_args(&["--config", "/tmp/init.toml", "init"]).expect("ok");
        assert_eq!(
            got,
            Action::Init {
                config: Some(PathBuf::from("/tmp/init.toml"))
            }
        );
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
