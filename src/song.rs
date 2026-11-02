//! Musical data model: notes, tempo map, MIDI import/export.
//!
//! Times are stored in ticks so that the file can be exported losslessly and
//! playback stays correct across tempo changes.  Seconds are derived through
//! [`Song::tick_to_sec`] / [`Song::sec_to_tick`], which integrate the tempo map.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use midly::num::{u4, u7, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

pub const DEFAULT_PPQ: u16 = 480;
/// A0, the lowest key of a standard 88-key piano.
pub const FIRST_KEY: u8 = 21;
/// C8, the highest key of a standard 88-key piano.
pub const LAST_KEY: u8 = 108;
pub const KEY_COUNT: usize = (LAST_KEY - FIRST_KEY + 1) as usize;
pub const DRUM_CHANNEL: u8 = 9;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tempo {
    pub tick: u64,
    /// Quarter notes per minute.
    pub bpm: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub key: u8,
    pub velocity: u8,
    pub channel: u8,
    pub start_tick: u64,
    pub duration_ticks: u64,
}

impl Note {
    pub fn end_tick(&self) -> u64 {
        self.start_tick + self.duration_ticks
    }
}

/// A note flattened onto the wall-clock timeline, ready for audio/visuals.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimedNote {
    pub key: u8,
    pub velocity: u8,
    pub channel: u8,
    pub start: f64,
    pub end: f64,
}

impl TimedNote {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    pub fn is_active(&self, t: f64) -> bool {
        t >= self.start && t < self.end
    }
}

#[derive(Clone, Debug, Default)]
pub struct Timeline {
    pub notes: Vec<TimedNote>,
    pub duration: f64,
}

impl Timeline {
    pub fn active_at(&self, t: f64) -> impl Iterator<Item = &TimedNote> {
        self.notes.iter().filter(move |n| n.is_active(t))
    }

    /// Notes whose start time falls in `[from, to)`.
    pub fn starting_in(&self, from: f64, to: f64) -> impl Iterator<Item = &TimedNote> {
        self.notes
            .iter()
            .filter(move |n| n.start >= from && n.start < to)
    }
}

/// A note captured from live input.  Times are seconds relative to the start
/// of the recording.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordedNote {
    pub key: u8,
    pub velocity: u8,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug)]
pub struct Song {
    pub name: String,
    pub ppq: u16,
    /// Sorted by tick, always contains an entry at tick 0.
    pub tempos: Vec<Tempo>,
    pub time_signature: (u8, u8),
    pub notes: Vec<Note>,
    pub source_path: Option<PathBuf>,
}

impl Default for Song {
    fn default() -> Self {
        Self::empty()
    }
}

