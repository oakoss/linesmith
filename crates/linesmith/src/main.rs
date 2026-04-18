use linesmith::{cli, config};
use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let action = match cli::parse(std::env::args_os().skip(1)) {
        Ok(a) => a,
        Err(err) => {
            let mut stderr = io::stderr().lock();
            let _ = writeln!(stderr, "linesmith: {err}");
            let _ = writeln!(stderr, "Try --help for usage.");
            return ExitCode::from(2);
        }
    };

    match action {
        cli::Action::Help => {
            let _ = write!(io::stdout().lock(), "{}", cli::HELP);
            ExitCode::SUCCESS
        }
        cli::Action::Version => {
            let _ = writeln!(
                io::stdout().lock(),
                "linesmith {}",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        cli::Action::Run(args) => run(args),
    }
}

fn run(args: cli::CliArgs) -> ExitCode {
    let resolved = config::detect_config_path(args.config.clone());
    let (config, load_error) = load_config(resolved.as_ref());

    if args.check_config {
        return check_config(resolved.as_ref(), config.as_ref(), load_error);
    }

    let segments = linesmith::build_segments(config.as_ref(), |msg| {
        let _ = writeln!(io::stderr().lock(), "linesmith: {msg}");
    });

    let width = linesmith::detect_terminal_width();
    if let Err(err) = linesmith::run_with_segments_and_width(
        io::stdin().lock(),
        io::stdout().lock(),
        &segments,
        width,
    ) {
        let _ = writeln!(io::stderr().lock(), "linesmith: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

/// Load the config at `resolved` if present. Missing files are silent
/// for implicit paths (first-run users) but warn for explicit paths
/// (the user asked for a specific file and it wasn't there).
fn load_config(
    resolved: Option<&config::ConfigPath>,
) -> (Option<config::Config>, Option<config::ConfigError>) {
    let Some(cp) = resolved else {
        return (None, None);
    };
    match config::Config::load(&cp.path) {
        Ok(Some(c)) => (Some(c), None),
        Ok(None) => {
            if cp.explicit {
                let _ = writeln!(
                    io::stderr().lock(),
                    "linesmith: config not found at {}",
                    cp.path.display()
                );
            }
            (None, None)
        }
        Err(e) => {
            let _ = writeln!(io::stderr().lock(), "linesmith: {e}");
            (None, Some(e))
        }
    }
}

fn check_config(
    resolved: Option<&config::ConfigPath>,
    config: Option<&config::Config>,
    load_error: Option<config::ConfigError>,
) -> ExitCode {
    let mut stderr = io::stderr().lock();
    // `--check-config` is the CI / editor contract for strict
    // validation; if we can't even resolve a config path, that's a
    // failure rather than a "use defaults" fallback.
    let Some(cp) = resolved else {
        let _ = writeln!(
            stderr,
            "linesmith: no config path (HOME and XDG_CONFIG_HOME both unset, no --config)"
        );
        return ExitCode::from(1);
    };
    if load_error.is_some() {
        let _ = writeln!(stderr, "linesmith: config invalid ({})", cp.path.display());
        return ExitCode::from(1);
    }
    let Some(cfg) = config else {
        let _ = writeln!(
            stderr,
            "linesmith: no config at {}; using built-in defaults",
            cp.path.display()
        );
        return ExitCode::SUCCESS;
    };

    let mut warn_count = 0_usize;
    let _ = linesmith::build_segments(Some(cfg), |msg| {
        let _ = writeln!(stderr, "linesmith: {msg}");
        warn_count += 1;
    });
    let _ = writeln!(stderr, "linesmith: config ok ({})", cp.path.display());
    if warn_count > 0 {
        let _ = writeln!(stderr, "linesmith: {warn_count} warning(s)");
    }
    ExitCode::SUCCESS
}
