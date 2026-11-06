//! Offline rendering of a timeline to a WAV file.

use std::path::Path;

use anyhow::{Context, Result};

use super::Instrument;
use super::synth::Synth;
use surf_core::song::{Song, Timeline};

#[derive(Clone, Debug)]
pub struct RenderOptions {
    pub sample_rate: u32,
    pub tail_secs: f64,
    pub gain: f32,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            tail_secs: 2.5,
            gain: 0.9,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RenderStats {
    pub frames: u64,
    pub duration: f64,
    pub peak: f32,
    pub path: String,
}

pub fn render_song_to_wav(
    song: &Song,
    path: impl AsRef<Path>,
    options: &RenderOptions,
) -> Result<RenderStats> {
    render_timeline_to_wav(&song.timeline(), path, options)
}

pub fn render_timeline_to_wav(
    timeline: &Timeline,
    path: impl AsRef<Path>,
    options: &RenderOptions,
) -> Result<RenderStats> {
    let mut synth = Synth::new(options.sample_rate as f32);
    synth.set_master_gain(options.gain);
    render_with_instrument(timeline, &mut synth, path, options)
}

/// Render a timeline through any instrument.  The instrument is consumed by the
/// render loop and must be silent before the first call (a fresh instance).
pub fn render_with_instrument(
    timeline: &Timeline,
    instrument: &mut dyn Instrument,
    path: impl AsRef<Path>,
    options: &RenderOptions,
) -> Result<RenderStats> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).ok();
        }
    }
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: options.sample_rate,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("could not create {}", path.display()))?;

    let sample_rate = options.sample_rate as f64;
    let tail = options.tail_secs.max(0.0);
    // A plugin that reports latency delays every sample; render that much extra
    // and discard the leading silence so the exported file stays in sync.
    let latency = instrument.latency_samples() as u64;
    let content_frames = ((timeline.duration + tail) * sample_rate).ceil() as u64;
    let total_frames = content_frames + latency;
    let mut next_note = 0usize;
    let mut active: Vec<(u8, u8, u64)> = Vec::new();
    let mut pos = 0u64;
    // 64 frames matches the VST3 host block size used by the live engine, so
    // the same code path works for the built-in synth and for Kontakt.
    let mut block = vec![0.0f32; 64 * 2];
    let mut peak = 0.0f32;
    let mut written = 0u64;

    while pos < total_frames {
        let chunk = (block.len() / 2).min((total_frames - pos) as usize);
        let end = pos + chunk as u64;

        let mut ended = Vec::new();
        active.retain(|(channel, key, note_end)| {
            if *note_end <= end {
                ended.push((*channel, *key));
                false
            } else {
                true
            }
        });
        for (channel, key) in ended {
            instrument.note_off(channel, key);
        }
        while next_note < timeline.notes.len() {
            let note = &timeline.notes[next_note];
            let start = (note.start * sample_rate) as u64;
            if start >= end {
                break;
            }
            next_note += 1;
            if start < pos {
                continue;
            }
            instrument.note_on(note.channel, note.key, note.velocity);
            active.push((
                note.channel,
                note.key,
                ((note.end * sample_rate) as u64).max(start + 1),
            ));
        }

        instrument.process(&mut block[..chunk * 2], chunk);
        for frame in 0..chunk {
            let absolute = pos + frame as u64;
            if absolute < latency {
                continue;
            }
            let l = (block[frame * 2].clamp(-1.0, 1.0) * 8_388_607.0) as i32;
            let r = (block[frame * 2 + 1].clamp(-1.0, 1.0) * 8_388_607.0) as i32;
            peak = peak.max((l as f32 / 8_388_607.0).abs());
            writer.write_sample(l)?;
            writer.write_sample(r)?;
            written += 1;
        }
        pos = end;
    }

    instrument.all_notes_off();
    writer.finalize()?;
    Ok(RenderStats {
        frames: written,
        duration: written as f64 / sample_rate,
        peak,
        path: path.display().to_string(),
    })
}

/// Mix rendered stems into one file: each stem scaled by its gain, the sum
/// by `master`. Stems may differ in length; the mix is as long as the longest.
/// All stems must share `sample_rate` and be 24-bit stereo, as
/// [`render_with_instrument`] writes them.
pub fn mix_stems(
    stems: &[(std::path::PathBuf, f32)],
    path: impl AsRef<Path>,
    sample_rate: u32,
    master: f32,
) -> Result<RenderStats> {
    const FULL: f32 = 8_388_607.0;
    let mut mix: Vec<f32> = Vec::new();
    for (stem, gain) in stems {
        let mut reader = hound::WavReader::open(stem)
            .with_context(|| format!("could not read stem {}", stem.display()))?;
        let spec = reader.spec();
        if spec.channels != 2 || spec.sample_rate != sample_rate {
            anyhow::bail!("stem {} is not {sample_rate} Hz stereo", stem.display());
        }
        for (i, sample) in reader.samples::<i32>().enumerate() {
            let v = sample? as f32 / FULL * gain;
            if i >= mix.len() {
                mix.push(v);
            } else {
                mix[i] += v;
            }
        }
    }
    let path = path.as_ref();
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("could not create {}", path.display()))?;
    let mut peak = 0.0f32;
    for v in &mix {
        let v = (v * master).clamp(-1.0, 1.0);
        peak = peak.max(v.abs());
        writer.write_sample((v * FULL) as i32)?;
    }
    writer.finalize()?;
    let frames = (mix.len() / 2) as u64;
    Ok(RenderStats {
        frames,
        duration: frames as f64 / sample_rate.max(1) as f64,
        peak,
        path: path.display().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use surf_core::song::{Note, Song};

    #[test]
    fn writes_non_silent_wav() {
        let mut song = Song::empty();
        song.notes = vec![
            Note {
                key: 60,
                velocity: 100,
                channel: 0,
                start_tick: 0,
                duration_ticks: 480,
            },
            Note {
                key: 67,
                velocity: 90,
                channel: 0,
                start_tick: 480,
                duration_ticks: 960,
            },
        ];
        let path = std::env::temp_dir().join(format!(
            "surf-test-{}-{}.wav",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let stats = render_song_to_wav(
            &song,
            &path,
            &RenderOptions {
                tail_secs: 0.5,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(stats.frames > 48_000);
        assert!(stats.peak > 0.001, "peak={}", stats.peak);
        let reader = hound::WavReader::open(&path).unwrap();
        assert_eq!(reader.spec().channels, 2);
        assert_eq!(reader.spec().bits_per_sample, 24);

        // Two copies at half gain mix back to the original level.
        let mixed = path.with_extension("mix.wav");
        let half = mix_stems(
            &[(path.clone(), 0.5), (path.clone(), 0.5)],
            &mixed,
            48_000,
            1.0,
        )
        .unwrap();
        assert_eq!(half.frames, stats.frames);
        assert!((half.peak - stats.peak).abs() < 1e-3, "{} vs {}", half.peak, stats.peak);
        std::fs::remove_file(mixed).ok();
        std::fs::remove_file(path).ok();
    }
}
