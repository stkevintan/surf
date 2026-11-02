//! Does initialising COM before loading change anything?
//!
//! Contact 8.14's plugin aborts with a ZeroMQ "Bad file descriptor" in hosts
//! that never call `CoInitializeEx`, while its standalone app (which does) runs
//! fine. This probe loads the same plugin after joining an STA:
//!
//! ```powershell
//! cargo run --example com_probe -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
//! ```

use surf::audio::vst::VstPlugin;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: com_probe <path-to.vst3>");
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        println!("CoInitializeEx -> {hr:?}");
    }
    println!("loading {path}");
    let plugin = VstPlugin::load(&path, 48_000)?;
    println!("name: {}", plugin.name);
    println!("has_editor: {}", plugin.has_editor());
    Ok(())
}
