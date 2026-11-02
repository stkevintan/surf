//! Print the plugins the `SCAN` button would list.
//!
//! ```powershell
//! cargo run --example vst_scan_list
//! # include a folder that is not in the standard locations:
//! $env:SURF_VST_SCAN="D:\Plugins\Kontakt 8\x64"
//! cargo run --example vst_scan_list
//! ```

use surf::audio::vst_scan;

fn main() {
    let plugins = vst_scan::scan_plugin_paths();
    println!("{} plugin(s) found", plugins.len());
    for plugin in &plugins {
        println!(
            "{:>4}  {:<30} {}",
            if plugin.vst2 { "VST2" } else { "VST3" },
            plugin.name,
            plugin.path.display()
        );
    }
    if plugins.is_empty() {
        println!(
            "nothing found — set {} to the folder that contains your plugin",
            vst_scan::EXTRA_ROOTS_ENV
        );
    }
}
