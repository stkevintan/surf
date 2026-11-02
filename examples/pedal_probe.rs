//! Does the sustain pedal actually reach the plugin?
//!
//! VST3 has no inbound "MIDI CC" event. A host must call
//! `IMidiMapping::getMidiControllerAssignment(bus, channel, cc)` to learn which
//! *parameter* a controller maps to, then drive that parameter. Sending
//! `kLegacyMIDICCOutEvent` instead — which is a plugin-to-host event — is silently
//! ignored, so the pedal does nothing.
//!
//! This probe reports whether the plugin advertises a CC 64 mapping, then measures
//! the audible difference: play a note, release the key, and compare the tail energy
//! with the pedal up vs held. A working pedal keeps the note ringing.
//!
//! ```powershell
//! cargo run --example pedal_probe -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
//! ```

use surf::audio::Instrument;
use surf::audio::vst::VstPlugin;

const SR: u32 = 48_000;
const BLOCK: usize = 64;

/// Render `secs` of audio and return the peak level.
fn render_peak(instrument: &mut impl Instrument, secs: f64) -> f32 {
    let blocks = (secs * SR as f64 / BLOCK as f64) as usize;
    let mut buffer = [0.0f32; BLOCK * 2];
    let mut peak = 0.0f32;
    for _ in 0..blocks {
        buffer.fill(0.0);
        instrument.process(&mut buffer, BLOCK);
        for sample in buffer.iter() {
            peak = peak.max(sample.abs());
        }
    }
    peak
}

/// One take: hold a note, release it, optionally with the pedal down, and report
/// how loud the tail still is a second after the key came up.
fn tail_with_pedal(plugin: &VstPlugin, pedal: bool) -> f32 {
    let mut instrument = plugin.instrument();
    instrument.all_notes_off();
    render_peak(&mut instrument, 0.2);

    if pedal {
        instrument.set_sustain(true);
    }
    instrument.note_on(60, 110);
    render_peak(&mut instrument, 0.5); // let it speak
    instrument.note_off(60);
    // Skip the natural decay right after release, then measure what is left.
    render_peak(&mut instrument, 0.5);
    let tail = render_peak(&mut instrument, 1.0);

    instrument.set_sustain(false);
    instrument.all_notes_off();
    tail
}

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .expect("usage: pedal_probe <bundle.vst3>");

    println!("loading {path}");
    let plugin = VstPlugin::load(&path, SR)?;
    println!("plugin: {}", plugin.name);
    println!();

    println!("--- CC 64 -> parameter mapping (IMidiMapping) ---");
    match plugin.midi_cc_parameter(0, 0, 64) {
        Some(id) => println!("CC64 maps to parameter id {id} — host can drive the pedal"),
        None => println!("no CC64 mapping advertised (falling back to legacy CC event)"),
    }
    println!();

    println!("--- audible test: note tail 1s after key release ---");
    let dry = tail_with_pedal(&plugin, false);
    let wet = tail_with_pedal(&plugin, true);
    println!("pedal up   : peak {:.4}", dry);
    println!("pedal held : peak {:.4}", wet);
    println!();

    if wet <= 0.0001 && dry <= 0.0001 {
        println!("verdict: silent either way — load an instrument into Kontakt first");
    } else if wet > dry * 2.0 {
        println!("verdict: PEDAL WORKS — note sustains {:.1}x longer", wet / dry.max(1e-6));
    } else {
        println!("verdict: NO PEDAL EFFECT — plugin is not receiving CC 64");
    }
    Ok(())
}
