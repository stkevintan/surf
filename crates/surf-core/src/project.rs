//! The multi-track project: tracks, regions and notes, in ticks.
//!
//! This is what the editors show and what [`crate::edit`] changes. [`Song`]
//! stays the MIDI-file interchange format: [`Project::from_song`] splits one
//! into tracks, and [`Project::to_song`] / [`Project::timeline`] flatten it
//! back for export and playback.
//!
//! Note positions are relative to their region, so moving a region moves its
//! notes. Every object has a stable id so selection and undo survive edits.

use std::collections::BTreeMap;

use crate::song::{self, Song, Tempo, TimedNote, Timeline};

/// Stable identity of a track, region or note within a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Id(pub u64);

#[derive(Clone, Debug, PartialEq)]
pub struct Note {
    pub id: Id,
    pub key: u8,
    pub velocity: u8,
    /// Ticks from the region start.
    pub start: u64,
    pub len: u64,
}

impl Note {
    pub fn end(&self) -> u64 {
        self.start + self.len
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub id: Id,
    pub name: String,
    /// Ticks from the project start.
    pub start: u64,
    pub len: u64,
    /// Sorted by start, then key.
    pub notes: Vec<Note>,
}

impl Region {
    pub fn end(&self) -> u64 {
        self.start + self.len
    }

