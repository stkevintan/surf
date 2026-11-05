//! Watch the audio ring's fill level while consuming it like the real callback does.
//!
//! Live-play lag means sound arrives late, and every frame queued in the ring is
//! exactly that: already rendered, not yet audible. The helper fills the ring on a
//! wall-clock schedule while the UI drains one output buffer per callback, so if the
//! producer runs even slightly ahead the backlog grows and *stays* grown — nothing in
//! the current design ever shortens it.
//!
//! This consumes 480 frames every 10 ms (the measured WASAPI period on this machine)
//! and prints the backlog in ms, so the steady-state latency is a number rather than
//! a theory.
//!
//! ```powershell
//! cargo run --example ring_backlog -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
//! ```

use std::time::{Duration, Instant};

use surf::audio::Instrument;
use surf::plugin::client::VstClient;

const SR: f64 = 48_000.0;
/// Matches the device period measured by `examples/latency_probe.rs`.
const BLOCK: usize = 480;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: ring_backlog <bundle.vst3>");
    let client = VstClient::load(&path, 48_000, None)?;
    let map = client.shared_map();
    let mut instrument = client.instrument();

    println!("plugin loaded; settling for 1s");
    std::thread::sleep(Duration::from_secs(1));

    println!(
        "{:>7}  {:>9}  {:>9}",
        "t (s)", "queued", "latency"
    );

    let start = Instant::now();
    let mut buffer = vec![0.0f32; BLOCK * 2];
    let mut next = Instant::now();
    let mut note_on = false;

    while start.elapsed() < Duration::from_secs(6) {
        // Press a key a second in, like a player would.
        if !note_on && start.elapsed() > Duration::from_secs(1) {
            instrument.note_on(60, 110);
            note_on = true;
        }

        instrument.process(&mut buffer, BLOCK);

        let queued = map.audio_backlog();
        let secs = start.elapsed().as_secs_f64();
        if (secs * 100.0) as u64 % 50 == 0 {
            println!(
                "{:>7.2}  {:>9}  {:>7.1}ms",
                secs,
                queued,
                queued as f64 * 1000.0 / SR
            );
        }

        // Pace like a real audio device instead of spinning.
        next += Duration::from_micros((BLOCK as f64 * 1_000_000.0 / SR) as u64);
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }

    instrument.note_off(60);
    let queued = map.audio_backlog();
    println!();
    println!(
        "final backlog: {} frames = {:.1} ms",
        queued,
        queued as f64 * 1000.0 / SR
    );
    Ok(())
}
