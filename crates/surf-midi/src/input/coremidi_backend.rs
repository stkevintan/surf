//! macOS backend: CoreMIDI.
//!
//! CoreMIDI calls the input port's read block on its own high-priority thread
//! for every packet list, which plays the role of the WinMM driver callback.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result};
use coremidi::{Client, InputPort, Notification, PacketList, Source, Sources};

use super::{Deliver, parse_message};

pub(crate) struct Connection {
    // Field order is drop order: disconnect the port before the client goes away.
    port: InputPort,
    source: Source,
    _client: Client,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let _ = self.port.disconnect_source(&self.source);
    }
}

fn source_name(source: &Source) -> String {
    source
        .display_name()
        .or_else(|| source.name())
        .unwrap_or_else(|| "Unknown MIDI input".to_string())
}

/// Keeps a CoreMIDI client alive whose notify block marks the port list dirty
/// when devices are added, removed or reconfigured.
pub(crate) struct Watcher {
    _client: Client,
}

pub(crate) fn watch(dirty: Arc<AtomicBool>) -> Option<Watcher> {
    // CoreMIDI only delivers notifications for clients created on a thread
    // with a run loop; the GPUI main thread has one, and the 2 s fallback
    // poll covers anywhere else.
    let client = Client::new_with_notifications("Surf Watcher", move |n: &Notification| {
        if matches!(
            n,
            Notification::SetupChanged
                | Notification::ObjectAdded(_)
                | Notification::ObjectRemoved(_)
        ) {
            dirty.store(true, Ordering::Release);
        }
    })
    .ok()?;
    Some(Watcher { _client: client })
}

pub(crate) fn list_ports() -> Result<Vec<String>> {
    Ok(Sources.into_iter().map(|s| source_name(&s)).collect())
}

pub(crate) fn open(index: usize, deliver: Deliver) -> Result<Connection> {
    let source =
        Source::from_index(index).with_context(|| format!("no MIDI input at index {index}"))?;
    let client = Client::new("Surf")
        .map_err(|status| anyhow::anyhow!("MIDIClientCreate failed ({status})"))?;
    let port = client
        .input_port("Surf Input", move |packets: &PacketList| {
            for packet in packets.iter() {
                dispatch(packet.data(), &deliver);
            }
        })
        .map_err(|status| anyhow::anyhow!("MIDIInputPortCreate failed ({status})"))?;
    port.connect_source(&source)
        .map_err(|status| anyhow::anyhow!("MIDIPortConnectSource failed ({status})"))?;
    Ok(Connection {
        port,
        source,
        _client: client,
    })
}