    pub fn note(&self, id: Id) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id)
    }

    pub(crate) fn sort_notes(&mut self) {
        self.notes.sort_by(|a, b| {
            a.start
                .cmp(&b.start)
                .then(a.key.cmp(&b.key))
                .then(a.id.cmp(&b.id))
        });
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Track {
    pub id: Id,
    pub name: String,
    /// Index into the theme's track palette.
    pub color: usize,
    /// MIDI channel 0..=15 the track plays on. Tracks that share a plugin
    /// instance (Kontakt) are told apart by channel.
    pub channel: u8,
    pub mute: bool,
    pub solo: bool,
    /// Linear gain, 0.0..=1.5.
    pub volume: f32,
    pub regions: Vec<Region>,
}

impl Track {
    pub fn region(&self, id: Id) -> Option<&Region> {
        self.regions.iter().find(|r| r.id == id)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Project {
    pub name: String,
    pub ppq: u16,
    pub tempos: Vec<Tempo>,
    pub time_signature: (u8, u8),
    pub tracks: Vec<Track>,
    next_id: u64,
}

impl Project {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ppq: song::DEFAULT_PPQ,
            tempos: vec![Tempo {
                tick: 0,
                bpm: 120.0,
            }],
            time_signature: (4, 4),
            tracks: Vec::new(),
            next_id: 1,
        }
    }

    /// A fresh id, unique within this project.
    pub fn alloc(&mut self) -> Id {
        let id = Id(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn ticks_per_beat(&self) -> u64 {
        self.ppq.max(1) as u64
    }

    pub fn ticks_per_bar(&self) -> u64 {
        let (num, den) = self.time_signature;
        // A bar is `num` beats of 1/`den`; the PPQ counts quarter notes.
        self.ticks_per_beat() * num.max(1) as u64 * 4 / den.max(1) as u64
    }

    /// Split a MIDI song into one track per MIDI channel, each with a single
    /// region covering the song, rounded up to whole bars.
    pub fn from_song(song: &Song) -> Self {
        let mut p = Project::new(song.name.clone());
        p.ppq = song.ppq;
        p.tempos = song.tempos.clone();
        p.time_signature = song.time_signature;
        let bar = p.ticks_per_bar().max(1);
        let len = song.duration_ticks().div_ceil(bar).max(1) * bar;

        let mut by_channel: BTreeMap<u8, Vec<&song::Note>> = BTreeMap::new();
        for n in &song.notes {
            by_channel.entry(n.channel).or_default().push(n);
        }
        if by_channel.is_empty() {
            by_channel.insert(0, Vec::new());
        }
        let several = by_channel.len() > 1;
        for (index, (channel, notes)) in by_channel.into_iter().enumerate() {
            let notes = notes
                .into_iter()
                .map(|n| Note {
                    id: p.alloc(),
                    key: n.key,
                    velocity: n.velocity,
                    start: n.start_tick,
                    len: n.duration_ticks.max(1),
                })
                .collect();
            let name = if several {
                format!("{} · Ch {}", song.name, channel + 1)
            } else {
                song.name.clone()
            };
            let mut region = Region {
                id: p.alloc(),
                name: name.clone(),
                start: 0,
                len,
                notes,
            };
            region.sort_notes();
            let id = p.alloc();
            p.tracks.push(Track {
                id,
                name: track_name(channel, several),
                color: index,
                channel,
                mute: false,
                solo: false,
                volume: 1.0,
                regions: vec![region],
            });
        }
        p
    }

    /// Flatten to a MIDI song: every note, at its absolute tick, on its
    /// track's channel. Notes past their region's end are not part of it.
    pub fn to_song(&self) -> Song {
        let mut s = Song::empty();
        s.name = self.name.clone();
        s.ppq = self.ppq;
        s.tempos = self.tempos.clone();
        s.time_signature = self.time_signature;
        for t in &self.tracks {
            for r in &t.regions {
                for n in &r.notes {
                    if n.start >= r.len {
                        continue;
                    }
                    s.notes.push(song::Note {
                        key: n.key,
                        velocity: n.velocity,
                        channel: t.channel,
                        start_tick: r.start + n.start,
                        duration_ticks: n.len.min(r.len - n.start).max(1),
                    });
                }
            }
        }
        s.notes.sort_by_key(|n| (n.start_tick, n.key));
        s
    }

    /// Whether a track should sound, given mute and solo across tracks.
    pub fn audible(&self, track: &Track) -> bool {
        if self.tracks.iter().any(|t| t.solo) {
            track.solo
        } else {
            !track.mute
        }
    }

    /// What the engine plays: audible tracks only, in seconds.
    ///
    /// Until each track has its own instrument and gain stage, a track's
    /// volume is applied by scaling its note velocities (so a quieter track
    /// plays softer on the shared instrument).
    pub fn timeline(&self) -> Timeline {
        let mut audible = self.clone();
        let keep: Vec<Id> = self
            .tracks
            .iter()
            .filter(|t| self.audible(t))
            .map(|t| t.id)
            .collect();
        audible.tracks.retain(|t| keep.contains(&t.id));
        for t in &mut audible.tracks {
            if (t.volume - 1.0).abs() > f32::EPSILON {
                for n in t.regions.iter_mut().flat_map(|r| r.notes.iter_mut()) {
                    n.velocity =
                        ((n.velocity as f32 * t.volume).round() as i32).clamp(1, 127) as u8;
                }
            }
        }
        let mut tl = audible.to_song().timeline();
        // The song ends where its last region ends, even if that bar is silent.
        tl.duration = tl.duration.max(self.tick_to_sec(self.end()));
        tl
    }

    /// Every note at its absolute position, with its track. For drawing.
    pub fn timed_notes(&self) -> Vec<(Id, TimedNote)> {
        let tempo = self.to_song_tempo();
        let mut out = Vec::new();
        for t in &self.tracks {
            for r in &t.regions {
                for n in r.notes.iter().filter(|n| n.start < r.len) {
                    let start = r.start + n.start;
                    let end = start + n.len.min(r.len - n.start);
                    out.push((
                        t.id,
                        TimedNote {
                            key: n.key,
                            velocity: n.velocity,
                            channel: t.channel,
                            start: tempo.tick_to_sec(start),
                            end: tempo.tick_to_sec(end),
                        },
                    ));
                }
            }
        }
        out.sort_by(|a, b| a.1.start.total_cmp(&b.1.start));
        out
    }

    /// Last region end, in ticks.
    pub fn end(&self) -> u64 {
        self.tracks
            .iter()
            .flat_map(|t| t.regions.iter().map(Region::end))
            .max()
            .unwrap_or(0)
    }

    pub fn tick_to_sec(&self, tick: u64) -> f64 {
        self.to_song_tempo().tick_to_sec(tick)
    }

    pub fn sec_to_tick(&self, sec: f64) -> u64 {
        self.to_song_tempo().sec_to_tick(sec)
    }

    fn to_song_tempo(&self) -> Song {
        let mut s = Song::empty();
        s.ppq = self.ppq;
        s.tempos = self.tempos.clone();
        s
    }

    pub fn track(&self, id: Id) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub(crate) fn track_mut(&mut self, id: Id) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    /// The region with `id`, and the track that holds it.
    pub fn find_region(&self, id: Id) -> Option<(&Track, &Region)> {
        self.tracks
            .iter()
            .find_map(|t| t.region(id).map(|r| (t, r)))
    }

    pub(crate) fn region_mut(&mut self, id: Id) -> Option<&mut Region> {
        self.tracks
            .iter_mut()
            .find_map(|t| t.regions.iter_mut().find(|r| r.id == id))
    }

    /// Where a region sits: (track index, region index).
    pub(crate) fn region_index(&self, id: Id) -> Option<(usize, usize)> {
        self.tracks
            .iter()
            .enumerate()
            .find_map(|(ti, t)| t.regions.iter().position(|r| r.id == id).map(|ri| (ti, ri)))
    }
}

fn track_name(channel: u8, several: bool) -> String {
    if channel == song::DRUM_CHANNEL {
        "Drums".to_string()
    } else if several {
        format!("Channel {}", channel + 1)
    } else {
        "Grand Piano".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_channel_song() -> Song {
        let mut s = Song::empty();
        s.name = "Duet".into();
        s.notes = vec![
            song::Note {
                key: 60,
                velocity: 100,
                channel: 0,
                start_tick: 0,
                duration_ticks: 480,
            },
            song::Note {
                key: 64,
                velocity: 90,
                channel: 0,
                start_tick: 480,
                duration_ticks: 480,
            },
            song::Note {
                key: 40,
                velocity: 80,
                channel: 1,
                start_tick: 0,
                duration_ticks: 1920,
            },
        ];
        s
    }

    #[test]
    fn song_splits_into_one_track_per_channel() {
        let p = Project::from_song(&two_channel_song());
        assert_eq!(p.tracks.len(), 2);
        assert_eq!(p.tracks[0].channel, 0);
        assert_eq!(p.tracks[0].regions[0].notes.len(), 2);
        assert_eq!(p.tracks[1].regions[0].notes.len(), 1);
        // Regions round up to whole bars (4/4 at 480 PPQ = 1920 ticks).
        assert_eq!(p.tracks[0].regions[0].len, 1920);
    }

    #[test]
    fn to_song_round_trips_notes_and_channels() {
        let song = two_channel_song();
        let back = Project::from_song(&song).to_song();
        let mut want: Vec<_> = song
            .notes
            .iter()
            .map(|n| (n.start_tick, n.key, n.channel, n.duration_ticks))
            .collect();
        let mut got: Vec<_> = back
            .notes
            .iter()
            .map(|n| (n.start_tick, n.key, n.channel, n.duration_ticks))
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn ids_are_unique() {
        let p = Project::from_song(&two_channel_song());
        let mut ids: Vec<Id> = p
            .tracks
            .iter()
            .flat_map(|t| {
                std::iter::once(t.id).chain(
                    t.regions
                        .iter()
                        .flat_map(|r| std::iter::once(r.id).chain(r.notes.iter().map(|n| n.id))),
                )
            })
            .collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), n);
    }

    #[test]
    fn solo_and_mute_decide_what_plays() {
        let mut p = Project::from_song(&two_channel_song());
        assert_eq!(p.timeline().notes.len(), 3);
        p.tracks[1].mute = true;
        assert_eq!(p.timeline().notes.len(), 2);
        p.tracks[1].mute = false;
        p.tracks[1].solo = true;
        let tl = p.timeline();
        assert_eq!(tl.notes.len(), 1);
        assert_eq!(tl.notes[0].key, 40);
    }

    #[test]
    fn track_volume_scales_playback_velocity_only() {
        let mut p = Project::from_song(&two_channel_song());
        p.tracks[0].volume = 0.5;
        let v: Vec<u8> = p
            .timeline()
            .notes
            .iter()
            .filter(|n| n.channel == 0)
            .map(|n| n.velocity)
            .collect();
        assert_eq!(v, vec![50, 45]);
        // Export keeps the written velocities.
        let exported: Vec<u8> = p
            .to_song()
            .notes
            .iter()
            .filter(|n| n.channel == 0)
            .map(|n| n.velocity)
            .collect();
        assert_eq!(exported, vec![100, 90]);
    }

    #[test]
    fn notes_past_the_region_end_are_cut() {
        let mut p = Project::from_song(&two_channel_song());
        p.tracks[0].regions[0].len = 480;
        let s = p.to_song();
        let keys: Vec<u8> = s
            .notes
            .iter()
            .filter(|n| n.channel == 0)
            .map(|n| n.key)
            .collect();
        assert_eq!(keys, vec![60]);
    }
}
