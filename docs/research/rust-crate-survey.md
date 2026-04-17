# Rust Crate Survey for linesmith

- Date: 2026-04-17
- Author: Claude Code research agent session
- Scope: Evaluate current best-in-class Rust crates across every category linesmith needs, with the goal of locking the stack before scaffolding. Target: <20ms cold start, ~3-5MB stripped binary.

## Question

Which Rust crates should linesmith use for JSON parsing, terminal rendering, git operations, config, plugins, async, HTTP, caching, CLI args, TUI, and distribution, such that we meet our performance budget and keep iteration velocity high?

## Sources

- [crates.io](https://crates.io) · [lib.rs](https://lib.rs)
- Individual crate GitHub READMEs (linked per section)
- Download counts reflect monthly crates.io figures at time of research

## Findings

### 1. JSON parsing (~2KB payload per invocation)

| Crate                                                  | Monthly downloads | Notes                                                        |
| ------------------------------------------------------ | ----------------- | ------------------------------------------------------------ |
| [serde_json](https://lib.rs/crates/serde_json) 1.0.149 | 52.7M             | Default, zero fuss, ~500-1000 MB/s                           |
| [sonic-rs](https://lib.rs/crates/sonic-rs) 0.5.8       | 679K              | Fastest (~3x serde_json on large), needs `target-cpu=native` |
| [simd-json](https://lib.rs/crates/simd-json) 0.17.0    | 1.26M             | Fast, higher setup cost                                      |

At 2KB payloads, **cold start and allocator warmup dominate**; parser throughput is irrelevant. SIMD crates have per-call setup overhead that swamps the win.

**Pick: `serde_json`.**

### 2. Terminal rendering / ANSI styling

| Crate                                                     | Monthly downloads | Notes                                               |
| --------------------------------------------------------- | ----------------- | --------------------------------------------------- |
| [anstyle](https://lib.rs/crates/anstyle) 1.0.14           | 39.3M             | Zero deps, just ANSI escape types                   |
| [owo-colors](https://lib.rs/crates/owo-colors) 5.0.0      | 7.9M              | Zero alloc, no_std, NO_COLOR/FORCE_COLOR, truecolor |
| [nu-ansi-term](https://lib.rs/crates/nu-ansi-term) 0.50.3 | 25.8M             | More allocation-y                                   |
| [crossterm](https://lib.rs/crates/crossterm) 0.29.0       | 11.16M            | Full terminal control (overkill)                    |

**Pick: `owo-colors`.** Zero-alloc extension traits are the ergonomic sweet spot. Pair with `supports-color` for capability detection if needed.

### 3. Git operations (worktrees, branch, ahead/behind, dirty)

| Crate                                               | Notes                                                                           |
| --------------------------------------------------- | ------------------------------------------------------------------------------- |
| [gix](https://lib.rs/crates/gix) 0.81.0 (2.4M/mo)   | Pure Rust, ~4.5MB, **native worktree support** (`.git`-as-file), no C toolchain |
| [git2](https://lib.rs/crates/git2) 0.20.4 (4.2M/mo) | libgit2 bindings, ~143K lines of C, complicates cross-compile                   |
| Shelling out to `git`                               | Fork+exec costs 5-15ms per prompt; violates <20ms budget                        |

**Pick: `gix`.** Pure-Rust means painless cross-compilation via `cross`, first-class worktree support, no libgit2 versioning. Use fine-grained sub-crates (`gix-repository`, `gix-status`) to keep binary lean.

### 4. Config parsing

| Crate                                            | Monthly downloads | Notes                                               |
| ------------------------------------------------ | ----------------- | --------------------------------------------------- |
| [toml](https://lib.rs/crates/toml) 1.1.2         | 41.2M             | Serde-native                                        |
| [figment](https://lib.rs/crates/figment) 0.10.19 | 1.7M              | Layered providers, last release May 2024 (stagnant) |
| [config-rs](https://lib.rs/crates/config)        | -                 | Heavier than needed                                 |

**Pick: `toml` + `serde_json`.** Roll a ~20-line `Config::load()` that reads `~/.config/linesmith/config.toml` and overlays `$LINESMITH_CONFIG` env JSON. Layering is trivial; figment is unmaintained.

### 5. Plugin systems (cold-start critical)

| Crate                                                           | Notes                                                                                  |
| --------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| [rhai](https://lib.rs/crates/rhai) 1.24.0 (444K/mo)             | ~2MB, pure Rust, engine init sub-ms, no FFI                                            |
| [mlua](https://lib.rs/crates/mlua) 0.11.6 (258K/mo)             | Lua 5.4 vendored ~200KB, init ~1ms, widely understood, C FFI complicates cross-compile |
| [steel-core](https://lib.rs/crates/steel-core) 0.8.2 (3K/mo)    | Niche Scheme, immature                                                                 |
| [wasmtime](https://lib.rs/crates/wasmtime) 43.0.1 (1.36M/mo)    | 6.5MB, Cranelift JIT cold-compile ~10-50ms per module                                  |
| [wasmi](https://lib.rs/crates/wasmi) 1.0.9 (901K/mo)            | Interpreter, ~5ms startup, slower execution                                            |
| [extism](https://lib.rs/crates/extism) 1.21.0 (44K/mo)          | Wraps wasmtime, inherits startup cost                                                  |
| [libloading](https://lib.rs/crates/libloading) 0.9.0 (21.5M/mo) | Fastest (dlopen ~0.1ms), but `.so`/`.dylib`/`.dll` per platform + ABI pain             |

For a 20ms budget fired every prompt, WASM (Wasmtime JIT) is too expensive. Lua/Rhai boot in under a millisecond.

**Pick: `rhai`.** Pure Rust means static binaries on all three targets, sandboxed by default, sub-ms engine creation. Lua is a reasonable alternative if user familiarity matters more, but pulls a C dep. Avoid WASM until language-agnostic plugins become a real requirement.

### 6. Async runtime

For a sub-20ms short-lived CLI, tokio's multi-thread runtime has a ~1-3ms init cost and brings ~30 transitive deps.

**Pick: blocking threads + `std::thread::spawn`**, or `tokio` with `features = ["rt", "macros"]` if HTTP needs it. Never `rt-multi-thread`. A future background daemon (if built) should be its own binary where tokio cost amortizes.

### 7. HTTP client (for rate-limit header scraping)

| Crate                                               | Monthly downloads | Notes                                  |
| --------------------------------------------------- | ----------------- | -------------------------------------- |
| [ureq](https://lib.rs/crates/ureq) 3.3.0            | 11.2M             | Pure Rust, blocking, rustls, ~3MB deps |
| [reqwest](https://lib.rs/crates/reqwest) 0.13.2     | 34.16M            | Drags tokio/hyper, 3-37MB deps         |
| [attohttpc](https://lib.rs/crates/attohttpc) 0.30.1 | 1.5M              | Smaller but less active                |

**Pick: `ureq`.** Blocking fits the CLI model, rustls keeps it static, zero async runtime cost.

### 8. Caching / storage (<1MB between invocations)

| Option                                                      | Notes                                         |
| ----------------------------------------------------------- | --------------------------------------------- |
| `std::fs::write` + `bincode` or `serde_json`                | ~50 LOC, no deps                              |
| [redb](https://lib.rs/crates/redb) 4.0.0 (603K/mo)          | Pure Rust ACID KV, ~10MB binary impact        |
| [rusqlite](https://lib.rs/crates/rusqlite) 0.39.0 (4.4M/mo) | Bundled SQLite adds 21MB source / ~1MB binary |

**Pick: plain `fs` + serde.** Atomic-rename `cache.json.tmp` → `cache.json`. Upgrade to redb only if we later need concurrent readers.

### 9. CLI argument parsing

| Crate                                        | Monthly downloads | Notes                                          |
| -------------------------------------------- | ----------------- | ---------------------------------------------- |
| [clap](https://lib.rs/crates/clap) 4.6.1     | 47.4M             | ~1MB binary impact even minimized              |
| [lexopt](https://lib.rs/crates/lexopt) 0.3+  | 524K              | One file, no macros, ~10KB                     |
| [pico-args](https://lib.rs/crates/pico-args) | 3.6M              | Tiny, but arbitrary-order parsing is a footgun |

**Pick: `lexopt`.** 100 LOC of hand-written parsing keeps binary lean and cold-start fast. Upgrade to `clap` only if subcommand tree becomes rich.

### 10. TUI framework (for config tool, optional)

| Crate                                                         | Notes                                                         |
| ------------------------------------------------------------- | ------------------------------------------------------------- |
| [ratatui](https://lib.rs/crates/ratatui) 0.30.0 (2.94M/mo)    | Actively maintained, works on top of crossterm                |
| [dialoguer](https://lib.rs/crates/dialoguer) 0.12.0 (4.5M/mo) | Single-question prompts (Select, MultiSelect, Input, Confirm) |

**Pick: `dialoguer` for v1**, `ratatui` later if users want live preview. Keep TUI behind a `config-ui` cargo feature so the `run` binary stays small.

### 11. Build / distribution

**Pick: [dist](https://lib.rs/crates/dist) (formerly cargo-dist) 0.31.0.** Generates GitHub Actions workflow, creates releases, builds macOS (x86_64+aarch64 universal), Linux (x86_64+aarch64 glibc+musl via `cross`), Windows (x86_64+aarch64 MSVC), uploads tarballs, writes shell/powershell installers, emits Homebrew formulas.

Pair with `Cargo.toml` release profile:

```toml
[profile.release]
lto = "fat"
codegen-units = 1
strip = true
panic = "abort"
```

Shaves 30-50% off binary size.

### 12. Bonus utilities

- **WCAG contrast**: implement inline (~20 lines: relative luminance per sRGB → `(L1+0.05)/(L2+0.05)`). Use [palette](https://lib.rs/crates/palette) if we also want Oklab/HSL transforms.
- **Terminal width**: **[terminal_size](https://lib.rs/crates/terminal_size) 0.4.4** (8.76M/mo); tiny, works everywhere.
- **OSC 8 hyperlinks**: **[supports-hyperlinks](https://lib.rs/crates/supports-hyperlinks) 3.2.0** (1.98M/mo) for detection; emit the escape yourself: `\x1b]8;;URL\x1b\\text\x1b]8;;\x1b\\`.
- **Nerd Font icons**: no mainstream crate. Generate `const ICONS: &[(&str, char)]` from [nerd-fonts glyphnames.json](https://github.com/ryanoasis/nerd-fonts/blob/master/glyphnames.json) via `build.rs`, or hard-code the dozen we actually render.

## Conclusions

**Recommended stack:**

```text
serde_json, owo-colors, gix, toml, rhai (optional),
ureq (optional), lexopt, dialoguer, terminal_size,
supports-hyperlinks, supports-color, dist for release.
```

**Estimated stripped release binary:** ~3-5MB on macOS aarch64 with LTO, assuming `gix`, `rhai`, and `ureq` are feature-gated (`git`, `plugins`, `http`) so users who don't need them get smaller builds.

## Implications / actions

- Drives [ADR-0004: Rhai for Plugins](../adrs/0004-rhai-for-plugins.md): WASM's cold-start blows our budget; rhai is the correct choice
- Drives [ADR-0007: cargo-dist Distribution](../adrs/0007-cargo-dist-distribution.md): industry standard for Rust CLI shipping
- Feature gating is a first-class design concern from day one; keep the minimal `run` binary as small as possible

## Open questions

- Can we realistically stay under 20ms cold start once we compose segments + read cache + maybe run a rhai script? Benchmarks needed early.
- Do we actually need `ureq` if Anthropic exposes rate limits in the statusline JSON directly someday? (If so, feature-gate or leave it out of v0.1.)
- Is [catppuccin](https://lib.rs/crates/catppuccin) (the official Rust palette crate) worth adopting, or is it better to ship our own theme data in TOML files?
