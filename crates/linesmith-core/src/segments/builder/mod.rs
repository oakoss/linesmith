//! `Config` → `Vec<Box<dyn Segment>>` with validation. Hides built-in
//! registry lookup, duplicate handling, unknown-ID warnings,
//! per-segment override merging, and plugin-registry consultation.

mod dispatch;
mod layout;
mod plugins;

pub use dispatch::{build_default_segments, build_lines, build_segments};

#[cfg(test)]
mod tests;
