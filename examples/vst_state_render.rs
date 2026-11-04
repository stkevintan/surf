//! Load a plugin with a saved state file and render the demo song, to check
//! that restoring state really changes the sound.
//!
//! ```sh
//! cargo run --example vst_state_render -- <plugin> <state-file> <out.wav>
//! ```

use surf::core::song::Song;
use surf::plugin::client::{RenderNote, VstClient};

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let plugin = args
        .next()
        .expect("usage: vst_state_render <plugin> <state> <out.wav>");
    let state = args.next().expect("missing state file");
    let out = args.next().expect("missing output path");
    let mut client = VstClient::load(&plugin, 48_000, Some(state.as_ref()))?;
    let timeline = Song::demo().timeline();
    let notes = timeline
        .notes
        .iter()
        .map(|n| RenderNote {
            key: n.key,
            velocity: n.velocity,
            channel: n.channel,
            start: n.start,
            end: n.end,
        })
        .collect();
    let (path, secs, peak) = client.render(out.as_ref(), notes, timeline.duration, 48_000, 1.0)?;
    println!("{path} · {secs:.2}s · peak {:.1}%", peak * 100.0);
    Ok(())
}
