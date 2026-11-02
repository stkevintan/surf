//! Is the audio chain pitch-accurate, and do the two ends agree on sample rate?
//!
//! "Pressed E, heard F" with Kontakt's own keyboard showing E means Kontakt got the
//! right note and rendered the wrong pitch. Nothing in this app resamples, so a
//! sustained pitch error can only come from the plugin rendering at one rate while
//! the device plays at another: playing R_render audio at R_device multiplies
//! frequency by R_device / R_render.
//!
//! Reference points, from A4 = 440 Hz:
//!   48000 / 44100 = 1.0884 -> +1.47 semitones  (E would land between F and F#)
//!   44100 / 48000 = 0.9188 -> -1.47 semitones
//!   one semitone  = 1.0595
//!
//! Part 1 measures the built-in synth as a control: it shares the engine and device
//! path but no plugin, so a correct reading here clears everything except the
//! plugin's own rate. Part 2 prints what the device, the engine and the helper each
//! believe the rate is -- a mismatch there is the bug.
//!
//! ```powershell
//! cargo run --example pitch_probe
//! cargo run --example pitch_probe -- "C:\Program Files\Common Files\VST3\Kontakt 8.vst3"
//! ```

use cpal::traits::{DeviceTrait, HostTrait};
use surf::audio::Instrument;
use surf::audio::synth::Synth;

/// Estimate the fundamental by autocorrelation, which handles the synth's
/// harmonics far better than counting zero crossings.
fn fundamental(samples: &[f32], sample_rate: f64) -> f64 {
    // Search 50 Hz .. 2 kHz; a piano note under test sits comfortably inside.
    let min_lag = (sample_rate / 2000.0) as usize;
    let max_lag = (sample_rate / 50.0) as usize;
    let mut best_lag = 0usize;
    let mut best_score = f64::MIN;
    for lag in min_lag..max_lag.min(samples.len() / 2) {
        let mut sum = 0.0;
        for i in 0..samples.len() - lag {
            sum += samples[i] as f64 * samples[i + lag] as f64;
        }
        // Normalise so longer lags are not penalised by the shorter overlap.
        let score = sum / (samples.len() - lag) as f64;
        if score > best_score {
            best_score = score;
            best_lag = lag;
        }
    }
    if best_lag == 0 {
        return 0.0;
    }
    sample_rate / best_lag as f64
}

fn note_name(freq: f64) -> String {
    if freq <= 0.0 {
        return "silence".to_string();
    }
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    // MIDI 69 = A4 = 440 Hz.
    let midi = 69.0 + 12.0 * (freq / 440.0).log2();
    let rounded = midi.round();
    let cents = (midi - rounded) * 100.0;
    let index = ((rounded as i64).rem_euclid(12)) as usize;
    let octave = (rounded as i64) / 12 - 1;
    format!(
        "{}{} ({:+.0} cents, midi {:.2})",
        NAMES[index], octave, cents, midi
    )
}

fn main() -> anyhow::Result<()> {
    // --- what each layer believes the rate is ---
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| anyhow::anyhow!("no default output device"))?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Default output".to_string());
    let device_rate = device.default_output_config()?.sample_rate();
    println!("device              : {name}");
    println!("device default rate : {device_rate} Hz");
    println!("audio::SAMPLE_RATE  : {} Hz", surf::audio::SAMPLE_RATE);

    // --- control: built-in synth, known note, measured pitch ---
    // MIDI 69 is A4; if the chain is honest this must read ~440 Hz.
    const KEY: u8 = 69;
    let mut synth = Synth::new(device_rate as f32);
    synth.note_on(KEY, 110);
    let frames = device_rate as usize; // one second
    let mut stereo = vec![0.0f32; frames * 2];
    // Render in the engine's block size so the measurement sees the real path.
    let mut offset = 0;
    while offset < frames {
        let block = 64.min(frames - offset);
        synth.process(&mut stereo[offset * 2..(offset + block) * 2], block);
        offset += block;
    }
    // Skip the attack, measure the steady part, left channel only.
    let left: Vec<f32> = stereo[(frames / 4) * 2..]
        .iter()
        .step_by(2)
        .copied()
        .collect();
    let freq = fundamental(&left, device_rate as f64);
    println!();
    println!("--- control: built-in synth, MIDI {KEY} (A4, expect 440 Hz) ---");
    println!("measured            : {freq:.1} Hz  = {}", note_name(freq));
    let ratio = freq / 440.0;
    println!(
        "ratio vs expected   : {ratio:.4}  ({:+.2} semitones)",
        12.0 * ratio.log2()
    );
    if (12.0 * ratio.log2()).abs() < 0.5 {
        println!("verdict             : engine + device path is pitch-accurate");
    } else {
        println!("verdict             : MISMATCH in the engine path itself");
    }

    // --- optional: what rate the helper reports for a plugin ---
    if let Some(path) = std::env::args().nth(1) {
        println!();
        println!("--- helper rate check for {path} ---");
        println!("(loading at the device rate, like the app does)");
        let client = surf::audio::vst_client::VstClient::load(&path, device_rate, None)?;
        let map = client.shared_map();
        println!("helper reports      : {} Hz", map.sample_rate());
        if map.sample_rate() != device_rate {
            println!("verdict             : MISMATCH -> pitch shifts by {:.4}x ({:+.2} semitones)",
                device_rate as f64 / map.sample_rate() as f64,
                12.0 * (device_rate as f64 / map.sample_rate() as f64).log2());
        } else {
            println!("verdict             : helper and device agree");
        }
    }

    Ok(())
}
