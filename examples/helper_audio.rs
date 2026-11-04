//! Real-time shared-memory smoke test:
//! `cargo run --example helper_audio -- <bundle.vst3>`

use surf::audio::Instrument;
use surf::plugin::client::VstClient;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: helper_audio <bundle.vst3>");
    let client = VstClient::load(&path, 48_000, None)?;
    let mut instrument = client.instrument();

    std::thread::sleep(std::time::Duration::from_millis(800));
    instrument.note_on(60, 110);
    let mut buffer = [0.0f32; 128];
    let mut peak = 0.0f32;
    for _ in 0..3_000 {
        instrument.process(&mut buffer, 64);
        for sample in &buffer {
            peak = peak.max(sample.abs());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    instrument.note_off(60);
    println!("shared-memory audio peak: {:.1}%", peak * 100.0);
    Ok(())
}