impl Song {
    pub fn empty() -> Self {
        Self {
            name: "Untitled".to_string(),
            ppq: DEFAULT_PPQ,
            tempos: vec![Tempo {
                tick: 0,
                bpm: 120.0,
            }],
            time_signature: (4, 4),
            notes: Vec::new(),
            source_path: None,
        }
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)
            .with_context(|| format!("could not read MIDI file {}", path.display()))?;
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("Untitled")
            .to_string();
        let mut song = Self::from_midi_bytes(&bytes, &name)
            .with_context(|| format!("could not parse MIDI file {}", path.display()))?;
        song.source_path = Some(path.to_path_buf());
        Ok(song)
    }

    pub fn from_midi_bytes(bytes: &[u8], name: &str) -> Result<Self> {
        let smf = Smf::parse(bytes).context("invalid Standard MIDI File")?;
        let ppq = match smf.header.timing {
            Timing::Metrical(ppq) => ppq.as_int(),
            Timing::Timecode(_, _) => {
                // SMPTE timing is vanishingly rare in piano MIDI; fall back to
                // a musical grid rather than refusing the file.
                DEFAULT_PPQ
            }
        }
        .max(1);

        let mut tempos = Vec::new();
        let mut time_signature = (4u8, 4u8);
        let mut notes: Vec<Note> = Vec::new();
        let mut pedals: HashMap<u8, Vec<(u64, bool)>> = HashMap::new();

        for track in smf.tracks.iter() {
            let mut tick = 0u64;
            // (channel, key) -> stack of (start_tick, velocity)
            let mut open: HashMap<(u8, u8), Vec<(u64, u8)>> = HashMap::new();

            for event in track {
                tick += event.delta.as_int() as u64;
                match event.kind {
                    TrackEventKind::Meta(MetaMessage::Tempo(us_per_quarter)) => {
                        let us = us_per_quarter.as_int();
                        if us > 0 {
                            tempos.push(Tempo {
                                tick,
                                bpm: 60_000_000.0 / us as f64,
                            });
                        }
                    }
                    TrackEventKind::Meta(MetaMessage::TimeSignature(num, den_pow, _, _)) => {
                        if num > 0 {
                            let den = 1u8.checked_shl(den_pow as u32).unwrap_or(4);
                            time_signature = (num, den);
                        }
                    }
                    TrackEventKind::Midi { channel, message } => {
                        let ch = channel.as_int();
                        match message {
                            MidiMessage::NoteOn { key, vel } => {
                                let vel = vel.as_int();
                                if vel == 0 {
                                    close_note(&mut open, &mut notes, ch, key.as_int(), tick);
                                } else {
                                    open.entry((ch, key.as_int()))
                                        .or_default()
                                        .push((tick, vel));
                                }
                            }
                            MidiMessage::NoteOff { key, .. } => {
                                close_note(&mut open, &mut notes, ch, key.as_int(), tick);
                            }
                            MidiMessage::Controller { controller, value }
                                if controller.as_int() == 64 =>
                            {
                                pedals
                                    .entry(ch)
                                    .or_default()
                                    .push((tick, value.as_int() >= 64));
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            // Notes without an explicit note-off end at the last event.
            for ((ch, key), stack) in open {
                for (start, vel) in stack {
                    notes.push(Note {
                        key,
                        velocity: vel,
                        channel: ch,
                        start_tick: start,
                        duration_ticks: tick.saturating_sub(start).max(1),
                    });
                }
            }
        }

        // A file that only carries tempo should still start at 120 BPM.
        if tempos.is_empty() {
            tempos.push(Tempo {
                tick: 0,
                bpm: 120.0,
            });
        }
        tempos.sort_by_key(|t| t.tick);
        if tempos[0].tick != 0 {
            tempos.insert(
                0,
                Tempo {
                    tick: 0,
                    bpm: 120.0,
                },
            );
        }
        // Remove duplicate ticks, keeping the last value at each tick.
        let mut deduped: Vec<Tempo> = Vec::with_capacity(tempos.len());
        for tempo in tempos {
            match deduped.last_mut() {
                Some(last) if last.tick == tempo.tick => *last = tempo,
                _ => deduped.push(tempo),
            }
        }
        tempos = deduped;

        for list in pedals.values_mut() {
            list.sort_by_key(|(tick, _)| *tick);
        }
        apply_sustain(&mut notes, &pedals);

        // Drum tracks are not useful in a piano roll.
        notes.retain(|n| n.channel != DRUM_CHANNEL && n.key <= 127);
        notes.sort_by(|a, b| {
            a.start_tick
                .cmp(&b.start_tick)
                .then_with(|| a.key.cmp(&b.key))
        });

        Ok(Self {
            name: name.to_string(),
            ppq,
            tempos,
            time_signature,
            notes,
            source_path: None,
        })
    }

    pub fn from_recorded(name: impl Into<String>, recorded: &[RecordedNote]) -> Self {
        let mut song = Self::empty();
        song.name = name.into();
        let mut notes = Vec::with_capacity(recorded.len());
        for r in recorded {
            if r.end <= r.start {
                continue;
            }
            notes.push(Note {
                key: r.key,
                velocity: r.velocity,
                channel: 0,
                start_tick: song.sec_to_tick(r.start),
                duration_ticks: (song.sec_to_tick(r.end) - song.sec_to_tick(r.start)).max(1),
            });
        }
        notes.sort_by_key(|n| n.start_tick);
        song.notes = notes;
        song
    }

    /// Build a new song from a timeline at a fixed tempo.
    pub fn from_timeline(name: impl Into<String>, timeline: &Timeline, bpm: f64) -> Self {
        let mut song = Self::empty();
        song.name = name.into();
        song.tempos = vec![Tempo {
            tick: 0,
            bpm: bpm.max(1.0),
        }];
        song.notes = timeline
            .notes
            .iter()
            .filter(|n| n.end > n.start)
            .map(|n| Note {
                key: n.key,
                velocity: n.velocity,
                channel: n.channel,
                start_tick: song.sec_to_tick(n.start),
                duration_ticks: (song.sec_to_tick(n.end) - song.sec_to_tick(n.start)).max(1),
            })
            .collect();
        song
    }

    /// A short, hand-written demo so the visualizer has something to show
    /// before the user opens a file.
    pub fn demo() -> Self {
        let mut song = Self::empty();
        song.name = "Surf Demo".to_string();
        song.tempos = vec![Tempo { tick: 0, bpm: 92.0 }];
        let beat = 480u64;
        // Am – F – C – G, one bar each.
        let chords: [[u8; 3]; 4] = [[57, 60, 64], [53, 57, 60], [48, 52, 55], [55, 59, 62]];
        let melody = [
            76u8, 74, 72, 71, 69, 67, 65, 64, 67, 69, 72, 74, 76, 79, 76, 72,
        ];

        for (bar, chord) in chords.iter().enumerate() {
            let bar_tick = bar as u64 * 4 * beat;
            song.notes.push(Note {
                key: chord[0].saturating_sub(12),
                velocity: 78,
                channel: 0,
                start_tick: bar_tick,
                duration_ticks: 4 * beat - 30,
            });
            for step in 0..8 {
                song.notes.push(Note {
                    key: chord[step % 3],
                    velocity: 82 + (step as u8 % 3) * 12,
                    channel: 0,
                    start_tick: bar_tick + step as u64 * beat / 2,
                    duration_ticks: beat / 2 - 25,
                });
            }
            song.notes.push(Note {
                key: melody[bar * 4],
                velocity: 100,
                channel: 0,
                start_tick: bar_tick + beat / 2,
                duration_ticks: beat,
            });
            song.notes.push(Note {
                key: melody[bar * 4 + 2],
                velocity: 92,
                channel: 0,
                start_tick: bar_tick + 2 * beat + beat / 2,
                duration_ticks: beat,
            });
        }
        song.notes.sort_by_key(|n| n.start_tick);
        song
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn note_count(&self) -> usize {
        self.notes.len()
    }

    pub fn duration_ticks(&self) -> u64 {
        self.notes.iter().map(Note::end_tick).max().unwrap_or(0)
    }

    pub fn duration_secs(&self) -> f64 {
        self.tick_to_sec(self.duration_ticks())
    }

    /// Convert a tick to wall-clock seconds, integrating every tempo change.
    pub fn tick_to_sec(&self, tick: u64) -> f64 {
        let mut secs = 0.0;
        let mut prev_tick = 0u64;
        let mut bpm = 120.0;
        for tempo in &self.tempos {
            if tempo.tick > tick {
                break;
            }
            secs += ticks_to_secs(tick.min(tempo.tick) - prev_tick, self.ppq, bpm);
            prev_tick = tempo.tick;
            bpm = tempo.bpm.max(1.0);
        }
        secs + ticks_to_secs(tick.saturating_sub(prev_tick), self.ppq, bpm)
    }

    /// Inverse of [`Self::tick_to_sec`].
    pub fn sec_to_tick(&self, sec: f64) -> u64 {
        if sec <= 0.0 {
            return 0;
        }
        let mut remaining = sec;
        let mut prev_tick = 0u64;
        let mut bpm = 120.0;
        for tempo in &self.tempos {
            let seg_secs = ticks_to_secs(tempo.tick.saturating_sub(prev_tick), self.ppq, bpm);
            if remaining < seg_secs {
                return prev_tick
                    + secs_to_ticks(remaining, self.ppq, bpm).min(u64::MAX - prev_tick);
            }
            remaining -= seg_secs;
            prev_tick = tempo.tick;
            bpm = tempo.bpm.max(1.0);
        }
        prev_tick + secs_to_ticks(remaining, self.ppq, bpm)
    }

    pub fn timeline(&self) -> Timeline {
        let mut notes: Vec<TimedNote> = self
            .notes
            .iter()
            .map(|n| TimedNote {
                key: n.key,
                velocity: n.velocity,
                channel: n.channel,
                start: self.tick_to_sec(n.start_tick),
                end: self.tick_to_sec(n.end_tick()),
            })
            .collect();
        notes.sort_by(|a, b| a.start.total_cmp(&b.start).then_with(|| a.key.cmp(&b.key)));
        let duration = notes.iter().map(|n| n.end).fold(0.0, f64::max);
        Timeline { notes, duration }
    }

    /// Write a type-1 Standard MIDI File with a tempo track and a note track.
    pub fn save_midi(&self, path: impl AsRef<Path>) -> Result<()> {
        let bytes = self.to_midi_bytes()?;
        std::fs::write(path.as_ref(), bytes)
            .with_context(|| format!("could not write MIDI file {}", path.as_ref().display()))?;
        Ok(())
    }

    pub fn to_midi_bytes(&self) -> Result<Vec<u8>> {
        let mut smf = Smf::new(Header::new(
            Format::Parallel,
            Timing::Metrical(self.ppq.max(1).into()),
        ));

        // Tempo / time-signature track.
        let mut conductor = midly::Track::new();
        conductor.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(b"Surf Tempo")),
        });
        let (num, den) = self.time_signature;
        let den_pow = (den.max(1) as f32).log2().round().clamp(0.0, 7.0) as u8;
        conductor.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TimeSignature(num.max(1), den_pow, 24, 8)),
        });
        let mut last_tick = 0u64;
        for tempo in &self.tempos {
            let delta = tempo.tick.saturating_sub(last_tick).min(0x0FFF_FFFF);
            last_tick += delta;
            let us = (60_000_000.0 / tempo.bpm.max(1.0))
                .round()
                .clamp(1.0, 16_777_215.0) as u32;
            conductor.push(TrackEvent {
                delta: u28::new(delta as u32),
                kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(us))),
            });
        }
        conductor.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(conductor);

        #[derive(Clone, Copy)]
        enum Ev {
            On { key: u8, vel: u8, channel: u8 },
            Off { key: u8, channel: u8 },
        }

        let mut events: Vec<(u64, u8, Ev)> = Vec::with_capacity(self.notes.len() * 2);
        for note in &self.notes {
            events.push((
                note.start_tick,
                1,
                Ev::On {
                    key: note.key,
                    vel: note.velocity.clamp(1, 127),
                    channel: note.channel.min(15),
                },
            ));
            events.push((
                note.end_tick(),
                0,
                Ev::Off {
                    key: note.key,
                    channel: note.channel.min(15),
                },
            ));
        }
        events.sort_by_key(|(tick, order, _)| (*tick, *order));

        let mut track = midly::Track::new();
        let mut track_name = self.name.as_bytes();
        if track_name.len() > 127 {
            track_name = &track_name[..127];
        }
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(track_name)),
        });
        let mut now = 0u64;
        for (tick, _, ev) in events {
            let delta = tick.saturating_sub(now).min(0x0FFF_FFFF);
            now += delta;
            let kind = match ev {
                Ev::On { key, vel, channel } => TrackEventKind::Midi {
                    channel: u4::new(channel),
                    message: MidiMessage::NoteOn {
                        key: u7::new(key.min(127)),
                        vel: u7::new(vel),
                    },
                },
                Ev::Off { key, channel } => TrackEventKind::Midi {
                    channel: u4::new(channel),
                    message: MidiMessage::NoteOff {
                        key: u7::new(key.min(127)),
                        vel: u7::new(0),
                    },
                },
            };
            track.push(TrackEvent {
                delta: u28::new(delta as u32),
                kind,
            });
        }
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(track);

        let mut bytes = Vec::new();
        smf.write(&mut bytes)
            .map_err(|err| anyhow::anyhow!("could not encode MIDI: {err}"))?;
        Ok(bytes)
    }
}

