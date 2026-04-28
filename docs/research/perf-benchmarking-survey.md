# CI perf benchmarking for small-to-medium Rust CLIs

- Date: 2026-04-27
- Author: Jace Babin (w/ Claude Code research agent)
- Scope: Survey what comparable Rust CLI projects actually gate on for perf in CI, and what tooling produces meaningful signal vs theatre. Drives the decision on how linesmith should defend its `<20ms cold start` claim now that the Criterion + bash-budget approach was flagged as low-signal.

## Executive summary

**Almost no small-to-medium Rust CLI runs a perf gate in CI.** Of seven projects surveyed (starship, ripgrep, fd, bat, hyperfine, CCometixLine, claudia-statusline), none has an automated CI perf regression gate. Only the largest projects (uv at 84k stars, ruff at 47k) gate, and they both converge on the same architecture: **CodSpeed + Criterion-shaped benches, with separate walltime and simulation/instruction-count modes**, run on dedicated bare-metal runners. The lesson is two-edged: (1) the entire community has tacitly concluded that GitHub-hosted-runner Criterion gating is not worth the maintenance, and (2) the projects that do gate solve the variance problem with paid infrastructure, not cleverer thresholds. For linesmith, the realistic options are run a CodSpeed-style instrumented bench on every PR (precise but instruction-count, not wall-clock), keep Criterion local-only with `--save-baseline`/`critcmp` for engineering use, and replace the bash-budget script with a hyperfine-driven smoke check that reports rather than gates.

## Question

What do comparable Rust CLI projects do to defend perf claims in CI? Which approaches produce real signal vs ceremony? What should linesmith do about the `<20ms cold start` claim?

## Sources

Direct inspection of `.github/workflows/`, `Cargo.toml`, and `benches/` via the GitHub API on 2026-04-27:

- starship @ <https://github.com/starship/starship> (56.9k★, last push 2026-04-28). Workflows: `workflow.yml`, `release.yml`, etc. No `benches/` directory; no `criterion`/`divan`/`iai`/`codspeed` in `Cargo.toml`; no `bench` keyword in `workflow.yml`.
- ripgrep @ <https://github.com/BurntSushi/ripgrep> (63.0k★). Workflows: `ci.yml`, `release.yml`. No bench step in CI. `benchsuite/runs/` contains hyperfine-style runs done **manually by the maintainer** — latest entry `2022-12-16-archlinux-duff` (over 3 years old).
- fd @ <https://github.com/sharkdp/fd>, bat @ <https://github.com/sharkdp/bat>, hyperfine @ <https://github.com/sharkdp/hyperfine> (28.0k★). All three sharkdp projects: single `CICD.yml` workflow. No bench step. fd's `Cargo.toml` excludes `/benchmarks/*` from the package and has no Criterion dep. Hyperfine itself has **no internal benchmark suite**.
- CCometixLine @ <https://github.com/Haleclipse/CCometixLine> (2.8k★, linesmith's direct competitor). Workflows: `ci.yml`, `release.yml`. No bench step. No `benches/` directory. No perf-related dev-deps in `Cargo.toml`.
- claudia-statusline (no canonical fork; two small forks `hagan/claudia-statusline` and `taskx6004/claudia-statusline`). Both lack a CI perf gate.
- uv @ <https://github.com/astral-sh/uv> (84.0k★). Dedicated `bench.yml` workflow ([source](https://github.com/astral-sh/uv/blob/main/.github/workflows/bench.yml)) using CodSpeed in two modes. `crates/uv-bench/benches/uv.rs` is plain Criterion (`use criterion::{Criterion, criterion_group, ...}`) with `harness = false` so CodSpeed can swap the runner.
- ruff @ <https://github.com/astral-sh/ruff> (47.3k★). `ci.yaml` includes `benchmarks-instrumented-ruff` job ([snippet](https://github.com/astral-sh/ruff/blob/main/.github/workflows/ci.yaml)) gated on a `determine_changes` job that only runs benches when bench code or relevant crates change. Uses `cargo codspeed build -m simulation -m memory`. `Cargo.toml` workspace has both `codspeed-criterion-compat = "4.4.1"` and `divan = { package = "codspeed-divan-compat", version = "4.4.1" }`.

Tool maturity (GitHub API, 2026-04-27):

- Criterion (`bheisler/criterion.rs`): 5,466★, 222 open issues, last push 2026-04-23. Active.
- Divan (`nvzqz/divan`): 1,390★, last push **2025-04-17** (~1 year stale).
- iai-callgrind (`iai-callgrind/iai-callgrind`): 250★, last push 2026-04-27, latest release same day. Active but small audience.
- Bencher (`bencherdev/bencher`): 829★, last push 2026-04-24. Active.
- codspeed-rust (`CodSpeedHQ/codspeed-rust`): 61★, last push 2026-04-24. Active. (The shim crates `codspeed-criterion-compat` / `codspeed-divan-compat` live here.)

