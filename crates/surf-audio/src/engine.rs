//! Real-time audio output plus the clock that drives both sound and visuals.
//!
//! The engine deliberately keeps one authoritative timeline position in frames.
//! The UI reads it through atomics; the audio callback advances it.  If no audio
//! device is available (common on CI / remote Windows sessions) a silent timer
//! thread runs the same code path so the visualizer remains usable.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use super::Instrument;
use surf_core::song::Timeline;

const CHUNK: usize = 64;
const MAX_BLOCK: usize = 8192;
const SILENT_BLOCK: usize = 240; // 5 ms at 48 kHz

pub enum Command {
    NoteOn { key: u8, velocity: u8 },
    NoteOff { key: u8 },
    Sustain(bool),
    AllNotesOff,
    SetInstrument {
        instrument: Box<dyn Instrument>,
        /// Signalled after the old instrument has been dropped on the audio
        /// thread, so the UI can safely release the last plugin reference.
        ack: Option<crossbeam_channel::Sender<()>>,
    },
    Play { timeline: Timeline, from: f64 },
    Pause,
    Resume,
    Stop,
    Seek(f64),
    SetLoop(bool),
    SetVolume(f32),
}

/// Something that happened to the output stream, reported by the audio backend.
#[derive(Clone, Debug, PartialEq)]
pub enum OutputEvent {
    /// The system default output changed and the stream followed it (for
    /// example, a USB MIDI keyboard that is also an audio interface was
    /// plugged in). `device` is the new output's name.
    Rerouted { device: String },
    /// The output device went away and no default replaced it.
    Lost,
    /// The device asked for audio faster than it was produced; a click is
    /// possible. Counted rather than reported one by one.
    Glitch,
    /// Any other backend error, as text.
    Error(String),
}

#[derive(Clone, Debug)]
pub struct EngineStatus {
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    pub peak: f32,
    pub sample_rate: u32,
    pub output: String,
    pub sample_accurate: bool,
    /// Audio glitches (underruns/overruns) since the engine started.
    pub glitches: u32,
}

struct Shared {
    position_frames: AtomicU64,
    duration_frames: AtomicU64,
    playing: AtomicBool,
    peak: AtomicU32,
    sample_rate: AtomicU32,
    glitches: AtomicU32,
}

struct ActiveNote {
    key: u8,
    end_frame: u64,
}

struct Core {
    instrument: Box<dyn Instrument>,
    timeline: Option<Timeline>,
    next_note: usize,
    active: Vec<ActiveNote>,
    /// Scratch for keys released in the current chunk. Preallocated so the
    /// audio callback never allocates.
    ended: Vec<u8>,
    pos_frames: u64,
    duration_frames: u64,
    playing: bool,
    looping: bool,
    volume: f32,
    sample_rate: f64,
    scratch: Vec<f32>,
    shared: Arc<Shared>,
}

impl Core {
    fn new(mut instrument: Box<dyn Instrument>, sample_rate: u32, shared: Arc<Shared>) -> Self {
        instrument.set_sample_rate(sample_rate as f32);
        Self {
            instrument,
            timeline: None,
            next_note: 0,
            active: Vec::with_capacity(256),
            ended: Vec::with_capacity(256),
            pos_frames: 0,
            duration_frames: 0,
            playing: false,
            looping: false,
            volume: 0.9,
            sample_rate: sample_rate as f64,
            scratch: vec![0.0; MAX_BLOCK * 2],
            shared,
        }
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::NoteOn { key, velocity } => self.instrument.note_on(key, velocity),
            Command::NoteOff { key } => self.instrument.note_off(key),
            Command::Sustain(down) => self.instrument.set_sustain(down),
            Command::AllNotesOff => self.instrument.all_notes_off(),
            Command::SetInstrument { instrument, ack } => {
                self.instrument.all_notes_off();
                self.instrument = instrument;
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
            Command::Play { timeline, from } => {
                self.instrument.all_notes_off();
                self.duration_frames = (timeline.duration * self.sample_rate).ceil() as u64;
                self.pos_frames = (from.max(0.0) * self.sample_rate) as u64;
                self.next_note = timeline.notes.partition_point(|n| n.start < from.max(0.0));
                self.active.clear();
                self.timeline = Some(timeline);
                self.playing = true;
                self.shared
                    .duration_frames
                    .store(self.duration_frames, Ordering::Relaxed);
                self.shared.playing.store(true, Ordering::Release);
            }
            Command::Pause => {
                self.playing = false;
                self.instrument.all_notes_off();
                self.shared.playing.store(false, Ordering::Release);
            }
            Command::Resume => {
                if self.timeline.is_some() {
                    self.playing = true;
                    self.shared.playing.store(true, Ordering::Release);
                }
            }
            Command::Stop => {
                self.playing = false;
                self.pos_frames = 0;
                self.next_note = 0;
                self.active.clear();
                self.instrument.all_notes_off();
                self.shared.playing.store(false, Ordering::Release);
                self.shared.position_frames.store(0, Ordering::Release);
            }
            Command::Seek(sec) => {
                let sec = sec.max(0.0);
                self.pos_frames = (sec * self.sample_rate) as u64;
                self.active.clear();
                self.instrument.all_notes_off();
                if let Some(timeline) = &self.timeline {
                    self.next_note = timeline.notes.partition_point(|n| n.start < sec);
                    self.shared
                        .position_frames
                        .store(self.pos_frames, Ordering::Release);
                }
            }
            Command::SetLoop(looping) => self.looping = looping,
            Command::SetVolume(volume) => self.volume = volume.clamp(0.0, 2.0),
        }
    }

