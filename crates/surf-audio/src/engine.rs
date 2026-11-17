//! Real-time audio output plus the clock that drives both sound and visuals.
//!
//! The engine deliberately keeps one authoritative timeline position in frames.
//! The UI reads it through atomics; the audio callback advances it.  If no audio
//! device is available (common on CI / remote Windows sessions) a silent timer
//! thread runs the same code path so the visualizer remains usable.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use super::Instrument;
use surf_core::song::{TimedNote, Timeline};

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
    NoteOn {
        key: u8,
        velocity: u8,
    },
    NoteOff {
        key: u8,
    },
    /// The hardware sustain pedal.
    Sustain(bool),
    /// Musical Typing's pedal key. Held together with the hardware pedal, the
    /// pedal stays down until both are up.
    TypingSustain(bool),
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
    /// New routes and live-input track together, as one change: a held pedal
    /// moves only from the old input route to the final one, never through an
    /// in-between pairing of new routes with the old input index.
    SetRoutesAndInput {
        routes: Vec<Route>,
        input: u16,
    },
    /// Linear output gain of a slot: the track's fader, or for a shared
    /// plugin the loudest of its tracks.
    SetSlotGain {
        slot: u8,
        gain: f32,
    },
    /// The track live input plays, by index.
    SetInput(u16),
    ControlChange {
        slot: u8,
        channel: u8,
        controller: u8,
        value: u8,
    },
    Play {
        timeline: Timeline,
        from: f64,
    },
    /// Play / Pause, decided on the audio thread so that it follows the
    /// commands queued before it: playing, pause; stopped at the start or the
    /// end, play `timeline` from the start; stopped elsewhere, resume.
    TogglePlay {
        timeline: Timeline,
    },
    /// Stop as in Logic, decided on the audio thread like `TogglePlay`:
    /// playing, pause where the playhead is; stopped, go back to the start.
    /// Pedals the player holds stay down either way.
    StopOrRewind,
    /// Swap in an edited timeline without restarting: the position stays,
    /// notes still sounding and still in it keep sounding, and only notes
    /// that were removed or changed stop. Works while paused too, so Resume
    /// plays the edit.
    Update {
        timeline: Timeline,
    },
    Pause,
    Resume,
    Stop,
    /// `Stop`, but the pedals the player holds stay down (around a take). The
    /// engine's own pedal state is used, so a pedal-up already queued ahead of
    /// this is not undone.
    StopKeepingPedals,
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
    /// How many `TogglePlay` / `StopOrRewind` the audio thread has applied.
    /// Stored after their effect, so a reader that sees it also sees the state.
    transport_done: AtomicU64,
}

struct ActiveNote {
    /// The track's stable id, to find the note again after an edit.
    track_id: u64,
    route: Route,
    key: u8,
    end_frame: u64,
    /// The block in which its voice was struck (its start frame).
    struck_at: u64,
}

/// Something the audio thread swapped out and wants the UI thread to free.
///
/// A large `Vec` is past glibc's mmap threshold, so dropping it is a `munmap` syscall
/// inside the callback. Both kinds go back over the trash channel instead.
#[allow(dead_code)] // payloads are freed on the UI side, never read
#[derive(Debug)]
enum Retired {
    Timeline(Timeline),
    Routes(Vec<Route>),
}

struct Core {
    slots: Vec<Option<Box<dyn Instrument>>>,
    gains: [f32; MAX_SLOTS],
    routes: Vec<Route>,
    input: u16,
    /// Where each live key went, so its note-off follows it even if the
    /// input track changes while the key is down.
    held: [Option<Route>; 128],
    /// Which pedals are down: `HW_PEDAL` and `TYPING_PEDAL`. The instrument's
    /// pedal is down while any is.
    pedals: u8,
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
    /// Replaced timelines and route lists go back to the UI thread to be freed, so the
    /// audio callback doesn't call the allocator.
    trash: crossbeam_channel::Sender<Retired>,
    /// What didn't fit in `trash`, kept until a slot frees. Its capacity is reserved up
    /// front, so holding one never allocates; past that, the newest still waits in
    /// `timeline` until the next update.
    retired: Vec<Retired>,
    /// A command that would retire a payload, held while `retired` is full so the
    /// callback never frees it. Applied (before anything newer) once there is room.
    deferred: Option<Command>,
    shared: Arc<Shared>,
}

/// Notes the engine tracks at once. Beyond this, new ones are dropped
/// instead of growing the list in the audio callback.
const MAX_ACTIVE: usize = 512;

/// Retired timelines `Core` can hold without allocating, beyond `trash`.
const RETIRED_CAP: usize = 64;

impl Core {
    fn new(
        sample_rate: u32,
        shared: Arc<Shared>,
        trash: crossbeam_channel::Sender<Retired>,
    ) -> Self {
        Self {
            slots: std::iter::repeat_with(|| None).take(MAX_SLOTS).collect(),
            gains: [1.0; MAX_SLOTS],
            routes: Vec::new(),
            input: 0,
            held: [None; 128],
            pedals: 0,
            timeline: None,
            next_note: 0,
            active: Vec::with_capacity(MAX_ACTIVE),
            ended: Vec::with_capacity(MAX_ACTIVE),
            pos_frames: 0,
            duration_frames: 0,
            playing: false,
            looping: false,
            volume: 0.9,
            sample_rate: sample_rate as f64,
            scratch: vec![0.0; MAX_BLOCK * 2],
            slot_buf: vec![0.0; CHUNK * 2],
            trash,
            retired: Vec::with_capacity(RETIRED_CAP),
            deferred: None,
            shared,
        }
    }

    fn route(&self, track: u16) -> Route {
        self.routes.get(track as usize).copied().unwrap_or_default()
    }

    fn slot(&mut self, route: Route) -> Option<&mut Box<dyn Instrument>> {
        self.slots
            .get_mut(route.slot as usize)
            .and_then(Option::as_mut)
    }

    fn all_notes_off(&mut self) {
        for instrument in self.slots.iter_mut().flatten() {
            instrument.all_notes_off();
        }
        // Nothing sounds any more, so no note owns a key: a later live press and
        // release of one must not be held back on behalf of a silenced note.
        self.active.clear();
        self.held = [None; 128];
        self.pedals = 0;
    }

    fn sustain(&self) -> bool {
        self.pedals != 0
    }

    /// The live input moved from `before` to the current input route (a new
    /// input track, or new routes): a held pedal follows it.
    fn hand_pedal_over(&mut self, before: Route) {
        let now = self.route(self.input);
        if self.sustain() && before != now {
            if let Some(i) = self.slot(before) {
                i.set_sustain(before.channel, false);
            }
            if let Some(i) = self.slot(now) {
                i.set_sustain(now.channel, true);
            }
        }
    }

    fn play(&mut self, timeline: Timeline, from: f64) {
        self.release_playback();
        self.duration_frames = (timeline.duration * self.sample_rate).ceil() as u64;
        self.pos_frames = (from.max(0.0) * self.sample_rate) as u64;
        self.next_note = timeline.notes.partition_point(|n| n.start < from.max(0.0));
        if let Some(old) = self.timeline.replace(timeline) {
            self.retire(Retired::Timeline(old));
        }
        self.playing = true;
        self.shared
            .duration_frames
            .store(self.duration_frames, Ordering::Relaxed);
        self.shared.playing.store(true, Ordering::Release);
    }

    fn pause(&mut self) {
        self.playing = false;
        self.release_playback();
        self.shared.playing.store(false, Ordering::Release);
    }

    fn stop_keeping_pedals(&mut self) {
        let pedals = self.pedals;
        self.stop();
        // all_notes_off lifted the instrument's pedal: put it back down.
        for which in [HW_PEDAL, TYPING_PEDAL] {
            if pedals & which != 0 {
                self.set_pedal(which, true);
            }
        }
    }

