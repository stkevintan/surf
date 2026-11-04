//! Plugin instruments for Surf.
//!
//! Plugins never run in the UI process. The UI side ([`client`], [`remote`])
//! starts a `surf-vst-host` process per plugin, talks to it with the JSON
//! [`protocol`], and moves real-time audio and MIDI through [`shm`]. The host
//! binary links the format adapters in [`backends`] (feature `host`).
//!
//! [`scan`] only walks the file system, so it is safe to call from the UI.

pub mod client;
pub mod protocol;
pub mod remote;
pub mod scan;
pub mod shm;

#[cfg(feature = "host")]
pub mod backends;
