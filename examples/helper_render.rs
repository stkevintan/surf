//! Render through the isolated helper:
//! `cargo run --example helper_render -- <bundle.vst3> out.wav`

use surf::plugin::client::{RenderNote, VstClient};
use surf::core::song::Song;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let plugin = args.next().expect("usage: helper_render <bundle.vst3> <out.wav>");
    let out = args.next().unwrap_or_else(|| "helper-render.wav".to_string());
    let song = Song::demo();
    let timeline = song.timeline();
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
        .collect::<Vec<_>>();

    let mut client = VstClient::load(&plugin, 48_000, None)?;
    let (path, duration, peak) =
        client.render(out.as_ref(), notes, timeline.duration, 48_000, 2.0)?;
    println!("helper rendered {path} · {duration:.2}s · peak {:.1}%", peak * 100.0);
    Ok(())
}
