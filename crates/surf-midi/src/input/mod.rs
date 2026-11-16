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
//! Devices may come and go while Surf runs. [`MidiHub::poll`] re-enumerates
//! ports when the backend reports a change (or every couple of seconds as a
//! fallback), reconnects to the same port by name, and picks up a newly
//! plugged-in keyboard when nothing is connected.
//!
//! Backends talk to each OS API directly: WinMM on Windows, CoreMIDI on macOS,
//! the ALSA sequencer on Linux. (`midir` would wrap these, but its versions
//! conflict with GPUI's and cpal's pinned dependencies; see the workspace
//! manifest.)

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

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

/// What [`MidiHub::poll`] found, for the status bar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PortChange {
    /// Connected to a port that was not connected before (often a newly
    /// plugged-in keyboard).
    Connected(String),
    /// The connected port disappeared.
    Disconnected(String),
}

/// How often to re-enumerate when the backend cannot notify us.
const POLL_FALLBACK: Duration = Duration::from_secs(2);

pub struct MidiHub {
    devices: Vec<String>,
    connection: Option<backend::Connection>,
    selected: Option<usize>,
    /// The port to be on: the last one connected. Kept while it is unplugged, so
    /// it is picked up again when it returns instead of Surf staying on whatever
    /// else happens to be listed.
    wanted: Option<String>,
    rx: Receiver<TimedMidi>,
    tx: Sender<TimedMidi>,
    sink: NoteSink,
    error: Option<String>,
    /// Set by the backend's change notification; cleared by `poll`.
    dirty: Arc<AtomicBool>,
    _watcher: Option<backend::Watcher>,
    last_scan: Instant,
}

impl MidiHub {
    /// Create a hub and connect to the first available port, if any.
    pub fn new(sink: NoteSink) -> Self {
        let (tx, rx) = unbounded();
        let dirty = Arc::new(AtomicBool::new(false));
        let watcher = backend::watch(dirty.clone());
        let mut hub = Self {
            devices: Vec::new(),
            connection: None,
            selected: None,
            wanted: None,
            rx,
            tx,
            sink,
            error: None,
            dirty,
            _watcher: watcher,
            last_scan: Instant::now(),
        };
        hub.refresh();
        hub
    }

    /// Notice ports that appeared or went away. Call once per UI frame; cheap
    /// unless something changed.
    pub fn poll(&mut self) -> Option<PortChange> {
        let notified = self.dirty.swap(false, Ordering::AcqRel);
        if !notified && self.last_scan.elapsed() < POLL_FALLBACK {
            return None;
        }
        let connected = self.selected.and_then(|i| self.devices.get(i).cloned());
        let before_ports = self.devices.clone();
        let ports = match backend::list_ports() {
            Ok(ports) => ports,
            Err(err) => {
                self.error = Some(err.to_string());
                self.last_scan = Instant::now();
                return None;
            }
        };
        self.last_scan = Instant::now();
        if ports == before_ports {
            return None;
        }
        self.devices = ports;

        let target = pick(self.wanted.as_deref(), &before_ports, &self.devices);
        match (connected, target) {
            // Already on the right port. Indices may have shifted; the connection
            // was opened by name and stays as it is.
            (Some(now), target)
                if self.devices.contains(&now) && target.as_ref().is_none_or(|t| *t == now) =>
            {
                self.selected = self.devices.iter().position(|p| *p == now);
                None
            }
            (connected, Some(target)) => match self.connect_by_name(&target) {
                Ok(()) => Some(PortChange::Connected(target)),
                Err(_) => connected.map(|name| {
                    self.disconnect();
                    PortChange::Disconnected(name)
                }),
            },
            (Some(name), None) => {
                // Gone, and nothing to switch to. Stay disconnected rather than
                // grabbing an unrelated port; `wanted` brings it back on replug.
                self.disconnect();
                Some(PortChange::Disconnected(name))
            }
            (None, None) => None,
        }
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
        // Open by name: the backend re-enumerates, and an index could point at a
        // different port if one came or went in between.
        let name = self.devices[index].clone();
        match backend::open(&name, deliver) {
            Ok(connection) => {
                self.connection = Some(connection);
                self.selected = Some(index);
                self.wanted = Some(name);
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

/// Which port to be on after the list changed from `before` to `now`: the wanted
/// one if it is listed, else one that just appeared (a keyboard plugged in), else
/// none. Never an arbitrary port that was already there.
fn pick(wanted: Option<&str>, before: &[String], now: &[String]) -> Option<String> {
    if let Some(wanted) = wanted
        && now.iter().any(|p| p == wanted)
    {
        return Some(wanted.to_string());
    }
    now.iter().find(|p| !before.contains(p)).cloned()
}

/// Parse a channel voice message. Surf only acts on notes and the sustain pedal.
// The WinMM and CoreMIDI backends use it; ALSA hands over parsed events.
#[cfg_attr(target_os = "linux", allow(dead_code))]
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

    fn ports(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    /// Review M16: unplugging the keyboard must not grab an unrelated port, and
    /// plugging it back in returns to it.
    #[test]
    fn hot_plug_returns_to_the_wanted_port() {
        let all = ports(&["Midi Through", "Keyboard"]);
        let unplugged = ports(&["Midi Through"]);
        // Unplugged: nothing new appeared, so no port is picked.
        assert_eq!(pick(Some("Keyboard"), &all, &unplugged), None);
        // Plugged back in: it is the wanted port again.
        assert_eq!(
            pick(Some("Keyboard"), &unplugged, &all),
            Some("Keyboard".to_string())
        );
        // A new device while on the wanted one does not steal the input.
        let more = ports(&["Midi Through", "Keyboard", "Pad"]);
        assert_eq!(
            pick(Some("Keyboard"), &all, &more),
            Some("Keyboard".to_string())
        );
        // With the wanted one gone, a newly plugged device is taken.
        let swap = ports(&["Midi Through", "Pad"]);
        assert_eq!(
            pick(Some("Keyboard"), &unplugged, &swap),
            Some("Pad".to_string())
        );
        // Nothing wanted yet: only a new port, never an old one.
        assert_eq!(pick(None, &unplugged, &unplugged), None);
    }

    #[test]
    fn only_cc64_is_sustain() {
        assert_eq!(parse_message(0xB0, 64, 127), Some(LiveEvent::Sustain(true)));
        assert_eq!(parse_message(0xB0, 64, 10), Some(LiveEvent::Sustain(false)));
        assert_eq!(parse_message(0xB0, 1, 127), None);
        assert_eq!(parse_message(0xE0, 0, 64), None);
    }
}
