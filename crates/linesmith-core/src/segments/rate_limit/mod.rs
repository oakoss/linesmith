//! Rate-limit segment family. Renders the 5-hour and 7-day usage
//! windows from `ctx.usage()` per `docs/specs/rate-limit-segments.md`.
//!
//! See `format` for the shared render helpers, and the per-window
//! files (`five_hour`, `five_hour_reset`, `seven_day`, `seven_day_reset`)
//! for the four `Segment` impls. The dispatcher in `segments::mod.rs`
//! wires the user-facing `[segments.rate_limit_*]` config keys to the
//! segment structs re-exported below.

pub mod five_hour;
pub mod five_hour_reset;
pub mod format;
pub mod seven_day;
pub mod seven_day_reset;

pub use five_hour::RateLimit5hSegment;
pub use five_hour_reset::RateLimit5hResetSegment;
pub use seven_day::RateLimit7dSegment;
pub use seven_day_reset::RateLimit7dResetSegment;
