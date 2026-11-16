//! Standard MIDI File import and export.
//!
//! Import pairs note-on/off per `(channel, key)`, keeps the tempo map and time
//! signature, extends notes held by the sustain pedal, and drops the drum
//! channel. Export writes a type-1 file: a conductor track, then one note track.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use midly::num::{u4, u7, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};
use surf_core::song::{DRUM_CHANNEL, Note, Song, Tempo};

pub fn load(path: impl AsRef<Path>) -> Result<Song> {
    let path = path.as_ref();
    let bytes = std::fs::read(path)
        .with_context(|| format!("could not read MIDI file {}", path.display()))?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Untitled")
        .to_string();
    let mut song = from_bytes(&bytes, &name)
        .with_context(|| format!("could not parse MIDI file {}", path.display()))?;
    song.source_path = Some(path.to_path_buf());
    Ok(song)
}

pub fn from_bytes(bytes: &[u8], name: &str) -> Result<Song> {
    let smf = Smf::parse(bytes).context("invalid Standard MIDI File")?;
    // SMPTE files count ticks in real time (frames x subframes per second) and
    // ignore tempo events. Model that as one quarter per second at a fixed tempo,
    // so a tick lasts what the file says instead of 1/960 s.
    let (ppq, smpte_bpm) = match smf.header.timing {
        Timing::Metrical(ppq) => (ppq.as_int().max(1), None),
        Timing::Timecode(fps, subframes) => {
            let per_sec = fps.as_f32() as f64 * subframes.max(1) as f64;
            let ppq = per_sec.round().clamp(1.0, u16::MAX as f64) as u16;
            (ppq, Some(60.0 * per_sec / ppq as f64))
        }
    };

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
                TrackEventKind::Meta(MetaMessage::Tempo(_)) if smpte_bpm.is_some() => {}
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

    if let Some(bpm) = smpte_bpm {
        tempos = vec![Tempo { tick: 0, bpm }];
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

    Ok(Song {
        name: name.to_string(),
        ppq,
        tempos,
        time_signature,
        notes,
        source_path: None,
    })
}

/// Write a type-1 Standard MIDI File with a tempo track and a note track.
pub fn save(song: &Song, path: impl AsRef<Path>) -> Result<()> {
    let bytes = to_bytes(song)?;
    std::fs::write(path.as_ref(), bytes)
        .with_context(|| format!("could not write MIDI file {}", path.as_ref().display()))?;
    Ok(())
}

pub fn to_bytes(song: &Song) -> Result<Vec<u8>> {
    let mut smf = Smf::new(Header::new(
        Format::Parallel,
        Timing::Metrical(song.ppq.max(1).into()),
    ));

    // Tempo / time-signature track.
    let mut conductor = midly::Track::new();
    conductor.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::TrackName(b"Surf Tempo")),
    });
    let (num, den) = song.time_signature;
    let den_pow = (den.max(1) as f32).log2().round().clamp(0.0, 7.0) as u8;
    conductor.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::TimeSignature(num.max(1), den_pow, 24, 8)),
    });
    let mut last_tick = 0u64;
    for tempo in &song.tempos {
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

    let mut events: Vec<(u64, u8, Ev)> = Vec::with_capacity(song.notes.len() * 2);
    for note in &song.notes {
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
    let mut track_name = song.name.as_bytes();
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

/// Extend note-offs that happen while the sustain pedal is held, but never past
/// the next note-on of the same key on the same channel: re-striking a held key
/// ends the old note, so extending past it would overlap the two.
fn apply_sustain(notes: &mut [Note], pedals: &HashMap<u8, Vec<(u64, bool)>>) {
    let mut starts: HashMap<(u8, u8), Vec<u64>> = HashMap::new();
    for n in notes.iter() {
        starts
            .entry((n.channel, n.key))
            .or_default()
            .push(n.start_tick);
    }
    for list in starts.values_mut() {
        list.sort_unstable();
    }
    let next_strike = |note: &Note| {
        starts.get(&(note.channel, note.key)).and_then(|list| {
            let i = list.partition_point(|t| *t <= note.start_tick);
            list.get(i).copied()
        })
    };
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
        // Sustain is down: hold until the next pedal-up, or the key is struck again.
        let mut held_to = match events[idx + 1..].iter().find(|(_, down)| !*down) {
            Some((up_tick, _)) => *up_tick,
            None => end.saturating_add(1),
        };
        if let Some(strike) = next_strike(note) {
            held_to = held_to.min(strike.max(end));
        }
        note.duration_ticks = held_to.saturating_sub(note.start_tick).max(1);
    }
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

    fn on(delta: u32, key: u8) -> TrackEvent<'static> {
        TrackEvent {
            delta: u28::new(delta),
            kind: TrackEventKind::Midi {
                channel: u4::new(0),
                message: MidiMessage::NoteOn {
                    key: u7::new(key),
                    vel: u7::new(100),
                },
            },
        }
    }

    fn off(delta: u32, key: u8) -> TrackEvent<'static> {
        TrackEvent {
            delta: u28::new(delta),
            kind: TrackEventKind::Midi {
                channel: u4::new(0),
                message: MidiMessage::NoteOff {
                    key: u7::new(key),
                    vel: u7::new(0),
                },
            },
        }
    }

    fn pedal(delta: u32, down: bool) -> TrackEvent<'static> {
        TrackEvent {
            delta: u28::new(delta),
            kind: TrackEventKind::Midi {
                channel: u4::new(0),
                message: MidiMessage::Controller {
                    controller: u7::new(64),
                    value: u7::new(if down { 127 } else { 0 }),
                },
            },
        }
    }

    fn encode(timing: Timing, events: Vec<TrackEvent<'static>>) -> Vec<u8> {
        let mut smf = Smf::new(Header::new(Format::SingleTrack, timing));
        let mut track = events;
        track.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(track);
        let mut bytes = Vec::new();
        smf.write(&mut bytes).unwrap();
        bytes
    }

    /// Review M17: SMPTE timing counts real time. At 25 fps x 40 subframes a
    /// tick is 1 ms, so a note 1000 ticks in starts at 1 s and lasts 0.5 s.
    #[test]
    fn smpte_files_keep_their_real_time() {
        let bytes = encode(
            Timing::Timecode(midly::Fps::Fps25, 40),
            vec![on(1000, 60), off(500, 60)],
        );
        let song = from_bytes(&bytes, "smpte").unwrap();
        let tl = song.timeline();
        assert!(
            (tl.notes[0].start - 1.0).abs() < 1e-9,
            "{}",
            tl.notes[0].start
        );
        assert!((tl.notes[0].end - 1.5).abs() < 1e-9, "{}", tl.notes[0].end);
    }

    /// Review M17: a key re-struck under the pedal ends the earlier note there
    /// instead of both sounding until the pedal comes up.
    #[test]
    fn sustain_stops_at_the_next_strike_of_the_key() {
        let bytes = encode(
            Timing::Metrical(480.into()),
            vec![
                pedal(0, true),
                on(0, 60),
                off(100, 60),
                on(380, 60),
                off(100, 60),
                pedal(1440, false),
            ],
        );
        let song = from_bytes(&bytes, "sustain").unwrap();
        assert_eq!(song.notes.len(), 2);
        let (a, b) = (&song.notes[0], &song.notes[1]);
        assert_eq!(
            (a.start_tick, a.end_tick()),
            (0, 480),
            "first held to the re-strike"
        );
        assert_eq!(
            (b.start_tick, b.end_tick()),
            (480, 2020),
            "second held to pedal-up"
        );
    }

    #[test]
    fn midi_roundtrip_preserves_notes() {
        let song = demo();
        let bytes = to_bytes(&song).unwrap();
        let parsed = from_bytes(&bytes, "roundtrip").unwrap();
        assert_eq!(parsed.notes.len(), 2);
        assert_eq!(parsed.notes[0].key, 60);
        assert_eq!(parsed.notes[1].key, 64);
        assert_eq!(parsed.ppq, 480);
        let tl = parsed.timeline();
        assert!((tl.notes[0].start - 0.0).abs() < 1e-9);
        assert!((tl.notes[1].start - 0.5).abs() < 1e-9);
    }
}
