//! A small, dependency-free additive piano-ish instrument.
//!
//! It is not a sample library, but it is good enough to hear timing while
//! building the visualizer and it doubles as the fallback when no VST3 plugin
//! is loaded.

use super::Instrument;

const PARTIALS: usize = 6;
const MAX_VOICES: usize = 64;
const PARTIAL_RATIOS: [f64; PARTIALS] = [1.0, 2.003, 3.009, 4.021, 5.041, 6.069];
const PARTIAL_GAINS: [f32; PARTIALS] = [1.0, 0.42, 0.21, 0.11, 0.06, 0.035];

struct Voice {
    key: u8,
    phases: [f64; PARTIALS],
    phase_incs: [f64; PARTIALS],
    age: f32,
    release_age: Option<f32>,
    velocity: f32,
    tau: f32,
    pan: f32,
    /// Whether the physical key is still down (as opposed to held by pedal).
    held: bool,
}

impl Voice {
    fn new(key: u8, velocity: u8, sample_rate: f32) -> Self {
        let freq = midi_to_hz(key);
        let mut phases = [0.0; PARTIALS];
        // Slightly randomize the starting phase so stacked notes do not phase
        //-cancel into an unnaturally sharp transient.
        let seed = (key as f64 * 12.9898 + velocity as f64 * 78.233).sin() * 43758.5453;
        for (i, phase) in phases.iter_mut().enumerate() {
            *phase = (seed + i as f64 * 0.37).fract().abs();
        }
        let phase_incs = std::array::from_fn(|i| {
            std::f64::consts::TAU * freq * PARTIAL_RATIOS[i] / sample_rate as f64
        });
        // Low notes ring longer; high notes decay quickly, like a piano.
        let tau = (3.8 * (261.63 / freq).powf(0.35)).clamp(0.9, 9.0) as f32;
        // Spread the keyboard gently across the stereo field.
        let pan = ((key as f32 - 64.0) / 44.0).clamp(-1.0, 1.0) * 0.35;
        Self {
            key,
            phases,
            phase_incs,
            age: 0.0,
            release_age: None,
            velocity: (velocity as f32 / 127.0).clamp(0.02, 1.0).powf(1.4),
            tau,
            pan,
            held: true,
        }
    }

    #[inline]
    fn sample(&mut self, sample_rate: f32) -> f32 {
        let mut value = 0.0f32;
        for i in 0..PARTIALS {
            self.phases[i] += self.phase_incs[i];
            if self.phases[i] > std::f64::consts::TAU {
                self.phases[i] -= std::f64::consts::TAU;
            }
            value += self.phases[i].sin() as f32 * PARTIAL_GAINS[i];
        }
        let attack = (self.age / 0.006).min(1.0);
        let decay = (-self.age / self.tau).exp();
        let release = self
            .release_age
            .map(|age| (-age / 0.16).exp())
            .unwrap_or(1.0);
        self.age += 1.0 / sample_rate;
        if self.release_age.is_some() {
            self.release_age = Some(self.release_age.unwrap() + 1.0 / sample_rate);
        }
        value * self.velocity * attack * decay * release * 0.22
    }

    fn finished(&self) -> bool {
        self.release_age.map(|age| age > 0.9).unwrap_or(false)
    }
}

pub struct Synth {
    sample_rate: f32,
    voices: Vec<Voice>,
    sustain: bool,
    master: f32,
}

impl Synth {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            voices: Vec::with_capacity(MAX_VOICES),
            sustain: false,
            master: 0.9,
        }
    }

    pub fn set_master_gain(&mut self, gain: f32) {
        self.master = gain.clamp(0.0, 2.0);
    }

    pub fn active_voice_count(&self) -> usize {
        self.voices.len()
    }

    fn release_key(&mut self, key: u8) {
        for voice in &mut self.voices {
            if voice.key == key {
                voice.held = false;
                if !self.sustain && voice.release_age.is_none() {
                    voice.release_age = Some(0.0);
                }
            }
        }
    }
}

