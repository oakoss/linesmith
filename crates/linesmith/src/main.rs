use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let env = linesmith::CliEnv::from_process();
    let code = linesmith::cli_main(
        std::env::args_os().skip(1),
        io::stdin().lock(),
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
        &env,
    );
    ExitCode::from(code)
}
