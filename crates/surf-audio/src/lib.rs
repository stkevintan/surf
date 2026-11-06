//! Audio for Surf: the real-time engine, the [`Instrument`] trait, the
//! built-in piano and offline WAV rendering.
//!
//! Plugin instruments live in `surf-plugin`; they implement [`Instrument`] too.

pub mod engine;
pub mod render;
pub mod synth;

pub use engine::{AudioEngine, Command, EngineStatus, MAX_SLOTS, OutputEvent, Route};
pub use synth::Synth;

pub const MIN_KEY: u8 = 21;
pub const MAX_KEY: u8 = 108;
pub const SAMPLE_RATE: u32 = 48_000;

/// Something that can turn note events into stereo audio.
///
/// Events carry a MIDI channel (0..=15): several tracks can share one
/// multitimbral plugin (Kontakt) and are told apart by channel. Single-voice
/// instruments like the built-in piano ignore it.
///
/// Implementations must be real-time friendly: `process` is called from the
/// audio thread and must not block or allocate.
pub trait Instrument: Send {
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8);
    fn note_off(&mut self, channel: u8, key: u8);
    fn all_notes_off(&mut self);
    fn set_sustain(&mut self, channel: u8, down: bool);
    /// A MIDI control change. Surf sends CC 7 (channel volume) for tracks
    /// that share a plugin, since their audio comes back already mixed.
    fn control_change(&mut self, _channel: u8, _controller: u8, _value: u8) {}
    fn process(&mut self, out: &mut [f32], frames: usize);
    fn latency_samples(&self) -> u32 {
        0
    }
    /// Called once when the audio device is opened.  Instruments that cannot
    /// change sample rate after construction (VST3) may ignore it.
    fn set_sample_rate(&mut self, _sample_rate: f32) {}
    fn name(&self) -> &str;
}