Documentation snapshots: CodSpeed walltime vs simulation docs at <https://codspeed.io/docs/instruments/walltime> (confirms walltime needs `codspeed-macro` bare-metal runners, free for public repos but org-gated); Bencher learn-Criterion guide at <https://bencher.dev/learn/benchmarking/rust/criterion/>.

Notable iai-callgrind adopters (from `Cargo.toml` code search): SpacetimeDB, GraphiteEditor/Graphite, scryer-prolog, microsoft/DiskANN, paradigmxyz/solar — research-grade and infra projects, not user-facing CLIs.

## Findings

### What surveyed projects actually do

| Project      | ★    | CI perf gate?                                                  | What they bench                                             | Tooling                                                                    |
| ------------ | ---- | -------------------------------------------------------------- | ----------------------------------------------------------- | -------------------------------------------------------------------------- |
| starship     | 57k  | No                                                             | Nothing in-repo                                             | None                                                                       |
| ripgrep      | 63k  | No (manual runs, last 2022)                                    | End-to-end search throughput on real corpora vs grep/ag/ack | hyperfine, run by maintainer                                               |
| fd           | —    | No                                                             | Excluded from package; no internal suite                    | None                                                                       |
| bat          | —    | No                                                             | Nothing in-repo                                             | None                                                                       |
| hyperfine    | 28k  | No                                                             | Nothing — it _is_ the benchmark tool                        | None                                                                       |
| CCometixLine | 2.8k | No                                                             | Nothing                                                     | None                                                                       |
| uv           | 84k  | **Yes** (every PR via reusable wkflow)                         | Resolver hot paths (e.g. `resolve_warm_jupyter`, airflow)   | Criterion + CodSpeed, walltime AND simulation, dedicated crate `uv-bench`  |
| ruff         | 47k  | **Yes** (path-filtered, runs on bench changes + crate changes) | Lexer, parser, linter, formatter on representative inputs   | `codspeed-criterion-compat` + `codspeed-divan-compat`, simulation + memory |

The bimodal distribution is striking: either nothing, or full CodSpeed. Nobody runs Criterion + a homegrown threshold script in CI. The two projects in the survey at linesmith's scale and below (CCometixLine, claudia-statusline) have zero perf gating.

### Two-axis composition (uv is the reference)

uv's `bench.yml` is the only surveyed example that gates on **both** axes:

1. **Walltime** (`-m walltime`) on `codspeed-macro` runners — bare-metal ARM, separate build/run jobs to avoid contaminating the measurement with build noise. Captures process-level effects (real wall-clock of the resolver).
2. **Simulation** (`-m simulation`) on a generic Depot runner — instruction-count via Cachegrind, deterministic across noisy runners. Catches micro-regressions in hot paths.

Both modes use the same Criterion benches; CodSpeed swaps the harness. They're cumulative, not redundant — walltime catches I/O / parallelism regressions, simulation catches code-path regressions. ruff runs simulation only (no walltime), which fits a compiler/linter workload where I/O is amortized.

### Tool landscape and what each produces

