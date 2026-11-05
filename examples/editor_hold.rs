//! Open a plugin's editor through the plugin host and keep it open, so the
//! window can be inspected or used by hand.
//!
//! ```sh
//! cargo run --example editor_hold -- "/Library/Audio/Plug-Ins/VST3/Kontakt 8.vst3" 60
//! ```

use surf::plugin::client::VstClient;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: editor_hold <plugin> [seconds]");
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(30);
    let mut client = VstClient::load(&path, 48_000, None)?;
    println!("loaded: {}", client.name);
    client.open_editor()?;
    println!("editor opened; holding {secs}s");
    for _ in 0..secs {
        std::thread::sleep(std::time::Duration::from_secs(1));
        if !client.is_alive() {
            println!("host exited");
            break;
        }
    }
    Ok(())
}
