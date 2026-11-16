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
    /// What the open connection's device holds down, to release on disconnect.
    held: Option<Arc<Held>>,
    selected: Option<usize>,
    /// The port the connection is on.
    on: Option<PortId>,
    /// The port the user chose ([`MidiHub::connect`]). Kept while it is unplugged,
    /// and through any port Surf connects to on its own meanwhile, so it is taken
    /// again when it returns.
    wanted: Option<PortId>,
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
            held: None,
            selected: None,
            on: None,
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

        let on = self.on.clone();
        match plan(
            self.wanted.as_ref(),
            on.as_ref(),
            &before_ports,
            &self.devices,
        ) {
            // Indices may have shifted; the connection itself stays as it is.
            Plan::Stay(index) => {
                self.selected = Some(index);
                None
            }
            Plan::Open(index) => match self.open(index) {
                Ok(()) => Some(PortChange::Connected(self.devices[index].clone())),
                Err(_) => on.map(|port| PortChange::Disconnected(port.name)),
            },
            // Gone, and nothing to switch to: stay disconnected rather than grab
            // an unrelated port. `wanted`, or a fresh plug, brings one back.
            Plan::Close => {
                self.disconnect();
                on.map(|port| PortChange::Disconnected(port.name))
            }
            Plan::Nothing => None,
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
            let _ = self.open(0);
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

    /// Connect to the port at `index`, as the user's choice: it is returned to
    /// after being unplugged.
    pub fn connect(&mut self, index: usize) -> Result<()> {
        self.open(index)?;
        self.wanted = self.on.clone();
        Ok(())
    }

    /// Open the port at `index` without making it the user's choice.
    fn open(&mut self, index: usize) -> Result<()> {
        let Some(port) = port_at(&self.devices, index) else {
            anyhow::bail!("no MIDI input at index {index}");
        };
        self.disconnect();
        let held = Arc::new(Held::default());
        let deliver = tracking(held.clone(), self.sink.clone(), self.tx.clone());
        // Open by name and occurrence: the backend enumerates again, and a bare
        // index could point at another port if one came or went in between.
        match backend::open(&port.name, port.nth, deliver) {
            Ok(connection) => {
                self.connection = Some(connection);
                self.held = Some(held);
                self.selected = Some(index);
                self.on = Some(port);
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
        // Close first: once the device's callbacks have stopped, nothing can race
        // the releases below.
        self.connection = None;
        if let Some(held) = self.held.take() {
            release_held(&held, &self.sink, &self.tx);
        }
        self.selected = None;
        self.on = None;
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

/// Keys and the sustain pedal a device holds down. Updated on the driver's
/// thread, so plain atomics: no locks there.
struct Held {
    keys: [AtomicBool; 128],
    pedal: AtomicBool,
}

impl Default for Held {
    fn default() -> Self {
        Held {
            keys: std::array::from_fn(|_| AtomicBool::new(false)),
            pedal: AtomicBool::new(false),
        }
    }
}

/// Deliver events to the sink and the UI, keeping track of what is held.
fn tracking(held: Arc<Held>, sink: NoteSink, tx: Sender<TimedMidi>) -> Deliver {
    Arc::new(move |event| {
        // Timestamp before anything else so the UI sees arrival time.
        let at = Instant::now();
        match event {
            LiveEvent::NoteOn { key, .. } => {
                held.keys[key as usize & 127].store(true, Ordering::Relaxed)
            }
            LiveEvent::NoteOff { key } => {
                held.keys[key as usize & 127].store(false, Ordering::Relaxed)
            }
            LiveEvent::Sustain(down) => held.pedal.store(down, Ordering::Relaxed),
        }
        sink(event);
        let _ = tx.send(TimedMidi { at, event });
    })
}

/// A device went away (unplugged, or Surf switched to another one) while keys or
/// the pedal were down: their releases can no longer arrive, so send them now to
/// the audio engine and the UI alike, so nothing hangs, lights up or records on.
fn release_held(held: &Held, sink: &NoteSink, tx: &Sender<TimedMidi>) {
    let at = Instant::now();
    let send = |event| {
        sink(event);
        let _ = tx.send(TimedMidi { at, event });
    };
    for (key, down) in held.keys.iter().enumerate() {
        if down.swap(false, Ordering::Relaxed) {
            send(LiveEvent::NoteOff { key: key as u8 });
        }
    }
    if held.pedal.swap(false, Ordering::Relaxed) {
        send(LiveEvent::Sustain(false));
    }
}

/// A port as Surf tells ports apart: its name and, when several share it (two
/// identical keyboards), which of them it is in list order. Backends give no id
/// that is stable on every OS, so identical devices are told apart by order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PortId {
    name: String,
    nth: usize,
}

fn port_at(list: &[String], index: usize) -> Option<PortId> {
    let name = list.get(index)?;
    let nth = list[..index].iter().filter(|p| *p == name).count();
    Some(PortId {
        name: name.clone(),
        nth,
    })
}

fn index_of(list: &[String], port: &PortId) -> Option<usize> {
    list.iter()
        .enumerate()
        .filter(|(_, p)| **p == port.name)
        .nth(port.nth)
        .map(|(i, _)| i)
}

/// What to do after the port list changed.
#[derive(Debug, PartialEq, Eq)]
enum Plan {
    /// Keep the connection; the port is now at this index.
    Stay(usize),
    /// Connect to the port at this index.
    Open(usize),
    /// Drop the connection: its port is gone.
    Close,
    Nothing,
}

/// The wanted port when it is listed; else a port that just appeared (a keyboard
/// plugged in); else the current one if it is still there. Never a port that was
/// already listed and not chosen.
fn plan(wanted: Option<&PortId>, on: Option<&PortId>, before: &[String], now: &[String]) -> Plan {
    let here = |port: &PortId| index_of(now, port);
    let target = wanted.and_then(here).or_else(|| {
        (0..now.len()).find(|&i| port_at(now, i).is_some_and(|p| index_of(before, &p).is_none()))
    });
    let current = on.and_then(here);
    match (current, target) {
        (Some(c), Some(t)) if c == t => Plan::Stay(c),
        (_, Some(t)) => Plan::Open(t),
        (Some(c), None) => Plan::Stay(c),
        (None, None) if on.is_some() => Plan::Close,
        (None, None) => Plan::Nothing,
    }
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

    /// The hub's port bookkeeping, without a backend: applies each plan the way
    /// `poll` does, with `connect` as the user's choice.
    struct Sim {
        list: Vec<String>,
        on: Option<PortId>,
        wanted: Option<PortId>,
    }

    impl Sim {
        fn choose(list: &[&str], index: usize) -> Self {
            let list = ports(list);
            let on = port_at(&list, index);
            Sim {
                wanted: on.clone(),
                on,
                list,
            }
        }

        fn change(&mut self, list: &[&str]) -> Option<&str> {
            let before = std::mem::replace(&mut self.list, ports(list));
            match plan(self.wanted.as_ref(), self.on.as_ref(), &before, &self.list) {
                Plan::Stay(_) | Plan::Nothing => {}
                Plan::Open(i) => self.on = port_at(&self.list, i),
                Plan::Close => self.on = None,
            }
            self.on.as_ref().map(|p| p.name.as_str())
        }
    }

    /// Review M16: unplugging the chosen keyboard must not grab an unrelated
    /// port, and it is taken back when it returns, even after Surf moved to a
    /// device plugged in meanwhile.
    #[test]
    fn hot_plug_returns_to_the_chosen_port() {
        let mut s = Sim::choose(&["Midi Through", "Keyboard"], 1);
        assert_eq!(s.change(&["Midi Through"]), None, "nothing new: stay off");
        assert_eq!(
            s.change(&["Midi Through", "Pad"]),
            Some("Pad"),
            "new device taken"
        );
        assert_eq!(
            s.change(&["Midi Through", "Pad", "Keyboard"]),
            Some("Keyboard"),
            "the chosen one wins when it returns"
        );
        assert_eq!(
            s.change(&["Midi Through", "Pad", "Keyboard", "Drums"]),
            Some("Keyboard"),
            "a new device does not take over the chosen one"
        );
    }

    /// Without a choice, Surf follows what is plugged in and never falls back to
    /// a port that was already there.
    #[test]
    fn without_a_choice_the_newest_device_is_used() {
        let mut s = Sim {
            list: ports(&["Midi Through"]),
            on: port_at(&ports(&["Midi Through"]), 0),
            wanted: None,
        };
        assert_eq!(s.change(&["Midi Through", "Keyboard"]), Some("Keyboard"));
        assert_eq!(s.change(&["Midi Through"]), None);
        assert_eq!(s.change(&["Midi Through", "Keyboard"]), Some("Keyboard"));
    }

    /// Review: two ports with one name are two ports. The second stays reachable
    /// and is the one kept.
    #[test]
    fn identical_names_are_told_apart_by_order() {
        let two = ports(&["Keyboard", "Keyboard"]);
        let second = port_at(&two, 1).unwrap();
        assert_eq!(second.nth, 1);
        assert_eq!(index_of(&two, &second), Some(1));
        assert_eq!(index_of(&ports(&["Keyboard"]), &second), None);
        let mut s = Sim::choose(&["Keyboard", "Keyboard"], 1);
        assert_eq!(s.change(&["Keyboard", "Keyboard", "Pad"]), Some("Keyboard"));
        assert_eq!(
            s.on.as_ref().map(|p| p.nth),
            Some(1),
            "still the second one"
        );
    }

    /// Review: switching devices (or unplugging one) while a key and the pedal
    /// are down releases them, to the audio engine and the UI alike.
    #[test]
    fn a_dropped_device_releases_what_it_held() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let audio = seen.clone();
        let sink: NoteSink = Arc::new(move |e| audio.lock().unwrap().push(e));
        let (tx, rx) = unbounded();
        let held = Arc::new(Held::default());
        let deliver = tracking(held.clone(), sink.clone(), tx.clone());
        deliver(LiveEvent::NoteOn {
            key: 60,
            velocity: 90,
        });
        deliver(LiveEvent::NoteOn {
            key: 64,
            velocity: 90,
        });
        deliver(LiveEvent::NoteOff { key: 64 });
        deliver(LiveEvent::Sustain(true));
        seen.lock().unwrap().clear();
        let _: Vec<_> = rx.try_iter().collect();

        release_held(&held, &sink, &tx);
        let want = vec![LiveEvent::NoteOff { key: 60 }, LiveEvent::Sustain(false)];
        assert_eq!(*seen.lock().unwrap(), want, "to the audio engine");
        let ui: Vec<_> = rx.try_iter().map(|m| m.event).collect();
        assert_eq!(ui, want, "to the UI");
        // Released once: a second drop sends nothing.
        release_held(&held, &sink, &tx);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[test]
    fn only_cc64_is_sustain() {
        assert_eq!(parse_message(0xB0, 64, 127), Some(LiveEvent::Sustain(true)));
        assert_eq!(parse_message(0xB0, 64, 10), Some(LiveEvent::Sustain(false)));
        assert_eq!(parse_message(0xB0, 1, 127), None);
        assert_eq!(parse_message(0xE0, 0, 64), None);
    }
}
