//! Runtime predicates that both the CLI driver and the doctor must
//! agree on. Lifted out of `driver.rs` so doctor can call them
//! directly instead of mirroring them; every mirror is a drift
//! opportunity vs. what the runtime actually does (the parity rule).
//!
//! Each predicate takes whatever env-snapshot it needs — typically
//! [`crate::data_context::xdg::XdgEnv`] — rather than `&CliEnv`, so
//! doctor's `EnvVarState`-derived inputs can call them directly
//! without constructing a synthetic CLI environment. The driver
//! builds the snapshots at the call site.

pub(crate) mod config;
pub(crate) mod plugins;
pub(crate) mod themes;
