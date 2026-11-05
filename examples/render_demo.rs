//! Render the built-in demo song to WAV:
//! `cargo run --example render_demo -- demo.wav`

use surf::audio::render::{RenderOptions, render_song_to_wav};
use surf::core::song::Song;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "surf-demo.wav".to_string());
    let song = Song::demo();
    let stats = render_song_to_wav(
        &song,
        &path,
        &RenderOptions {
            sample_rate: 48_000,
            tail_secs: 2.0,
            gain: 0.9,
        },
    )?;
    println!(
        "wrote {} ({:.2}s, peak {:.0}%)",
        stats.path,
        stats.duration,
        stats.peak * 100.0
    );
    Ok(())
}
