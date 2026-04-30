//! `linesmith doctor` — diagnostic subcommand. Renders a categorized
//! health report with PASS / WARN / FAIL / SKIP severities, then exits
//! with a contract-defined code (any FAIL → 1; otherwise 0). Spec:
//! `docs/specs/doctor.md`.
//!
//! Encapsulation note: `CheckResult` hides its fields behind
//! constructors because severity and hint must agree (PASS forbids
//! hints; non-PASS requires them). `Category` and `Report` have no
//! cross-field invariants, so their fields stay public — same shape
//! as `std::process::Output`. If `Category` grows a non-empty-name
//! invariant or `Report` gains check-id-uniqueness, both gain
//! constructors and seal their fields.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;

/// One of four outcomes a check can report. See `docs/specs/doctor.md`
/// §Severity levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Severity {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl Severity {
    /// Unicode glyph (used unless `--plain`).
    #[must_use]
    pub fn unicode_glyph(self) -> &'static str {
        match self {
            Self::Pass => "✓",
            Self::Warn => "⚠",
            Self::Fail => "✗",
            Self::Skip => "·",
        }
    }

    /// ASCII glyph (used under `--plain`).
    #[must_use]
    pub fn ascii_glyph(self) -> &'static str {
        match self {
            Self::Pass => "OK",
            Self::Warn => "!!",
            Self::Fail => "XX",
            Self::Skip => "--",
        }
    }
}

/// One check's outcome. Construct via [`CheckResult::pass`],
/// [`CheckResult::warn`], [`CheckResult::fail`], or
/// [`CheckResult::skip`] — direct construction is not allowed so the
/// "PASS-with-hint" anti-state is unrepresentable. `id` is the stable
/// machine-readable key documented in `docs/specs/doctor.md` §JSON
/// output; reserved for v0.2 `--json` consumers but populated now so
/// adding a serializer later is purely additive.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CheckResult {
    pub(crate) severity: Severity,
    pub(crate) id: &'static str,
    pub(crate) label: String,
    /// Renders as an indented second line. PASS constructors don't
    /// accept this; non-PASS constructors require it.
    pub(crate) hint: Option<String>,
}

impl CheckResult {
    /// PASS check. Hints are not accepted — there's nothing to remediate.
    #[must_use]
    pub fn pass(id: &'static str, label: impl Into<String>) -> Self {
        Self {
            severity: Severity::Pass,
            id,
            label: label.into(),
            hint: None,
        }
    }

    /// WARN check. `hint` is required so the user has something
    /// actionable to read on the indented second line.
    #[must_use]
    pub fn warn(id: &'static str, label: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warn,
            id,
            label: label.into(),
            hint: Some(hint.into()),
        }
    }

    /// FAIL check. `hint` is required.
    #[must_use]
    pub fn fail(id: &'static str, label: impl Into<String>, hint: impl Into<String>) -> Self {
        Self {
            severity: Severity::Fail,
            id,
            label: label.into(),
            hint: Some(hint.into()),
        }
    }

    /// SKIP check. `reason` explains why the check didn't run (e.g.
    /// "config not loaded", "no plugins configured"); rendered on the
    /// indented second line.
    #[must_use]
    pub fn skip(id: &'static str, label: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            severity: Severity::Skip,
            id,
            label: label.into(),
            hint: Some(reason.into()),
        }
    }

    #[must_use]
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// Stable machine-readable identifier (e.g. `"env.stdout_tty"`).
    /// Reserved for v0.2 `--json` consumers and structured logging;
    /// not surfaced in the human renderer.
    #[must_use]
    pub fn id(&self) -> &'static str {
        self.id
    }

    /// Human-readable label rendered next to the severity glyph.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Remediation hint (for WARN / FAIL) or skip reason (for SKIP),
    /// or `None` for PASS. Rendered as an indented second line.
    #[must_use]
    pub fn hint(&self) -> Option<&str> {
        self.hint.as_deref()
    }
}

/// One named group of checks, e.g. `"Environment"`, `"Config"`.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Category {
    pub name: &'static str,
    pub checks: Vec<CheckResult>,
}

impl Category {
    #[must_use]
    pub fn new(name: &'static str, checks: Vec<CheckResult>) -> Self {
        Self { name, checks }
    }
}

/// Aggregated severity histogram, one count per [`Severity`] variant.
/// Named fields prevent positional-destructure mistakes when the
/// renderer / JSON serializer reads them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct SummaryCounts {
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub skip: usize,
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Report {
    pub linesmith_version: &'static str,
    pub categories: Vec<Category>,
}

