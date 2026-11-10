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

/// Instrument slots the engine mixes. Slot 0 is the shared built-in piano
/// that tracks fall back to; the session assigns the others.
pub const MAX_SLOTS: usize = 32;

/// Where a track's notes go: an instrument slot and the MIDI channel on it.
/// Tracks that share a plugin (Kontakt) share its slot on different channels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Route {
    pub slot: u8,
    pub channel: u8,
}

pub enum Command {
    /// Live input, played on the input track's route (see `SetInput`).
    NoteOn { key: u8, velocity: u8 },
    NoteOff { key: u8 },
    Sustain(bool),
    AllNotesOff,
    /// Put an instrument in a slot, or empty it.
    SetSlot {
        slot: u8,
        instrument: Option<Box<dyn Instrument>>,
        /// Signalled after the old instrument has been dropped on the audio
        /// thread, so the UI can safely release the last plugin reference.
        ack: Option<crossbeam_channel::Sender<()>>,
    },
    /// The route of every track, by track index (`TimedNote::track`).
    SetRoutes(Vec<Route>),
    /// Linear output gain of a slot: the track's fader, or for a shared
    /// plugin the loudest of its tracks.
    SetSlotGain { slot: u8, gain: f32 },
    /// The track live input plays, by index.
    SetInput(u16),
    ControlChange { slot: u8, channel: u8, controller: u8, value: u8 },
    Play { timeline: Timeline, from: f64 },
    /// Swap in an edited timeline without restarting: the position stays,
    /// notes still sounding and still in it keep sounding, and only notes
    /// that were removed or changed stop. Works while paused too, so Resume
    /// plays the edit.
    Update { timeline: Timeline },
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
    /// Peak of each slot after its gain, for the track meters.
    pub slot_peaks: [f32; MAX_SLOTS],
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
    slot_peaks: [AtomicU32; MAX_SLOTS],
    sample_rate: AtomicU32,
    glitches: AtomicU32,
}

struct ActiveNote {
    track: u16,
    route: Route,
    key: u8,
    end_frame: u64,
}

struct Core {
    slots: Vec<Option<Box<dyn Instrument>>>,
    gains: [f32; MAX_SLOTS],
    routes: Vec<Route>,
    input: u16,
    /// Where each live key went, so its note-off follows it even if the
    /// input track changes while the key is down.
    held: [Option<Route>; 128],
    sustain: bool,
    timeline: Option<Timeline>,
    next_note: usize,
    active: Vec<ActiveNote>,
    /// Scratch for notes released in the current chunk. Preallocated so the
    /// audio callback never allocates.
    ended: Vec<(Route, u8)>,
    pos_frames: u64,
    duration_frames: u64,
    playing: bool,
    looping: bool,
    volume: f32,
    sample_rate: f64,
    scratch: Vec<f32>,
    /// One slot's output for the current chunk, before it is mixed.
    slot_buf: Vec<f32>,
    /// Replaced timelines go back to the UI thread to be freed, so the audio
    /// callback doesn't call the allocator.
    trash: crossbeam_channel::Sender<Timeline>,
    shared: Arc<Shared>,
}

impl Core {
    fn new(
        sample_rate: u32,
        shared: Arc<Shared>,
        trash: crossbeam_channel::Sender<Timeline>,
    ) -> Self {
        Self {
            slots: std::iter::repeat_with(|| None).take(MAX_SLOTS).collect(),
            gains: [1.0; MAX_SLOTS],
            routes: Vec::new(),
            input: 0,
            held: [None; 128],
            sustain: false,
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
            slot_buf: vec![0.0; CHUNK * 2],
            trash,
            shared,
        }
    }

    fn route(&self, track: u16) -> Route {
        self.routes.get(track as usize).copied().unwrap_or_default()
    }

    fn slot(&mut self, route: Route) -> Option<&mut Box<dyn Instrument>> {
        self.slots.get_mut(route.slot as usize).and_then(Option::as_mut)
    }

