//! Audio for Surf: the real-time engine, the [`Instrument`] trait, the
//! built-in piano and offline WAV rendering.
//!
//! Plugin instruments live in `surf-plugin`; they implement [`Instrument`] too.

pub mod engine;
pub mod render;
pub mod synth;

pub use engine::{AudioEngine, Command, EngineStatus, OutputEvent};
pub use synth::Synth;

pub const MIN_KEY: u8 = 21;
pub const MAX_KEY: u8 = 108;
pub const SAMPLE_RATE: u32 = 48_000;

/// Something that can turn note events into stereo audio.
///
/// Implementations must be real-time friendly: `process` is called from the
/// audio thread and must not block or allocate.
pub trait Instrument: Send {
    fn note_on(&mut self, key: u8, velocity: u8);
    fn note_off(&mut self, key: u8);
    fn all_notes_off(&mut self);
    fn set_sustain(&mut self, down: bool);
    fn process(&mut self, out: &mut [f32], frames: usize);
    fn latency_samples(&self) -> u32 {
        0
    }
    /// Called once when the audio device is opened.  Instruments that cannot
    /// change sample rate after construction (VST3) may ignore it.
    fn set_sample_rate(&mut self, _sample_rate: f32) {}
    fn name(&self) -> &str;
}
