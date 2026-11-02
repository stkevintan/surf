//! `cargo run --example vst2_audio -- "<plugin.dll>"`

use surf::audio::Instrument;
use surf::audio::vst2::Vst2Plugin;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: vst2_audio <plugin.dll>");
    let plugin = Vst2Plugin::load(&path, 48_000)?;
    println!("name={} outputs={}", plugin.name, plugin.output_channels);
    let mut instrument = plugin.instrument();
    std::thread::sleep(std::time::Duration::from_millis(300));
    instrument.note_on(60, 110);
    let mut buffer = [0.0f32; 128];
    let mut peak = 0.0f32;
    for _ in 0..2_000 {
        instrument.process(&mut buffer, 64);
        for sample in &buffer {
            peak = peak.max(sample.abs());
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    instrument.note_off(60);
    println!("peak={:.1}%", peak * 100.0);
    Ok(())
}