    fn stop(&mut self) {
        self.playing = false;
        self.pos_frames = 0;
        self.next_note = 0;
        self.active.clear();
        self.all_notes_off();
        self.shared.playing.store(false, Ordering::Release);
        self.shared.position_frames.store(0, Ordering::Release);
    }

    /// Set one pedal; the instrument hears only the combined pedal changing.
    fn set_pedal(&mut self, which: u8, down: bool) {
        let was = self.sustain();
        if down {
            self.pedals |= which;
        } else {
            self.pedals &= !which;
        }
        let now = self.sustain();
        if now != was {
            let route = self.route(self.input);
            if let Some(i) = self.slot(route) {
                i.set_sustain(route.channel, now);
            }
        }
    }

    /// Whether a playback note or the player is sounding `key` on `route`.
    fn sounding(&self, route: Route, key: u8) -> bool {
        self.active.iter().any(|a| a.route == route && a.key == key)
            || self.held[key as usize & 127] == Some(route)
    }

    /// Note-on for `key` on `route`. If it is already sounding there (another
    /// playback note, or the player), end that voice first: one voice per key and
    /// route, as on an instrument, so every note-on is matched by one note-off.
    fn strike(&mut self, route: Route, key: u8, velocity: u8) {
        self.strike_at(route, key, velocity, None);
    }

    /// `strike` for playback. Notes struck in the same block (frames from
    /// `block`) on a voice already struck in that block share it instead of
    /// retriggering: hundreds of identical starts would otherwise send an
    /// off/on pair each and overflow a plugin's per-block event batch.
    fn strike_at(&mut self, route: Route, key: u8, velocity: u8, block: Option<u64>) {
        if let Some(block) = block
            && self
                .active
                .iter()
                .any(|a| a.route == route && a.key == key && a.struck_at >= block)
        {
            return;
        }
        let again = self.sounding(route, key);
        if let Some(i) = self.slot(route) {
            if again {
                i.note_off(route.channel, key);
            }
            i.note_on(route.channel, key, velocity);
        }
    }

    /// Note-off for a playback note, unless that voice still has to sound:
    /// another playback note of the same key on the same route, or the player
    /// holding the key there. Whichever ends last sends the note-off.
    fn release(&mut self, route: Route, key: u8) {
        if !self.sounding(route, key)
            && let Some(i) = self.slot(route)
        {
            i.note_off(route.channel, key);
        }
    }

    /// Note-offs for the voices in `ended`. A key and route is one voice (see
    /// `strike`), so several owners ending together send one note-off. Sorting
    /// and dedup work in place: nothing allocates in the callback.
    fn release_ended(&mut self) {
        self.ended
            .sort_unstable_by_key(|(r, k)| (r.slot, r.channel, *k));
        self.ended.dedup();
        for i in 0..self.ended.len() {
            let (route, key) = self.ended[i];
            self.release(route, key);
        }
    }

    /// End every playback note, leaving what the player holds alone: keys still
    /// down and the sustain pedal. Transport moves (loop, seek, play, pause) use
    /// this; only Stop and AllNotesOff reset everything.
    fn release_playback(&mut self) {
        self.ended.clear();
        self.ended
            .extend(self.active.drain(..).map(|a| (a.route, a.key)));
        self.release_ended();
    }

