//! Regression probe: close a plugin editor with its title-bar button, then
//! reopen it. Reopening used to fail with "Editor already open".
//!
//! The close goes through the same path as a user clicking the window's close
//! button: the host is started with `SURF_TEST_CLOSE_EDITOR_AFTER_MS`, which
//! makes it call `performClose:` on its windows (macOS only).
//!
//! ```sh
//! cargo run --example editor_reopen -- "/Library/Audio/Plug-Ins/VST3/Kontakt 8.vst3"
//! ```

use std::time::Duration;

use surf::plugin::client::VstClient;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: editor_reopen <plugin>");
    // SAFETY: set before any thread is spawned; the host process inherits it.
    unsafe { std::env::set_var("SURF_TEST_CLOSE_EDITOR_AFTER_MS", "1500") };
    let mut client = VstClient::load(&path, 48_000, None)?;
    println!("loaded: {}", client.name);

    for round in 1..=3 {
        client.open_editor()?;
        println!("round {round}: editor opened");
        // The host closes it after 1.5 s; wait past that.
        std::thread::sleep(Duration::from_secs(3));
    }
    client.open_editor()?;
    println!("final reopen: ok");
    Ok(())
}
