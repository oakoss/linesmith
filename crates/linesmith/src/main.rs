use std::io::{self, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    if let Err(err) = linesmith::run(io::stdin().lock(), io::stdout().lock()) {
        // Stderr write may itself fail (closed pipe); swallow rather than panic.
        let _ = writeln!(io::stderr().lock(), "linesmith: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
