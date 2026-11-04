//! Plugin format adapters. Only the host binary links these.

#[cfg(windows)]
pub mod vst2;
pub mod vst3;
