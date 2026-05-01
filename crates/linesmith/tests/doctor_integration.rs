//! Integration tests for `linesmith doctor` per
//! `docs/specs/doctor.md` §Testing strategy §Integration tests.
//!
//! Scope: end-to-end render-layer goldens, exit-code matrix, plain-
//! mode ASCII contract, token-redaction smoke, and width-tolerance
//! invariants. Per-check unit tests live alongside each category in
//! `crates/linesmith/src/doctor/mod.rs`; this file only locks the
//! aggregate envelope (header, separator, summary, exit lines) and
//! the CLI surface (unknown-flag → exit 2, help → exit 0).
//!
//! Reports here are built from the `pub` `Report` / `Category` /
//! `CheckResult` API rather than from a live `DoctorEnv`. The
//! `DoctorEnv::healthy()` test seam is `#[cfg(test)]`-only on
//! purpose, so the integration boundary stays at the data-shape
//! contract rather than the snapshot-builder internals.

use std::io::Cursor;

use linesmith::doctor::{self, Category, CheckResult, RenderMode, Report};

// --- Synthetic report builders ---------------------------------------------

fn report_all_pass() -> Report {
    Report::new(
        "0.1.0",
        vec![
            Category::new(
                "Environment",
                vec![
                    CheckResult::pass("env.stdout_tty", "Terminal is a tty (stdout fd 1)"),
                    CheckResult::pass("env.term", "TERM is set"),
                ],
            ),
            Category::new(
                "Self",
                vec![CheckResult::pass(
                    "self.binary_resolvable",
                    "linesmith binary resolvable",
                )],
            ),
        ],
    )
}

fn report_warn_only() -> Report {
    Report::new(
        "0.1.0",
        vec![Category::new(
            "Environment",
            vec![
                CheckResult::pass("env.stdout_tty", "Terminal is a tty (stdout fd 1)"),
                CheckResult::warn(
                    "env.terminal_width",
                    "Terminal width unknown",
                    "set COLUMNS or run from a real tty",
                ),
            ],
        )],
    )
}

fn report_skip_only() -> Report {
    Report::new(
        "0.1.0",
        vec![Category::new(
            "Config",
            vec![CheckResult::skip(
                "config.parses",
                "Config parses",
                "config not loaded",
            )],
        )],
    )
}

fn report_with_fail() -> Report {
    Report::new(
        "0.1.0",
        vec![
            Category::new(
                "Environment",
                vec![CheckResult::pass(
                    "env.stdout_tty",
                    "Terminal is a tty (stdout fd 1)",
                )],
            ),
            Category::new(
                "Credentials",
                vec![CheckResult::fail(
                    "credentials.resolved",
                    "Credentials not found",
                    "run `claude login` or place a token in ~/.claude/.credentials.json",
                )],
            ),
        ],
    )
}

fn report_fail_with_warnings() -> Report {
    Report::new(
        "0.1.0",
        vec![Category::new(
            "Mixed",
            vec![
                CheckResult::warn("mix.w", "warn-line", "warn-hint"),
                CheckResult::fail("mix.f", "fail-line", "fail-hint"),
            ],
        )],
    )
}

// --- Golden envelopes (plain + default render modes) ----------------------

#[test]
fn plain_render_full_envelope_matches_golden() {
    let mut out = Vec::new();
    doctor::render(&mut out, &report_all_pass(), RenderMode::Plain).expect("render ok");
    let actual = String::from_utf8(out).expect("utf8");
    let expected = "linesmith doctor (v0.1.0)\n\
                    \n\
                    Environment\n\
                    \x20\x20OK Terminal is a tty (stdout fd 1)\n\
                    \x20\x20OK TERM is set\n\
                    \n\
                    Self\n\
                    \x20\x20OK linesmith binary resolvable\n\
                    \n\
                    Summary: 3 PASS / 0 WARN / 0 FAIL / 0 SKIP\n\
                    Exit: 0\n";
    assert_eq!(
        actual, expected,
        "plain envelope drift; actual:\n{actual}---\nexpected:\n{expected}"
    );
}

#[test]
fn default_render_full_envelope_matches_golden() {
    let mut out = Vec::new();
    doctor::render(&mut out, &report_all_pass(), RenderMode::Default).expect("render ok");
    let actual = String::from_utf8(out).expect("utf8");
    let expected = "linesmith doctor (v0.1.0)\n\
                    \n\
                    Environment\n\
                    \x20\x20\u{2713} Terminal is a tty (stdout fd 1)\n\
                    \x20\x20\u{2713} TERM is set\n\
                    \n\
                    Self\n\
                    \x20\x20\u{2713} linesmith binary resolvable\n\
                    \n\
                    Summary: 3 PASS \u{00b7} 0 WARN \u{00b7} 0 FAIL \u{00b7} 0 SKIP\n\
                    Exit: 0\n";
    assert_eq!(
        actual, expected,
        "default envelope drift; actual:\n{actual}---\nexpected:\n{expected}"
    );
}

