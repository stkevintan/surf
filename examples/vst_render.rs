//! Offline-render a short demo through a VST3 instrument:
//! `cargo run --example vst_render -- <bundle.vst3> out.wav`
//!
//! This is the same code path the app uses for "Export WAV" with Kontakt.

use surf::audio::render::RenderOptions;
use surf::audio::vst::render_vst_to_wav;
use surf::song::Song;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let plugin = args
        .next()
        .expect("usage: vst_render <bundle.vst3> <out.wav>");
    let out = args.next().unwrap_or_else(|| "vst-render.wav".to_string());

    let song = Song::demo();
    let mut timeline = song.timeline();
    // Keep the smoke test short; the real exporter uses the whole song.
    timeline.notes.retain(|n| n.start < 2.0);
    timeline.duration = timeline.notes.iter().map(|n| n.end).fold(0.0, f64::max) + 0.5;

    let stats = render_vst_to_wav(
        plugin,
        None,
        &timeline,
        &out,
        48_000,
        &RenderOptions {
            sample_rate: 48_000,
            tail_secs: 1.5,
            gain: 0.9,
        },
    )?;
    println!(
        "wrote {} ({:.2}s, peak {:.1}%)",
        stats.path,
        stats.duration,
        stats.peak * 100.0
    );
    Ok(())
}
