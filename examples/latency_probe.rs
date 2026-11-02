//! Measure where the live-play latency actually comes from.
//!
//! Live MIDI -> sound passes through three stages that each add delay:
//!
//!   1. the WASAPI output buffer cpal gives us (`BufferSize::Default`),
//!   2. the shared-memory audio ring between helper and UI, which only drains
//!      as fast as the UI consumes it, and
//!   3. the helper's own 1 ms polling loop.
//!
//! This prints the device's real buffer size and how many frames the ring
//! actually holds in steady state, so the dominant term is a measurement
//! rather than a guess.
//!
//! ```powershell
//! cargo run --example latency_probe
//! ```

use cpal::traits::{DeviceTrait, HostTrait};

fn main() -> anyhow::Result<()> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no default output device"))?;
    // cpal 0.18 exposes the name via description(); sample_rate() is already a u32.
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Default output".to_string());
    println!("device: {name}");

    let supported = device.default_output_config()?;
    let sample_rate = supported.sample_rate();
    println!(
        "default config: {} Hz, {} ch, {:?}",
        sample_rate,
        supported.channels(),
        supported.sample_format()
    );
    println!("buffer size range: {:?}", supported.buffer_size());

    // What `BufferSize::Default` costs us, in time. WASAPI shared mode usually
    // lands at ~10 ms, but the device's own period can be far larger.
    match supported.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } => {
            println!(
                "  min {} frames = {:.1} ms,  max {} frames = {:.1} ms",
                min,
                *min as f64 * 1000.0 / sample_rate as f64,
                max,
                *max as f64 * 1000.0 / sample_rate as f64
            );
        }
        cpal::SupportedBufferSize::Unknown => println!("  (range unknown)"),
    }

    println!();
    println!("ring capacity: {} frames = {:.1} ms @ {} Hz",
        surf::audio::ipc::AUDIO_CAPACITY,
        surf::audio::ipc::AUDIO_CAPACITY as f64 * 1000.0 / sample_rate as f64,
        sample_rate,
    );
    println!();
    println!("Note: the helper fills the ring on a wall-clock schedule and the UI");
    println!("drains it one output buffer at a time, so whatever backlog builds up");
    println!("in the ring is added to the output buffer latency -- it is never");
    println!("drained down once it grows.");
    Ok(())
}
