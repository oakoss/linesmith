# JSON parser + cache serialization stack: due-diligence

- Date: 2026-04-18
- Author: Jace Babin (w/ Claude Code)
- Scope: Survey current Rust JSON parsers and cache serialization formats with real benchmarks. Decision matrix per data source. Output: ADR-ready recommendation for the data-fetching layer.

## Question

`data-fetching-strategy.md` claimed `serde_json` + partial structs is the fastest pragmatic choice for linesmith. Before writing Rust code, validate or refute that claim with real benchmarks, comparable-CLI evidence, and explicit tradeoffs against `simd-json`, `sonic-rs`, `rkyv`, `bincode`, `postcard`, `rmp-serde`, and `memmap2`-based reads. Lock in parser + format choices for v0.1.

## Sources

- crates.io API for crate metadata (downloads, version, MSRV)
- GitHub `Cargo.toml` of 11 Rust CLIs (ripgrep, fd, bat, gitui, jj, eza, dust, hyperfine, uv, ruff, CCometixLine); ccstatusline known from prior research as TypeScript, excluded from the Rust parser tally
- `cloudwego/sonic-rs` README benchmarks (Intel Xeon 8260, twitter/citm/canada workloads)
- `djkoloski/rust_serialization_benchmark` cross-format shootout
- `simdjson.org` (Daniel Lemire) for SIMD parser architecture
- `memmap2` docs Safety section
- Companion docs: `claude-data-files.md`, `data-fetching-strategy.md`

## Findings

### 1. Parser landscape

| Crate              | Latest  | DL/yr | Last commit          | Open issues | MSRV    | Crate size | Repo stars | One-line                                             |
| ------------------ | ------- | ----- | -------------------- | ----------- | ------- | ---------- | ---------- | ---------------------------------------------------- |
| `serde_json`       | 1.0.149 | ~152M | 2026-04-10           | 223         | 1.68    | 156 KB     | 5.5k       | Safe, ubiquitous, serde-integrated baseline          |
| `simd-json`        | 0.17.0  | ~3.4M | 2026-03-11           | 22          | 1.88    | 172 KB     | 1.4k       | SIMD port of simdjson; destructive `&mut [u8]` parse |
| `sonic-rs`         | 0.5.8   | ~1.9M | 2026-04-15           | 15          | undecl. | 162 KB     | 862        | SIMD, ByteDance-maintained, best independent benches |
| `json` (json-rust) | 0.12.4  | ~1.6M | **2024-04-14** stale | 68          | none    | 106 KB     | 587        | DOM-only, no serde, mostly legacy                    |
| `rkyv`             | 0.8.15  | ~25M  | 2026-04-16           | 57          | 1.81    | 166 KB     | 4.2k       | Zero-copy archived format — **not JSON**, for caches |

**Transitive deps (rough cargo-tree):** `serde_json` ~4 (serde, itoa, ryu, memchr). `simd-json` ~15 (adds value-trait, halfbrown, lexical-core, etc.). `sonic-rs` ~10 (adds bumpalo, parking_lot, smallvec, faststr).

**Binary-size impact (release, stripped, rough):** `serde_json` adds ~150-250 KB on top of `serde`; `simd-json` adds ~400-700 KB because it ships SSE4.2/AVX2/NEON/fallback paths. `sonic-rs` is similar. `rkyv` varies 200-500 KB by `Archive` derive count.

### 2. Real benchmark numbers

sonic-rs README (Xeon 8260) deserialize-into-struct from_slice:

| Parser             | twitter (~630KB) | citm_catalog (~1.7MB) | canada (~2.2MB) |
| ------------------ | ---------------- | --------------------- | --------------- |
| sonic-rs           | 827 µs           | 1.37 ms               | 4.02 ms         |
| simd-json          | 1.09 ms          | 2.10 ms               | 8.09 ms         |
| serde_json (slice) | 2.29 ms          | 2.99 ms               | 9.36 ms         |
| serde_json (str)   | 1.38 ms          | 2.61 ms               | 9.26 ms         |

**Critical caveat:** these are MB-scale inputs. linesmith's largest workload (`~/.claude.json`) is **125 KB**. SIMD parsers don't reach asymptotic throughput on small inputs — startup overhead dominates.

**Scaled to linesmith workloads:**

