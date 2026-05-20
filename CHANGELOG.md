# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [0.1.4] - 2026-05-20

### Bug Fixes

- **core:** Bound CapturedSink ring + add inter-frame wake signal ([#8](https://github.com/oakoss/linesmith/pull/8))([3248bc9](https://github.com/oakoss/linesmith/commit/3248bc9ffba9910e6fac9ba5c83315e49128e41d))

### Features

- **tui:** PropertyScreen reusable widget([bd5c5a1](https://github.com/oakoss/linesmith/commit/bd5c5a19109554116bc27d16ad08f276174edc71))

## [0.1.3] - 2026-05-18

### Bug Fixes

- **tui:** Retry on EINTR + recoverable I/O blips in event loop([68f03c3](https://github.com/oakoss/linesmith/commit/68f03c360bbe495d3890347814151eb897bff5b8))
- **cli:** Reject --check-config outside the render path([b27951f](https://github.com/oakoss/linesmith/commit/b27951f444891db9daf3a3da9ace3501bd774814))
- **cli:** Route logging macros through swappable LogSink([eb1df31](https://github.com/oakoss/linesmith/commit/eb1df31a458f72e82c2c6e6410109a2dcf63c7f9))
- **cli:** Filter non-Press key events to stop Windows double-fire([60abd53](https://github.com/oakoss/linesmith/commit/60abd53b4b721da5a7dfe4d0a74b0ef7eb2b2482))

### Features

- **core:** LayoutObservers struct + render_with_observers rename([089c060](https://github.com/oakoss/linesmith/commit/089c06083edeedfc3608f7f257cdfa1e0a16810b))
- **core:** LineItem::Segment struct-variant refactor with id field([011be10](https://github.com/oakoss/linesmith/commit/011be10352485b54eca5c5cc51f3b946ec2d5e94))
- **tui:** Honor CliEnv env and CLI color flags([d1d7ecb](https://github.com/oakoss/linesmith/commit/d1d7ecbf71a0288c27677f132503fa2bded58073))
- **cli:** Install/uninstall Claude Code statusLine([01e6713](https://github.com/oakoss/linesmith/commit/01e6713f03bff0dae8331f45350aa01529f9b84a))
- **cli:** TUI instant-apply save model([4727269](https://github.com/oakoss/linesmith/commit/472726982c26bf1c639a9a77cd7e04707080cf5c))
- **cli:** Theme picker + user-theme discovery([237a17e](https://github.com/oakoss/linesmith/commit/237a17e17dcbf87756412529b0204cbf651bd8ea))
- **cli:** EnvironmentWarning footer (NO_COLOR / palette / VSCode / tmux)([5dcd964](https://github.com/oakoss/linesmith/commit/5dcd964b28767d6967a06ab31572e64cdc5056e4))
- **cli:** Items editor finishing slice (LinePicker + Space/m verbs)([f496b30](https://github.com/oakoss/linesmith/commit/f496b30aff045ac92b0a8b982bdf85462f01bf0d))
- **cli:** Items editor raw value verb (r)([6782ed5](https://github.com/oakoss/linesmith/commit/6782ed53515b1e5a3031b85ff6fa4f68b3197df4))
- **cli:** Items editor type picker (a/i verbs + arrows)([aeece9b](https://github.com/oakoss/linesmith/commit/aeece9b3a42ee36a8494a83e41c49d78ce0d6421))
- **cli:** Items editor delete/clear/clone verbs([10098ac](https://github.com/oakoss/linesmith/commit/10098ac1e80fadec1c441fcf26f9b96ebbb85d20))
- **cli:** Items editor screen with move-mode reorder([866f5dc](https://github.com/oakoss/linesmith/commit/866f5dc6702dbc9248311898187b8aa2f513e0b6))
- **cli:** TOML write-back for linesmith config TUI([b9a539c](https://github.com/oakoss/linesmith/commit/b9a539cb66b26cd97dfea9227ed45e72fc8915ba))
- **cli:** Live-preview header for linesmith config TUI([32cd241](https://github.com/oakoss/linesmith/commit/32cd2418f7711c7e3e0b59d4158f467fdb83dffb))
- **cli:** Main menu screen + back-navigable placeholder([014ea83](https://github.com/oakoss/linesmith/commit/014ea83bd3cc0c73cb6efffd03629497b3f7a7e9))
- **cli:** ListScreen widget for linesmith config TUI([c28d4b9](https://github.com/oakoss/linesmith/commit/c28d4b95e98c30d225cd6f5e8b7e1cf141b9e3d1))
- **cli:** Linesmith config TUI boot([5e98461](https://github.com/oakoss/linesmith/commit/5e98461af342bb3574b806f0f21fdc377e2876be))

### Refactoring

- **cli:** Caller owns cursor on ListScreen MoveSwap([99d8941](https://github.com/oakoss/linesmith/commit/99d89412ea4636576fae7ad9dfde3629870f9e16))
- **core:** Separator-as-item layout([4ecbefd](https://github.com/oakoss/linesmith/commit/4ecbefd4ad39040085c11555d92054e9584ac089))
- **repo:** Migrate chrono → jiff across the workspace([0be2835](https://github.com/oakoss/linesmith/commit/0be2835fd5f645e52f84b6429f43ce035170cb11))
- **env:** Preserve non-UTF-8 paths through CliEnv + EnvVarState([f667eb4](https://github.com/oakoss/linesmith/commit/f667eb4d4ae63f84a0b4a48916d91c83e5a16280))
- **repo:** Centralize lints in [workspace.lints]([dd40e0c](https://github.com/oakoss/linesmith/commit/dd40e0c71882c133592e5b0d581f25921886f083))
- **plugins:** Consume plugin host types from origin([6199a33](https://github.com/oakoss/linesmith/commit/6199a33c8556649283427e94ea891cc370059375))

## [0.1.2] - 2026-05-01

### Bug Fixes

- **doctor:** Use FileCascadeEnv::new() in cross-crate test sites([1afa91a](https://github.com/oakoss/linesmith/commit/1afa91aac6dea5eda67760783eb42b71b8fe468b))
- **doctor:** Make cache-root check read-only + audit snapshot helpers([ff8c0d0](https://github.com/oakoss/linesmith/commit/ff8c0d07d81dcf6264dbc8078f43c70a7d9466c4))
- **core:** Cfg-gate `~` to POSIX in needs_shell_quoting([8601a45](https://github.com/oakoss/linesmith/commit/8601a4586c234f2bff7f186a3e60feaa952d8a8a))
- **core:** Correct Windows test failures in driver.rs([c8c213f](https://github.com/oakoss/linesmith/commit/c8c213fe423e93884cc82a4857d7f4728dff0f21))
- **core:** Tolerate Windows MoveFileEx race losers in cascade persistence([abe697d](https://github.com/oakoss/linesmith/commit/abe697d8c614a2269382fe2e26d1410725a52012))
- **core:** Gate KEYCHAIN_SERVICE to macOS so Ubuntu clippy passes([769e355](https://github.com/oakoss/linesmith/commit/769e355e70c043a72e4d620a0f61fff1c29755ec))
- **core:** Tolerate null required leaves in parse_context_window([93506ac](https://github.com/oakoss/linesmith/commit/93506acca6f9cb475b9dc365d74156abf489d02f))

### Features

- **doctor:** Land linesmith doctor v0.1 (Config..Self categories)([16d1950](https://github.com/oakoss/linesmith/commit/16d19509a95eae9cb34b5b395a696f8f136c72ae))
- **doctor:** Wire Environment + Self check categories([546029c](https://github.com/oakoss/linesmith/commit/546029c6ecf65c0ed7dbda7505d60766cfdd40ad))
- **doctor:** Scaffold linesmith doctor subcommand([bdac37f](https://github.com/oakoss/linesmith/commit/bdac37f8099374de416d3962bd9c9eaae5301124))
- **data:** Honor HTTPS_PROXY / HTTP_PROXY / NO_PROXY for OAuth fetcher([4cab952](https://github.com/oakoss/linesmith/commit/4cab95255c699ff62d2cd6a3684a52967f7a69a5))
- **config:** Publish config.schema.json + wire CI drift check([70f0723](https://github.com/oakoss/linesmith/commit/70f0723dd98d3c93697528a5cf1b3600e20800e4))
- **input:** Warn-and-degrade for cost / effort / vim / output_style / agent / version([c6ea8cf](https://github.com/oakoss/linesmith/commit/c6ea8cf71a7fc49ed5589c19e344a910860a0906))
- **input:** Warn-and-degrade parse for model/workspace/context_window([44b8d5a](https://github.com/oakoss/linesmith/commit/44b8d5a5564db7e8cd491bdadee8a9746dcdff78))
- **layout:** Add multi-line layout via [line.N] + layout key([414d955](https://github.com/oakoss/linesmith/commit/414d95566ad9f6670ac662681a29c96547ba1ae8))
- **cli:** Add interactive `linesmith init` subcommand([7ce72d8](https://github.com/oakoss/linesmith/commit/7ce72d8d718d5eec0b5e4a5c04899f575570e931))
- **layout:** Add [layout_options].separator + powerline_width config([f34b8df](https://github.com/oakoss/linesmith/commit/f34b8dfa618fa2e0de45c1e5561a0705bf975568))
- **themes:** Add 5 curated presets + extract default/minimal([e1d17ae](https://github.com/oakoss/linesmith/commit/e1d17ae4e9bfc6ec17408c3783555d5d71baaf1c))
- **engine:** Wire OSC 8 hyperlinks end-to-end with sanitized URLs([d016c72](https://github.com/oakoss/linesmith/commit/d016c72d594e5e461db5105f03f040f5f7716bf4))
- **segments:** Add opt-in version segment for CC CLI version([d0bc5d7](https://github.com/oakoss/linesmith/commit/d0bc5d7a6e51cc2e0633a43727b9a8fbd5e12bd3))
- **layout:** Add render_to_runs styled-run emit path([06945ad](https://github.com/oakoss/linesmith/commit/06945ad8e1ddb9cc8770d2f2d63681d5aaec9276))
- **core:** Land render perf benchmarks + cold-start hyperfine task([f2d5096](https://github.com/oakoss/linesmith/commit/f2d5096a18ec29467d8b47257fb9262c09caf9fb))
- **segments:** Add output_style + vim + agent segments([d249322](https://github.com/oakoss/linesmith/commit/d249322a50abd8a9f73ad9955d52b3151c51fa0e))
- **core:** Add Segment::shrink_to_fit for layout-pressure compaction([327390f](https://github.com/oakoss/linesmith/commit/327390fb1a831930baa06bcd061b55af25f0f678))
- **segments:** Add per-marker hide_below_cells thresholds to git_branch([ab74914](https://github.com/oakoss/linesmith/commit/ab7491458397842b49e91e1cd1c870af07284b83))
- **segments:** Add model.format config (compact strips "context" filler)([8a5c845](https://github.com/oakoss/linesmith/commit/8a5c8458eb09f2ee99b78ce0ae0b9752e5945174))
- **core:** Add RenderContext arg to Segment::render([fecbf33](https://github.com/oakoss/linesmith/commit/fecbf33e84fc80a70b0c92d7dba79abc22e349d4))
- **core:** Reflow truncatable segments before dropping([4ea4df1](https://github.com/oakoss/linesmith/commit/4ea4df1372c8b736e44ffc595f6c2113347c61e5))
- **segments:** Add session_duration segment([5fdf737](https://github.com/oakoss/linesmith/commit/5fdf7370bd9c6938e47420e176fc483c7e23bc4a))
- **segments:** Add context_bar visual fill segment([b293f03](https://github.com/oakoss/linesmith/commit/b293f03ffe89c2295fe2d5f7dd6786bb2f49d021))

### Refactoring

- **repo:** Split linesmith into linesmith-core + linesmith-cli workspace([602d9f0](https://github.com/oakoss/linesmith/commit/602d9f06e8fe70ebc400c15c8c61b995bb4f40a8))
- **runtime:** Extract runtime predicates into a dedicated module([88dc4f9](https://github.com/oakoss/linesmith/commit/88dc4f9020a2c3381935eefa0bf8500fcef54d6e))
- **data:** Collapse XDG cascade + lift credentials env to a snapshot([0141749](https://github.com/oakoss/linesmith/commit/0141749a5f4e6154f5f60d7e9c288c48f748b8ce))
- **config:** Pin schema URL to main per industry pragma([b1e83cc](https://github.com/oakoss/linesmith/commit/b1e83cce91eec1c8fb1834e86626d5ef560f94be))

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