#[test]
fn plain_render_failure_envelope_matches_golden() {
    let mut out = Vec::new();
    doctor::render(&mut out, &report_with_fail(), RenderMode::Plain).expect("render ok");
    let actual = String::from_utf8(out).expect("utf8");
    let expected = "linesmith doctor (v0.1.0)\n\
                    \n\
                    Environment\n\
                    \x20\x20OK Terminal is a tty (stdout fd 1)\n\
                    \n\
                    Credentials\n\
                    \x20\x20XX Credentials not found\n\
                    \x20\x20\x20\x20-> run `claude login` or place a token in ~/.claude/.credentials.json\n\
                    \n\
                    Summary: 1 PASS / 0 WARN / 1 FAIL / 0 SKIP\n\
                    Exit: 1\n";
    assert_eq!(
        actual, expected,
        "plain failure envelope drift; actual:\n{actual}---\nexpected:\n{expected}"
    );
}

// --- Exit-code matrix per spec §Exit code contract --------------------------

#[test]
fn exit_code_all_pass_is_zero() {
    assert_eq!(report_all_pass().exit_code(), 0);
}

#[test]
fn exit_code_warn_only_is_zero() {
    assert_eq!(report_warn_only().exit_code(), 0);
}

#[test]
fn exit_code_skip_only_is_zero() {
    assert_eq!(report_skip_only().exit_code(), 0);
}

#[test]
fn exit_code_any_fail_is_one() {
    assert_eq!(report_with_fail().exit_code(), 1);
}

#[test]
fn exit_code_fail_with_warnings_is_one() {
    assert_eq!(report_fail_with_warnings().exit_code(), 1);
}

// --- Plain-mode ASCII contract per spec §plain caveat -----------------------
//
// Renderer-emitted strings (glyphs, separators, fixed labels, summary)
// are guaranteed ASCII under `--plain`. User-supplied label/hint
// content passes through verbatim — that's a deliberate spec carve-out
// (see `crates/linesmith/src/doctor/mod.rs` ::tests
// `plain_mode_passes_user_supplied_unicode_through_verbatim`). These
// tests pin the "renderer strings only" half of the contract.

#[test]
fn plain_render_emits_only_ascii_when_user_strings_are_ascii() {
    for (name, report) in [
        ("all_pass", report_all_pass()),
        ("warn_only", report_warn_only()),
        ("skip_only", report_skip_only()),
        ("with_fail", report_with_fail()),
        ("fail_with_warnings", report_fail_with_warnings()),
    ] {
        let mut out = Vec::new();
        doctor::render(&mut out, &report, RenderMode::Plain).expect("render ok");
        for (idx, &b) in out.iter().enumerate() {
            assert!(
                b.is_ascii(),
                "scenario {name}: non-ASCII byte 0x{b:02x} at offset {idx}"
            );
        }
    }
}

#[test]
fn plain_summary_separator_is_ascii_slash() {
    let mut out = Vec::new();
    doctor::render(&mut out, &report_all_pass(), RenderMode::Plain).expect("render ok");
    let s = String::from_utf8(out).expect("utf8");
    assert!(
        s.contains(" / "),
        "plain summary separator should be ' / ':\n{s}"
    );
    assert!(
        !s.contains(" \u{00b7} "),
        "plain summary should not contain the default '·' separator:\n{s}"
    );
}

#[test]
fn default_summary_separator_is_unicode_dot() {
    let mut out = Vec::new();
    doctor::render(&mut out, &report_all_pass(), RenderMode::Default).expect("render ok");
    let s = String::from_utf8(out).expect("utf8");
    assert!(
        s.contains(" \u{00b7} "),
        "default summary separator should be ' · ':\n{s}"
    );
}

// --- Token redaction smoke -------------------------------------------------
//
// The credentials category is constructed via the `pub` `CheckResult`
// API (label + hint, never a token). This test pins that the rendered
// output for any failure path never carries token-shaped substrings.
// Real bearer tokens flow through `data_context::credentials` and
// stop at the `CredentialsSummary` boundary — see
// `doctor/snapshot.rs::snapshot_credentials`.

