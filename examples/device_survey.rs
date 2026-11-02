//! Survey every output device's buffer-size range.
//!
//! WASAPI shared mode gives each device a fixed period, and that period is a hard floor
//! on output latency no matter how the render loop is written. The default device here
//! reports `min == max == 480` frames (10 ms), i.e. no room to ask for less. Another
//! device on the same machine may allow far smaller buffers, which would be free latency
//! with no new dependencies — worth checking before reaching for ASIO.
//!
//! ```powershell
//! cargo run --example device_survey
//! ```

use cpal::traits::{DeviceTrait, HostTrait};

fn ms(frames: u32, rate: u32) -> f64 {
    frames as f64 * 1000.0 / rate as f64
}

fn main() -> anyhow::Result<()> {
    let host = cpal::default_host();
    println!("host: {:?}", host.id());

    let default_name = host
        .default_output_device()
        .and_then(|d| d.description().ok().map(|desc| desc.name().to_string()));
    println!("default output: {}\n", default_name.as_deref().unwrap_or("(none)"));

    for device in host.output_devices()? {
        let name = device
            .description()
            .map(|d| d.name().to_string())
            .unwrap_or_else(|_| "(unnamed)".to_string());
        let marker = if Some(&name) == default_name.as_ref() {
            "  <-- current"
        } else {
            ""
        };
        println!("{name}{marker}");

        match device.default_output_config() {
            Ok(cfg) => {
                let rate = cfg.sample_rate();
                print!("  default: {} Hz, {} ch, {:?}", rate, cfg.channels(), cfg.sample_format());
                match cfg.buffer_size() {
                    cpal::SupportedBufferSize::Range { min, max } => {
                        println!(
                            "  buffer {}..{} frames ({:.1}..{:.1} ms)",
                            min,
                            max,
                            ms(*min, rate),
                            ms(*max, rate)
                        );
                    }
                    cpal::SupportedBufferSize::Unknown => println!("  buffer: unknown"),
                }
            }
            Err(err) => println!("  default config unavailable: {err}"),
        }
    }
    Ok(())
}