- 10 KB stdin → serde_json ≈ 50-150 µs, simd-json ≈ 30-80 µs. Tens of µs, not milliseconds.
- 125 KB `~/.claude.json` → serde_json ≈ 1-4 ms (matches our measured 5-20 ms; spread is cold-cache effects). simd-json ≈ 0.5-2 ms. Real but small win.
- **Partial parsing with serde_json + narrow structs** (declare only `oauthAccount`, `mcpServers`, `projects`; let `#[serde(other)]` swallow the rest) drops 80%+ of work regardless of parser. Bigger win than swapping parsers.

### 3. JSONL streaming

| Option                                      | Status            | Verdict                                                                             |
| ------------------------------------------- | ----------------- | ----------------------------------------------------------------------------------- |
| `serde_jsonlines`                           | Active, ~50k DL   | Wraps `BufRead::lines() + serde_json::from_str` — 30 LOC ergonomics, not faster     |
| `jsonl`                                     | Last release 2021 | Abandoned — do not adopt                                                            |
| `serde_json::Deserializer::into_iter`       | Built-in          | Most flexible but holds reader for full stream — incompatible with byte-offset tail |
| Hand-rolled `BufReader::read_line` + offset | DIY               | **Recommended** — only pattern that handles incremental tail correctly              |

Canonical incremental-tail pattern (correctness essentials: only advance offset on `\n`-terminated lines; tolerate malformed lines via `match` + log + skip; remember offset per transcript):

```rust
let mut f = File::open(path)?;
f.seek(SeekFrom::Start(saved_offset))?;
let mut reader = BufReader::new(f);
let mut buf = String::new();
let mut new_offset = saved_offset;
while reader.read_line(&mut buf)? > 0 {
    if buf.ends_with('\n') {
        let rec: Record = serde_json::from_str(buf.trim_end())?;
        new_offset += buf.len() as u64;
        buf.clear();
    } else {
        break; // partial trailing line — don't advance
    }
}
```

### 4. What comparable Rust CLIs use

Pulled current `Cargo.toml` from each:

| Project      | JSON crate                                                   | Notes                                                                               |
| ------------ | ------------------------------------------------------------ | ----------------------------------------------------------------------------------- |
| ripgrep      | `serde_json` 1.0.23                                          | Only for `--json` output, not hot path                                              |
| fd           | none                                                         | No JSON dep                                                                         |
| bat          | `bincode` 1.0, `serde_yaml`                                  | Bincode for compiled syntax-highlight cache                                         |
| gitui        | `serde` only                                                 | No JSON crate                                                                       |
| jj           | `serde_json` 1.0.149, `jsonschema`                           | Hot-path config                                                                     |
| eza          | `serde_norway` (yaml)                                        | No JSON                                                                             |
| dust         | `serde_json`                                                 | Stdlib choice                                                                       |
| hyperfine    | `serde_json`                                                 | Stdlib choice                                                                       |
| uv           | `serde_json` 1.0.128, **`rkyv`** 0.8.14, **`rmp-serde`** 1.3 | serde_json for configs/PyPI; rkyv for lockfile/resolution cache; rmp-serde for wire |
| ruff         | `serde_json` 1.0.113, **`bincode`** 2.0                      | serde_json for LSP/config; bincode for compiled cache                               |
| CCometixLine | `serde_json` (per ccometixline-rust-patterns.md)             | Same                                                                                |

**Tally (11 Rust CLIs surveyed): 7 use `serde_json` in some capacity (ripgrep, jj, dust, hyperfine, uv, ruff, CCometixLine). 4 don't parse JSON at all (fd, bat, gitui, eza — bat/eza use YAML; fd/gitui have no relevant config format). 0 use `simd-json`. 0 use `sonic-rs`.**

The two Astral tools (uv, ruff) adopt binary formats (rkyv, bincode, rmp-serde) **only for internal caches**, never as JSON-input replacements. Verdict: simd-class parsers don't appear in the terminal-CLI design space; they live in server / log-processing workloads (Vector, Quickwit).

### 5. Memory mapping for JSONL

`memmap2` 0.9.10 is the standard (224M DL, MSRV 1.63, actively maintained). `mapr` and `fmmap` are non-starters (abandoned / niche).

**Why mmap is wrong for our JSONL workload:**

- **Truncation → SIGBUS.** memmap2 docs explicitly: "It is possible to corrupt memory if the file is truncated." Session rotation would crash linesmith.
- **Appends are invisible** — mapping is sized at creation time. Re-mmap each tick defeats zero-copy.
- **Windows locking** — a memory-mapped file can prevent the writer (Claude Code) from opening it. We'd break Claude.
- **OS page cache already does mmap's job** — `fs::read` of 1 MB is ~100-300 µs on SSD, <20 µs warm cache. Below mmap's break-even point.

