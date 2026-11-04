//! Live MIDI input.
//!
//! [`MidiHub`] enumerates input ports and keeps one connected. Every parsed
//! event goes two ways at once:
//!
//! - to a [`NoteSink`], called on the driver's callback thread. The app points
//!   this at the audio engine so notes sound without waiting for a UI frame;
//! - to a channel of [`TimedMidi`], timestamped on arrival, which the UI drains
//!   once per frame for drawing and recording.
//!
//! Backends talk to each OS API directly: WinMM on Windows, CoreMIDI on macOS,
//! the ALSA sequencer on Linux. (`midir` would wrap these, but its versions
//! conflict with GPUI's and cpal's pinned dependencies; see the workspace
//! manifest.)

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, unbounded};

#[cfg(target_os = "linux")]
mod alsa_seq;
#[cfg(target_os = "linux")]
use alsa_seq as backend;
#[cfg(target_os = "macos")]
mod coremidi_backend;
#[cfg(target_os = "macos")]
use coremidi_backend as backend;
#[cfg(windows)]
mod winmm;
#[cfg(windows)]
use winmm as backend;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LiveEvent {
    NoteOn { key: u8, velocity: u8 },
    NoteOff { key: u8 },
    Sustain(bool),
}

#[derive(Clone, Copy, Debug)]
pub struct TimedMidi {
    pub at: Instant,
    pub event: LiveEvent,
}

/// Receives every event on the driver's callback thread. Must not block.
pub type NoteSink = Arc<dyn Fn(LiveEvent) + Send + Sync>;

/// What a backend calls for each parsed message.
pub(crate) type Deliver = Arc<dyn Fn(LiveEvent) + Send + Sync>;

pub struct MidiHub {
    devices: Vec<String>,
    connection: Option<backend::Connection>,
    selected: Option<usize>,
    rx: Receiver<TimedMidi>,
    tx: Sender<TimedMidi>,
    sink: NoteSink,
    error: Option<String>,
}

impl MidiHub {
    /// Create a hub and connect to the first available port, if any.
    pub fn new(sink: NoteSink) -> Self {
        let (tx, rx) = unbounded();
        let mut hub = Self {
            devices: Vec::new(),
            connection: None,
            selected: None,
            rx,
            tx,
            sink,
            error: None,
        };
        hub.refresh();
        hub
    }

    /// Re-enumerate ports. Connects to the first port when nothing is connected.
    pub fn refresh(&mut self) {
        match backend::list_ports() {
            Ok(devices) => self.devices = devices,
            Err(err) => {
                self.devices.clear();
                self.error = Some(err.to_string());
            }
        }
        if self.selected.is_some_and(|i| i >= self.devices.len()) {
            self.disconnect();
        }
        if self.connection.is_none() && !self.devices.is_empty() {
            let _ = self.connect(0);
        }
    }

    pub fn ports(&self) -> Vec<String> {
        self.devices.clone()
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn connect(&mut self, index: usize) -> Result<()> {
        if index >= self.devices.len() {
            anyhow::bail!("no MIDI input at index {index}");
        }
        self.connection = None;
        self.selected = None;
        let sink = self.sink.clone();
        let tx = self.tx.clone();
        let deliver: Deliver = Arc::new(move |event| {
            // Timestamp before anything else so the UI sees arrival time.
            let at = Instant::now();
            sink(event);
            let _ = tx.send(TimedMidi { at, event });
        });
        match backend::open(index, deliver) {
            Ok(connection) => {
                self.connection = Some(connection);
                self.selected = Some(index);
                self.error = None;
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.to_string());
                Err(err)
            }
        }
    }

    pub fn disconnect(&mut self) {
        self.connection = None;
        self.selected = None;
    }

    /// Events that arrived since the last call.
    pub fn drain(&self) -> Vec<TimedMidi> {
        self.rx.try_iter().collect()
    }

    /// Connect to a port by name; useful for settings persistence.
    pub fn connect_by_name(&mut self, name: &str) -> Result<()> {
        match self.devices.iter().position(|p| p == name) {
            Some(index) => self.connect(index),
            None => Err(anyhow::anyhow!("MIDI input {name} is not connected")),
        }
    }
}

/// Parse a channel voice message. Surf only acts on notes and the sustain pedal.
pub(crate) fn parse_message(status_byte: u8, data1: u8, data2: u8) -> Option<LiveEvent> {
    let status = status_byte & 0xF0;
    let key = data1 & 0x7F;
    let value = data2 & 0x7F;
    match status {
        0x90 => {
            if value == 0 {
                Some(LiveEvent::NoteOff { key })
            } else {
                Some(LiveEvent::NoteOn {
                    key,
                    velocity: value,
                })
            }
        }
        0x80 => Some(LiveEvent::NoteOff { key }),
        0xB0 if key == 64 => Some(LiveEvent::Sustain(value >= 64)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_on_with_zero_velocity_is_note_off() {
        assert_eq!(
            parse_message(0x90, 60, 0),
            Some(LiveEvent::NoteOff { key: 60 })
        );
        assert_eq!(
            parse_message(0x93, 60, 100),
            Some(LiveEvent::NoteOn {
                key: 60,
                velocity: 100
            })
        );
    }

    #[test]
    fn only_cc64_is_sustain() {
        assert_eq!(parse_message(0xB0, 64, 127), Some(LiveEvent::Sustain(true)));
        assert_eq!(parse_message(0xB0, 64, 10), Some(LiveEvent::Sustain(false)));
        assert_eq!(parse_message(0xB0, 1, 127), None);
        assert_eq!(parse_message(0xE0, 0, 64), None);
    }
}