impl Report {
    /// Any FAIL → 1, otherwise 0. Usage errors (bad flags) are handled
    /// by the parser and never reach this function — don't add a `2`
    /// branch here.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        if self
            .categories
            .iter()
            .flat_map(|c| &c.checks)
            .any(|c| c.severity == Severity::Fail)
        {
            1
        } else {
            0
        }
    }

    #[must_use]
    pub fn summary_counts(&self) -> SummaryCounts {
        let mut counts = SummaryCounts::default();
        for c in self.categories.iter().flat_map(|c| &c.checks) {
            match c.severity {
                Severity::Pass => counts.pass += 1,
                Severity::Warn => counts.warn += 1,
                Severity::Fail => counts.fail += 1,
                Severity::Skip => counts.skip += 1,
            }
        }
        counts
    }
}

/// Render mode for the report. `Plain` swaps Unicode glyphs for ASCII
/// and uses an ASCII summary separator.
///
/// **Plain-mode caveat:** the renderer guarantees no Unicode bytes in
/// the strings *it* emits (glyphs, separators, fixed labels). User-
/// supplied `label` and `hint` strings (paths like `~/café/config`,
/// gix branch names, parser errors) pass through verbatim. CI scripts
/// that need byte-clean ASCII should ASCII-fold their environment, not
/// rely on `--plain` to do it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum RenderMode {
    Default,
    Plain,
}

/// Render `report` to `out`. Tree-style: header + version, then each
/// category as a name line followed by indented checks. Non-PASS
/// checks emit an indented hint/reason line.
///
/// On I/O error, partial output may already have been flushed —
/// including a missing `Exit:` line. Callers that parse doctor output
/// must treat a truncated report (no `Exit:` line) as "I/O failed
/// mid-render," not as a successful run.
///
/// # Errors
///
/// Returns the first `io::Error` from a `writeln!` to `out`.
pub fn render(out: &mut dyn Write, report: &Report, mode: RenderMode) -> std::io::Result<()> {
    writeln!(out, "linesmith doctor (v{})", report.linesmith_version)?;
    for category in &report.categories {
        writeln!(out)?;
        writeln!(out, "{}", category.name)?;
        for check in &category.checks {
            let glyph = match mode {
                RenderMode::Default => check.severity.unicode_glyph(),
                RenderMode::Plain => check.severity.ascii_glyph(),
            };
            writeln!(out, "  {glyph} {}", check.label)?;
            if let Some(hint) = &check.hint {
                writeln!(out, "    -> {hint}")?;
            }
        }
    }
    writeln!(out)?;
    let counts = report.summary_counts();
    let sep = match mode {
        RenderMode::Default => "·",
        RenderMode::Plain => "/",
    };
    writeln!(
        out,
        "Summary: {} PASS {sep} {} WARN {sep} {} FAIL {sep} {} SKIP",
        counts.pass, counts.warn, counts.fail, counts.skip,
    )?;
    writeln!(out, "Exit: {}", report.exit_code())?;
    Ok(())
}

/// One environment variable's read state. Distinguishes the three
/// outcomes a check has to remediate differently: not present (set
/// it), set-to-empty (also set it), set-but-non-UTF-8 (the hint
/// "set $X" would be wrong — `$X` *is* set, it's just unreadable).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvVarState {
    Unset,
    Set(String),
    /// Variable is set but the value contains bytes that aren't valid
    /// UTF-8. Carries a lossy preview so the hint can quote the
    /// actual offending value rather than just say "missing".
    NonUtf8(String),
}

impl EnvVarState {
    fn snapshot(name: &str) -> Self {
        match std::env::var(name) {
            Ok(s) => Self::Set(s),
            Err(std::env::VarError::NotPresent) => Self::Unset,
            Err(std::env::VarError::NotUnicode(raw)) => {
                Self::NonUtf8(raw.to_string_lossy().into_owned())
            }
        }
    }

    /// Convenience accessor: `Some(s)` only when the variable is set
    /// AND non-empty AND valid UTF-8. Centralizes the
    /// `Some(s) if !s.is_empty()` predicate that every consumer would
    /// otherwise duplicate.
    #[must_use]
    pub fn nonempty(&self) -> Option<&str> {
        match self {
            Self::Set(s) if !s.is_empty() => Some(s),
            _ => None,
        }
    }
}

