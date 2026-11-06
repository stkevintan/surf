//! Count ring underruns — the cause of audible crackle on sustained notes.
//!
//! When the consumer asks for a buffer and the ring holds fewer frames, the
//! shortfall is emitted as silence spliced into the middle of a note. That
//! discontinuity is what gets heard as "毛刺" / crackle, and it is distinct from
//! latency: the queue can be *too shallow* (glitches) or *too deep* (lag).
//!
//! This drains at the real device rate and reports, per buffer, whether the ring
//! could satisfy it. A healthy run is 0 short reads with the backlog hovering near
//! the helper's cushion.
//!
//! ```powershell
//! cargo run --example glitch_probe -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
//! ```

use std::time::{Duration, Instant};

use surf::audio::Instrument;
use surf::plugin::client::VstClient;

const SR: f64 = 48_000.0;
/// The measured WASAPI period on this machine (see `examples/latency_probe.rs`).
const BLOCK: usize = 480;
const SECONDS: u64 = 8;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: glitch_probe <bundle.vst3>");
    let client = VstClient::load(&path, 48_000, None)?;
    let map = client.shared_map();
    let mut instrument = client.instrument();

    println!("plugin loaded; settling 1s");
    std::thread::sleep(Duration::from_secs(1));

    // Hold a note so the plugin is genuinely producing audio; silence would hide
    // underruns rather than expose them.
    instrument.note_on(0, 60, 110);

    let mut buffer = vec![0.0f32; BLOCK * 2];
    let mut short_reads = 0u64;
    let mut buffers = 0u64;
    let mut min_backlog = u64::MAX;
    let mut max_backlog = 0u64;
    let mut sum_backlog = 0u128;

    let start = Instant::now();
    let mut next = Instant::now();
    while start.elapsed() < Duration::from_secs(SECONDS) {
        let before = map.audio_backlog();
        min_backlog = min_backlog.min(before);
        max_backlog = max_backlog.max(before);
        sum_backlog += before as u128;
        buffers += 1;

        // A buffer the ring cannot fill is exactly one audible glitch.
        if before < BLOCK as u64 {
            short_reads += 1;
        }

        instrument.process(&mut buffer, BLOCK);

        next += Duration::from_micros((BLOCK as f64 * 1_000_000.0 / SR) as u64);
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        } else {
            next = now;
        }
    }
    instrument.note_off(0, 60);

    let avg = (sum_backlog / buffers.max(1) as u128) as f64;
    println!();
    println!("buffers consumed : {buffers}");
    println!(
        "underruns        : {short_reads}  ({:.2}% of buffers)",
        short_reads as f64 * 100.0 / buffers.max(1) as f64
    );
    println!(
        "backlog min/avg/max : {} / {:.0} / {} frames  ({:.1} / {:.1} / {:.1} ms)",
        min_backlog,
        avg,
        max_backlog,
        min_backlog as f64 * 1000.0 / SR,
        avg * 1000.0 / SR,
        max_backlog as f64 * 1000.0 / SR
    );
    println!();
    if short_reads == 0 {
        println!("verdict: no underruns — should be glitch-free");
    } else {
        println!("verdict: UNDERRUNS — expect audible crackle; cushion is too shallow");
    }
    Ok(())
}
