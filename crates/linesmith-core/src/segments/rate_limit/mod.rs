//! Rate-limit segment family. Renders the 5-hour and 7-day usage
//! windows from `ctx.usage()` per `docs/specs/rate-limit-segments.md`.
//!
//! See `format` for the shared render helpers, and the per-window
//! files (`five_hour`, `seven_day`, `model_scoped`)
//! for the four `Segment` impls. The dispatcher in `segments::mod.rs`
//! wires the user-facing `[segments.rate_limit_*]` config keys to the
//! segment structs re-exported below.

pub mod config;
pub mod five_hour;
pub mod format;
pub mod model_scoped;
pub mod seven_day;
pub mod window;

pub use five_hour::{RateLimit5hResetSegment, RateLimit5hSegment};
pub use model_scoped::{RateLimit7dModelSegment, Visibility};
pub use seven_day::{RateLimit7dResetSegment, RateLimit7dSegment};