    fn reset_to_start(&mut self) {
        self.pos_frames = 0;
        self.next_note = 0;
        self.active.clear();
        self.instrument.all_notes_off();
        self.shared.position_frames.store(0, Ordering::Release);
    }

    /// Render interleaved stereo frames into `scratch` and return that slice.
    fn render(&mut self, frames: usize) -> &[f32] {
        let frames = frames.min(MAX_BLOCK);
        let mut done = 0usize;
        let mut peak = 0.0f32;

        while done < frames {
            let chunk = CHUNK.min(frames - done);
            if self.playing {
                let start = self.pos_frames;
                let end = start + chunk as u64;
                self.trigger(start, end);
                if end >= self.duration_frames && self.duration_frames > 0 {
                    if self.looping {
                        self.reset_to_start();
                    } else {
                        self.pos_frames = self.duration_frames;
                        self.playing = false;
                        self.instrument.all_notes_off();
                        self.shared.playing.store(false, Ordering::Release);
                    }
                } else {
                    self.pos_frames = end;
                }
            }

            let out = &mut self.scratch[done * 2..(done + chunk) * 2];
            self.instrument.process(out, chunk);
            for sample in out.iter_mut() {
                *sample = (*sample * self.volume).clamp(-1.0, 1.0);
                peak = peak.max(sample.abs());
            }
            done += chunk;
        }

        self.shared
            .position_frames
            .store(self.pos_frames, Ordering::Release);
        self.shared.peak.store(peak.to_bits(), Ordering::Relaxed);
        &self.scratch[..frames * 2]
    }

    fn trigger(&mut self, start: u64, end: u64) {
        let Some(timeline) = &self.timeline else {
            return;
        };

        // Note-offs first so back-to-back notes on the same key retrigger cleanly.
        let ended = &mut self.ended;
        ended.clear();
        self.active.retain(|note| {
            if note.end_frame <= end {
                ended.push(note.key);
                false
            } else {
                true
            }
        });
        for &key in &self.ended {
            self.instrument.note_off(key);
        }

        while self.next_note < timeline.notes.len() {
            let note = &timeline.notes[self.next_note];
            let note_start = (note.start * self.sample_rate) as u64;
            if note_start >= end {
                break;
            }
            self.next_note += 1;
            if note_start < start {
                continue;
            }
            self.instrument.note_on(note.key, note.velocity);
            let note_end = ((note.end * self.sample_rate) as u64).max(note_start + 1);
            self.active.push(ActiveNote {
                key: note.key,
                end_frame: note_end,
            });
        }
    }
}

