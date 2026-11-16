//! Linux backend: the ALSA sequencer.
//!
//! Surf creates one input port, subscribes it to the chosen source, and reads
//! events on a dedicated thread, which plays the role of the WinMM driver
//! callback. This file is compiled on Linux only.

use std::ffi::CString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use alsa::seq::{
    Addr, ClientIter, EvCtrl, EvNote, EventType, PortCap, PortIter, PortSubscribe, PortType, Seq,
};
use alsa::{Direction, PollDescriptors};
use anyhow::{Context, Result};

use super::{Deliver, LiveEvent};

pub(crate) struct Connection {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The ALSA sequencer's own client (`SND_SEQ_CLIENT_SYSTEM`). Its `Timer` and
/// `Announce` ports are readable and subscribable, but they carry timer ticks and
/// port announcements, never notes.
const SYSTEM_CLIENT: i32 = 0;

/// Whether a client's ports can be a MIDI input for Surf: not Surf itself, and not
/// the sequencer's System client. Listing `System: Timer` first made Surf
/// auto-connect to it on start, so the keyboard stayed silent.
fn is_input_client(client: i32, own: i32) -> bool {
    client != own && client != SYSTEM_CLIENT
}

/// Readable, subscribable ports from other clients, in a stable order.
fn sources(seq: &Seq) -> Vec<(Addr, String)> {
    let own = seq.client_id().unwrap_or(-1);
    let mut out = Vec::new();
    for client in ClientIter::new(seq) {
        if !is_input_client(client.get_client(), own) {
            continue;
        }
        let client_name = client.get_name().unwrap_or("").to_string();
        for port in PortIter::new(seq, client.get_client()) {
            let caps = port.get_capability();
            if !caps.contains(PortCap::READ | PortCap::SUBS_READ) {
                continue;
            }
            let name = match port.get_name() {
                Ok(name) if name != client_name => format!("{client_name}: {name}"),
                _ => client_name.clone(),
            };
            out.push((port.addr(), name));
        }
    }
    out
}

fn open_seq() -> Result<Seq> {
    let seq = Seq::open(None, Some(Direction::Capture), true)
        .context("could not open the ALSA sequencer")?;
    seq.set_client_name(&CString::new("Surf").unwrap())?;
    Ok(seq)
}

/// Port changes are announced on the sequencer's `System:Announce` port;
/// subscribing to it is a follow-up. Until then `MidiHub::poll` re-enumerates
/// on its fallback timer.
pub(crate) struct Watcher;

pub(crate) fn watch(_dirty: Arc<AtomicBool>) -> Option<Watcher> {
    None
}

pub(crate) fn list_ports() -> Result<Vec<String>> {
    let seq = open_seq()?;
    Ok(sources(&seq).into_iter().map(|(_, name)| name).collect())
}

pub(crate) fn open(name: &str, deliver: Deliver) -> Result<Connection> {
    let seq = open_seq()?;
    let (source, _) = sources(&seq)
        .into_iter()
        .find(|(_, n)| n == name)
        .with_context(|| format!("no MIDI input named {name}"))?;
    let port = seq.create_simple_port(
        &CString::new("Surf Input").unwrap(),
        PortCap::WRITE | PortCap::SUBS_WRITE,
        PortType::MIDI_GENERIC | PortType::APPLICATION,
    )?;
    let sub = PortSubscribe::empty()?;
    sub.set_sender(source);
    sub.set_dest(Addr {
        client: seq.client_id()?,
        port,
    });
    seq.subscribe_port(&sub)
        .context("could not subscribe to the MIDI input")?;

    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread = std::thread::Builder::new()
        .name("surf-midi-in".into())
        .spawn(move || read_loop(seq, deliver, thread_stop))
        .context("could not spawn the MIDI input thread")?;
    Ok(Connection {
        stop,
        thread: Some(thread),
    })
}

fn read_loop(seq: Seq, deliver: Deliver, stop: Arc<AtomicBool>) {
    let mut fds = (&seq, Some(Direction::Capture)).get().unwrap_or_default();
    let mut input = seq.input();
    while !stop.load(Ordering::Acquire) {
        // Wake at least every 50 ms so a disconnect is noticed promptly.
        let _ = alsa::poll::poll(&mut fds, 50);
        while let Ok(event) = input.event_input() {
            let live = match event.get_type() {
                EventType::Noteon => event.get_data::<EvNote>().map(|n| {
                    if n.velocity == 0 {
                        LiveEvent::NoteOff { key: n.note }
                    } else {
                        LiveEvent::NoteOn {
                            key: n.note,
                            velocity: n.velocity,
                        }
                    }
                }),
                EventType::Noteoff => event
                    .get_data::<EvNote>()
                    .map(|n| LiveEvent::NoteOff { key: n.note }),
                EventType::Controller => event
                    .get_data::<EvCtrl>()
                    .filter(|c| c.param == 64)
                    .map(|c| LiveEvent::Sustain(c.value >= 64)),
                _ => None,
            };
            if let Some(live) = live {
                deliver(live);
            }
        }
        std::thread::sleep(Duration::from_millis(0));
    }
}

#[cfg(test)]
mod tests {
    use super::{SYSTEM_CLIENT, is_input_client};

    #[test]
    fn system_and_own_ports_are_not_inputs() {
        let own = 128;
        assert!(
            !is_input_client(SYSTEM_CLIENT, own),
            "System: Timer/Announce"
        );
        assert!(!is_input_client(own, own), "Surf's own port");
        assert!(is_input_client(14, own), "Midi Through");
        assert!(is_input_client(24, own), "a USB keyboard");
    }
}
