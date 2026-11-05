//! Surf, a desktop MIDI studio.
//!
//! This root crate is the app binary plus a thin facade over the workspace
//! crates, so the `examples/` probes can keep using short `surf::` paths.

pub use surf_audio as audio;
pub use surf_core as core;
pub use surf_midi as midi;
pub use surf_platform as platform;
pub use surf_plugin as plugin;
pub use surf_ui as ui;