struct NullClock {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for NullClock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

pub struct AudioEngine {
    shared: Arc<Shared>,
    tx: crossbeam_channel::Sender<Command>,
    _stream: Option<cpal::Stream>,
    _null_clock: Option<NullClock>,
    sample_rate: u32,
    output: parking_lot::Mutex<String>,
    sample_accurate: bool,
    events: crossbeam_channel::Receiver<OutputEvent>,
}

impl AudioEngine {
    pub fn new(instrument: Box<dyn Instrument>) -> Self {
        let shared = Arc::new(Shared {
            position_frames: AtomicU64::new(0),
            duration_frames: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            peak: AtomicU32::new(0),
            sample_rate: AtomicU32::new(48_000),
            glitches: AtomicU32::new(0),
        });
        let (event_tx, events) = crossbeam_channel::unbounded();
        let (tx, rx) = crossbeam_channel::unbounded();

        // Try the default output device first.
        match open_output() {
            Ok((device, sample_rate, channels, name)) => {
                shared.sample_rate.store(sample_rate, Ordering::Relaxed);
                let core = Arc::new(Mutex::new(Core::new(
                    instrument,
                    sample_rate,
                    shared.clone(),
                )));
                let callback_core = core.clone();
                let err_shared = shared.clone();
                let err_fn = move |err: cpal::Error| {
                    let event = classify(&err);
                    if event == OutputEvent::Glitch {
                        err_shared.glitches.fetch_add(1, Ordering::Relaxed);
                        return;
                    }
                    let _ = event_tx.send(event);
                };
                let config = cpal::StreamConfig {
                    channels,
                    sample_rate,
                    buffer_size: cpal::BufferSize::Default,
                };
                let stream = device
                    .build_output_stream(
                        config,
                        move |data: &mut [f32], _| {
                            let Some(mut core) = callback_core.try_lock() else {
                                data.fill(0.0);
                                return;
                            };
                            while let Ok(command) = rx.try_recv() {
                                core.apply(command);
                            }
                            let channels = channels as usize;
                            let frames = data.len() / channels.max(1);
                            let stereo = core.render(frames);
                            for frame in 0..frames {
                                let l = stereo[frame * 2];
                                let r = stereo[frame * 2 + 1];
                                if channels == 1 {
                                    data[frame] = (l + r) * 0.5;
                                } else {
                                    data[frame * channels] = l;
                                    data[frame * channels + 1] = r;
                                    for ch in 2..channels {
                                        data[frame * channels + ch] = (l + r) * 0.5;
                                    }
                                }
                            }
                        },
                        err_fn,
                        None,
                    )
                    .expect("failed to build output stream");
                stream.play().expect("failed to start audio stream");
                Self {
                    shared,
                    tx,
                    _stream: Some(stream),
                    _null_clock: None,
                    sample_rate,
                    output: parking_lot::Mutex::new(name),
                    sample_accurate: true,
                    events,
                }
            }
            Err(err) => {
                // No device: keep a real-time clock and run the synth silently.
                eprintln!("no audio output device ({err}); running silent clock");
                let sample_rate = 48_000;
                shared.sample_rate.store(sample_rate, Ordering::Relaxed);
                let core = Arc::new(Mutex::new(Core::new(
                    instrument,
                    sample_rate,
                    shared.clone(),
                )));
                let stop = Arc::new(AtomicBool::new(false));
                let thread_core = core.clone();
                let thread_stop = stop.clone();
                let handle = thread::Builder::new()
                    .name("surf-silent-clock".into())
                    .spawn(move || {
                        let start = Instant::now();
                        let mut produced_frames = 0u64;
                        while !thread_stop.load(Ordering::Acquire) {
                            while let Ok(command) = rx.try_recv() {
                                thread_core.lock().apply(command);
                            }
                            let elapsed = start.elapsed().as_secs_f64();
                            let target = (elapsed * sample_rate as f64) as u64;
                            while produced_frames < target {
                                produced_frames += SILENT_BLOCK as u64;
                                let mut core = thread_core.lock();
                                core.render(SILENT_BLOCK);
                            }
                            thread::sleep(Duration::from_millis(5));
                        }
                    })
                    .expect("failed to spawn silent clock");
                Self {
                    shared,
                    tx,
                    _stream: None,
                    _null_clock: Some(NullClock {
                        stop,
                        handle: Some(handle),
                    }),
                    sample_rate,
                    output: parking_lot::Mutex::new("Silent (no audio device)".to_string()),
                    sample_accurate: false,
                    events,
                }
            }
        }
    }

    /// Try to open a real device again after starting in silent mode.
    pub fn is_silent(&self) -> bool {
        self._stream.is_none()
    }

    pub fn status(&self) -> EngineStatus {
        let sample_rate = self.shared.sample_rate.load(Ordering::Relaxed);
        EngineStatus {
            playing: self.shared.playing.load(Ordering::Acquire),
            position: self.shared.position_frames.load(Ordering::Acquire) as f64
                / sample_rate.max(1) as f64,
            duration: self.shared.duration_frames.load(Ordering::Relaxed) as f64
                / sample_rate.max(1) as f64,
            peak: f32::from_bits(self.shared.peak.load(Ordering::Relaxed)),
            sample_rate,
            output: self.output.lock().clone(),
            sample_accurate: self.sample_accurate,
            glitches: self.shared.glitches.load(Ordering::Relaxed),
        }
    }

