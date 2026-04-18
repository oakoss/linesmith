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
/// `Help` and `Version` are meta-commands that print and exit.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Run(CliArgs),
    Help,
    Version,
}

/// Help text. Kept short; full docs live at
/// <https://github.com/oakoss/linesmith>.
pub const HELP: &str = "\
linesmith — status line for Claude Code and other AI coding CLIs

USAGE:
    linesmith [OPTIONS]

OPTIONS:
    -c, --config <PATH>    Config file path (overrides default resolution)
        --check-config     Validate config and exit
        --no-color         Strip all color (equivalent to NO_COLOR=1)
        --force-color      Emit color even in non-TTY output
    -h, --help             Print this help text
    -V, --version          Print version

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
            Short('h') | Long("help") => return Ok(Action::Help),
            Short('V') | Long("version") => return Ok(Action::Version),
            _ => return Err(arg.unexpected()),
        }
    }
    Ok(Action::Run(args))
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