- **Criterion** — actively maintained, the lingua franca. On its own in CI it's notoriously variance-prone on shared GitHub runners (ripgrep, starship, fd, bat all opted out). Its `--save-baseline` / `critcmp` workflow is genuinely useful **locally** for engineering regression hunts.
- **Divan** — slick API, but last push 2025-04-17 (one year stale at survey time). The version that ships in CI is the CodSpeed-forked `codspeed-divan-compat`, not upstream. Risky to bet on upstream Divan as a gate.
- **iai-callgrind** — instruction-count via Cachegrind, deterministic. Real adopters skew to research/infra (SpacetimeDB, scryer-prolog, DiskANN). Installation pain: requires `valgrind` on the runner; Linux-only effectively (macOS/Windows runners can't host it). For a cross-platform CLI like linesmith, this is a hard constraint — half the user base wouldn't be covered.
- **CodSpeed (`codspeed-criterion-compat` / `codspeed-divan-compat`)** — the shim crate adoption count is small (61★) but the actual adoption signal is who: uv and ruff are the two most-cited modern Rust CLIs. Public-repo Macro tier exists but is org-gated. Free for public repos in theory; worth verifying the linesmith org has access before committing.
- **hyperfine** — best-in-class for end-to-end wall-clock measurements of a CLI invocation (the actual "did `linesmith` finish in under 20ms?" question). Easy to script, but **not a CI gate by default** — even ripgrep, where the maintainer literally runs the suite, doesn't gate on it. Variance on GitHub-hosted runners is the reason.
- **Bencher CLI / SaaS** — modest adoption (829★ for the whole project, including the SaaS). Open-source CLI exists but the `bencher` workflow assumes either bare-metal self-hosted runners or their paid backend; the survey turned up no Rust CLI peer using it.

### Variance reality on GitHub-hosted runners

The pattern across blogs, the ripgrep maintainer's stale benchsuite, and CodSpeed's own marketing copy is consistent: **GitHub-hosted runners have ~10–20% wall-clock variance**, which swamps any threshold tighter than ~25%. The two practical responses are (a) accept that wall-clock CI gating is noise theatre and don't gate, or (b) buy out of the variance problem with bare-metal runners (CodSpeed, BuildJet, self-hosted). The Criterion + bash-threshold approach currently in linesmith is option (c): pretend the variance isn't there, get flaky failures, eventually disable the gate.

## Synthesis — what's actually meaningful

Three claims are well-supported:

1. **A CI perf gate is not table-stakes for a Rust CLI of linesmith's size.** Every direct peer (starship, CCometixLine, fd, bat, hyperfine itself) ships without one. The marketing claim "<20ms" is defended by occasional maintainer measurement, not automation.
2. **If you do gate, instruction-count via CodSpeed simulation is the only mode that produces real signal on shared runners.** Walltime gating without bare-metal is a flake factory. Every project that gates seriously (uv, ruff) leans on CodSpeed precisely because it solves the runner-variance problem.
3. **Wall-clock end-to-end measurement of the binary is a separate concern from microbench regression hunting.** uv composes both. For a 10-segment CLI invoked 10×/min, the wall-clock axis is the user-facing one; render-path regressions only matter as inputs to it.

A negative claim worth stating: **the current Criterion + bash-budget approach is the worst of both worlds** — it inherits Criterion's variance characteristics, adds bash fragility on top, and gates on absolute thresholds that will either be too loose to catch anything or too tight to survive runner noise. The reviewer's instinct is right.

## Conclusions and recommendation for linesmith

**Recommended path:**

1. **Drop the bash budget script.** It produces signal indistinguishable from runner noise and adds maintenance cost. Replace with no gate at all in v0.1.x.
2. **Adopt CodSpeed simulation mode on PRs**, using `codspeed-criterion-compat` as a drop-in over the existing Criterion benches. Free for public repos, deterministic across runners, and matches what uv/ruff do. Defends the **render-path** axis.
3. **For the `<20ms` cold-start claim, run hyperfine in a release-tag job** (not per-PR), publish results in the release notes, and check it manually before announcing a release. This matches ripgrep's pattern of out-of-band measurement and ties the marketing claim to a reproducible methodology rather than a CI green check. Defends the **process-spawn** axis.
4. **Keep Criterion + `critcmp` documented as the local engineering workflow** (`cargo bench --save-baseline before` then `critcmp before after`) for contributors investigating regressions. This is what Criterion is genuinely good at.
5. **Defer iai-callgrind**: the `valgrind`-on-Linux constraint excludes macOS/Windows users from the gate, and the marginal precision over CodSpeed simulation isn't worth the platform skew for a cross-platform CLI.

This is one less moving part in CI, real signal where it matters, and the marketing claim is defended by a real number rather than a green checkmark.

## Implications / actions

- **Done.** `scripts/check-bench-budget.sh` and the `bench` job in `.github/workflows/ci.yml` were removed; only `mise run bench` (Criterion, local-only) and `mise run bench:cold-start` (hyperfine end-to-end) remain.
- **Wire up CodSpeed** (filed as `lsm-ue14`, v0.2+). Verify oakoss org enrollment, add `codspeed-criterion-compat` as a workspace dep behind a feature flag matching uv's pattern, add a `bench.yml` mirrored on uv's structure (build job + run job), add `cargo codspeed build` invocation.
- **Hyperfine release smoke** (filed as `lsm-r37e`, v0.2). A `release.yml` step running `hyperfine` against the cargo-dist binary with the worktree fixture, publishing the table to release notes. Not a gate.
- **Local engineering workflow** is documented in the README "How fast" section. The `<20ms` claim is now defended by a reproducible methodology rather than a CI green check.

## Open questions

Resolved at rescope: the v0.1 gate was dropped and `bench:cold-start` shipped on demand. The questions below scope `lsm-ue14`'s implementation when it picks up.

- Is oakoss/linesmith enrolled (or enrollable) in CodSpeed's free public-repo Macro tier? If not, the recommendation collapses to "no gate, hyperfine smoke at release" — still better than today.
- Should the hyperfine release smoke be cross-platform (matrix across linux/macos/windows runners) or just one canonical Linux runner? Cross-platform variance is real but so is the value of catching e.g. a Windows-specific regression. Defer the call to ADR.
- Does the rhai plugin runtime warrant its own bench (plugin init time)? Sub-ms init was an explicit ADR-0005 claim and is testable in isolation. Worth a follow-up bead, not blocking.