    /// Output events since the last call. Updates the reported output name
    /// when the stream was rerouted. Call once per UI frame.
    pub fn drain_output_events(&self) -> Vec<OutputEvent> {
        let events: Vec<OutputEvent> = self.events.try_iter().collect();
        for event in &events {
            match event {
                OutputEvent::Rerouted { device } => *self.output.lock() = device.clone(),
                OutputEvent::Lost => *self.output.lock() = "No output device".to_string(),
                _ => {}
            }
        }
        events
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// A cloneable command sender, used by the MIDI callback to reach the audio
    /// thread without going through the UI at 60 fps.
    pub fn sender(&self) -> crossbeam_channel::Sender<Command> {
        self.tx.clone()
    }

    pub fn send(&self, command: Command) {
        // The audio side drains at block rate; an unbounded queue keeps the UI
        // responsive even if the device stalls.
        let _ = self.tx.send(command);
    }

    pub fn note_on(&self, key: u8, velocity: u8) {
        self.send(Command::NoteOn { key, velocity });
    }

    pub fn note_off(&self, key: u8) {
        self.send(Command::NoteOff { key });
    }

    pub fn sustain(&self, down: bool) {
        self.send(Command::Sustain(down));
    }

    pub fn play(&self, timeline: Timeline, from: f64) {
        self.send(Command::Play { timeline, from });
    }

    pub fn pause(&self) {
        self.send(Command::Pause);
    }

    pub fn resume(&self) {
        self.send(Command::Resume);
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn seek(&self, sec: f64) {
        self.send(Command::Seek(sec));
    }

    pub fn set_loop(&self, looping: bool) {
        self.send(Command::SetLoop(looping));
    }

    pub fn set_volume(&self, volume: f32) {
        self.send(Command::SetVolume(volume));
    }

    pub fn set_instrument(&self, instrument: Box<dyn Instrument>) {
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        let _ = self.tx.send(Command::SetInstrument {
            instrument,
            ack: Some(ack_tx),
        });
        // Wait for the audio thread to release the previous instrument; the
        // caller may then drop the last Arc<Plugin> on the UI thread.  Kontakt
        // must not be destroyed on the audio thread.
        let _ = ack_rx.recv_timeout(Duration::from_millis(750));
    }
}

/// Map a cpal stream error to what Surf cares about.
fn classify(err: &cpal::Error) -> OutputEvent {
    use cpal::ErrorKind;
    match err.kind() {
        // cpal has already rerouted the stream; the new device is the default.
        ErrorKind::DeviceChanged => OutputEvent::Rerouted {
            device: default_output_name().unwrap_or_else(|| "default output".to_string()),
        },
        ErrorKind::DeviceNotAvailable => OutputEvent::Lost,
        ErrorKind::Xrun => OutputEvent::Glitch,
        _ => OutputEvent::Error(err.to_string()),
    }
}

fn default_output_name() -> Option<String> {
    let device = cpal::default_host().default_output_device()?;
    device.description().ok().map(|d| d.name().to_string())
}

fn open_output() -> Result<(cpal::Device, u32, u16, String)> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .context("no default audio output device")?;
    let name = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "Default output".to_string());
    let supported = device
        .default_output_config()
        .context("no default output config")?;
    if supported.sample_format() != cpal::SampleFormat::F32 {
        anyhow::bail!("default output does not support f32 samples");
    }
    if supported.channels() < 1 {
        anyhow::bail!("output device has no channels");
    }
    let sample_rate = supported.sample_rate();
    Ok((device, sample_rate, supported.channels(), name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::ErrorKind;

    #[test]
    fn route_changes_and_glitches_are_not_reported_as_errors() {
        assert!(matches!(
            classify(&cpal::Error::new(ErrorKind::DeviceChanged)),
            OutputEvent::Rerouted { .. }
        ));
        assert_eq!(
            classify(&cpal::Error::new(ErrorKind::DeviceNotAvailable)),
            OutputEvent::Lost
        );
        assert_eq!(
            classify(&cpal::Error::new(ErrorKind::Xrun)),
            OutputEvent::Glitch
        );
        assert!(matches!(
            classify(&cpal::Error::new(ErrorKind::DeviceBusy)),
            OutputEvent::Error(_)
        ));
    }
}