/// Snapshot of the process state the doctor inspects, taken once at
/// the call boundary and handed to [`build_report`]. Snapshotting
/// keeps checks pure and tests hermetic — no test races the live env
/// or sees mutations from a parallel test.
///
/// Deliberately does NOT derive `Clone`: `current_exe` carries an
/// `io::Error` which isn't `Clone`, and rebuilding the snapshot
/// (test fixture or `from_process()`) is the right pattern when a
/// caller wants a fresh one.
#[derive(Debug)]
#[non_exhaustive]
pub struct DoctorEnv {
    /// Raw `$HOME` env var. NOT the `dirs::home_dir()` resolved path
    /// — Unix `dirs` falls back to passwd entries which this snapshot
    /// does not capture. Slices that need the resolved home directory
    /// (e.g. Config) compute it themselves.
    pub home_env: EnvVarState,
    pub xdg_config_home: EnvVarState,
    pub xdg_cache_home: EnvVarState,
    pub term: EnvVarState,
    pub colorterm: EnvVarState,
    pub no_color: bool,
    /// `Ok(path)` when `std::env::current_exe()` succeeds; the error
    /// is preserved (rather than collapsed to `None`) so the binary-
    /// path check can render the actual cause — "permission denied"
    /// vs "broken symlink" vs "/proc unavailable" all need different
    /// remediation hints.
    pub current_exe: Result<PathBuf, std::io::Error>,
    pub stdout_is_terminal: bool,
    pub terminal_width_cells: Option<u16>,
}

impl DoctorEnv {
    /// Snapshot the live process env. Only the binary entry should
    /// call this; tests and any non-binary caller construct
    /// `DoctorEnv` manually to stay hermetic.
    #[must_use]
    pub fn from_process() -> Self {
        Self {
            home_env: EnvVarState::snapshot("HOME"),
            xdg_config_home: EnvVarState::snapshot("XDG_CONFIG_HOME"),
            xdg_cache_home: EnvVarState::snapshot("XDG_CACHE_HOME"),
            term: EnvVarState::snapshot("TERM"),
            colorterm: EnvVarState::snapshot("COLORTERM"),
            no_color: std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()),
            current_exe: std::env::current_exe(),
            stdout_is_terminal: std::io::stdout().is_terminal(),
            terminal_width_cells: terminal_size::terminal_size()
                .map(|(terminal_size::Width(w), _)| w),
        }
    }

    /// Baseline "everything healthy" fixture for tests. Mutate
    /// individual fields to exercise specific check branches.
    /// `cfg(test)` keeps it out of the public API entirely —
    /// external embedders can't fabricate a snapshot that lies
    /// about a real environment.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn healthy() -> Self {
        Self {
            home_env: EnvVarState::Set("/home/user".to_string()),
            xdg_config_home: EnvVarState::Unset,
            xdg_cache_home: EnvVarState::Unset,
            term: EnvVarState::Set("xterm-256color".to_string()),
            colorterm: EnvVarState::Set("truecolor".to_string()),
            no_color: false,
            current_exe: Ok(PathBuf::from("/usr/local/bin/linesmith")),
            stdout_is_terminal: true,
            terminal_width_cells: Some(120),
        }
    }
}

/// Build the diagnostic report. Catalog scope is tracked in
/// `docs/specs/doctor.md` §Check catalog.
#[must_use]
pub fn build_report(env: &DoctorEnv) -> Report {
    Report {
        linesmith_version: env!("CARGO_PKG_VERSION"),
        categories: vec![environment_category(env), self_category(env)],
    }
}

fn environment_category(env: &DoctorEnv) -> Category {
    Category::new(
        "Environment",
        vec![
            check_stdout_tty(env),
            check_terminal_width(env),
            check_term(env),
            check_no_color(env),
            check_home(env),
        ],
    )
}

fn check_stdout_tty(env: &DoctorEnv) -> CheckResult {
    if env.stdout_is_terminal {
        CheckResult::pass("env.stdout_tty", "Terminal is a tty (stdout fd 1)")
    } else {
        CheckResult::warn(
            "env.stdout_tty",
            "Stdout is not a tty (piped or redirected)",
            "use --plain for CI or log capture",
        )
    }
}

/// Single source for the env.terminal_width and env.term hint
/// strings — the WARN branches share the same remediation, so any
/// future wording change touches one place.
const TERMINAL_WIDTH_HINT: &str = "set $COLUMNS or use --plain; narrow widths may wrap output";
const TERM_HINT: &str = "set TERM=xterm-256color, or accept plain-mode fallback";

fn check_terminal_width(env: &DoctorEnv) -> CheckResult {
    match env.terminal_width_cells {
        Some(0) => CheckResult::warn(
            "env.terminal_width",
            "Terminal reported 0 cells (likely driver or terminfo bug)",
            "set $COLUMNS to override, or report the issue to your terminal emulator",
        ),
        Some(w) if w >= 40 => CheckResult::pass(
            "env.terminal_width",
            format!("Terminal width detected: {w} cells"),
        ),
        Some(w) => CheckResult::warn(
            "env.terminal_width",
            format!("Terminal width is {w} cells (narrow)"),
            TERMINAL_WIDTH_HINT,
        ),
        None => CheckResult::warn(
            "env.terminal_width",
            "Terminal width could not be detected",
            TERMINAL_WIDTH_HINT,
        ),
    }
}