impl Instrument for Synth {
    fn note_on(&mut self, _channel: u8, key: u8, velocity: u8) {
        if !(super::MIN_KEY..=super::MAX_KEY).contains(&key) {
            return;
        }
        // Retrigger: release an existing voice for this key.
        for voice in &mut self.voices {
            if voice.key == key {
                voice.held = false;
                if voice.release_age.is_none() {
                    voice.release_age = Some(0.0);
                }
            }
        }
        if self.voices.len() >= MAX_VOICES {
            // Steal the oldest voice.
            if let Some((idx, _)) = self
                .voices
                .iter()
                .enumerate()
                .max_by(|(_, a), (_, b)| a.age.total_cmp(&b.age))
            {
                self.voices.swap_remove(idx);
            }
        }
        self.voices
            .push(Voice::new(key, velocity, self.sample_rate));
    }

    fn note_off(&mut self, _channel: u8, key: u8) {
        self.release_key(key);
    }

    fn set_sustain(&mut self, _channel: u8, down: bool) {
        self.sustain = down;
        if !down {
            // Pedal up only releases notes whose keys are already up.
            for voice in &mut self.voices {
                if !voice.held && voice.release_age.is_none() {
                    voice.release_age = Some(0.0);
                }
            }
        }
    }

    fn all_notes_off(&mut self) {
        for voice in &mut self.voices {
            voice.release_age = Some(0.0);
        }
        self.sustain = false;
    }

    fn process(&mut self, out: &mut [f32], frames: usize) {
        let frames = frames.min(out.len() / 2);
        out[..frames * 2].fill(0.0);
        for voice in &mut self.voices {
            let left_gain = ((1.0 - voice.pan) * 0.5).sqrt();
            let right_gain = ((1.0 + voice.pan) * 0.5).sqrt();
            for frame in 0..frames {
                let sample = voice.sample(self.sample_rate) * self.master;
                out[frame * 2] += sample * left_gain;
                out[frame * 2 + 1] += sample * right_gain;
            }
        }
        for sample in &mut out[..frames * 2] {
            // Gentle saturation keeps chords from clipping harshly.
            *sample = (*sample * 1.15).tanh() * 0.92;
        }
        self.voices.retain(|v| !v.finished());
    }

    fn latency_samples(&self) -> u32 {
        0
    }

    fn set_sample_rate(&mut self, sample_rate: f32) {
        self.sample_rate = sample_rate;
        self.voices.clear();
    }

    fn name(&self) -> &str {
        "Surf Grand (built-in)"
    }
}

pub fn midi_to_hz(key: u8) -> f64 {
    440.0 * 2f64.powf((key as f64 - 69.0) / 12.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_note() {
        let mut synth = Synth::new(48_000.0);
        synth.note_on(0, 60, 100);
        let mut buffer = vec![0.0f32; 4800 * 2];
        synth.process(&mut buffer, 4800);
        assert!(buffer.iter().any(|s| s.abs() > 1e-4));
        assert!(buffer.iter().all(|s| s.is_finite()));
    }

    #[test]
    fn note_off_releases() {
        let mut synth = Synth::new(48_000.0);
        synth.note_on(0, 60, 100);
        let mut buffer = vec![0.0f32; 4800 * 2];
        synth.process(&mut buffer, 4800);
        synth.note_off(0, 60);
        let mut tail = vec![0.0f32; 48_000 * 2];
        synth.process(&mut tail, 48_000);
        assert!(tail.iter().all(|s| s.is_finite()));
        // After a second the release envelope is essentially silent.
        let late = &tail[40_000 * 2..];
        let peak = late.iter().fold(0.0f32, |a, b| a.max(b.abs()));
        assert!(peak < 0.01, "peak={peak}");
    }

    #[test]
    fn sustain_holds_notes() {
        let mut synth = Synth::new(48_000.0);
        synth.set_sustain(0, true);
        synth.note_on(0, 60, 100);
        synth.note_off(0, 60);
        let mut buffer = vec![0.0f32; 9600 * 2];
        synth.process(&mut buffer, 9600);
        let late = &buffer[8000 * 2..];
        assert!(late.iter().any(|s| s.abs() > 1e-4));
    }
}