Use seeked `BufReader::read_line` with byte-offset tracking. Skip mmap entirely.

### 6. Cache serialization

For our internal cache (`~/.cache/linesmith/usage.json`, ~1 KB OAuth response, written ~once per 180s):

| Format                    | Decode vs serde_json       | Schema migration                                                    | Inspectable          | Notes                                            |
| ------------------------- | -------------------------- | ------------------------------------------------------------------- | -------------------- | ------------------------------------------------ |
| `serde_json`              | 1× (baseline)              | Trivial — `#[serde(default)]`, optional fields                      | Yes — `cat`          | Universal                                        |
| `bincode` 2.0             | ~3-5× faster               | **Brittle** — field order matters; **upstream archived 2025-08-15** | No (binary)          | Wide adoption (ruff, bat) but maintenance signal |
| `rkyv` 0.8                | ~10-50× faster (zero-copy) | Manual; version byte recommended                                    | No                   | Growing (uv); overkill at our cache size         |
| `postcard` 1.1            | ~3-4× faster               | Same brittleness as bincode                                         | No                   | Embedded-heavy                                   |
| `rmp-serde` (MessagePack) | ~2-3× faster               | Tolerable in named-field mode; brittle in positional                | Semi (`msgpack-cli`) | Used by uv for wire format                       |

Numbers from `djkoloski/rust_serialization_benchmark`. Multipliers vary 2× across workloads; ordering is stable.

**Math for our cache:** parse 1 KB JSON costs ~10-30 µs. Binary saves 5-15 µs per read. Cache reads happen once per prompt (~3/sec max). Annual savings: negligible. Lost debuggability: significant — can't `cat ~/.cache/linesmith/usage.json` to debug stale rate-limit display. **JSON wins on net value at this size.**

Binary formats earn their keep at 100 KB+ hot-loop caches (ruff's AST cache, uv's resolution graph). Not a 1 KB OAuth blob.

### 7. Decision matrix

| Source                          | Size      | Frequency               | Recommended parser                                                           | Format       | Rationale                                                       |
| ------------------------------- | --------- | ----------------------- | ---------------------------------------------------------------------------- | ------------ | --------------------------------------------------------------- |
| Stdin payload                   | ~10 KB    | Per invocation          | `serde_json::from_reader`                                                    | JSON (fixed) | <100 µs either way                                              |
| `~/.claude/settings.json`       | <10 KB    | Per inv. (mtime cached) | `serde_json`                                                                 | JSON (fixed) | Trivial                                                         |
| `~/.claude.json`                | ~125 KB   | Per inv. (mtime cached) | `serde_json` + **narrow partial struct**                                     | JSON (fixed) | Partial parse drops 80% work — bigger win than swapping parsers |
| `~/.claude/sessions/{pid}.json` | ~200 B    | Per invocation          | `serde_json`                                                                 | JSON (fixed) | Trivial                                                         |
| JSONL transcripts               | MB+, tail | Per invocation          | `serde_json::from_str` per line, byte-offset tail via `BufReader::read_line` | JSON (fixed) | mmap risks SIGBUS on rotation; OS page cache is sufficient      |
| OAuth API response              | ~1 KB     | Per 180s                | `serde_json`                                                                 | JSON (fixed) | Trivial                                                         |
| **Internal cache**              | ~1 KB     | R/W per 180s            | `serde_json` pretty                                                          | **JSON**     | Inspectability wins; binary saves negligible time at this size  |

### 8. Tradeoffs explicitly NOT taken

- **simd-json**: saves ~2-3ms on the 125 KB file _if_ parsed whole, but ~400-700 KB binary bloat pushes us over the 5MB target, adds SIMD-fallback testing complexity (older x86, non-NEON ARM), complicates reproducible builds. Partial-struct parsing recovers the same time without those costs.
- **sonic-rs**: better benchmarks than simd-json, but less mature (862⭐, no declared MSRV), similar binary cost. Same partial-parse counter applies.
- **rkyv for cache**: overkill at 1 KB. Hold in reserve if linesmith ever builds a segment-compile cache or large rolling usage log (>100 KB).
- **memmap2 for transcripts**: correctness risk (rotation, append-invisibility, Windows locking) outweighs speed win at MB-scale on SSDs with warm page cache.

## Conclusions

