//! MIDI I/O for Surf: live input from devices and Standard MIDI Files.

pub mod input;
pub mod smf;

pub use input::{LiveEvent, MidiHub, NoteSink, PortChange, TimedMidi};