#[test]
fn render_with_credentials_check_emits_no_token_substrings() {
    let r = Report::new(
        "0.1.0",
        vec![Category::new(
            "Credentials",
            vec![
                CheckResult::pass(
                    "credentials.resolved",
                    "Credentials resolved (claude_legacy: ~/.claude/.credentials.json)",
                ),
                CheckResult::pass("credentials.scopes", "Scopes: user:inference, user:profile"),
            ],
        )],
    );
    let mut out = Vec::new();
    doctor::render(&mut out, &r, RenderMode::Plain).expect("render ok");
    let s = String::from_utf8(out).expect("utf8");

    for needle in ["sk-ant-", "sk-or-", "Bearer ", "ya29.", "oat_"] {
        assert!(
            !s.contains(needle),
            "credentials render leaked token-shaped substring {needle:?}:\n{s}"
        );
    }
}

// --- Width / box-drawing invariants ----------------------------------------
//
// The renderer is line-based and width-agnostic by design (see
// `docs/specs/doctor.md` §Output format). A future refactor that
// introduced terminal-width-aware wrapping or box-drawing glyphs
// would be a deliberate spec change; these tests pin the current
// contract so the change has to be intentional.

#[test]
fn render_with_long_labels_emits_no_unicode_box_drawing_glyphs() {
    let long_label: String = "x".repeat(500);
    let long_hint: String = "y".repeat(500);
    let r = Report::new(
        "0.1.0",
        vec![Category::new(
            "X",
            vec![CheckResult::warn("x.long", long_label, long_hint)],
        )],
    );
    for mode in [RenderMode::Default, RenderMode::Plain] {
        let mut out = Vec::new();
        doctor::render(&mut out, &r, mode).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        for box_char in [
            '\u{2500}', '\u{2502}', '\u{250c}', '\u{2510}', '\u{2514}', '\u{2518}', '\u{251c}',
            '\u{2524}', '\u{252c}', '\u{2534}', '\u{253c}', '\u{256d}', '\u{256e}', '\u{256f}',
            '\u{2570}',
        ] {
            assert!(
                !s.contains(box_char),
                "{mode:?} render emitted box-drawing glyph U+{:04X}:\n{s}",
                box_char as u32
            );
        }
    }
}

// --- CLI surface: unknown flag → exit 2, help → exit 0 ----------------------
//
// `doctor`-specific flags (`--plain`) reject when supplied to other
// subcommands; unknown flags supplied to `doctor` are routed through
// the same `cli::parse` error path. Both must produce exit-2 (usage
// error), distinct from FAIL's exit-1.

#[test]
fn cli_main_with_unknown_doctor_flag_exits_two() {
    let env = linesmith::CliEnv::for_tests();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = linesmith::cli_main(
        ["linesmith", "doctor", "--bogus-flag"],
        Cursor::new(b""),
        &mut stdout,
        &mut stderr,
        &env,
    );
    let stderr_text = String::from_utf8_lossy(&stderr);
    assert_eq!(
        code,
        2,
        "unknown flag should exit-2; stdout: {:?}, stderr: {:?}",
        String::from_utf8_lossy(&stdout),
        stderr_text,
    );
    // Pin the actual UX contract: the offending flag is named in the
    // error so the user can see what was rejected. A future change
    // that drops the flag echo (real UX regression) must fail this
    // test; "stderr contains --help" is a weaker check satisfied by
    // any message that mentions the help banner anywhere.
    assert!(
        stderr_text.contains("bogus-flag"),
        "stderr should echo the offending flag: {stderr_text}"
    );
}

#[test]
fn cli_main_with_help_flag_exits_zero_and_lists_doctor() {
    let env = linesmith::CliEnv::for_tests();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = linesmith::cli_main(
        ["linesmith", "--help"],
        Cursor::new(b""),
        &mut stdout,
        &mut stderr,
        &env,
    );
    assert_eq!(code, 0, "stderr: {}", String::from_utf8_lossy(&stderr));
    let help = String::from_utf8_lossy(&stdout);
    assert!(help.contains("doctor"), "help missing 'doctor':\n{help}");
    assert!(help.contains("--plain"), "help missing '--plain':\n{help}");
    assert!(
        help.contains("--no-doctor"),
        "help missing '--no-doctor':\n{help}"
    );
}

#[test]
fn cli_main_with_plain_flag_outside_doctor_exits_two() {
    let env = linesmith::CliEnv::for_tests();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = linesmith::cli_main(
        ["linesmith", "--plain", "themes", "list"],
        Cursor::new(b""),
        &mut stdout,
        &mut stderr,
        &env,
    );
    assert_eq!(
        code,
        2,
        "--plain outside doctor should exit-2; stdout: {:?}, stderr: {:?}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr),
    );
}