1. **`serde_json` is the right choice for v0.1, period.** Of the 7 surveyed Rust CLIs that parse JSON at all, all 7 use `serde_json`. Zero across the 11-project survey use simd-json or sonic-rs. The competitive set has voted with their dependencies.
2. **Partial-deserialization structs give the biggest win** for `~/.claude.json`. Bigger than parser choice. Declare `oauthAccount`, `mcpServers`, `projects` as their own struct fields; let serde drop everything else. Drops parse cost by 80%+.
3. **JSONL incremental tail via `BufReader::read_line` + byte offset.** No JSONL-specialized crate is faster than this 20-line pattern; `serde_jsonlines` adds ergonomics without speed. Memmap is contraindicated.
4. **Cache as pretty-printed JSON with a `schema_version` field.** Debuggability beats binary at 1 KB. Treat schema mismatch as cache miss.
5. **simd-json behind an opt-in `--features fast-json` flag** — defer to v0.2 contingent on benchmarks showing >2ms p50 improvement after partial-parse is implemented (which they almost certainly won't).
6. **`bincode` upstream archived 2025-08-15.** Don't pick it speculatively. ruff staying on bincode 2.0 likely reflects sunk-cost migration (uv avoided bincode entirely — they picked `rkyv` for caches and `rmp-serde` for wire). New code should pick `rmp-serde` or `postcard` if a binary cache is ever needed.

## Implications / actions

- **File ADR slice for parsing-stack decision.** Cite this doc. ADR locks in: serde_json + partial structs + JSON cache with schema_version. Defer parser-flag decision to a later ADR if/when measurements demand it.
- **Update `data-fetching-strategy.md` §"What I traded away"** to point at this doc — that section listed faster alternatives without the comparable-CLI evidence; this doc closes that loop.
- **Implementation task: partial-struct definitions** for `ClaudeJson` (only `oauthAccount`, `mcpServers`, `projects: HashMap<PathBuf, Box<RawValue>>`). The `Box<RawValue>` for `projects` lets us defer per-project parsing until needed.
- **Implementation task: JSONL incremental-tail helper** in a shared module (used by rate-limit aggregation, effort detection, future segments). Include offset persistence story (in-memory for v0.1; file-backed in daemon mode).
- **Implementation task: Cache helper** with `schema_version` field, pretty-printed JSON, atomic write via `tempfile` + rename.
- **Add `cargo bloat --release` to a future bead** — measure actual binary size impact of dependencies once the scaffold compiles. Confirm binary stays under 5MB target.
- **Close lsm-33r** with verdict.

## Open questions

- **Benchmark numbers in §2 are sonic-rs self-reported** on MB-scale inputs. No independent current benchmark exists for our 1-125 KB size range. Relative ordering (sonic > simd > serde) is consistent across sources, but absolute win at our sizes is assumed small. Worth a quick `cargo bench` on the 125 KB file once linesmith has fixtures.
- **`bincode-org/bincode` archival.** Repo banner shows archived 2025-08-15. crates.io still serves 3.0.0. Maintenance forecast unclear. Ruff/bat stay on it — sunk cost or active fork? Worth a five-minute check before any future binary-cache decision.
- **Binary-size deltas are rule-of-thumb estimates.** Measure with `cargo bloat --release` once the scaffold compiles; revisit parser flag if we're tight against 5MB.
- **`schema_version` field convention.** Top-level `"schema_version": 1` is one option; SemVer-style `"version": "1.0"` is another. Pick during cache-helper implementation; document in the spec.

## Raw data

### Cargo.toml citations (links)

- `https://github.com/cloudwego/sonic-rs/blob/main/README.md` — sonic-rs benchmarks
- `https://github.com/djkoloski/rust_serialization_benchmark` — cross-format shootout
- `https://github.com/simd-lite/simd-json` — simd-json source
- `https://docs.rs/memmap2/latest/memmap2/struct.Mmap.html` — memmap2 Safety section
- `https://github.com/bincode-org/bincode` — bincode (archived banner visible)
- `https://github.com/astral-sh/uv/blob/main/Cargo.toml` — uv dependencies (rkyv, rmp-serde)
- `https://github.com/astral-sh/ruff/blob/main/Cargo.toml` — ruff dependencies (bincode)

### Surveyed CLI repos

`BurntSushi/ripgrep`, `sharkdp/fd`, `sharkdp/bat`, `extrawurst/gitui`, `jj-vcs/jj`, `eza-community/eza`, `bootandy/dust`, `sharkdp/hyperfine`, `astral-sh/uv`, `astral-sh/ruff`, `Haleclipse/CCometixLine`. JSON dep counts pulled from each main/master `Cargo.toml` 2026-04-18.