    fn all_notes_off(&mut self) {
        for instrument in self.slots.iter_mut().flatten() {
            instrument.all_notes_off();
        }
        self.held = [None; 128];
        self.sustain = false;
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::NoteOn { key, velocity } => {
                let route = self.route(self.input);
                self.held[key as usize & 127] = Some(route);
                if let Some(i) = self.slot(route) {
                    i.note_on(route.channel, key, velocity);
                }
            }
            Command::NoteOff { key } => {
                let route = self.held[key as usize & 127]
                    .take()
                    .unwrap_or_else(|| self.route(self.input));
                if let Some(i) = self.slot(route) {
                    i.note_off(route.channel, key);
                }
            }
            Command::Sustain(down) => {
                self.sustain = down;
                let route = self.route(self.input);
                if let Some(i) = self.slot(route) {
                    i.set_sustain(route.channel, down);
                }
            }
            Command::AllNotesOff => self.all_notes_off(),
            Command::SetSlot {
                slot,
                instrument,
                ack,
            } => {
                if let Some(cell) = self.slots.get_mut(slot as usize) {
                    if let Some(old) = cell.as_mut() {
                        old.all_notes_off();
                    }
                    *cell = instrument.map(|mut i| {
                        i.set_sample_rate(self.sample_rate as f32);
                        i
                    });
                    // Notes on the old instrument are gone with it.
                    self.active.retain(|n| n.route.slot != slot);
                }
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
            Command::SetRoutes(routes) => self.routes = routes,
            Command::SetSlotGain { slot, gain } => {
                if let Some(g) = self.gains.get_mut(slot as usize) {
                    *g = gain.clamp(0.0, 2.0);
                }
            }
            Command::SetInput(track) => {
                let (old, new) = (self.route(self.input), self.route(track));
                self.input = track;
                // Hand the pedal over to the new instrument.
                if self.sustain && old != new {
                    if let Some(i) = self.slot(old) {
                        i.set_sustain(old.channel, false);
                    }
                    if let Some(i) = self.slot(new) {
                        i.set_sustain(new.channel, true);
                    }
                }
            }
            Command::ControlChange {
                slot,
                channel,
                controller,
                value,
            } => {
                if let Some(i) = self.slot(Route { slot, channel }) {
                    i.control_change(channel, controller, value);
                }
            }
            Command::Play { timeline, from } => {
                self.all_notes_off();
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
            Command::Update { timeline } => self.update(timeline),
            Command::Pause => {
                self.playing = false;
                self.all_notes_off();
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
                self.all_notes_off();
                self.shared.playing.store(false, Ordering::Release);
                self.shared.position_frames.store(0, Ordering::Release);
            }
            Command::Seek(sec) => {
                let sec = sec.max(0.0);
                self.pos_frames = (sec * self.sample_rate) as u64;
                self.active.clear();
                self.all_notes_off();
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

    fn update(&mut self, timeline: Timeline) {
        let sr = self.sample_rate;
        let pos = self.pos_frames;
        let frame = |t: f64| (t * sr) as u64;
        // Notes that started before now: the ones already triggered.
        let started = timeline.notes.partition_point(|n| frame(n.start) < pos);
        let ended = &mut self.ended;
        ended.clear();
        self.active.retain_mut(|a| {
            let same = timeline.notes[..started]
                .iter()
                .rev()
                .find(|n| n.track == a.track && n.key == a.key && frame(n.end) > pos);
            match same {
                Some(n) => {
                    a.end_frame = frame(n.end).max(pos + 1);
                    true
                }
                None => {
                    ended.push((a.route, a.key));
                    false
                }
            }
        });
        for &(route, key) in &self.ended {
            if let Some(i) = self.slots.get_mut(route.slot as usize).and_then(Option::as_mut) {
                i.note_off(route.channel, key);
            }
        }
        self.next_note = started;
        self.duration_frames = (timeline.duration * sr).ceil() as u64;
        self.shared
            .duration_frames
            .store(self.duration_frames, Ordering::Relaxed);
        if let Some(old) = self.timeline.replace(timeline) {
            // Full only if the UI stopped draining; then free it here.
            let _ = self.trash.try_send(old);
        }
    }

    fn reset_to_start(&mut self) {
        self.pos_frames = 0;
        self.next_note = 0;
        self.active.clear();
        self.all_notes_off();
        self.shared.position_frames.store(0, Ordering::Release);
    }

    /// Render interleaved stereo frames into `scratch` and return that slice.
    fn render(&mut self, frames: usize) -> &[f32] {
        let frames = frames.min(MAX_BLOCK);
        let mut done = 0usize;
        let mut peak = 0.0f32;
        let mut slot_peaks = [0.0f32; MAX_SLOTS];

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
                        self.all_notes_off();
                        self.shared.playing.store(false, Ordering::Release);
                    }
                } else {
                    self.pos_frames = end;
                }
            }

            let out = &mut self.scratch[done * 2..(done + chunk) * 2];
            out.fill(0.0);
            let buf = &mut self.slot_buf[..chunk * 2];
            for (index, cell) in self.slots.iter_mut().enumerate() {
                let Some(instrument) = cell else { continue };
                // Every slot runs each chunk, even a silent one: a plugin
                // host's audio ring has to keep draining.
                instrument.process(buf, chunk);
                let gain = self.gains[index];
                for (mix, sample) in out.iter_mut().zip(buf.iter()) {
                    let s = sample * gain;
                    slot_peaks[index] = slot_peaks[index].max(s.abs());
                    *mix += s;
                }
            }
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
        for (cell, p) in self.shared.slot_peaks.iter().zip(slot_peaks) {
            cell.store(p.to_bits(), Ordering::Relaxed);
        }
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
                ended.push((note.route, note.key));
                false
            } else {
                true
            }
        });
        for &(route, key) in &self.ended {
            if let Some(i) = self.slots.get_mut(route.slot as usize).and_then(Option::as_mut) {
                i.note_off(route.channel, key);
            }
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
            let route = self
                .routes
                .get(note.track as usize)
                .copied()
                .unwrap_or_default();
            let Some(i) = self.slots.get_mut(route.slot as usize).and_then(Option::as_mut) else {
                continue;
            };
            i.note_on(route.channel, note.key, note.velocity);
            let note_end = ((note.end * self.sample_rate) as u64).max(note_start + 1);
            self.active.push(ActiveNote {
                track: note.track,
                route,
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
    trash: crossbeam_channel::Receiver<Timeline>,
}

impl AudioEngine {
    /// Open the default output. Every instrument slot starts empty; fill
    /// them with [`AudioEngine::set_slot`].
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            position_frames: AtomicU64::new(0),
            duration_frames: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            peak: AtomicU32::new(0),
            slot_peaks: std::array::from_fn(|_| AtomicU32::new(0)),
            sample_rate: AtomicU32::new(48_000),
            glitches: AtomicU32::new(0),
        });
        let (event_tx, events) = crossbeam_channel::unbounded();
        let (tx, rx) = crossbeam_channel::unbounded();
        let (trash_tx, trash) = crossbeam_channel::bounded(8);

        // Try the default output device first.
        match open_output() {
            Ok((device, sample_rate, channels, name)) => {
                shared.sample_rate.store(sample_rate, Ordering::Relaxed);
                let core = Arc::new(Mutex::new(Core::new(
                    sample_rate,
                    shared.clone(),
                    trash_tx.clone(),
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
                    trash,
                }
            }
            Err(err) => {
                // No device: keep a real-time clock and run the synth silently.
                eprintln!("no audio output device ({err}); running silent clock");
                let sample_rate = 48_000;
                shared.sample_rate.store(sample_rate, Ordering::Relaxed);
                let core = Arc::new(Mutex::new(Core::new(
                    sample_rate,
                    shared.clone(),
                    trash_tx.clone(),
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
                    trash,
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
            slot_peaks: std::array::from_fn(|i| {
                f32::from_bits(self.shared.slot_peaks[i].load(Ordering::Relaxed))
            }),
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
        // Free timelines the audio thread swapped out.
        while self.trash.try_recv().is_ok() {}
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

    /// Replace what plays with an edited timeline; see [`Command::Update`].
    pub fn update(&self, timeline: Timeline) {
        self.send(Command::Update { timeline });
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

    /// Put `instrument` in `slot` (None empties it). Waits for the audio
    /// thread to release the previous one, so the caller may then drop the
    /// last reference to its plugin host on the UI thread.
    pub fn set_slot(&self, slot: u8, instrument: Option<Box<dyn Instrument>>) {
        let (ack_tx, ack_rx) = crossbeam_channel::bounded(1);
        let _ = self.tx.send(Command::SetSlot {
            slot,
            instrument,
            ack: Some(ack_tx),
        });
        let _ = ack_rx.recv_timeout(Duration::from_millis(750));
    }
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
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
    use surf_core::song::TimedNote;

    type Log = Arc<parking_lot::Mutex<Vec<(u8, u8, bool)>>>;

    /// Records (channel, key, on) and outputs a constant 0.5.
    struct Probe(Log);

    impl Instrument for Probe {
        fn note_on(&mut self, channel: u8, key: u8, _velocity: u8) {
            self.0.lock().push((channel, key, true));
        }
        fn note_off(&mut self, channel: u8, key: u8) {
            self.0.lock().push((channel, key, false));
        }
        fn all_notes_off(&mut self) {}
        fn set_sustain(&mut self, _channel: u8, _down: bool) {}
        fn process(&mut self, out: &mut [f32], frames: usize) {
            out[..frames * 2].fill(0.5);
        }
        fn name(&self) -> &str {
            "probe"
        }
    }

    fn core() -> Core {
        let shared = Arc::new(Shared {
            position_frames: AtomicU64::new(0),
            duration_frames: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            peak: AtomicU32::new(0),
            slot_peaks: std::array::from_fn(|_| AtomicU32::new(0)),
            sample_rate: AtomicU32::new(48_000),
            glitches: AtomicU32::new(0),
        });
        // Tests keep the receiver alive so swapped timelines queue up.
        let (trash, keep) = crossbeam_channel::bounded(8);
        std::mem::forget(keep);
        Core::new(48_000, shared, trash)
    }

    fn note(track: u16, key: u8, start: f64) -> TimedNote {
        TimedNote {
            key,
            velocity: 100,
            channel: 0,
            track,
            start,
            end: start + 0.01,
        }
    }

    #[test]
    fn tracks_play_on_their_slot_and_channel_with_their_gain() {
        let mut c = core();
        let (piano, kontakt): (Log, Log) = Default::default();
        for (slot, log) in [(1u8, &piano), (2, &kontakt)] {
            c.apply(Command::SetSlot {
                slot,
                instrument: Some(Box::new(Probe(log.clone()))),
                ack: None,
            });
        }
        // Track 0 on its own piano; tracks 1 and 2 share Kontakt on ch 2 and 3.
        c.apply(Command::SetRoutes(vec![
            Route { slot: 1, channel: 0 },
            Route { slot: 2, channel: 1 },
            Route { slot: 2, channel: 2 },
        ]));
        c.apply(Command::SetSlotGain { slot: 1, gain: 0.5 });
        c.apply(Command::SetVolume(1.0));
        c.apply(Command::Play {
            timeline: Timeline {
                notes: vec![note(0, 60, 0.0), note(1, 40, 0.0), note(2, 72, 0.0)],
                duration: 1.0,
            },
            from: 0.0,
        });
        let out = c.render(2048).to_vec();
        assert_eq!(*piano.lock(), vec![(0, 60, true), (0, 60, false)]);
        assert_eq!(
            *kontakt.lock(),
            vec![(1, 40, true), (2, 72, true), (1, 40, false), (2, 72, false)]
        );
        // 0.5 * 0.5 from the piano plus 0.5 from Kontakt.
        assert!((out[0] - 0.75).abs() < 1e-6, "{}", out[0]);
        let peaks = &c.shared.slot_peaks;
        assert_eq!(f32::from_bits(peaks[1].load(Ordering::Relaxed)), 0.25);
        assert_eq!(f32::from_bits(peaks[2].load(Ordering::Relaxed)), 0.5);
    }

    #[test]
    fn live_note_off_follows_its_note_on_after_the_input_moves() {
        let mut c = core();
        let (a, b): (Log, Log) = Default::default();
        c.apply(Command::SetSlot { slot: 0, instrument: Some(Box::new(Probe(a.clone()))), ack: None });
        c.apply(Command::SetSlot { slot: 1, instrument: Some(Box::new(Probe(b.clone()))), ack: None });
        c.apply(Command::SetRoutes(vec![
            Route { slot: 0, channel: 0 },
            Route { slot: 1, channel: 4 },
        ]));
        c.apply(Command::SetInput(1));
        c.apply(Command::NoteOn { key: 60, velocity: 90 });
        c.apply(Command::SetInput(0));
        c.apply(Command::NoteOff { key: 60 });
        assert!(a.lock().is_empty());
        assert_eq!(*b.lock(), vec![(4, 60, true), (4, 60, false)]);
    }

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

    fn long(track: u16, key: u8, start: f64, end: f64) -> TimedNote {
        TimedNote {
            key,
            velocity: 100,
            channel: 0,
            track,
            start,
            end,
        }
    }

    /// Render `frames`, in as many callbacks as `render`'s cap needs.
    fn run(c: &mut Core, mut frames: usize) {
        while frames > 0 {
            let n = frames.min(MAX_BLOCK);
            c.render(n);
            frames -= n;
        }
    }

    fn probe_core() -> (Core, Log) {
        let mut c = core();
        let log: Log = Default::default();
        c.apply(Command::SetSlot {
            slot: 1,
            instrument: Some(Box::new(Probe(log.clone()))),
            ack: None,
        });
        c.apply(Command::SetRoutes(vec![
            Route { slot: 1, channel: 0 },
            Route { slot: 1, channel: 1 },
        ]));
        (c, log)
    }

    /// An edit while playing keeps the position and leaves sounding notes
    /// alone; it doesn't restart the song or cut every note.
    #[test]
    fn an_edit_while_playing_keeps_sounding_notes() {
        let (mut c, log) = probe_core();
        // A held chord (60 + 64) and a later note 67.
        let a = long(0, 60, 0.0, 1.0);
        let b = long(1, 64, 0.0, 1.0);
        let later = long(0, 67, 0.5, 0.6);
        c.apply(Command::Play {
            timeline: Timeline { notes: vec![a, b, later], duration: 1.0 },
            from: 0.0,
        });
        c.render(4800); // 0.1 s in: 60 and 64 are sounding
        let pos = c.pos_frames;
        log.lock().clear();

        // Delete 64 and move the later note to 0.3 s.
        let moved = long(0, 67, 0.3, 0.4);
        c.apply(Command::Update {
            timeline: Timeline { notes: vec![a, moved], duration: 1.0 },
        });
        assert_eq!(c.pos_frames, pos, "position kept");
        assert_eq!(
            *log.lock(),
            vec![(1, 64, false)],
            "only the deleted note stops; 60 keeps ringing"
        );
        log.lock().clear();

        // Play on past 0.5 s: the moved note sounds at its new time, once.
        run(&mut c, 24_000);
        let ons: Vec<u8> = log.lock().iter().filter(|e| e.2).map(|e| e.1).collect();
        assert_eq!(ons, vec![67], "moved note plays once, nothing replays");
    }

    /// Shortening a sounding note ends it at its new end, not the old one.
    #[test]
    fn a_shortened_note_ends_on_time() {
        let (mut c, log) = probe_core();
        c.apply(Command::Play {
            timeline: Timeline { notes: vec![long(0, 60, 0.0, 2.0)], duration: 2.0 },
            from: 0.0,
        });
        c.render(4800); // 0.1 s
        c.apply(Command::Update {
            timeline: Timeline { notes: vec![long(0, 60, 0.0, 0.2)], duration: 2.0 },
        });
        assert!(log.lock().iter().all(|e| e.2), "still sounding after the edit");
        c.render(9600); // to 0.3 s
        assert_eq!(log.lock().last(), Some(&(0, 60, false)), "ended near 0.2 s");
    }

    /// While paused an update just takes effect for Resume.
    #[test]
    fn an_update_while_paused_plays_on_resume() {
        let (mut c, log) = probe_core();
        c.apply(Command::Play {
            timeline: Timeline { notes: vec![long(0, 60, 0.5, 0.6)], duration: 1.0 },
            from: 0.0,
        });
        c.render(4800);
        c.apply(Command::Pause);
        c.apply(Command::Update {
            timeline: Timeline { notes: vec![long(0, 72, 0.5, 0.6)], duration: 1.0 },
        });
        c.apply(Command::Resume);
        run(&mut c, 48_000);
        let ons: Vec<u8> = log.lock().iter().filter(|e| e.2).map(|e| e.1).collect();
        assert_eq!(ons, vec![72]);
    }

}
