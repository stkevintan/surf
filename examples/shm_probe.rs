//! Diagnose helper-only crashes: same startup order as `surf-vst-host.exe`
//! (shared memory first, then the plugin), but in a normal console process.
//!
//! ```powershell
//! cargo run --example shm_probe -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
//! ```

use surf::audio::ipc::SharedMap;
use surf::audio::vst::VstPlugin;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: shm_probe <path-to.vst3>");
    let name = surf::audio::ipc::make_name();
    let map = SharedMap::create(&name)?;
    println!("shared memory {name} mapped; loading {path}");
    let plugin = VstPlugin::load(&path, 48_000)?;
    println!("name: {}", plugin.name);
    println!("has_editor: {}", plugin.has_editor());
    map.set_helper_alive(true);
    Ok(())
}
