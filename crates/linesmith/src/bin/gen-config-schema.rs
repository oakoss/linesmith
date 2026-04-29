//! Print `config.schema.json` to stdout.
//!
//! Used by `mise run schema:update` to regenerate the committed
//! `config.schema.json` at the repo root, and by `mise run
//! schema:check` to fail CI when the committed file drifts from
//! the Rust types in `crate::config`.
//!
//! The schema is consumed by editor integrations (taplo / VS Code /
//! Zed) via the `#:schema` directive that `linesmith init` writes
//! into user-generated configs.

use linesmith::config::Config;

fn main() {
    let schema = schemars::schema_for!(Config);
    let json = serde_json::to_string_pretty(&schema)
        .expect("schemars-generated schema must serialize as JSON");
    println!("{json}");
}