fn close_note(
    open: &mut HashMap<(u8, u8), Vec<(u64, u8)>>,
    notes: &mut Vec<Note>,
    channel: u8,
    key: u8,
    tick: u64,
) {
    let Some(stack) = open.get_mut(&(channel, key)) else {
        return;
    };
    let Some((start, velocity)) = stack.pop() else {
        return;
    };
    notes.push(Note {
        key,
        velocity,
        channel,
        start_tick: start,
        duration_ticks: tick.saturating_sub(start).max(1),
    });
}

/// Extend note-offs that happen while the sustain pedal is held.
fn apply_sustain(notes: &mut [Note], pedals: &HashMap<u8, Vec<(u64, bool)>>) {
    for note in notes {
        let Some(events) = pedals.get(&note.channel) else {
            continue;
        };
        let end = note.end_tick();
        // Find the last pedal event at or before the note-off.
        let idx = match events.partition_point(|(tick, _)| *tick <= end) {
            0 => continue,
            n => n - 1,
        };
        if !events[idx].1 {
            continue;
        }
        // Sustain is down: hold until the next pedal-up.
        if let Some((up_tick, _)) = events[idx + 1..].iter().find(|(_, down)| !*down) {
            note.duration_ticks = up_tick.saturating_sub(note.start_tick).max(1);
        } else {
            note.duration_ticks = note.duration_ticks.saturating_add(1);
        }
    }
}

