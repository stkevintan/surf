//! State round-trip smoke test:
//! `cargo run --example vst_state -- <bundle.vst3> [saved.state]`

use surf::audio::vst::VstPlugin;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: vst_state <bundle.vst3> [state]");
    let state_path = args.next();

    let a = VstPlugin::load(&path, 48_000)?;
    let state = a.state()?;
    println!("saved state: {} bytes", state.len());
    if let Some(state_path) = state_path {
        std::fs::write(&state_path, &state)?;
        println!("wrote {state_path}");
    }

    let b = VstPlugin::load_with_state(&path, 48_000, Some(&state))?;
    println!("restored before activation: {}", b.state_restored);
    println!("state after restore: {} bytes", b.state()?.len());
    Ok(())
}
