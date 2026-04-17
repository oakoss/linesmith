//! linesmith: Rust status line for Claude Code and other AI coding CLIs.
//!
//! `run` reads a JSON payload from a `Read`, renders a status line, and
//! writes the result to a `Write`. The binary in `main.rs` wires this to
//! stdin/stdout. See `docs/specs/` for the segment, theme, config, and
//! plugin contracts.

pub mod input;
pub mod segments;

use std::io::{self, Read, Write};

/// Read a JSON payload from `reader`, render a status line, and write it
/// to `writer`. Parse failures render a `?` marker to `writer` and log
/// detail to stderr; only I/O failures surface as errors.
///
/// # Errors
///
/// Returns an `io::Error` if reading from `reader` or writing to `writer`
/// fails. Parse errors are handled internally.
pub fn run(mut reader: impl Read, mut writer: impl Write) -> io::Result<()> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;

    let ctx = match input::parse(&buf) {
        Ok(ctx) => ctx,
        Err(err) => {
            // Diagnostic to stderr, ignore write failures (broken pipe
            // must not panic a statusline).
            let _ = writeln!(io::stderr().lock(), "linesmith: parse: {err}");
            return writeln!(writer, "?");
        }
    };

    let built_in: Vec<Box<dyn segments::Segment>> = vec![
        Box::new(segments::model::ModelSegment),
        Box::new(segments::context_window::ContextWindowSegment),
        Box::new(segments::workspace::WorkspaceSegment),
    ];

    let parts: Vec<String> = built_in
        .iter()
        .filter_map(|seg| seg.render(&ctx))
        .map(|rendered| rendered.text)
        .collect();

    writeln!(writer, "{}", parts.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
