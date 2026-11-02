//! Minimal VST3 smoke test: `cargo run --example vst_probe -- <bundle.vst3>`
//!
//! Loading a plugin in-process can crash if the plugin is broken, so this is a
//! separate process from the visualizer.

use surf::audio::vst::VstPlugin;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: vst_probe <path-to.vst3-bundle>");
    println!("loading {path}");
    let plugin = VstPlugin::load(&path, 48_000)?;
    println!("name: {}", plugin.name);
    println!("has_editor: {}", plugin.has_editor());
    match plugin.state() {
        Ok(state) => println!("state bytes: {}", state.len()),
        Err(err) => println!("state error: {err:#}"),
    }
    Ok(())
}