fn ticks_to_secs(ticks: u64, ppq: u16, bpm: f64) -> f64 {
    ticks as f64 / ppq.max(1) as f64 * 60.0 / bpm.max(1.0)
}

fn secs_to_ticks(secs: f64, ppq: u16, bpm: f64) -> u64 {
    (secs * bpm.max(1.0) / 60.0 * ppq.max(1) as f64)
        .round()
        .max(0.0) as u64
}

pub fn key_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let name = NAMES[(key % 12) as usize];
    let octave = key as i32 / 12 - 1;
    format!("{name}{octave}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn demo() -> Song {
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
                key: 64,
                velocity: 80,
                channel: 0,
                start_tick: 480,
                duration_ticks: 480,
            },
        ];
        song
    }

    #[test]
    fn tempo_map_roundtrips() {
        let mut song = Song::empty();
        song.tempos = vec![
            Tempo {
                tick: 0,
                bpm: 120.0,
            },
            Tempo {
                tick: 960,
                bpm: 60.0,
            },
        ];
        // First beat at 120 BPM is 0.5s, second beat also 0.5s.
        assert!((song.tick_to_sec(480) - 0.5).abs() < 1e-9);
        assert!((song.tick_to_sec(960) - 1.0).abs() < 1e-9);
        // After the tempo change one beat takes a full second.
        assert!((song.tick_to_sec(1440) - 2.0).abs() < 1e-9);
        for sec in [0.0, 0.25, 0.5, 1.0, 2.0, 3.7] {
            let tick = song.sec_to_tick(sec);
            assert!((song.tick_to_sec(tick) - sec).abs() < 0.01, "sec={sec}");
        }
    }

    #[test]
    fn midi_roundtrip_preserves_notes() {
        let song = demo();
        let bytes = song.to_midi_bytes().unwrap();
        let parsed = Song::from_midi_bytes(&bytes, "roundtrip").unwrap();
        assert_eq!(parsed.notes.len(), 2);
        assert_eq!(parsed.notes[0].key, 60);
        assert_eq!(parsed.notes[1].key, 64);
        assert_eq!(parsed.ppq, 480);
        let tl = parsed.timeline();
        assert!((tl.notes[0].start - 0.0).abs() < 1e-9);
        assert!((tl.notes[1].start - 0.5).abs() < 1e-9);
    }

    #[test]
    fn recorded_notes_become_ticks() {
        let recorded = [
            RecordedNote {
                key: 60,
                velocity: 100,
                start: 0.0,
                end: 0.5,
            },
            RecordedNote {
                key: 62,
                velocity: 90,
                start: 0.25,
                end: 0.75,
            },
        ];
        let song = Song::from_recorded("take", &recorded);
        assert_eq!(song.notes.len(), 2);
        assert_eq!(song.notes[0].start_tick, 0);
        assert!((song.timeline().notes[1].start - 0.25).abs() < 0.01);
    }

    #[test]
    fn key_names_are_scientific_pitch() {
        assert_eq!(key_name(21), "A0");
        assert_eq!(key_name(60), "C4");
        assert_eq!(key_name(108), "C8");
    }
}