fn check_term(env: &DoctorEnv) -> CheckResult {
    match &env.term {
        EnvVarState::Set(t) if !t.is_empty() && t != "dumb" => {
            CheckResult::pass("env.term", format!("$TERM={t}"))
        }
        EnvVarState::Set(t) if t == "dumb" => {
            CheckResult::warn("env.term", "$TERM=dumb", TERM_HINT)
        }
        EnvVarState::NonUtf8(raw) => CheckResult::warn(
            "env.term",
            format!("$TERM is set but not valid UTF-8 (lossy: {raw:?})"),
            "rewrite $TERM with a UTF-8 value (e.g. xterm-256color)",
        ),
        // Unset OR Set-to-empty.
        _ => CheckResult::warn("env.term", "$TERM is unset", TERM_HINT),
    }
}

fn check_no_color(env: &DoctorEnv) -> CheckResult {
    if env.no_color {
        CheckResult::pass(
            "env.no_color",
            "NO_COLOR is set — colors disabled per user preference",
        )
    } else {
        CheckResult::pass("env.no_color", "NO_COLOR is unset")
    }
}

fn check_home(env: &DoctorEnv) -> CheckResult {
    match &env.home_env {
        EnvVarState::Set(h) if !h.is_empty() => CheckResult::pass("env.home", format!("$HOME={h}")),
        EnvVarState::NonUtf8(raw) => CheckResult::fail(
            "env.home",
            format!("$HOME is set but not valid UTF-8 (lossy: {raw:?})"),
            "rewrite $HOME with a UTF-8 path",
        ),
        // Unset OR Set-to-empty: same remediation either way.
        _ => CheckResult::fail(
            "env.home",
            "$HOME is unset",
            "set $HOME to your user directory",
        ),
    }
}

fn self_category(env: &DoctorEnv) -> Category {
    Category::new("Self", vec![check_self_version(), check_binary_path(env)])
}

fn check_self_version() -> CheckResult {
    CheckResult::pass(
        "self.version",
        format!("linesmith {}", env!("CARGO_PKG_VERSION")),
    )
}

