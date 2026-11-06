//! Drive one VST2 (or VST3) plugin through the plugin host the way the app does:
//! load, play a note, save and restore state, and ask for the editor.
//!
//! ```sh
//! cargo run --example vst2_session -- "/Library/Audio/Plug-Ins/VST/Some Synth.vst"
//! ```

use surf::audio::Instrument;
use surf::plugin::client::VstClient;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: vst2_session <plugin>");
    let mut client = VstClient::load(&path, 48_000, None)?;
    println!("loaded: {}", client.name);

    let mut instrument = client.instrument();
    instrument.note_on(0, 64, 100);
    let mut buffer = [0.0f32; 128];
    let mut peak = 0.0f32;
    for _ in 0..1_000 {
        instrument.process(&mut buffer, 64);
        peak = buffer.iter().fold(peak, |p, s| p.max(s.abs()));
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    instrument.note_off(0, 64);
    println!("audio peak: {:.1}%", peak * 100.0);

    let state =
        std::env::temp_dir().join(format!("surf-vst2-session-{}.state", std::process::id()));
    let bytes = client.save_state(&state)?;
    println!("state saved: {bytes} bytes");
    client.load_state(&state)?;
    println!("state restored");
    let _ = std::fs::remove_file(&state);

    match client.open_editor() {
        Ok(()) => println!("editor: opened"),
        Err(err) => println!("editor: {err:#}"),
    }
    println!("host still alive: {}", client.is_alive());
    Ok(())
}
