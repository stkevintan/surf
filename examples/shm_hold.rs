//! Create a shared-memory map and hold it open, so `surf-vst-host.exe` can be
//! run by hand with real console stdio instead of the pipes `VstClient` uses:
//!
//! ```powershell
//! # terminal 1 — prints the shm name, then waits
//! cargo run --example shm_hold
//! # terminal 2
//! .\target\debug\surf-vst-host.exe --shm <name> --plugin "<path>.vst3" --sample-rate 48000
//! ```
//!
//! If the plugin loads here but not when the app spawns the helper, the
//! difference is the helper's stdio/console setup rather than the plugin.

use surf::plugin::shm::SharedMap;

fn main() -> anyhow::Result<()> {
    let name = surf::plugin::shm::make_name();
    let _map = SharedMap::create(&name)?;
    println!("{name}");
    println!("holding for 120s — start surf-vst-host.exe with --shm {name}");
    std::thread::sleep(std::time::Duration::from_secs(120));
    Ok(())
}
