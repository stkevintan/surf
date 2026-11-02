//! Dump raw MIDI input, so a non-working pedal becomes a fact rather than a guess.
//!
//! Surf only treats **CC 64** as sustain (`midi_input.rs::parse_message`). Pedals
//! vary: some send CC 64 as expected, some send a different controller, some send
//! continuous "half-damper" values, and some are wired to a jack that sends nothing
//! at all. This prints every message with its status, controller and value, and
//! flags the ones Surf would act on.
//!
//! ```powershell
//! cargo run --example midi_monitor
//! ```
//! Then press keys and the pedal; Ctrl-C to stop.

use std::time::Duration;

use surf::midi_input::{LiveEvent, MidiHub};

fn main() -> anyhow::Result<()> {
    // The hub forwards to an audio thread in the real app; here the receiver is
    // just dropped on the floor, we only want the parsed-event stream.
    let (audio_tx, _audio_rx) = crossbeam_channel::unbounded();
    let mut hub = MidiHub::new(audio_tx);
    hub.refresh();
    let ports = hub.ports();
    if ports.is_empty() {
        println!("no MIDI input devices found");
        return Ok(());
    }
    println!("MIDI inputs:");
    for (i, name) in ports.iter().enumerate() {
        println!("  [{i}] {name}");
    }
    // Take an explicit index, else the first port.
    let index = std::env::args()
        .nth(1)
        .and_then(|a| a.parse::<usize>().ok())
        .unwrap_or(0);
    hub.connect(index)?;
    println!("connected to [{index}] {}", ports[index.min(ports.len() - 1)]);
    if let Some(err) = hub.error() {
        println!("hub error: {err}");
    }
    println!();
    println!("Play keys and press the pedal. Ctrl-C to stop.");
    println!("If the pedal prints nothing, the hardware/jack is the problem.");
    println!("If it prints a CC other than 64, that is why Surf ignores it.");
    println!();

    loop {
        for event in hub.drain() {
            // MidiHub hands back parsed events; show what Surf concluded.
            let line = match event.event {
                LiveEvent::NoteOn { key, velocity } => {
                    format!("parsed: NoteOn  key={key} vel={velocity}")
                }
                LiveEvent::NoteOff { key } => format!("parsed: NoteOff key={key}"),
                LiveEvent::Sustain(down) => {
                    format!("parsed: SUSTAIN {}", if down { "DOWN" } else { "UP" })
                }
            };
            println!("{line}");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}
