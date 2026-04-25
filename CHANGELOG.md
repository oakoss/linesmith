# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.1] - 2026-04-25

### Bug Fixes

- **themes:** Remap Catppuccin Muted to subtext0 for readable text([535c3af](https://github.com/oakoss/linesmith/commit/535c3afa7a0e9656df255d17109458736f39190c))
- **themes:** Env-first force-color capability detection via CliEnv([acdf35f](https://github.com/oakoss/linesmith/commit/acdf35f0dcc05331f27da655ca855ec8898956d9))
- **core:** Accept object-form effort payload([78d6a87](https://github.com/oakoss/linesmith/commit/78d6a8771bd5e87313afb51998b972495db17b48))
- **core:** Clamp context_window.used_percentage above 100, reject below 0([bd58bb8](https://github.com/oakoss/linesmith/commit/bd58bb821455fb4793ae56508d30a229f4ad5ecc))
- **core:** Lock-active refuses cached data when lock was written by 401([4a689d8](https://github.com/oakoss/linesmith/commit/4a689d851630cfbaf1200ffcb9a7d1922b22f815))
- **plugins:** Cap ctx_mirror JSON recursion depth, breadth, and string size([5517bc8](https://github.com/oakoss/linesmith/commit/5517bc8ebf3f71010d32e42d97649a075a948fb9))
- **segments:** Harden ahead/behind resolver + de-slopify polish([4faf9a8](https://github.com/oakoss/linesmith/commit/4faf9a8180cb107e277cf63dd2415cb7c840f00c))

### Features

- **segments:** Add per-turn tokens_{input,output,cached,total} segments([028caaf](https://github.com/oakoss/linesmith/commit/028caafb8b80672e29846aed313777008816420f))
- **core:** Parse context_window.current_usage as Option<TurnUsage>([39dad87](https://github.com/oakoss/linesmith/commit/39dad87daaf03b01b00d1fe1a4b614d4fd6568d0))
- **core:** JSONL fallback returns Ok(UsageData::Jsonl) on endpoint failure([cc690f0](https://github.com/oakoss/linesmith/commit/cc690f079222063e653f34bf107dcd0f0f14e548))
- **core:** In-crate structured logger for silent Ok(None) paths([f9137ae](https://github.com/oakoss/linesmith/commit/f9137ae7c9cc45193443d9506a5d1b02d4492e8e))
- **segments:** Workspace reads worktree name from ctx.git()([4d60443](https://github.com/oakoss/linesmith/commit/4d60443b03c0209096c2a826d7720f1978aa2d33))
- **segments:** Ahead/behind counters on git_branch([0798579](https://github.com/oakoss/linesmith/commit/079857920125dd8ba698654bc73cc289720d6caa))
- **segments:** Git_branch (branch + dirty indicator) via gix([72825cd](https://github.com/oakoss/linesmith/commit/72825cde6543f3182a3a9225d4b2dfeb595ccc5d))
- **segments:** Rate-limit segments on ctx.usage() + drop stdin rate_limits([971978d](https://github.com/oakoss/linesmith/commit/971978dc50ec3358d28ec64b02f7e756c42bf3c4))
- **data-context:** Wire DataContext::usage() fallback cascade([ea814b9](https://github.com/oakoss/linesmith/commit/ea814b9860144323f73da546bd6a629ccda88291))
- **data-context:** JSONL aggregator (5h billing blocks + 7d window)([6cdf450](https://github.com/oakoss/linesmith/commit/6cdf450a52fb3b34e3e0654628121a07a406f1aa))
- **core:** OAuth usage disk cache + lock file (atomic rename)([884862e](https://github.com/oakoss/linesmith/commit/884862e1963f610f13a3d966f3ef1d83a7cf9a2a))
- **core:** OAuth /api/oauth/usage HTTP client (ureq + retry-after)([215632f](https://github.com/oakoss/linesmith/commit/215632f680e96b2fbc66a4fec872042ed07db4c4))
- **core:** Credential reader with Keychain + file cascade([e155d11](https://github.com/oakoss/linesmith/commit/e155d1162e6a890207d5db745ed12fdbd9830790))
- **core:** UsageApiResponse + UsageData + UsageError types([470043f](https://github.com/oakoss/linesmith/commit/470043f2b1dc4d420b9edf6f048846df479878fe))
- **plugins:** Wallclock timeout, log rate-limit, registry-stored load errors([57acfee](https://github.com/oakoss/linesmith/commit/57acfee51bab145f1bcc2a46eda39882680e1382))
- **plugins:** RhaiSegment + ctx mirror + return validator + builder wiring([4d0e529](https://github.com/oakoss/linesmith/commit/4d0e52978d507b9c5faaedae39e4f958641db633))
- **plugins:** Discovery + @data_deps parser + compilation registry([b493084](https://github.com/oakoss/linesmith/commit/b49308412f5dbe1f8ccf76d5d6c4eedccb923da8))
- **plugins:** Rhai engine foundation for plugin runtime([e52e4af](https://github.com/oakoss/linesmith/commit/e52e4afe8c9ab817e5e35a9af4ac8cfceea43049))
- **core:** DataContext skeleton and Segment::data_deps method([3ae96bc](https://github.com/oakoss/linesmith/commit/3ae96bc19c2c9bc83100370343c88567a72eaef0))

### Refactoring

- **segments:** Annotate remaining Ok(None) hide paths([24cf621](https://github.com/oakoss/linesmith/commit/24cf62111695ff4d22f7f711f5774e436b9e9b0d))
- **core:** Route diagnostic writes through the crate logger([7b624a2](https://github.com/oakoss/linesmith/commit/7b624a24767d67beade8d727b23f5e35b810b0eb))
- **segments:** Simplify FormatTemplate + harden ahead/behind tests([50cff4f](https://github.com/oakoss/linesmith/commit/50cff4fb1cf7fe860e1448eb8bbe7b29f6186daf))
