# Use rhai for the plugin runtime

- Status: accepted
- Date: 2026-04-17
- Deciders: Jace

## Context and Problem Statement

No existing Claude Code statusline tool has a real plugin API — every tool hardcodes its widgets (see `research/competitor-landscape.md`). This is the single biggest architectural gap in the ecosystem. linesmith will ship a plugin system where users can define custom segments without recompiling the binary. Given our <20ms cold-start budget and desire for a single static binary (see [ADR-0001](0001-use-rust-for-runtime.md)), what plugin runtime should we use?

## Decision Drivers

- Cold-start budget: the plugin engine must initialize in ≤1ms
- Pure Rust: we want cross-compilation via `cross` to remain simple; no C FFI dependencies
- Sandboxing: plugins run on every prompt, must not be able to exec arbitrary shell or leak data
- User ergonomics: plugin authors should be able to write simple custom segments without learning Rust
- Single binary: the plugin runtime must be embeddable, not a separate process
- No hard dependency on language servers or toolchains for plugin authors

## Considered Options

- **rhai** — pure-Rust scripting language, sub-ms engine init, sandboxed by default
- **mlua (Lua 5.4)** — widely understood, ~1ms init, but pulls a C dependency
- **wasmtime / wasmer** — language-agnostic via WASM, but 10-50ms JIT compile per plugin
- **wasmi** — WASM interpreter, ~5ms startup, much slower runtime execution
- **libloading (dynamic libraries)** — fastest dlopen (~0.1ms), but ABI pain and per-platform builds
- **Subprocess plugins** — any language, fork+exec per plugin (5-15ms each), kills cold-start budget
- **No plugins** — just a rich config format with built-in segments

## Decision Outcome

Chosen option: **rhai**, because it's the only option that hits every decision driver at once: sub-millisecond engine init, pure Rust (no C FFI), sandboxed by default, embeddable in our single binary, and approachable enough for users who aren't Rust programmers. WASM is too expensive per-plugin at cold start (10-50ms) to justify running on every prompt. Lua is a fair alternative but the C dependency complicates `cross` cross-compilation.

### Consequences

- Good, because the plugin API becomes a real differentiator — no competitor has this
- Good, because rhai's sandboxed-by-default posture keeps plugins safe on every invocation
- Good, because pure-Rust means zero cross-compilation headaches
- Good, because the same `Segment` trait is used for built-in and plugin segments — no dual API
- Good, because users can iterate on plugins by editing a `.rhai` file without rebuilding
- Bad, because rhai is less widely known than Lua or JavaScript — there's a learning curve
- Bad, because rhai's ecosystem (libraries, editor support) is smaller than Lua's
- Bad, because debugging rhai scripts is less polished than debugging Python/Lua
- Bad, because language-agnostic plugin authoring is blocked; users who want Go/Python plugins can't
- Neutral, because plugin discovery/loading (where .rhai files live, how they're registered) is still to be designed in `specs/plugin-api.md`

### Confirmation

Revisit if:

- Rhai cold-start observed in benchmarks exceeds 2ms per plugin
- User friction with rhai syntax prevents meaningful plugin contribution
- A compelling use case emerges that requires language-agnostic plugins (might warrant adding WASM as an optional runtime alongside rhai)

## Pros and Cons of the Options

### rhai

- Good: sub-ms engine init, pure Rust, sandboxed, embeddable, ~2MB footprint
- Good: scripting syntax is friendly (Rust-ish but dynamic); error messages are reasonable
- Good: active maintenance (1.24.0 released recently, 444K monthly downloads)
- Bad: smaller community than Lua; fewer tutorials and editor plugins
- Bad: dynamic typing means plugin bugs surface at runtime rather than compile time

### mlua (Lua 5.4)

- Good: Lua is familiar to many (Neovim, game modding, OpenResty, Redis)
- Good: mature ecosystem and tooling (LuaCheck, LuaLS)
- Bad: C FFI complicates cross-compilation; `cross` builds become trickier
- Bad: vendored Lua adds ~200KB plus build complexity

### wasmtime / wasmer

- Good: language-agnostic — plugins in Rust, Go, AssemblyScript, C++, etc.
- Good: portable across architectures
- Bad: 10-50ms JIT compile per plugin on cold start — blows our 20ms budget on a single plugin
- Bad: 6.5MB binary impact
- Bad: sandbox model is strong but complex; syscall surface management is non-trivial

### wasmi

- Good: pure Rust, no JIT complexity
- Bad: interpreter is 10-100x slower at runtime than wasmtime
- Bad: ~5ms startup is still worse than rhai's sub-ms

### libloading (native dylibs)

- Good: fastest plugin invocation (dlopen ~0.1ms, direct function call)
- Bad: plugins must be built per platform (`.so`, `.dylib`, `.dll`)
- Bad: ABI stability across Rust versions is painful; plugins break on every Rust update
- Bad: no sandboxing — dylib has full process privileges

### Subprocess plugins

- Good: language-agnostic — plugin is any executable
- Good: fully isolated by OS process boundary
- Bad: fork+exec costs 5-15ms per plugin on macOS/Linux
- Bad: with 3-5 plugins, blows 20ms budget before rendering anything

### No plugins (built-ins only)

- Good: zero complexity
- Bad: gives up the biggest differentiator in the ecosystem
- Bad: users fork the project or move to competitors to get custom segments

## More Information

- Driven by: `research/rust-crate-survey.md` (rhai cold-start benchmarks, WASM overhead analysis), `research/competitor-landscape.md` (plugin API gap)
- Related ADRs: [ADR-0001](0001-use-rust-for-runtime.md) (why pure-Rust matters), [ADR-0003](0003-segment-widget-system.md) (plugins implement the same trait)
- Will drive: `specs/plugin-api.md` (plugin discovery, loading, sandboxing boundaries)
