//! Surf's musical data model.
//!
//! Pure data and logic: no I/O, no platform code, no audio. Every other crate
//! builds on these types.
//!
//! - [`project`]: tracks, regions and notes, what the editors work on.
//! - [`edit`]: every change to a project as an undoable command.
//! - [`song`]: the flat MIDI-file form, for import, export and playback.
//! - [`recorder`]: turns live MIDI into notes.

pub mod edit;
pub mod project;
pub mod recorder;
pub mod song;