/// A CoreMIDI packet may hold several messages back to back, and may use
/// running status (a data byte where a status byte would be). Walk it.
fn dispatch(bytes: &[u8], deliver: &Deliver) {
    let mut status = 0u8;
    let mut i = 0;
    while i < bytes.len() {
        let byte = bytes[i];
        if byte >= 0xF0 {
            // System messages: SysEx runs to 0xF7; real-time bytes stand alone.
            if byte == 0xF0 {
                while i < bytes.len() && bytes[i] != 0xF7 {
                    i += 1;
                }
            }
            i += 1;
            continue;
        }
        if byte & 0x80 != 0 {
            status = byte;
            i += 1;
        } else if status == 0 {
            i += 1;
            continue;
        }
        let len = match status & 0xF0 {
            0xC0 | 0xD0 => 1,
            _ => 2,
        };
        if i + len > bytes.len() {
            break;
        }
        let data1 = bytes[i];
        let data2 = if len == 2 { bytes[i + 1] } else { 0 };
        if let Some(event) = parse_message(status, data1, data2) {
            deliver(event);
        }
        i += len;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::LiveEvent;

    fn collect(bytes: &[u8]) -> Vec<LiveEvent> {
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink = out.clone();
        let deliver: Deliver = Arc::new(move |e| sink.lock().unwrap().push(e));
        dispatch(bytes, &deliver);
        out.lock().unwrap().clone()
    }

    #[test]
    fn running_status_and_multiple_messages() {
        let events = collect(&[0x90, 60, 100, 64, 90, 0x80, 60, 0, 0xB0, 64, 127]);
        assert_eq!(
            events,
            vec![
                LiveEvent::NoteOn {
                    key: 60,
                    velocity: 100
                },
                LiveEvent::NoteOn {
                    key: 64,
                    velocity: 90
                },
                LiveEvent::NoteOff { key: 60 },
                LiveEvent::Sustain(true),
            ]
        );
    }

    #[test]
    fn skips_sysex_and_realtime() {
        let events = collect(&[0xF8, 0xF0, 1, 2, 3, 0xF7, 0x90, 62, 80, 0xFE]);
        assert_eq!(
            events,
            vec![LiveEvent::NoteOn {
                key: 62,
                velocity: 80
            }]
        );
    }

    /// The CoreMIDI tests share one system-wide device list, and the test harness runs them on
    /// parallel threads. A virtual source from one test would otherwise show up in the other's
    /// hub: the hot-plug test auto-connects to it, then sees it vanish mid-test.
    static COREMIDI_TESTS: Mutex<()> = Mutex::new(());

    fn coremidi_lock() -> std::sync::MutexGuard<'static, ()> {
        COREMIDI_TESTS.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// End to end through CoreMIDI: a virtual source shows up as a port, and
    /// bytes sent from it arrive parsed through [`MidiHub`]. Needs no hardware.
    #[test]
    fn virtual_source_reaches_the_hub() {
        let _serial = coremidi_lock();
        use std::time::{Duration, Instant};

        use coremidi::PacketBuffer;

        use crate::MidiHub;

        let name = format!("Surf Test {}", std::process::id());
        let client = Client::new("Surf Test Sender").unwrap();
        let source = client.virtual_source(&name).unwrap();

        let heard = Arc::new(Mutex::new(Vec::new()));
        let sink = heard.clone();
        let mut hub = MidiHub::new(Arc::new(move |e| sink.lock().unwrap().push(e)));
        hub.refresh();
        let index = hub
            .ports()
            .iter()
            .position(|p| p == &name)
            .expect("virtual source is listed");
        hub.connect(index).unwrap();

        source
            .received(&PacketBuffer::new(0, &[0x90, 60, 100, 0x80, 60, 0]))
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(2);
        while heard.lock().unwrap().len() < 2 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let expected = vec![
            LiveEvent::NoteOn {
                key: 60,
                velocity: 100,
            },
            LiveEvent::NoteOff { key: 60 },
        ];
        assert_eq!(*heard.lock().unwrap(), expected, "sink saw the notes");
        let timed: Vec<_> = hub.drain().into_iter().map(|t| t.event).collect();
        assert_eq!(timed, expected, "UI stream saw the notes");
    }

    /// A source that appears after the hub started shows up in the port list.
    /// If nothing was connected it is connected automatically; a port that is
    /// already connected (a real keyboard on this machine) is not replaced.
    /// Removing the source takes it out of the list again.
    #[test]
    fn hot_plugged_source_is_listed_and_removed() {
        let _serial = coremidi_lock();
        use std::time::{Duration, Instant};

        use crate::{MidiHub, PortChange};

        let wait_for = |hub: &mut MidiHub, cond: &dyn Fn(&MidiHub) -> bool| {
            let mut changes = Vec::new();
            // Notifications need a run loop; without one the fallback poll
            // (every 2 s) still has to find the change.
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline && !cond(hub) {
                changes.extend(hub.poll());
                std::thread::sleep(Duration::from_millis(50));
            }
            changes
        };

        let mut hub = MidiHub::new(Arc::new(|_| {}));
        let had_connection = hub.selected().map(|i| hub.ports()[i].clone());
        let name = format!("Surf Hotplug {}", std::process::id());
        let client = Client::new("Surf Hotplug Sender").unwrap();
        let source = client.virtual_source(&name).unwrap();

        let changes = wait_for(&mut hub, &|h| h.ports().contains(&name));
        assert!(hub.ports().contains(&name), "new source is listed");
        let connected_to = hub.selected().map(|i| hub.ports()[i].clone());
        match &had_connection {
            Some(previous) => assert_eq!(
                connected_to.as_ref(),
                Some(previous),
                "an existing connection is kept"
            ),
            None => {
                assert_eq!(connected_to.as_deref(), Some(name.as_str()), "auto-connected");
                assert!(changes.contains(&PortChange::Connected(name.clone())));
            }
        }

        drop(source);
        wait_for(&mut hub, &|h| !h.ports().contains(&name));
        assert!(!hub.ports().contains(&name), "removed source is unlisted");
        let still = hub.selected().map(|i| hub.ports()[i].clone());
        assert_eq!(still, had_connection, "back to the original connection");
    }
}