    fn apply(&mut self, command: Command) {
        match command {
            Command::NoteOn { key, velocity } => {
                let route = self.route(self.input);
                self.strike(route, key, velocity);
                self.held[key as usize & 127] = Some(route);
            }
            Command::NoteOff { key } => {
                let route = self.held[key as usize & 127]
                    .take()
                    .unwrap_or_else(|| self.route(self.input));
                // Playback is sounding the same key there: it ends it.
                let playing = self.active.iter().any(|a| a.route == route && a.key == key);
                if !playing && let Some(i) = self.slot(route) {
                    i.note_off(route.channel, key);
                }
            }
            Command::Sustain(down) => self.set_pedal(HW_PEDAL, down),
            Command::TypingSustain(down) => self.set_pedal(TYPING_PEDAL, down),
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
                    // A pedal still held reaches the instrument that replaced it.
                    let input = self.route(self.input);
                    if self.sustain()
                        && input.slot == slot
                        && let Some(i) = self.slot(input)
                    {
                        i.set_sustain(input.channel, true);
                    }
                }
                if let Some(ack) = ack {
                    let _ = ack.send(());
                }
            }
            Command::SetRoutes(routes) => {
                let before = self.route(self.input);
                let old = std::mem::replace(&mut self.routes, routes);
                self.retire(Retired::Routes(old));
                self.hand_pedal_over(before);
            }
            Command::SetSlotGain { slot, gain } => {
                if let Some(g) = self.gains.get_mut(slot as usize) {
                    *g = gain.clamp(0.0, 2.0);
                }
            }
            Command::SetRoutesAndInput { routes, input } => {
                let before = self.route(self.input);
                let old = std::mem::replace(&mut self.routes, routes);
                self.input = input;
                self.retire(Retired::Routes(old));
                self.hand_pedal_over(before);
            }
            Command::SetInput(track) => {
                let before = self.route(self.input);
                self.input = track;
                self.hand_pedal_over(before);
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
            Command::Play { timeline, from } => self.play(timeline, from),
            Command::TogglePlay { timeline } => {
                // Near the end counts as the end, as the UI used to decide.
                let near_end = (0.05 * self.sample_rate) as u64;
                if self.playing {
                    self.pause();
                    self.retire(Retired::Timeline(timeline));
                } else if self.timeline.is_none()
                    || self.pos_frames == 0
                    || self.pos_frames + near_end >= self.duration_frames
                {
                    self.play(timeline, 0.0);
                } else {
                    // The current timeline is up to date: every edit sends Update.
                    self.playing = true;
                    self.shared.playing.store(true, Ordering::Release);
                    self.retire(Retired::Timeline(timeline));
                }
                self.shared.transport_done.fetch_add(1, Ordering::Release);
            }
            Command::StopOrRewind => {
                if self.playing {
                    self.pause();
                } else {
                    self.stop_keeping_pedals();
                }
                self.shared.transport_done.fetch_add(1, Ordering::Release);
            }
            Command::Update { timeline } => self.update(timeline),
            Command::Pause => self.pause(),
            Command::Resume => {
                if self.timeline.is_some() {
                    self.playing = true;
                    self.shared.playing.store(true, Ordering::Release);
                }
            }
            Command::Stop => self.stop(),
            Command::StopKeepingPedals => self.stop_keeping_pedals(),
            Command::Seek(sec) => {
                let sec = sec.max(0.0);
                self.pos_frames = (sec * self.sample_rate) as u64;
                self.release_playback();
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
        let covers = |n: &TimedNote| frame(n.start) < pos && frame(n.end) > pos;
        // Notes that started before now. Those after it play as usual.
        let started = timeline.notes.partition_point(|n| frame(n.start) < pos);
        let now = &timeline.notes[..started];

        // Keep a sounding note if the new timeline still has it: same track
        // (by stable id) and key, covering now. Move its end to the new
        // end; stop it otherwise. Nothing here allocates: this runs in the
        // audio callback.
        let ended = &mut self.ended;
        ended.clear();
        self.active.retain_mut(|a| {
            let found = now
                .iter()
                .rev()
                .find(|n| n.track_id == a.track_id && n.key == a.key && covers(n));
            match found {
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
        self.release_ended();

        // While playing, start edited notes that now cover the playhead but
        // aren't sounding: one dragged back under it, or a new one drawn
        // there. A voice is one (track, key), as on an instrument, so a key
        // that is already sounding isn't started twice. Paused, Resume picks
        // up at `started` like any seek.
        if self.playing {
            for n in now.iter().filter(|n| covers(n)) {
                let sounding = self
                    .active
                    .iter()
                    .any(|a| a.track_id == n.track_id && a.key == n.key);
                // Only start what can be tracked, so every note-on gets its
                // note-off.
                if sounding || self.active.len() == self.active.capacity() {
                    continue;
                }
                let route = self
                    .routes
                    .get(n.track as usize)
                    .copied()
                    .unwrap_or_default();
                if self.slot(route).is_none() {
                    continue;
                }
                self.strike_at(route, n.key, n.velocity, Some(pos));
                self.active.push(ActiveNote {
                    track_id: n.track_id,
                    route,
                    key: n.key,
                    end_frame: frame(n.end).max(pos + 1),
                    struck_at: pos,
                });
            }
        }

        self.next_note = started;
        self.duration_frames = (timeline.duration * sr).ceil() as u64;
        self.shared
            .duration_frames
            .store(self.duration_frames, Ordering::Relaxed);
        if let Some(old) = self.timeline.replace(timeline) {
            self.retire(Retired::Timeline(old));
        }
    }

    /// Send a replaced timeline or route list to the UI thread to be freed. If the
    /// channel is full, hold it (and any earlier ones) until it has room, so the audio
    /// thread never frees one.
    fn retire(&mut self, old: Retired) {
        while let Some(t) = self.retired.pop() {
            if let Err(e) = self.trash.try_send(t) {
                self.retired.push(e.into_inner());
                break;
            }
        }
        if let Err(e) = self.trash.try_send(old) {
            let old = e.into_inner();
            if self.retired.len() < self.retired.capacity() {
                self.retired.push(old);
            } else {
                // `drain` defers retiring commands until there is room, so the
                // callback never gets here; only a direct `apply` (tests) can.
                drop(old);
            }
        }
    }

    fn reset_to_start(&mut self) {
        self.pos_frames = 0;
        self.next_note = 0;
        self.release_playback();
        self.shared.position_frames.store(0, Ordering::Release);
    }

    /// Apply queued commands. A command that hands a timeline or route list back
    /// for freeing waits (with everything queued after it, so order is kept) while
    /// the retirement storage is full, rather than be applied and freed here.
    fn drain(&mut self, rx: &mut CommandRx) {
        // Hand held payloads back on every call, even with nothing queued, so the
        // reserve empties as soon as the UI makes room.
        let room = self.has_retire_room();
        if let Some(command) = self.deferred.take() {
            if !room {
                self.deferred = Some(command);
                return;
            }
            self.apply(command);
        }
        while let Some(command) = rx.try_recv() {
            let retires = matches!(
                command,
                Command::Play { .. }
                    | Command::TogglePlay { .. }
                    | Command::Update { .. }
                    | Command::SetRoutes(_)
                    | Command::SetRoutesAndInput { .. }
            );
            if retires && !self.has_retire_room() {
                self.deferred = Some(command);
                return;
            }
            self.apply(command);
        }
    }

    /// Whether one more replaced payload can be retired without freeing it here.
    fn has_retire_room(&mut self) -> bool {
        while let Some(t) = self.retired.pop() {
            if let Err(e) = self.trash.try_send(t) {
                self.retired.push(e.into_inner());
                break;
            }
        }
        self.retired.len() < self.retired.capacity()
    }

    /// Fill a device buffer of `channels` interleaved channels. `render` stops at
    /// MAX_BLOCK, so a larger buffer is rendered in pieces; the meters then report
    /// the loudest of all pieces, not just the last.
    fn fill_output(&mut self, data: &mut [f32], channels: usize) {
        let channels = channels.max(1);
        let frames = data.len() / channels;
        let mut peak = 0.0f32;
        let mut slot_peaks = [0.0f32; MAX_SLOTS];
        let mut rendered = 0usize;
        while rendered < frames {
            let piece = (frames - rendered).min(MAX_BLOCK);
            let (p, slots) = self.render_piece(piece);
            peak = peak.max(p);
            for (max, s) in slot_peaks.iter_mut().zip(slots) {
                *max = max.max(s);
            }
            for frame in 0..piece {
                let l = self.scratch[frame * 2];
                let r = self.scratch[frame * 2 + 1];
                let at = rendered + frame;
                if channels == 1 {
                    data[at] = (l + r) * 0.5;
                } else {
                    data[at * channels] = l;
                    data[at * channels + 1] = r;
                    for ch in 2..channels {
                        data[at * channels + ch] = (l + r) * 0.5;
                    }
                }
            }
            rendered += piece;
        }
        self.publish(peak, slot_peaks);
    }

    /// Render interleaved stereo frames into `scratch` and return that slice.
    fn render(&mut self, frames: usize) -> &[f32] {
        let frames = frames.min(MAX_BLOCK);
        let (peak, slot_peaks) = self.render_piece(frames);
        self.publish(peak, slot_peaks);
        &self.scratch[..frames * 2]
    }

    fn publish(&self, peak: f32, slot_peaks: [f32; MAX_SLOTS]) {
        self.shared
            .position_frames
            .store(self.pos_frames, Ordering::Release);
        self.shared.peak.store(peak.to_bits(), Ordering::Relaxed);
        for (cell, p) in self.shared.slot_peaks.iter().zip(slot_peaks) {
            cell.store(p.to_bits(), Ordering::Relaxed);
        }
    }

    /// Render up to MAX_BLOCK frames into `scratch`; returns the master and slot
    /// peaks without publishing them.
    fn render_piece(&mut self, frames: usize) -> (f32, [f32; MAX_SLOTS]) {
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
                        self.release_playback();
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
        (peak, slot_peaks)
    }

    fn trigger(&mut self, start: u64, end: u64) {
        if self.timeline.is_none() {
            return;
        }

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
        self.release_ended();

        loop {
            let Some(timeline) = &self.timeline else {
                return;
            };
            let Some(note) = timeline.notes.get(self.next_note) else {
                return;
            };
            let note = *note;
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
            // At the cap, drop the note rather than grow `active` here.
            if self.active.len() == self.active.capacity() {
                continue;
            }
            if self.slot(route).is_none() {
                continue;
            }
            self.strike_at(route, note.key, note.velocity, Some(start));
            let note_end = ((note.end * self.sample_rate) as u64).max(note_start + 1);
            self.active.push(ActiveNote {
                track_id: note.track_id,
                route,
                key: note.key,
                end_frame: note_end,
                struck_at: note_start,
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
    tx: CommandTx,
    _stream: Option<cpal::Stream>,
    _null_clock: Option<NullClock>,
    sample_rate: u32,
    output: parking_lot::Mutex<String>,
    sample_accurate: bool,
    events: crossbeam_channel::Receiver<OutputEvent>,
    trash: crossbeam_channel::Receiver<Retired>,
    /// How many `TogglePlay` / `StopOrRewind` were sent (see `transport_done`).
    transport_sent: AtomicU64,
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
            transport_done: AtomicU64::new(0),
        });
        let (event_tx, events) = crossbeam_channel::unbounded();
        let (tx, mut rx) = command_channel(COMMAND_CAP);
        let (trash_tx, trash) = crossbeam_channel::bounded::<Retired>(8);

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
                            core.drain(&mut rx);
                            core.fill_output(data, channels as usize);
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
                    transport_sent: AtomicU64::new(0),
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
                            thread_core.lock().drain(&mut rx);
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
                    transport_sent: AtomicU64::new(0),
                }
            }
        }
    }

    /// Try to open a real device again after starting in silent mode.
    pub fn is_silent(&self) -> bool {
        self._stream.is_none()
    }

    pub fn status(&self) -> EngineStatus {
        // The UI reads this every frame, so retired payloads are freed even when it
        // sends nothing, and a command the callback held back for room goes through.
        self.reclaim();
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
    /// thread without going through the UI at 60 fps. Sending never blocks.
    pub fn sender(&self) -> CommandTx {
        self.tx.clone()
    }

    /// Free the timelines and route lists the audio thread swapped out.
    fn reclaim(&self) {
        while self.trash.try_recv().is_ok() {}
    }

    pub fn send(&self, command: Command) {
        self.reclaim();
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

    pub fn typing_sustain(&self, down: bool) {
        self.send(Command::TypingSustain(down));
    }

    pub fn play(&self, timeline: Timeline, from: f64) {
        self.send(Command::Play { timeline, from });
    }

    /// Play / Pause; see [`Command::TogglePlay`]. Returns a ticket for
    /// [`transport_applied`](Self::transport_applied).
    pub fn toggle_play(&self, timeline: Timeline) -> u64 {
        self.send(Command::TogglePlay { timeline });
        self.transport_sent.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Stop, or go back to the start if stopped; see [`Command::StopOrRewind`].
    /// Returns a ticket for [`transport_applied`](Self::transport_applied).
    pub fn stop_or_rewind(&self) -> u64 {
        self.send(Command::StopOrRewind);
        self.transport_sent.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Whether the transport command with this ticket (and every one before
    /// it) has been applied, so [`status`](Self::status) shows its result.
    pub fn transport_applied(&self, ticket: u64) -> bool {
        self.shared.transport_done.load(Ordering::Acquire) >= ticket
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

    /// Stop, keeping the pedals that are down (see [`Command::StopKeepingPedals`]).
    pub fn stop_keeping_pedals(&self) {
        self.send(Command::StopKeepingPedals);
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
        let command = Command::SetSlot {
            slot,
            instrument,
            ack: Some(ack_tx),
        };
        if self.tx.send(command).is_err() {
            return; // The audio thread is gone.
        }
        // Keep freeing retired payloads while waiting: the callback may be holding
        // a retiring command (and this one behind it) until there is room.
        let deadline = Instant::now() + Duration::from_millis(750);
        while Instant::now() < deadline {
            self.reclaim();
            match ack_rx.recv_timeout(Duration::from_millis(5)) {
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                // Acknowledged, or dropped unapplied with the audio thread.
                _ => return,
            }
        }
    }
}

const HW_PEDAL: u8 = 1;
const TYPING_PEDAL: u8 = 2;

/// Commands the bounded queue holds. The callback drains it every block, so it
/// only fills if the device stops calling back.
const COMMAND_CAP: usize = 1024;

/// The audio thread's command queue, as the senders see it.
///
/// Commands go to a bounded queue, so in normal use the audio callback never
/// calls the allocator (crossbeam's unbounded channel frees a block about every
/// 31 messages on the receiving side). Nothing is ever dropped or blocks the
/// sender, though: losing a note-off, pedal-up, `Stop` or `SetSlot` would leave
/// state stuck. When the bounded queue is full, commands go to an unbounded
/// overflow queue instead, and keep going there until the audio thread has
/// taken them all, so each sender's commands stay in order. Only that rare case
/// (a stalled device) touches the allocator on the audio side.
#[derive(Clone)]
pub struct CommandTx {
    fast: crossbeam_channel::Sender<(u64, Command)>,
    overflow: crossbeam_channel::Sender<(u64, Command)>,
    pending: Arc<AtomicUsize>,
    /// Numbers commands as they are sent, so the receiver can merge the two
    /// queues back into each sender's order.
    seq: Arc<AtomicU64>,
}

/// The audio thread went away; the command was not queued.
#[derive(Debug)]
pub struct Disconnected;

impl CommandTx {
    /// Queue `command`. Never blocks and never drops it.
    pub fn send(&self, command: Command) -> Result<(), Disconnected> {
        let item = (self.seq.fetch_add(1, Ordering::AcqRel), command);
        let item = if self.pending.load(Ordering::Acquire) == 0 {
            match self.fast.try_send(item) {
                Ok(()) => return Ok(()),
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => return Err(Disconnected),
                Err(crossbeam_channel::TrySendError::Full(item)) => item,
            }
        } else {
            item
        };
        // Count it before it can be received, so later sends follow it here.
        self.pending.fetch_add(1, Ordering::AcqRel);
        self.overflow.send(item).map_err(|_| {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            Disconnected
        })
    }
}

/// The audio thread's end of [`CommandTx`].
///
/// It holds the next command of each queue and hands out the lower-numbered one.
/// A sender's commands are numbered in the order it sends them, and one that went
/// to the bounded queue before another went to overflow was in the bounded queue
/// before that one could be received. So after taking from overflow the receiver
/// looks at the bounded queue again before choosing: an older command from the
/// same sender cannot be passed over, however the threads interleave.
pub(crate) struct CommandRx {
    fast: crossbeam_channel::Receiver<(u64, Command)>,
    overflow: crossbeam_channel::Receiver<(u64, Command)>,
    pending: Arc<AtomicUsize>,
    next_fast: Option<(u64, Command)>,
    next_overflow: Option<(u64, Command)>,
}

impl CommandRx {
    fn pull_overflow(&mut self) {
        if self.next_overflow.is_none() {
            self.next_overflow = self.overflow.try_recv().ok();
        }
    }

    fn pull_fast(&mut self) {
        if self.next_fast.is_none() {
            self.next_fast = self.fast.try_recv().ok();
        }
    }

    /// The lower-numbered of the held commands.
    fn pop(&mut self) -> Option<Command> {
        let take_fast = match (&self.next_fast, &self.next_overflow) {
            (Some(f), Some(o)) => f.0 < o.0,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => return None,
        };
        if take_fast {
            self.next_fast.take().map(|(_, c)| c)
        } else {
            self.pending.fetch_sub(1, Ordering::AcqRel);
            self.next_overflow.take().map(|(_, c)| c)
        }
    }

    /// The next command, in each sender's order. Overflow first, then the bounded
    /// queue: see the type's docs for why this order matters.
    fn try_recv(&mut self) -> Option<Command> {
        self.pull_overflow();
        self.pull_fast();
        self.pop()
    }

    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.fast.is_empty()
            && self.overflow.is_empty()
            && self.next_fast.is_none()
            && self.next_overflow.is_none()
    }
}

fn command_channel(cap: usize) -> (CommandTx, CommandRx) {
    let (fast_tx, fast) = crossbeam_channel::bounded(cap);
    let (overflow_tx, overflow) = crossbeam_channel::unbounded();
    let pending = Arc::new(AtomicUsize::new(0));
    (
        CommandTx {
            fast: fast_tx,
            overflow: overflow_tx,
            pending: pending.clone(),
            seq: Arc::new(AtomicU64::new(0)),
        },
        CommandRx {
            fast,
            overflow,
            pending,
            next_fast: None,
            next_overflow: None,
        },
    )
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
            transport_done: AtomicU64::new(0),
        });
        // Tests keep the receiver alive so swapped timelines queue up.
        let (trash, keep) = crossbeam_channel::bounded::<Retired>(8);
        std::mem::forget(keep);
        Core::new(48_000, shared, trash)
    }

    fn note(track: u16, key: u8, start: f64) -> TimedNote {
        TimedNote {
            key,
            velocity: 100,
            channel: 0,
            track,
            track_id: 100 + track as u64,
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
            Route {
                slot: 1,
                channel: 0,
            },
            Route {
                slot: 2,
                channel: 1,
            },
            Route {
                slot: 2,
                channel: 2,
            },
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
        c.apply(Command::SetSlot {
            slot: 0,
            instrument: Some(Box::new(Probe(a.clone()))),
            ack: None,
        });
        c.apply(Command::SetSlot {
            slot: 1,
            instrument: Some(Box::new(Probe(b.clone()))),
            ack: None,
        });
        c.apply(Command::SetRoutes(vec![
            Route {
                slot: 0,
                channel: 0,
            },
            Route {
                slot: 1,
                channel: 4,
            },
        ]));
        c.apply(Command::SetInput(1));
        c.apply(Command::NoteOn {
            key: 60,
            velocity: 90,
        });
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
            track_id: 100 + track as u64,
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
            Route {
                slot: 1,
                channel: 0,
            },
            Route {
                slot: 1,
                channel: 1,
            },
        ]));
        (c, log)
    }

    /// Every call an instrument gets, in order.
    type Calls = Arc<parking_lot::Mutex<Vec<String>>>;

    struct Recorder(Calls);

    impl Instrument for Recorder {
        fn note_on(&mut self, _: u8, key: u8, _: u8) {
            self.0.lock().push(format!("on {key}"));
        }
        fn note_off(&mut self, _: u8, key: u8) {
            self.0.lock().push(format!("off {key}"));
        }
        fn all_notes_off(&mut self) {
            self.0.lock().push("all off".into());
        }
        fn set_sustain(&mut self, _: u8, down: bool) {
            self.0.lock().push(format!("pedal {down}"));
        }
        fn process(&mut self, out: &mut [f32], frames: usize) {
            out[..frames * 2].fill(0.0);
        }
        fn name(&self) -> &str {
            "recorder"
        }
    }

    fn recorder_core() -> (Core, Calls) {
        let mut c = core();
        let calls: Calls = Default::default();
        c.apply(Command::SetSlot {
            slot: 1,
            instrument: Some(Box::new(Recorder(calls.clone()))),
            ack: None,
        });
        c.apply(Command::SetRoutes(vec![Route {
            slot: 1,
            channel: 0,
        }]));
        calls.lock().clear();
        (c, calls)
    }

    fn secs(c: &mut Core, s: f64) {
        run(c, (s * 48_000.0) as usize);
    }

    /// Review (M24): Stop and Play are decided in command order on the audio
    /// thread, so quick sequences sent before the thread runs still do the
    /// right thing.
    #[test]
    fn stop_and_play_follow_the_commands_before_them() {
        let (mut c, _) = recorder_core();
        let tl = || Timeline {
            notes: vec![long(0, 60, 0.0, 5.0)],
            duration: 10.0,
        };
        c.apply(Command::TogglePlay { timeline: tl() });
        assert!(c.playing, "stopped at 0: play");
        secs(&mut c, 1.0);
        let at = c.pos_frames;
        // Stop, Stop with nothing in between: pause in place, then the start.
        c.apply(Command::StopOrRewind);
        assert!(!c.playing);
        assert_eq!(c.pos_frames, at, "first Stop stays put");
        c.apply(Command::StopOrRewind);
        assert_eq!(c.pos_frames, 0, "second Stop goes to the start");
        // Play, Stop, Play: resume where the Stop left it.
        c.apply(Command::TogglePlay { timeline: tl() });
        secs(&mut c, 1.0);
        let at = c.pos_frames;
        c.apply(Command::StopOrRewind);
        c.apply(Command::TogglePlay { timeline: tl() });
        assert!(c.playing, "Stop then Play plays");
        assert_eq!(c.pos_frames, at, "resumed in place");
        c.apply(Command::TogglePlay { timeline: tl() });
        assert!(!c.playing, "Play again pauses");
    }

    /// Review M6: two overlapping playback notes on one key. The first ending
    /// must not cut the second; the key goes off once, when the last one ends.
    #[test]
    fn overlapping_notes_on_a_key_end_with_the_last() {
        let (mut c, calls) = recorder_core();
        c.apply(Command::Play {
            timeline: Timeline {
                notes: vec![long(0, 60, 0.0, 1.0), long(0, 60, 0.5, 2.0)],
                duration: 3.0,
            },
            from: 0.0,
        });
        secs(&mut c, 1.5);
        // One voice per key: the re-strike ends the first, so every note-on is
        // matched by one note-off, and the first ending at 1 s cut nothing.
        assert_eq!(*calls.lock(), ["on 60", "off 60", "on 60"]);
        secs(&mut c, 1.0);
        assert_eq!(*calls.lock(), ["on 60", "off 60", "on 60", "off 60"]);
    }

    /// Review M6: playback ending a note must not cut the same key the player is
    /// holding, and the player letting go must not cut playback's note.
    #[test]
    fn playback_and_the_player_do_not_cut_each_other() {
        let (mut c, calls) = recorder_core();
        c.apply(Command::NoteOn {
            key: 60,
            velocity: 90,
        });
        c.apply(Command::Play {
            timeline: Timeline {
                notes: vec![long(0, 60, 0.0, 0.5), long(0, 62, 0.0, 2.0)],
                duration: 3.0,
            },
            from: 0.0,
        });
        secs(&mut c, 1.0);
        let count = |e: &str| calls.lock().iter().filter(|x| *x == e).count();
        // Playback's strike ended the player's voice first (one voice per key);
        // playback's own end at 0.5 s did not cut the key the player still holds.
        assert_eq!(count("off 60"), 1, "{:?}", calls.lock());
        c.apply(Command::NoteOff { key: 60 });
        assert_eq!(calls.lock().last().map(String::as_str), Some("off 60"));
        assert_eq!(count("on 60"), count("off 60"), "balanced");

        // The other way round: the player lets go while playback still sounds it.
        calls.lock().clear();
        c.apply(Command::NoteOn {
            key: 62,
            velocity: 90,
        });
        c.apply(Command::NoteOff { key: 62 });
        // The press re-struck playback's voice (off, on); letting go kept it.
        assert_eq!(*calls.lock(), ["off 62", "on 62"]);
        secs(&mut c, 1.5);
        assert_eq!(*calls.lock(), ["off 62", "on 62", "off 62"]);
    }

    /// Review: many identical starts on one key in one block share one voice, so
    /// they send one note-on (not an off/on pair each, which would overflow a
    /// VST2 plugin's 512-event batch) and one note-off when the last one ends.
    #[test]
    fn identical_starts_in_a_block_share_one_voice() {
        let (mut c, calls) = recorder_core();
        let notes: Vec<TimedNote> = (0..300)
            .map(|i| long(0, 60, 0.0, 0.5 + i as f64 * 0.001))
            .collect();
        c.apply(Command::Play {
            timeline: Timeline {
                notes,
                duration: 2.0,
            },
            from: 0.0,
        });
        secs(&mut c, 0.1);
        assert_eq!(*calls.lock(), ["on 60"]);
        assert_eq!(c.active.len(), 300, "every note is tracked");
        secs(&mut c, 1.5);
        assert_eq!(*calls.lock(), ["on 60", "off 60"]);
    }

    /// Review: after AllNotesOff, a silenced playback note no longer owns its key,
    /// so the player pressing and releasing that key does end it.
    #[test]
    fn all_notes_off_clears_playback_ownership() {
        let (mut c, calls) = recorder_core();
        c.apply(Command::Play {
            timeline: Timeline {
                notes: vec![long(0, 60, 0.0, 5.0)],
                duration: 6.0,
            },
            from: 0.0,
        });
        secs(&mut c, 0.5);
        c.apply(Command::AllNotesOff);
        calls.lock().clear();
        c.apply(Command::NoteOn {
            key: 60,
            velocity: 90,
        });
        c.apply(Command::NoteOff { key: 60 });
        assert_eq!(*calls.lock(), ["on 60", "off 60"]);
    }

    /// Review M4: looping back to the start, seeking or pausing must not drop the
    /// sustain pedal or a key the player holds.
    #[test]
    fn transport_moves_keep_the_players_pedal_and_keys() {
        let (mut c, calls) = recorder_core();
        c.apply(Command::SetLoop(true));
        c.apply(Command::Sustain(true));
        c.apply(Command::NoteOn {
            key: 50,
            velocity: 90,
        });
        c.apply(Command::Play {
            timeline: Timeline {
                notes: vec![long(0, 72, 0.0, 0.2)],
                duration: 0.5,
            },
            from: 0.0,
        });
        secs(&mut c, 1.2); // wraps twice
        c.apply(Command::Seek(0.1));
        c.apply(Command::Pause);
        let calls = calls.lock().clone();
        assert!(
            !calls
                .iter()
                .any(|e| e == "all off" || e == "pedal false" || e == "off 50"),
            "{calls:?}"
        );
        assert!(c.sustain(), "pedal still down");
        assert!(c.held[50].is_some(), "key still held");
        assert!(
            calls.iter().filter(|e| *e == "on 72").count() >= 2,
            "playback looped: {calls:?}"
        );
    }

    /// Review: the hardware pedal and Musical Typing's are held together; the
    /// instrument's pedal lifts only once both are up, whichever goes first.
    #[test]
    fn hardware_and_typing_pedals_combine() {
        let (mut c, calls) = recorder_core();
        c.apply(Command::TypingSustain(true));
        c.apply(Command::Sustain(true));
        c.apply(Command::TypingSustain(false));
        assert_eq!(*calls.lock(), ["pedal true"], "Tab up kept the pedal");
        c.apply(Command::Sustain(false));
        assert_eq!(*calls.lock(), ["pedal true", "pedal false"]);
        calls.lock().clear();
        c.apply(Command::Sustain(true));
        c.apply(Command::TypingSustain(true));
        c.apply(Command::Sustain(false));
        assert_eq!(*calls.lock(), ["pedal true"], "pedal up kept Tab's");
        c.apply(Command::TypingSustain(false));
        assert_eq!(*calls.lock(), ["pedal true", "pedal false"]);
    }

    /// Review: stopping around a take keeps the pedals held then, by the
    /// engine's own state: a pedal-up queued just before is not undone.
    #[test]
    fn stop_keeping_pedals_uses_the_engines_pedals() {
        let (mut c, calls) = recorder_core();
        c.apply(Command::TypingSustain(true));
        c.apply(Command::StopKeepingPedals);
        assert!(c.sustain(), "Tab still held");
        assert_eq!(
            *calls.lock(),
            ["pedal true", "all off", "pedal true"],
            "the instrument's pedal is put back"
        );
        calls.lock().clear();
        c.apply(Command::TypingSustain(false));
        c.apply(Command::Sustain(true));
        c.apply(Command::Sustain(false));
        c.apply(Command::StopKeepingPedals);
        assert!(!c.sustain(), "released before the stop: stays up");
        assert!(!calls.lock().iter().skip(3).any(|e| e == "pedal true"));
    }

    /// Review: a pedal held while the input's instrument is replaced (a project
    /// reopened, a plugin reloaded) holds the new instrument too.
    #[test]
    fn a_held_pedal_reaches_a_replaced_instrument() {
        let (mut c, calls) = recorder_core();
        c.apply(Command::Sustain(true));
        let fresh: Calls = Default::default();
        c.apply(Command::SetSlot {
            slot: 1,
            instrument: Some(Box::new(Recorder(fresh.clone()))),
            ack: None,
        });
        assert_eq!(*fresh.lock(), ["pedal true"]);
        assert!(
            calls.lock().contains(&"all off".to_string()),
            "old one silenced"
        );
        // Routes that move the input to another channel take the pedal along.
        fresh.lock().clear();
        c.apply(Command::SetRoutes(vec![Route {
            slot: 1,
            channel: 3,
        }]));
        assert_eq!(*fresh.lock(), ["pedal false", "pedal true"]);
    }

    /// Review: deleting an earlier track shifts both the routes and the input
    /// index. Applied together, the instrument the input stays on keeps its
    /// pedal: no up-down blip that would release a sustained note.
    #[test]
    fn routes_and_input_move_together() {
        let (mut c, calls) = recorder_core();
        c.apply(Command::SetRoutes(vec![
            Route {
                slot: 0,
                channel: 0,
            },
            Route {
                slot: 1,
                channel: 0,
            },
        ]));
        c.apply(Command::SetInput(1));
        c.apply(Command::Sustain(true));
        calls.lock().clear();
        // Track 0 deleted: track 1 (slot 1) is now index 0.
        c.apply(Command::SetRoutesAndInput {
            routes: vec![Route {
                slot: 1,
                channel: 0,
            }],
            input: 0,
        });
        assert!(calls.lock().is_empty(), "{:?}", calls.lock());
    }

    /// Stop still resets everything, pedal included.
    #[test]
    fn stop_resets_the_pedal() {
        let (mut c, calls) = recorder_core();
        c.apply(Command::Sustain(true));
        c.apply(Command::Stop);
        assert!(!c.sustain());
        assert!(calls.lock().contains(&"all off".to_string()));
    }

    /// Review M3: the queue is bounded, but a full one never drops or blocks.
    /// Overflow keeps each sender's order, and later sends follow the overflow
    /// until the audio thread has caught up.
    #[test]
    fn a_full_command_queue_keeps_every_command_in_order() {
        let (tx, mut rx) = command_channel(2);
        for v in 0..6 {
            tx.send(Command::SetVolume(v as f32 / 10.0)).unwrap();
        }
        // The audio thread takes one; the next send must still queue behind the
        // overflow, not jump ahead into the freed bounded slot.
        let first = rx.try_recv();
        tx.send(Command::SetVolume(0.6)).unwrap();
        let mut got: Vec<f32> = first
            .into_iter()
            .chain(std::iter::from_fn(|| rx.try_recv()))
            .map(|c| match c {
                Command::SetVolume(v) => v,
                _ => unreachable!(),
            })
            .collect();
        got.iter_mut().for_each(|v| *v = (*v * 10.0).round());
        assert_eq!(got, [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert!(rx.is_empty());
        // Caught up: the bounded queue is used again.
        tx.send(Command::Pause).unwrap();
        assert_eq!(tx.pending.load(Ordering::Acquire), 0);
        assert!(rx.fast.len() == 1);
    }

    /// Review: the receiver pauses right after finding the bounded queue empty.
    /// Meanwhile a sender puts a NoteOn in the bounded queue, fills it, and the
    /// matching NoteOff goes to overflow. The NoteOn must still come out first.
    #[test]
    fn a_receiver_paused_between_queues_keeps_order() {
        let (tx, mut rx) = command_channel(2);
        rx.pull_fast(); // empty: the receiver pauses here
        tx.send(Command::NoteOn {
            key: 60,
            velocity: 90,
        })
        .unwrap();
        tx.send(Command::SetVolume(0.5)).unwrap();
        tx.send(Command::NoteOff { key: 60 }).unwrap(); // full: overflow
        let order: Vec<&str> = std::iter::from_fn(|| rx.try_recv())
            .map(|c| match c {
                Command::NoteOn { .. } => "on",
                Command::SetVolume(_) => "volume",
                Command::NoteOff { .. } => "off",
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(order, ["on", "volume", "off"]);
        assert!(rx.is_empty());
        assert_eq!(rx.pending.load(Ordering::Acquire), 0);
    }

    /// Review: a note-off sent while the queue is full is not lost, so a held key
    /// does not stay stuck once the device calls back again.
    #[test]
    fn a_release_sent_into_a_full_queue_still_arrives() {
        let (mut c, calls) = recorder_core();
        let (tx, mut rx) = command_channel(1);
        tx.send(Command::NoteOn {
            key: 60,
            velocity: 90,
        })
        .unwrap();
        for _ in 0..10 {
            tx.send(Command::SetVolume(0.5)).unwrap();
        }
        tx.send(Command::NoteOff { key: 60 }).unwrap();
        c.drain(&mut rx);
        assert_eq!(*calls.lock(), ["on 60", "off 60"]);
        assert!(c.held[60].is_none());
    }

    /// Shortening a sounding note ends it at its new end, not the old one.
    #[test]
    fn a_shortened_note_ends_on_time() {
        let (mut c, log) = probe_core();
        c.apply(Command::Play {
            timeline: Timeline {
                notes: vec![long(0, 60, 0.0, 2.0)],
                duration: 2.0,
            },
            from: 0.0,
        });
        c.render(4800); // 0.1 s
        c.apply(Command::Update {
            timeline: Timeline {
                notes: vec![long(0, 60, 0.0, 0.2)],
                duration: 2.0,
            },
        });
        assert!(
            log.lock().iter().all(|e| e.2),
            "still sounding after the edit"
        );
        c.render(9600); // to 0.3 s
        assert_eq!(log.lock().last(), Some(&(0, 60, false)), "ended near 0.2 s");
    }

    /// While paused an update just takes effect for Resume.
    #[test]
    fn an_update_while_paused_plays_on_resume() {
        let (mut c, log) = probe_core();
        c.apply(Command::Play {
            timeline: Timeline {
                notes: vec![long(0, 60, 0.5, 0.6)],
                duration: 1.0,
            },
            from: 0.0,
        });
        c.render(4800);
        c.apply(Command::Pause);
        c.apply(Command::Update {
            timeline: Timeline {
                notes: vec![long(0, 72, 0.5, 0.6)],
                duration: 1.0,
            },
        });
        c.apply(Command::Resume);
        run(&mut c, 48_000);
        let ons: Vec<u8> = log.lock().iter().filter(|e| e.2).map(|e| e.1).collect();
        assert_eq!(ons, vec![72]);
    }

    /// Review: deleting a track shifts the others' indices. Their sounding
    /// notes must keep sounding, since they're matched by stable id.
    #[test]
    fn deleting_a_track_keeps_the_others_sounding() {
        let (mut c, log) = probe_core();
        // Track 0 (id 100) is empty; track 1 (id 101) holds a chord.
        let a = long(1, 60, 0.0, 1.0);
        let b = long(1, 64, 0.0, 1.0);
        c.apply(Command::Play {
            timeline: Timeline {
                notes: vec![a, b],
                duration: 1.0,
            },
            from: 0.0,
        });
        run(&mut c, 4800);
        log.lock().clear();
        // Delete track 0: track 1 becomes index 0, same id.
        let shift = |mut n: TimedNote| {
            n.track = 0;
            n
        };
        c.apply(Command::SetRoutes(vec![Route {
            slot: 1,
            channel: 1,
        }]));
        c.apply(Command::Update {
            timeline: Timeline {
                notes: vec![shift(a), shift(b)],
                duration: 1.0,
            },
        });
        assert!(log.lock().is_empty(), "no note-offs: {:?}", log.lock());
    }

    /// Review: a note dragged back under the playhead sounds again, and a
    /// note drawn under it starts right away.
    #[test]
    fn a_note_moved_under_the_playhead_starts_sounding() {
        let (mut c, log) = probe_core();
        let held = long(0, 60, 0.0, 1.0);
        c.apply(Command::Play {
            timeline: Timeline {
                notes: vec![held],
                duration: 2.0,
            },
            from: 0.0,
        });
        run(&mut c, 4800); // 0.1 s
        // Drag it ahead, off the playhead: it stops.
        c.apply(Command::Update {
            timeline: Timeline {
                notes: vec![long(0, 60, 0.5, 1.5)],
                duration: 2.0,
            },
        });
        assert_eq!(log.lock().last(), Some(&(0, 60, false)));
        log.lock().clear();
        // Drag it back under the playhead: it sounds again, once.
        c.apply(Command::Update {
            timeline: Timeline {
                notes: vec![held],
                duration: 2.0,
            },
        });
        assert_eq!(*log.lock(), vec![(0, 60, true)]);
        // Another update with nothing changed doesn't retrigger it.
        c.apply(Command::Update {
            timeline: Timeline {
                notes: vec![held],
                duration: 2.0,
            },
        });
        assert_eq!(log.lock().len(), 1, "no retrigger: {:?}", log.lock());
    }

    /// Review: with the UI not draining, replaced timelines are held, not
    /// freed on the audio thread; they go out once the channel has room.
    #[test]
    fn replaced_timelines_wait_when_the_channel_is_full() {
        let shared = Arc::new(Shared {
            position_frames: AtomicU64::new(0),
            duration_frames: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            peak: AtomicU32::new(0),
            slot_peaks: std::array::from_fn(|_| AtomicU32::new(0)),
            sample_rate: AtomicU32::new(48_000),
            glitches: AtomicU32::new(0),
            transport_done: AtomicU64::new(0),
        });
        let (trash, ui) = crossbeam_channel::bounded::<Retired>(2);
        let mut c = Core::new(48_000, shared, trash);
        let tl = || Timeline {
            notes: vec![long(0, 60, 0.0, 1.0)],
            duration: 1.0,
        };
        c.apply(Command::Play {
            timeline: tl(),
            from: 0.0,
        });
        for _ in 0..5 {
            c.apply(Command::Update { timeline: tl() });
        }
        assert_eq!(ui.len(), 2, "channel full");
        assert_eq!(c.retired.len(), 3, "the rest held, not freed");
        while ui.try_recv().is_ok() {}
        c.apply(Command::Update { timeline: tl() });
        assert_eq!(ui.len(), 2, "held ones go out once there's room");
        assert_eq!(c.retired.len(), 2);
    }

    /// Review: with the trash channel and the reserve both full, a command that
    /// would retire a payload waits in the queue instead of freeing it in the
    /// callback, and commands behind it keep their order.
    #[test]
    fn retiring_commands_wait_when_retirement_is_full() {
        let shared = Arc::new(Shared {
            position_frames: AtomicU64::new(0),
            duration_frames: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            peak: AtomicU32::new(0),
            slot_peaks: std::array::from_fn(|_| AtomicU32::new(0)),
            sample_rate: AtomicU32::new(48_000),
            glitches: AtomicU32::new(0),
            transport_done: AtomicU64::new(0),
        });
        let (trash, ui) = crossbeam_channel::bounded::<Retired>(2);
        let mut c = Core::new(48_000, shared, trash);
        let (tx, mut rx) = command_channel(COMMAND_CAP);
        let tl = || Timeline {
            notes: vec![long(0, 60, 0.0, 1.0)],
            duration: 1.0,
        };
        let total = 2 + RETIRED_CAP + 5;
        for _ in 0..total {
            tx.send(Command::Update { timeline: tl() }).unwrap();
        }
        tx.send(Command::SetVolume(0.25)).unwrap();
        c.drain(&mut rx);
        assert_eq!(ui.len(), 2);
        assert_eq!(
            c.retired.len(),
            RETIRED_CAP,
            "the reserve fills, nothing is dropped"
        );
        assert!(c.deferred.is_some(), "the next retiring command waits");
        assert_eq!(c.volume, 0.9, "commands behind it wait too");
        // The UI drains; everything goes through in order and nothing was freed here.
        // Count only what the UI received: the core keeps nothing once it is idle.
        let mut received = 0;
        for _ in 0..total + RETIRED_CAP {
            while ui.try_recv().is_ok() {
                received += 1;
            }
            c.drain(&mut rx);
        }
        assert!(c.deferred.is_none());
        assert_eq!(c.volume, 0.25);
        assert!(c.retired.is_empty(), "idle drains flush the reserve");
        // Every replaced timeline came back: one per update but the live one.
        assert_eq!(received, total - 1);
    }

    /// Review: a burst that fills retirement, then `Stop` and nothing else sent.
    /// With the UI only freeing what comes back (as `status` does every frame),
    /// the held updates and the `Stop` behind them all go through.
    #[test]
    fn stop_after_a_burst_goes_through_without_more_sends() {
        let shared = Arc::new(Shared {
            position_frames: AtomicU64::new(0),
            duration_frames: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            peak: AtomicU32::new(0),
            slot_peaks: std::array::from_fn(|_| AtomicU32::new(0)),
            sample_rate: AtomicU32::new(48_000),
            glitches: AtomicU32::new(0),
            transport_done: AtomicU64::new(0),
        });
        let (trash, ui) = crossbeam_channel::bounded::<Retired>(2);
        let mut c = Core::new(48_000, shared, trash);
        let (tx, mut rx) = command_channel(COMMAND_CAP);
        let tl = || Timeline {
            notes: vec![long(0, 60, 0.0, 1.0)],
            duration: 1.0,
        };
        tx.send(Command::Play {
            timeline: tl(),
            from: 0.0,
        })
        .unwrap();
        for _ in 0..2 + RETIRED_CAP + 20 {
            tx.send(Command::Update { timeline: tl() }).unwrap();
        }
        tx.send(Command::Stop).unwrap();
        c.drain(&mut rx);
        assert!(c.playing, "Stop is still queued behind the held update");
        // Callback blocks and UI frames alternate; the UI sends nothing more.
        for _ in 0..200 {
            while ui.try_recv().is_ok() {}
            c.drain(&mut rx);
            run(&mut c, 256);
            if !c.playing && c.deferred.is_none() && rx.is_empty() {
                break;
            }
        }
        assert!(c.deferred.is_none() && rx.is_empty(), "everything applied");
        assert!(!c.playing, "Stop went through");
    }

    /// Review: the device callback path, without a device. A buffer larger than
    /// MAX_BLOCK with a partial last piece fills every frame, the position moves by
    /// the whole buffer, and the meters keep the loudest piece.
    #[test]
    fn large_device_buffers_fill_every_frame() {
        for channels in [1usize, 2, 4] {
            let (mut c, _log) = probe_core();
            c.apply(Command::Play {
                timeline: Timeline {
                    notes: Vec::new(),
                    duration: 10.0,
                },
                from: 0.0,
            });
            let frames = MAX_BLOCK * 2 + 100;
            let mut data = vec![f32::NAN; frames * channels];
            c.fill_output(&mut data, channels);
            // Probe writes 0.5 to both channels; at volume 0.9 that is 0.45.
            assert!(
                data.iter().all(|s| (s - 0.45).abs() < 1e-6),
                "{channels} channels: every sample written"
            );
            assert_eq!(c.pos_frames, frames as u64);
            let peak = f32::from_bits(c.shared.peak.load(Ordering::Relaxed));
            assert!((peak - 0.45).abs() < 1e-6);
        }
    }

    /// Sound only in its first `left` frames, then silence.
    struct Burst {
        left: usize,
    }

    impl Instrument for Burst {
        fn note_on(&mut self, _: u8, _: u8, _: u8) {}
        fn note_off(&mut self, _: u8, _: u8) {}
        fn all_notes_off(&mut self) {}
        fn set_sustain(&mut self, _: u8, _: bool) {}
        fn process(&mut self, out: &mut [f32], frames: usize) {
            let loud = frames.min(self.left);
            out[..loud * 2].fill(0.5);
            out[loud * 2..frames * 2].fill(0.0);
            self.left -= loud;
        }
        fn name(&self) -> &str {
            "burst"
        }
    }

    /// Review: a loud first piece followed by a silent tail must still show on the
    /// meters; publishing per piece would leave only the silent last one.
    #[test]
    fn meters_keep_the_loudest_piece_of_a_large_buffer() {
        let mut c = core();
        c.apply(Command::SetSlot {
            slot: 0,
            instrument: Some(Box::new(Burst { left: MAX_BLOCK })),
            ack: None,
        });
        let mut data = vec![0.0; (MAX_BLOCK * 2 + 100) * 2];
        c.fill_output(&mut data, 2);
        assert!(data[..MAX_BLOCK * 2].iter().all(|s| *s > 0.0));
        assert!(data[MAX_BLOCK * 2..].iter().all(|s| *s == 0.0));
        let peak = f32::from_bits(c.shared.peak.load(Ordering::Relaxed));
        let slot = f32::from_bits(c.shared.slot_peaks[0].load(Ordering::Relaxed));
        assert!((peak - 0.45).abs() < 1e-6, "master peak {peak}");
        assert!((slot - 0.5).abs() < 1e-6, "slot peak {slot}");
    }

    /// Review: a replaced route list is a `Vec`, and a big one is freed with `munmap`.
    /// It must go back to the UI thread, not be dropped inside the callback.
    #[test]
    fn replaced_routes_are_freed_off_the_audio_thread() {
        let shared = Arc::new(Shared {
            position_frames: AtomicU64::new(0),
            duration_frames: AtomicU64::new(0),
            playing: AtomicBool::new(false),
            peak: AtomicU32::new(0),
            slot_peaks: std::array::from_fn(|_| AtomicU32::new(0)),
            sample_rate: AtomicU32::new(48_000),
            glitches: AtomicU32::new(0),
            transport_done: AtomicU64::new(0),
        });
        let (trash, ui) = crossbeam_channel::bounded::<Retired>(8);
        let mut c = Core::new(48_000, shared, trash);
        let routes = || {
            vec![
                Route {
                    slot: 0,
                    channel: 0
                };
                256
            ]
        };
        c.apply(Command::SetRoutes(routes()));
        c.apply(Command::SetRoutes(routes()));
        // The first apply retires the initial empty list, the second the first full one.
        let mut retired = Vec::new();
        while let Ok(item) = ui.try_recv() {
            retired.push(item);
        }
        assert!(
            retired
                .iter()
                .any(|r| matches!(r, Retired::Routes(v) if v.len() == 256)),
            "the replaced route list should come back to the UI thread"
        );
        assert_eq!(c.routes.len(), 256, "the live list stays in the core");
    }

    /// Review: `update` must not allocate in the callback. The list it
    /// tracks notes in keeps its reserved capacity, and nothing else grows.
    #[test]
    fn update_keeps_the_note_list_within_its_reserve() {
        let (mut c, _log) = probe_core();
        let cap = c.active.capacity();
        // Far more notes under the playhead than the list can hold.
        let crowd: Vec<TimedNote> = (0..cap + 50)
            .map(|i| {
                let mut n = long((i % 2) as u16, (i % 128) as u8, 0.0, 1.0);
                n.track_id = 1000 + (i / 128) as u64;
                n
            })
            .collect();
        c.apply(Command::Play {
            timeline: Timeline {
                notes: Vec::new(),
                duration: 1.0,
            },
            from: 0.0,
        });
        run(&mut c, 4800);
        c.apply(Command::Update {
            timeline: Timeline {
                notes: crowd.clone(),
                duration: 1.0,
            },
        });
        c.apply(Command::Update {
            timeline: Timeline {
                notes: crowd,
                duration: 1.0,
            },
        });
        assert_eq!(c.active.capacity(), cap, "never grew");
        assert!(c.active.len() <= cap);
    }

    /// Review: every note-on gets tracked, so it gets its note-off. Past
    /// the cap, notes are not started at all.
    #[test]
    fn notes_past_the_cap_are_not_started_untracked() {
        let (mut c, log) = probe_core();
        let cap = c.active.capacity();
        // 2 tracks x 128 keys = 256 distinct voices; pad with a third track.
        let mut notes = Vec::new();
        for t in 0..((cap / 128) + 1) as u16 {
            for k in 0..128u8 {
                let mut n = long(t % 2, k, 0.0, 1.0);
                n.track_id = 500 + t as u64;
                notes.push(n);
            }
        }
        assert!(notes.len() > cap);
        c.apply(Command::Play {
            timeline: Timeline {
                notes: Vec::new(),
                duration: 1.0,
            },
            from: 0.0,
        });
        run(&mut c, 4800);
        log.lock().clear();
        c.apply(Command::Update {
            timeline: Timeline {
                notes,
                duration: 1.0,
            },
        });
        let ons = log.lock().iter().filter(|e| e.2).count();
        assert_eq!(c.active.len(), cap, "tracked up to the cap, no further");
        // Notes on one key and route share a voice: one note-on per voice.
        let mut voices: Vec<(u8, u8)> = c.active.iter().map(|a| (a.route.channel, a.key)).collect();
        voices.sort_unstable();
        voices.dedup();
        assert_eq!(ons, voices.len(), "one note-on per tracked voice");
        // Play to the end: every one of them is released.
        run(&mut c, 48_000);
        let offs = log.lock().iter().filter(|e| !e.2).count();
        assert_eq!(offs, ons, "no stuck notes");
    }
}