fn check_binary_path(env: &DoctorEnv) -> CheckResult {
    match &env.current_exe {
        Ok(p) => CheckResult::pass("self.binary_path", format!("Binary: {}", p.display())),
        // Preserve the underlying error so the user sees whether it
        // was a missing /proc, a deleted exe, a permission issue,
        // etc. Generic "reinstall" advice is only right for some of
        // those.
        Err(err) => CheckResult::warn(
            "self.binary_path",
            format!("Could not resolve binary path: {err}"),
            "std::env::current_exe failed (unusual; check sandbox / permissions or reinstall)",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyphs_within_a_mode_are_pairwise_distinct() {
        // Reader must distinguish PASS from FAIL at a glance — the
        // user-facing contract is intra-mode distinctness, not
        // cross-mode (which would still allow collisions like
        // PASS_unicode == SKIP_unicode).
        let unicode: Vec<_> = [
            Severity::Pass,
            Severity::Warn,
            Severity::Fail,
            Severity::Skip,
        ]
        .iter()
        .map(|s| s.unicode_glyph())
        .collect();
        let ascii: Vec<_> = [
            Severity::Pass,
            Severity::Warn,
            Severity::Fail,
            Severity::Skip,
        ]
        .iter()
        .map(|s| s.ascii_glyph())
        .collect();
        for (i, a) in unicode.iter().enumerate() {
            for (j, b) in unicode.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "unicode glyph collision: {a} == {b}");
                }
            }
        }
        for (i, a) in ascii.iter().enumerate() {
            for (j, b) in ascii.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "ascii glyph collision: {a} == {b}");
                }
            }
        }
    }

    #[test]
    fn check_result_constructors_round_trip_id_and_severity() {
        let p = CheckResult::pass("p.id", "label");
        assert_eq!(p.id(), "p.id");
        assert_eq!(p.severity(), Severity::Pass);
        assert!(p.hint.is_none(), "PASS must not carry a hint");

        let w = CheckResult::warn("w.id", "label", "do thing");
        assert_eq!(w.id(), "w.id");
        assert_eq!(w.severity(), Severity::Warn);
        assert_eq!(w.hint.as_deref(), Some("do thing"));

        let f = CheckResult::fail("f.id", "label", "fix");
        assert_eq!(f.severity(), Severity::Fail);
        assert_eq!(f.hint.as_deref(), Some("fix"));

        let s = CheckResult::skip("s.id", "label", "no $HOME");
        assert_eq!(s.severity(), Severity::Skip);
        assert_eq!(s.hint.as_deref(), Some("no $HOME"));
    }

    #[test]
    fn ascii_glyphs_contain_no_unicode() {
        for s in [
            Severity::Pass,
            Severity::Warn,
            Severity::Fail,
            Severity::Skip,
        ] {
            assert!(
                s.ascii_glyph().is_ascii(),
                "ascii glyph for {s:?} contains non-ASCII bytes",
            );
        }
    }

    fn fail_only_report() -> Report {
        Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "Self",
                vec![CheckResult::fail("self.broken", "broken", "fix it")],
            )],
        }
    }

    #[test]
    fn exit_code_is_one_on_any_fail() {
        assert_eq!(fail_only_report().exit_code(), 1);
    }

    #[test]
    fn exit_code_is_zero_on_warn_only() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "Self",
                vec![CheckResult::warn("self.warn", "degraded", "do thing")],
            )],
        };
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn exit_code_is_zero_on_all_pass() {
        assert_eq!(build_report(&DoctorEnv::healthy()).exit_code(), 0);
    }

    #[test]
    fn exit_code_skip_does_not_fail() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "Self",
                vec![CheckResult::skip("self.na", "n/a", "not applicable")],
            )],
        };
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn exit_code_is_one_when_fail_mixed_with_other_severities() {
        // Defends against `.any() → .all()` typo: every other exit-
        // code test uses a homogeneous report, so the `.any()` could
        // be silently swapped without detection.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![
                Category::new(
                    "A",
                    vec![
                        CheckResult::pass("a.ok", "ok"),
                        CheckResult::warn("a.warn", "degraded", "do thing"),
                    ],
                ),
                Category::new(
                    "B",
                    vec![
                        CheckResult::skip("b.na", "n/a", "skipped"),
                        CheckResult::fail("b.broken", "broken", "fix"),
                    ],
                ),
            ],
        };
        assert_eq!(r.exit_code(), 1);
    }

    #[test]
    fn exit_code_is_zero_when_no_fail_in_mixed_report() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![
                Category::new(
                    "A",
                    vec![
                        CheckResult::pass("a.ok", "ok"),
                        CheckResult::warn("a.warn", "degraded", "do thing"),
                    ],
                ),
                Category::new("B", vec![CheckResult::skip("b.na", "n/a", "skipped")]),
            ],
        };
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn summary_counts_aggregate_across_categories() {
        // Distinct counts per severity (2/3/1/4) so any positional
        // swap in the renderer's format string surfaces — equal
        // counts would let a `{} WARN` ↔ `{} FAIL` swap slip through.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![
                Category::new(
                    "A",
                    vec![
                        CheckResult::pass("a.1", "ok"),
                        CheckResult::pass("a.2", "ok"),
                        CheckResult::warn("a.3", "deg", "hint"),
                        CheckResult::warn("a.4", "deg", "hint"),
                        CheckResult::warn("a.5", "deg", "hint"),
                    ],
                ),
                Category::new(
                    "B",
                    vec![
                        CheckResult::fail("b.1", "broken", "fix"),
                        CheckResult::skip("b.2", "na", "reason"),
                        CheckResult::skip("b.3", "na", "reason"),
                        CheckResult::skip("b.4", "na", "reason"),
                        CheckResult::skip("b.5", "na", "reason"),
                    ],
                ),
            ],
        };
        let counts = r.summary_counts();
        assert_eq!(counts.pass, 2);
        assert_eq!(counts.warn, 3);
        assert_eq!(counts.fail, 1);
        assert_eq!(counts.skip, 4);

        // Pin the field-to-position mapping in the format string —
        // catches typos like `{} WARN` interpolating counts.fail.
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("2 PASS"), "wrong PASS count rendered:\n{s}");
        assert!(s.contains("3 WARN"), "wrong WARN count rendered:\n{s}");
        assert!(s.contains("1 FAIL"), "wrong FAIL count rendered:\n{s}");
        assert!(s.contains("4 SKIP"), "wrong SKIP count rendered:\n{s}");
    }

    #[test]
    fn plain_mode_emits_no_unicode() {
        let mut out = Vec::new();
        render(&mut out, &fail_only_report(), RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.is_ascii(), "plain output contains non-ASCII bytes:\n{s}");
    }

    #[test]
    fn plain_mode_summary_separator_is_ascii() {
        let mut out = Vec::new();
        render(
            &mut out,
            &build_report(&DoctorEnv::healthy()),
            RenderMode::Plain,
        )
        .expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("PASS / "),
            "plain summary should use '/' separator:\n{s}"
        );
    }

    #[test]
    fn default_mode_emits_unicode_glyphs() {
        let mut out = Vec::new();
        render(&mut out, &fail_only_report(), RenderMode::Default).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains('✗'), "expected ✗ glyph in default render:\n{s}");
    }

    #[test]
    fn default_mode_summary_separator_is_middle_dot() {
        let mut out = Vec::new();
        render(
            &mut out,
            &build_report(&DoctorEnv::healthy()),
            RenderMode::Default,
        )
        .expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("PASS · "),
            "default summary should use '·' separator:\n{s}"
        );
    }

    #[test]
    fn render_includes_summary_and_exit_lines() {
        let mut out = Vec::new();
        render(
            &mut out,
            &build_report(&DoctorEnv::healthy()),
            RenderMode::Plain,
        )
        .expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("Summary:"), "missing Summary line:\n{s}");
        assert!(s.contains("Exit: 0"), "missing Exit line:\n{s}");
    }

    #[test]
    fn fail_check_renders_hint_indented_after_fail_line() {
        let mut out = Vec::new();
        render(&mut out, &fail_only_report(), RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        // Intent: hint appears indented after the failure line.
        // Exact arrow / spacing is presentation detail covered by
        // snapshot tests in the integration suite.
        assert!(s.contains("fix it"), "hint text missing:\n{s}");
        let fail_idx = s.find("XX broken").expect("FAIL line missing");
        let hint_idx = s.find("fix it").expect("hint missing");
        assert!(hint_idx > fail_idx, "hint must follow the FAIL line");
    }

    #[test]
    fn warn_and_skip_checks_render_their_hint() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "X",
                vec![
                    CheckResult::warn("x.w", "deg", "warn-hint"),
                    CheckResult::skip("x.s", "na", "skip-reason"),
                ],
            )],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("warn-hint"), "WARN hint missing:\n{s}");
        assert!(s.contains("skip-reason"), "SKIP reason missing:\n{s}");
    }

    #[test]
    fn render_includes_category_header() {
        let mut out = Vec::new();
        render(
            &mut out,
            &build_report(&DoctorEnv::healthy()),
            RenderMode::Plain,
        )
        .expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("\nSelf\n"), "missing category header:\n{s}");
    }

    #[test]
    fn render_emits_blank_line_between_categories() {
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![
                Category::new("A", vec![CheckResult::pass("a.1", "a-line")]),
                Category::new("B", vec![CheckResult::pass("b.1", "b-line")]),
            ],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("a-line\n\nB\n"),
            "expected blank line separating categories:\n{s}"
        );
    }

    #[test]
    fn plain_mode_passes_user_supplied_unicode_through_verbatim() {
        // Contract pin: per docs/specs/doctor.md §plain caveat, --plain
        // guarantees no Unicode in *renderer-emitted* strings only.
        // User-supplied label/hint (paths like ~/café/config) pass
        // through verbatim. A future "fix" that ASCII-folds user
        // content would be a contract change, not a bug fix.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new(
                "X",
                vec![CheckResult::warn("x.cfg", "config at ~/café", "edit ☃")],
            )],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("~/café"), "user label must pass through:\n{s}");
        assert!(s.contains('☃'), "user hint must pass through:\n{s}");
    }

    #[test]
    fn empty_report_renders_summary_and_exits_zero() {
        // Guards against a future regression: code that asserts
        // non-empty categories would slip through every existing test
        // since none currently exercise the empty case.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(s.contains("Summary: 0 PASS"), "missing summary:\n{s}");
        assert!(s.contains("Exit: 0"), "missing exit line:\n{s}");
        assert_eq!(r.exit_code(), 0);
        assert_eq!(r.summary_counts(), SummaryCounts::default());
    }

    #[test]
    fn empty_category_renders_header_with_no_checks() {
        // E.g. Plugins category when no plugins are configured, or
        // Git category outside a repo. Header must still render so
        // the user can see the category was considered.
        let r = Report {
            linesmith_version: "0.1.0",
            categories: vec![Category::new("Plugins", vec![])],
        };
        let mut out = Vec::new();
        render(&mut out, &r, RenderMode::Plain).expect("render ok");
        let s = String::from_utf8(out).expect("utf8");
        assert!(
            s.contains("\nPlugins\n"),
            "missing empty-category header:\n{s}"
        );
    }

    #[test]
    fn label_and_hint_accessors_return_constructor_inputs() {
        let p = CheckResult::pass("p.id", "label-p");
        assert_eq!(p.label(), "label-p");
        assert_eq!(p.hint(), None);

        let w = CheckResult::warn("w.id", "label-w", "warn-hint");
        assert_eq!(w.label(), "label-w");
        assert_eq!(w.hint(), Some("warn-hint"));

        let f = CheckResult::fail("f.id", "label-f", "fail-hint");
        assert_eq!(f.label(), "label-f");
        assert_eq!(f.hint(), Some("fail-hint"));

        let s = CheckResult::skip("s.id", "label-s", "skip-reason");
        assert_eq!(s.label(), "label-s");
        assert_eq!(s.hint(), Some("skip-reason"));
    }

    // --- Environment / Self check categories ---

    fn find_check<'a>(report: &'a Report, id: &str) -> &'a CheckResult {
        report
            .categories
            .iter()
            .flat_map(|c| &c.checks)
            .find(|c| c.id() == id)
            .unwrap_or_else(|| panic!("check {id} not present in report"))
    }

    #[test]
    fn healthy_env_produces_only_pass_checks() {
        let r = build_report(&DoctorEnv::healthy());
        for check in r.categories.iter().flat_map(|c| &c.checks) {
            assert_eq!(
                check.severity(),
                Severity::Pass,
                "check {} should be PASS in healthy env, got {:?}",
                check.id(),
                check.severity(),
            );
        }
        assert_eq!(r.exit_code(), 0);
    }

    #[test]
    fn report_categories_are_environment_then_self() {
        let r = build_report(&DoctorEnv::healthy());
        let names: Vec<_> = r.categories.iter().map(|c| c.name).collect();
        // Order matters for the user — Environment is the most basic
        // surface to inspect, Self closes the report. New categories
        // slot between these two.
        assert_eq!(names, vec!["Environment", "Self"]);
    }

    #[test]
    fn home_unset_fails_and_promotes_exit_code() {
        let mut env = DoctorEnv::healthy();
        env.home_env = EnvVarState::Unset;
        let r = build_report(&env);
        let home = find_check(&r, "env.home");
        assert_eq!(home.severity(), Severity::Fail);
        assert!(home.hint().unwrap().contains("$HOME"));
        assert_eq!(r.exit_code(), 1);
    }

    #[test]
    fn home_empty_string_fails() {
        // Empty $HOME is the same shape as missing — `dirs::home_dir`
        // returns None on Unix when $HOME is empty, and the check
        // mirrors that.
        let mut env = DoctorEnv::healthy();
        env.home_env = EnvVarState::Set(String::new());
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.home").severity(), Severity::Fail);
    }

    #[test]
    fn home_non_utf8_fails_with_distinct_hint() {
        // Critical: the user-facing hint must NOT say "$HOME is unset"
        // when $HOME is in fact set but unreadable. Misleading
        // remediation makes the user fight a phantom problem.
        let mut env = DoctorEnv::healthy();
        env.home_env = EnvVarState::NonUtf8("/home/\u{FFFD}".to_string());
        let r = build_report(&env);
        let home = find_check(&r, "env.home");
        assert_eq!(home.severity(), Severity::Fail);
        assert!(
            home.label().contains("UTF-8"),
            "label should mention UTF-8: {}",
            home.label()
        );
        assert!(
            home.hint().unwrap().contains("UTF-8") || home.hint().unwrap().contains("rewrite"),
            "hint should point at the real fix: {:?}",
            home.hint()
        );
    }

    #[test]
    fn no_color_set_or_unset_both_pass() {
        for no_color in [true, false] {
            let mut env = DoctorEnv::healthy();
            env.no_color = no_color;
            let r = build_report(&env);
            assert_eq!(find_check(&r, "env.no_color").severity(), Severity::Pass);
        }
    }

    #[test]
    fn term_dumb_warns_not_fails() {
        let mut env = DoctorEnv::healthy();
        env.term = EnvVarState::Set("dumb".to_string());
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.term").severity(), Severity::Warn);
    }

    #[test]
    fn term_unset_warns() {
        let mut env = DoctorEnv::healthy();
        env.term = EnvVarState::Unset;
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.term").severity(), Severity::Warn);
    }

    #[test]
    fn term_empty_warns() {
        let mut env = DoctorEnv::healthy();
        env.term = EnvVarState::Set(String::new());
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.term").severity(), Severity::Warn);
    }

    #[test]
    fn term_non_utf8_warns_with_distinct_hint() {
        let mut env = DoctorEnv::healthy();
        env.term = EnvVarState::NonUtf8("xterm-\u{FFFD}".to_string());
        let r = build_report(&env);
        let term = find_check(&r, "env.term");
        assert_eq!(term.severity(), Severity::Warn);
        assert!(term.label().contains("UTF-8"));
    }

    #[test]
    fn stdout_not_a_tty_warns_not_fails() {
        // Critical contract: piped/CI stdout is WARN, never FAIL, so
        // `linesmith doctor --plain | tee log.txt` in CI doesn't gate
        // exit-1. Per spec §Cross-category short-circuits.
        let mut env = DoctorEnv::healthy();
        env.stdout_is_terminal = false;
        let r = build_report(&env);
        assert_eq!(find_check(&r, "env.stdout_tty").severity(), Severity::Warn);
        assert_eq!(r.exit_code(), 0, "non-tty must not promote exit code");
    }

    #[test]
    fn terminal_width_unknown_warns() {
        let mut env = DoctorEnv::healthy();
        env.terminal_width_cells = None;
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "env.terminal_width").severity(),
            Severity::Warn
        );
    }

    #[test]
    fn terminal_width_under_threshold_warns() {
        // Spec §Environment: Some((W, _)) with W < 40 → WARN.
        let mut env = DoctorEnv::healthy();
        env.terminal_width_cells = Some(39);
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "env.terminal_width").severity(),
            Severity::Warn
        );
    }

    #[test]
    fn terminal_width_at_threshold_passes() {
        let mut env = DoctorEnv::healthy();
        env.terminal_width_cells = Some(40);
        let r = build_report(&env);
        assert_eq!(
            find_check(&r, "env.terminal_width").severity(),
            Severity::Pass
        );
    }

    #[test]
    fn terminal_width_zero_warns_with_distinct_hint() {
        // 0 cells is qualitatively different from "narrow" — it's a
        // driver / terminfo bug. The hint must point at the terminal
        // emulator, not "set $COLUMNS".
        let mut env = DoctorEnv::healthy();
        env.terminal_width_cells = Some(0);
        let r = build_report(&env);
        let w = find_check(&r, "env.terminal_width");
        assert_eq!(w.severity(), Severity::Warn);
        let hint = w.hint().unwrap();
        assert!(
            hint.contains("terminal emulator") || hint.contains("driver"),
            "hint should distinguish driver bug from narrow width: {hint}"
        );
    }

    #[test]
    fn binary_path_resolves_passes() {
        let env = DoctorEnv::healthy();
        let r = build_report(&env);
        let bin = find_check(&r, "self.binary_path");
        assert_eq!(bin.severity(), Severity::Pass);
        assert!(bin.label().contains("Binary"));
    }

    #[test]
    fn binary_path_failure_preserves_io_error_in_label() {
        // Generic "current_exe failed" hides the cause. The label
        // must include the underlying io::Error so a user sees
        // whether it's permission-denied vs broken-symlink etc.
        let mut env = DoctorEnv::healthy();
        env.current_exe = Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "no access to /proc/self/exe",
        ));
        let r = build_report(&env);
        let bin = find_check(&r, "self.binary_path");
        assert_eq!(bin.severity(), Severity::Warn);
        assert!(
            bin.label().contains("no access to /proc/self/exe"),
            "io::Error message must surface in label: {}",
            bin.label()
        );
    }

    #[test]
    fn self_version_check_includes_crate_version() {
        let r = build_report(&DoctorEnv::healthy());
        let v = find_check(&r, "self.version");
        assert_eq!(v.severity(), Severity::Pass);
        assert!(v.label().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn doctor_env_from_process_does_not_panic() {
        // Smoke: the production env-snapshot path must not panic
        // regardless of the host's env state. (The actual values are
        // host-dependent so we don't assert on them.)
        let _ = DoctorEnv::from_process();
    }

    #[test]
    fn env_var_state_nonempty_filters_unset_empty_and_nonutf8() {
        assert_eq!(EnvVarState::Unset.nonempty(), None);
        assert_eq!(EnvVarState::Set(String::new()).nonempty(), None);
        assert_eq!(
            EnvVarState::NonUtf8("garbage".into()).nonempty(),
            None,
            "non-UTF-8 must not surface as Some — caller would treat the lossy preview as the real value"
        );
        assert_eq!(EnvVarState::Set("x".into()).nonempty(), Some("x"));
    }

    #[test]
    fn check_ids_follow_namespacing_convention() {
        // Spec §JSON output: ids are <category>.<check_name> in
        // snake_case. Extend the prefix allowlist as new categories
        // ship per spec §Check catalog — this test is a tripwire,
        // not a free pass.
        let r = build_report(&DoctorEnv::healthy());
        for check in r.categories.iter().flat_map(|c| &c.checks) {
            let id = check.id();
            assert!(id.contains('.'), "id `{id}` missing dotted namespace",);
            let prefix = id.split('.').next().unwrap();
            assert!(
                matches!(prefix, "env" | "self"),
                "id `{id}` has unknown category prefix `{prefix}`",
            );
            assert!(
                id.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '_' || c == '.'),
                "id `{id}` not snake_case",
            );
        }
    }
}
