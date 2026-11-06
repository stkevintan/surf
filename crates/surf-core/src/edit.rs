//! Every change to a [`Project`], as an invertible command.
//!
//! [`Edit::apply`] makes the change and returns the edit that undoes it, so
//! [`History`] only stores what each step needs to reverse, not snapshots.
//! A drag commits once: the view applies its preview through
//! [`History::preview`], which replaces the previous preview step instead of
//! stacking a new one, and [`History::commit`] seals it on pointer-up.

use crate::project::{Id, Note, Project, Region, Track};

#[derive(Clone, Debug, PartialEq)]
pub enum Edit {
    /// Move notes of one region by (ticks, semitones). Notes stop at the
    /// region start and at the edges of the MIDI key range.
    MoveNotes {
        region: Id,
        notes: Vec<Id>,
        ticks: i64,
        keys: i32,
    },
    /// Change note lengths by `ticks`; a note never gets shorter than `min`.
    ResizeNotes {
        region: Id,
        notes: Vec<Id>,
        ticks: i64,
        min: u64,
    },
    /// Set note velocities (one entry per note).
    SetVelocity {
        region: Id,
        velocities: Vec<(Id, u8)>,
    },
    /// Insert notes as given (ids already allocated).
    AddNotes {
        region: Id,
        notes: Vec<Note>,
    },
    DeleteNotes {
        region: Id,
        notes: Vec<Id>,
    },
    /// Snap note starts to a grid of `grid` ticks with `strength` 0..=1.
    Quantize {
        region: Id,
        notes: Vec<Id>,
        grid: u64,
        strength: f32,
    },
    /// Put notes back exactly (inverse of edits that change several fields).
    RestoreNotes {
        region: Id,
        notes: Vec<Note>,
    },

    /// Move regions by `ticks` and `tracks` positions down (negative = up).
    MoveRegions {
        regions: Vec<Id>,
        ticks: i64,
        tracks: i32,
    },
    /// Change a region's length (its notes stay where they are).
    ResizeRegion {
        region: Id,
        len: u64,
    },
    /// Cut a region in two at an absolute tick. `new_region` is the id the
    /// right half gets; `new_notes` the ids for notes that move into it.
    SplitRegion {
        region: Id,
        at: u64,
        new_region: Id,
        new_notes: Vec<Id>,
    },
    /// Merge regions on one track into the first, which spans them all.
    JoinRegions {
        regions: Vec<Id>,
    },
    /// Put a removed region back on a track at an index.
    InsertRegion {
        track: Id,
        index: usize,
        region: Region,
    },
    RemoveRegion {
        region: Id,
    },

    AddTrack {
        index: usize,
        track: Track,
    },
    RemoveTrack {
        track: Id,
    },
    SetTrack {
        track: Id,
        field: TrackField,
    },

    /// Several edits as one undo step.
    Batch(Vec<Edit>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum TrackField {
    Name(String),
    Mute(bool),
    Solo(bool),
    Volume(f32),
    Channel(u8),
    /// The plugin the track plays; None is the built-in piano.
    Instrument(Option<std::path::PathBuf>),
}

impl Edit {
    /// Apply to `p` and return the inverse. Edits naming objects that no
    /// longer exist do nothing and return an empty batch.
    pub fn apply(&self, p: &mut Project) -> Edit {
        match self {
            Edit::MoveNotes {
                region,
                notes,
                ticks,
                keys,
            } => {
                let before = snapshot(p, *region, notes);
                if let Some(r) = p.region_mut(*region) {
                    for n in r.notes.iter_mut().filter(|n| notes.contains(&n.id)) {
                        n.start = (n.start as i64 + ticks).max(0) as u64;
                        n.key = (n.key as i32 + keys).clamp(0, 127) as u8;
                    }
                    r.sort_notes();
                }
                Edit::RestoreNotes {
                    region: *region,
                    notes: before,
                }
            }
            Edit::ResizeNotes {
                region,
                notes,
                ticks,
                min,
            } => {
                let before = snapshot(p, *region, notes);
                if let Some(r) = p.region_mut(*region) {
                    for n in r.notes.iter_mut().filter(|n| notes.contains(&n.id)) {
                        n.len = (n.len as i64 + ticks).max(*min as i64).max(1) as u64;
                    }
                }
                Edit::RestoreNotes {
                    region: *region,
                    notes: before,
                }
            }
            Edit::SetVelocity { region, velocities } => {
                let ids: Vec<Id> = velocities.iter().map(|(id, _)| *id).collect();
                let before = snapshot(p, *region, &ids);
                if let Some(r) = p.region_mut(*region) {
                    for (id, v) in velocities {
                        if let Some(n) = r.notes.iter_mut().find(|n| n.id == *id) {
                            n.velocity = (*v).clamp(1, 127);
                        }
                    }
                }
                Edit::RestoreNotes {
                    region: *region,
                    notes: before,
                }
            }
            Edit::AddNotes { region, notes } => {
                if let Some(r) = p.region_mut(*region) {
                    r.notes.extend(notes.iter().cloned());
                    r.sort_notes();
                }
                Edit::DeleteNotes {
                    region: *region,
                    notes: notes.iter().map(|n| n.id).collect(),
                }
            }
            Edit::DeleteNotes { region, notes } => {
                let removed = snapshot(p, *region, notes);
                if let Some(r) = p.region_mut(*region) {
                    r.notes.retain(|n| !notes.contains(&n.id));
                }
                Edit::AddNotes {
                    region: *region,
                    notes: removed,
                }
            }
            Edit::Quantize {
                region,
                notes,
                grid,
                strength,
            } => {
                let before = snapshot(p, *region, notes);
                let grid = (*grid).max(1) as f64;
                if let Some(r) = p.region_mut(*region) {
                    for n in r.notes.iter_mut().filter(|n| notes.contains(&n.id)) {
                        let target = (n.start as f64 / grid).round() * grid;
                        n.start = (n.start as f64 + (target - n.start as f64) * *strength as f64)
                            .round()
                            .max(0.0) as u64;
                    }
                    r.sort_notes();
                }
                Edit::RestoreNotes {
                    region: *region,
                    notes: before,
                }
            }
            Edit::RestoreNotes { region, notes } => {
                let ids: Vec<Id> = notes.iter().map(|n| n.id).collect();
                let before = snapshot(p, *region, &ids);
                if let Some(r) = p.region_mut(*region) {
                    for saved in notes {
                        if let Some(n) = r.notes.iter_mut().find(|n| n.id == saved.id) {
                            *n = saved.clone();
                        }
                    }
                    r.sort_notes();
                }
                Edit::RestoreNotes {
                    region: *region,
                    notes: before,
                }
            }

            Edit::MoveRegions {
                regions,
                ticks,
                tracks,
            } => {
                // Undo with the opposite offset is exact only if no region
                // was clamped at bar 0 or the first/last track, so record
                // where each region really was instead.
                let mut undo = Vec::new();
                let n_tracks = p.tracks.len() as i32;
                for id in regions {
                    let Some((ti, ri)) = p.region_index(*id) else {
                        continue;
                    };
                    let region = p.tracks[ti].regions.remove(ri);
                    let target = (ti as i32 + tracks).clamp(0, n_tracks - 1) as usize;
                    let mut moved = region.clone();
                    moved.start = (region.start as i64 + ticks).max(0) as u64;
                    p.tracks[target].regions.push(moved);
                    sort_regions(&mut p.tracks[target]);
                    undo.push(Edit::RemoveRegion { region: *id });
                    undo.push(Edit::InsertRegion {
                        track: p.tracks[ti].id,
                        index: ri,
                        region,
                    });
                }
                Edit::Batch(undo)
            }
            Edit::ResizeRegion { region, len } => {
                let Some(r) = p.region_mut(*region) else {
                    return Edit::Batch(vec![]);
                };
                let old = r.len;
                r.len = (*len).max(1);
                Edit::ResizeRegion {
                    region: *region,
                    len: old,
                }
            }
            Edit::SplitRegion {
                region,
                at,
                new_region,
                new_notes,
            } => {
                let Some((ti, ri)) = p.region_index(*region) else {
                    return Edit::Batch(vec![]);
                };
                let original = p.tracks[ti].regions[ri].clone();
                if *at <= original.start || *at >= original.end() {
                    return Edit::Batch(vec![]);
                }
                let cut = at - original.start;
                let (left, right): (Vec<Note>, Vec<Note>) =
                    original.notes.iter().cloned().partition(|n| n.start < cut);
                let right_notes = right
                    .into_iter()
                    .zip(new_notes.iter())
                    .map(|(mut n, id)| {
                        n.id = *id;
                        n.start -= cut;
                        n
                    })
                    .collect();
                let r = &mut p.tracks[ti].regions[ri];
                r.notes = left;
                r.len = cut;
                let right = Region {
                    id: *new_region,
                    name: original.name.clone(),
                    start: *at,
                    len: original.len - cut,
                    notes: right_notes,
                };
                p.tracks[ti].regions.insert(ri + 1, right);
                Edit::Batch(vec![
                    Edit::RemoveRegion { region: *region },
                    Edit::RemoveRegion {
                        region: *new_region,
                    },
                    Edit::InsertRegion {
                        track: p.tracks[ti].id,
                        index: ri,
                        region: original,
                    },
                ])
            }
            Edit::JoinRegions { regions } => {
                let mut found: Vec<(usize, usize)> = regions
                    .iter()
                    .filter_map(|id| p.region_index(*id))
                    .collect();
                if found.len() < 2 || found.iter().any(|(ti, _)| *ti != found[0].0) {
                    return Edit::Batch(vec![]);
                }
                let ti = found[0].0;
                found.sort_by_key(|(_, ri)| p.tracks[ti].regions[*ri].start);
                let originals: Vec<(usize, Region)> = found
                    .iter()
                    .map(|(_, ri)| (*ri, p.tracks[ti].regions[*ri].clone()))
                    .collect();
                let start = originals.iter().map(|(_, r)| r.start).min().unwrap_or(0);
                let end = originals.iter().map(|(_, r)| r.end()).max().unwrap_or(0);
                let mut notes = Vec::new();
                for (_, r) in &originals {
                    for n in r.notes.iter().filter(|n| n.start < r.len) {
                        let mut n = n.clone();
                        n.start += r.start - start;
                        notes.push(n);
                    }
                }
                let first = originals[0].1.clone();
                let track_id = p.tracks[ti].id;
                let ids: Vec<Id> = originals.iter().map(|(_, r)| r.id).collect();
                p.tracks[ti].regions.retain(|r| !ids.contains(&r.id));
                let mut joined = Region {
                    id: first.id,
                    name: first.name,
                    start,
                    len: end - start,
                    notes,
                };
                joined.sort_notes();
                p.tracks[ti].regions.push(joined);
                sort_regions(&mut p.tracks[ti]);
                let mut undo = vec![Edit::RemoveRegion { region: first.id }];
                let mut by_index = originals;
                by_index.sort_by_key(|(ri, _)| *ri);
                for (ri, region) in by_index {
                    undo.push(Edit::InsertRegion {
                        track: track_id,
                        index: ri,
                        region,
                    });
                }
                Edit::Batch(undo)
            }
            Edit::InsertRegion {
                track,
                index,
                region,
            } => {
                if let Some(t) = p.track_mut(*track) {
                    let i = (*index).min(t.regions.len());
                    t.regions.insert(i, region.clone());
                }
                Edit::RemoveRegion { region: region.id }
            }
            Edit::RemoveRegion { region } => {
                let Some((ti, ri)) = p.region_index(*region) else {
                    return Edit::Batch(vec![]);
                };
                let removed = p.tracks[ti].regions.remove(ri);
                Edit::InsertRegion {
                    track: p.tracks[ti].id,
                    index: ri,
                    region: removed,
                }
            }

            Edit::AddTrack { index, track } => {
                let i = (*index).min(p.tracks.len());
                p.tracks.insert(i, track.clone());
                Edit::RemoveTrack { track: track.id }
            }
            Edit::RemoveTrack { track } => {
                let Some(i) = p.tracks.iter().position(|t| t.id == *track) else {
                    return Edit::Batch(vec![]);
                };
                let removed = p.tracks.remove(i);
                Edit::AddTrack {
                    index: i,
                    track: removed,
                }
            }
            Edit::SetTrack { track, field } => {
                let Some(t) = p.track_mut(*track) else {
                    return Edit::Batch(vec![]);
                };
                let old = match field {
                    TrackField::Name(v) => {
                        TrackField::Name(std::mem::replace(&mut t.name, v.clone()))
                    }
                    TrackField::Mute(v) => TrackField::Mute(std::mem::replace(&mut t.mute, *v)),
                    TrackField::Solo(v) => TrackField::Solo(std::mem::replace(&mut t.solo, *v)),
                    TrackField::Volume(v) => {
                        TrackField::Volume(std::mem::replace(&mut t.volume, v.clamp(0.0, 1.5)))
                    }
                    TrackField::Channel(v) => {
                        TrackField::Channel(std::mem::replace(&mut t.channel, (*v).min(15)))
                    }
                    TrackField::Instrument(v) => {
                        TrackField::Instrument(std::mem::replace(&mut t.instrument, v.clone()))
                    }
                };
                Edit::SetTrack {
                    track: *track,
                    field: old,
                }
            }

            Edit::Batch(edits) => {
                let mut undo: Vec<Edit> = edits.iter().map(|e| e.apply(p)).collect();
                undo.reverse();
                Edit::Batch(undo)
            }
        }
    }
}

fn snapshot(p: &Project, region: Id, notes: &[Id]) -> Vec<Note> {
    p.find_region(region)
        .map(|(_, r)| {
            r.notes
                .iter()
                .filter(|n| notes.contains(&n.id))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn sort_regions(t: &mut Track) {
    t.regions
        .sort_by(|a, b| a.start.cmp(&b.start).then(a.id.cmp(&b.id)));
}

struct Step {
    label: String,
    undo: Edit,
    redo: Edit,
    serial: u64,
}

/// Undo / redo stacks of labelled steps.
#[derive(Default)]
pub struct History {
    past: Vec<Step>,
    future: Vec<Step>,
    /// The step currently being previewed by a drag, if any.
    open: bool,
    limit: usize,
    /// Serial of the next step pushed. Steps keep theirs through undo and
    /// redo, so the top of `past` says which state the project is in.
    next_serial: u64,
    /// The top step when the project was last saved (None: saved empty).
    saved: Option<u64>,
}

impl History {
    pub fn new() -> Self {
        Self {
            limit: 500,
            ..Default::default()
        }
    }

    /// Changes since the last [`mark_saved`](Self::mark_saved): false right
    /// after a save, true after any edit, and false again if undo walks back
    /// to the saved step.
    pub fn is_dirty(&self) -> bool {
        self.past.last().map(|s| s.serial) != self.saved
    }

    pub fn mark_saved(&mut self) {
        self.commit();
        self.saved = self.past.last().map(|s| s.serial);
    }

    /// Apply an edit as one undo step.
    pub fn apply(&mut self, p: &mut Project, label: impl Into<String>, edit: Edit) {
        self.commit();
        let undo = edit.apply(p);
        self.push(label.into(), undo, edit);
    }

    /// Apply `edit` as the live preview of a drag. Each call first undoes the
    /// previous preview, so the drag ends up as a single step with the
    /// latest offsets. Call [`commit`](Self::commit) when the drag ends.
    pub fn preview(&mut self, p: &mut Project, label: impl Into<String>, edit: Edit) {
        if self.open
            && let Some(step) = self.past.pop()
        {
            step.undo.apply(p);
        }
        let undo = edit.apply(p);
        self.push(label.into(), undo, edit);
        self.open = true;
    }

    /// Seal the current drag, if any.
    pub fn commit(&mut self) {
        self.open = false;
    }

    /// Throw away the current drag and restore the state before it.
    pub fn cancel(&mut self, p: &mut Project) {
        if self.open
            && let Some(step) = self.past.pop()
        {
            step.undo.apply(p);
        }
        self.open = false;
    }

    pub fn undo(&mut self, p: &mut Project) -> Option<String> {
        self.commit();
        let step = self.past.pop()?;
        let redo = step.undo.apply(p);
        let label = step.label.clone();
        self.future.push(Step {
            label: step.label,
            undo: step.redo,
            redo,
            serial: step.serial,
        });
        Some(label)
    }

    pub fn redo(&mut self, p: &mut Project) -> Option<String> {
        self.commit();
        let step = self.future.pop()?;
        let undo = step.redo.apply(p);
        let label = step.label.clone();
        self.past.push(Step {
            label: step.label,
            undo,
            redo: step.undo,
            serial: step.serial,
        });
        Some(label)
    }

    /// The step that [`undo`](Self::undo) would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.past.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.future.last().map(|s| s.label.as_str())
    }

    /// Forget every step. The current state counts as saved, as when a
    /// file has just been opened.
    pub fn clear(&mut self) {
        self.past.clear();
        self.future.clear();
        self.open = false;
        self.saved = None;
    }

    fn push(&mut self, label: String, undo: Edit, redo: Edit) {
        self.next_serial += 1;
        self.past.push(Step {
            label,
            undo,
            redo,
            serial: self.next_serial,
        });
        if self.past.len() > self.limit.max(1) {
            self.past.remove(0);
        }
        self.future.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::{self, Song};

    fn project() -> Project {
        let mut s = Song::empty();
        s.name = "T".into();
        for (i, key) in [60u8, 62, 64, 65].iter().enumerate() {
            s.notes.push(song::Note {
                key: *key,
                velocity: 90,
                channel: 0,
                start_tick: i as u64 * 480 + 7,
                duration_ticks: 400,
            });
        }
        s.notes.push(song::Note {
            key: 40,
            velocity: 70,
            channel: 1,
            start_tick: 0,
            duration_ticks: 1920,
        });
        Project::from_song(&s)
    }

    fn region(p: &Project, track: usize) -> Id {
        p.tracks[track].regions[0].id
    }

    fn notes(p: &Project, track: usize) -> Vec<Id> {
        p.tracks[track].regions[0]
            .notes
            .iter()
            .map(|n| n.id)
            .collect()
    }

    /// Every edit, applied then undone, must leave the project exactly as it was.
    fn round_trips(edit: Edit) {
        let mut p = project();
        let before = p.clone();
        let undo = edit.apply(&mut p);
        assert_ne!(p, before, "edit changed nothing: {edit:?}");
        undo.apply(&mut p);
        assert_eq!(p, before, "undo of {edit:?}");
    }

    #[test]
    fn note_edits_undo_exactly() {
        let p = project();
        let r = region(&p, 0);
        let ns = notes(&p, 0);
        round_trips(Edit::MoveNotes {
            region: r,
            notes: ns.clone(),
            ticks: 240,
            keys: 2,
        });
        // Clamped at tick 0 and key 127: the undo must still restore originals.
        round_trips(Edit::MoveNotes {
            region: r,
            notes: ns.clone(),
            ticks: -10_000,
            keys: 200,
        });
        round_trips(Edit::ResizeNotes {
            region: r,
            notes: ns.clone(),
            ticks: -10_000,
            min: 120,
        });
        round_trips(Edit::SetVelocity {
            region: r,
            velocities: ns.iter().map(|id| (*id, 127)).collect(),
        });
        round_trips(Edit::DeleteNotes {
            region: r,
            notes: ns[1..3].to_vec(),
        });
        round_trips(Edit::Quantize {
            region: r,
            notes: ns.clone(),
            grid: 120,
            strength: 1.0,
        });
        round_trips(Edit::AddNotes {
            region: r,
            notes: vec![Note {
                id: Id(9_999),
                key: 72,
                velocity: 64,
                start: 100,
                len: 50,
            }],
        });
    }

    #[test]
    fn region_and_track_edits_undo_exactly() {
        let p = project();
        let (r0, r1) = (region(&p, 0), region(&p, 1));
        round_trips(Edit::MoveRegions {
            regions: vec![r0],
            ticks: 1920,
            tracks: 1,
        });
        round_trips(Edit::MoveRegions {
            regions: vec![r0, r1],
            ticks: -99_999,
            tracks: -5,
        });
        round_trips(Edit::ResizeRegion {
            region: r0,
            len: 960,
        });
        let ids: Vec<Id> = (0..4).map(|i| Id(10_000 + i)).collect();
        round_trips(Edit::SplitRegion {
            region: r0,
            at: 960,
            new_region: Id(20_000),
            new_notes: ids,
        });
        round_trips(Edit::RemoveRegion { region: r1 });
        round_trips(Edit::RemoveTrack {
            track: p.tracks[0].id,
        });
        round_trips(Edit::SetTrack {
            track: p.tracks[1].id,
            field: TrackField::Mute(true),
        });
        round_trips(Edit::SetTrack {
            track: p.tracks[1].id,
            field: TrackField::Volume(0.3),
        });
        round_trips(Edit::SetTrack {
            track: p.tracks[0].id,
            field: TrackField::Instrument(Some("/plugins/Kontakt 8.vst3".into())),
        });
    }

    #[test]
    fn split_then_join_restores_the_notes() {
        let mut p = project();
        let r = region(&p, 0);
        let before: Vec<(u64, u8)> = p.tracks[0].regions[0]
            .notes
            .iter()
            .map(|n| (n.start, n.key))
            .collect();
        let ids: Vec<Id> = (0..4).map(|i| Id(10_000 + i)).collect();
        Edit::SplitRegion {
            region: r,
            at: 960,
            new_region: Id(20_000),
            new_notes: ids,
        }
        .apply(&mut p);
        assert_eq!(p.tracks[0].regions.len(), 2);
        assert_eq!(p.tracks[0].regions[1].start, 960);
        assert_eq!(
            p.tracks[0].regions[1].notes[0].start, 7,
            "right half is relative to its own start"
        );
        Edit::JoinRegions {
            regions: vec![r, Id(20_000)],
        }
        .apply(&mut p);
        assert_eq!(p.tracks[0].regions.len(), 1);
        let after: Vec<(u64, u8)> = p.tracks[0].regions[0]
            .notes
            .iter()
            .map(|n| (n.start, n.key))
            .collect();
        assert_eq!(after, before);
    }

    #[test]
    fn quantize_strength_moves_part_way() {
        let mut p = project();
        let r = region(&p, 0);
        let ns = notes(&p, 0);
        Edit::Quantize {
            region: r,
            notes: ns,
            grid: 480,
            strength: 0.5,
        }
        .apply(&mut p);
        // 7 -> 3.5 -> rounds to 4; 487 -> 483.5 -> 484
        let starts: Vec<u64> = p.tracks[0].regions[0]
            .notes
            .iter()
            .map(|n| n.start)
            .collect();
        assert_eq!(starts, vec![4, 484, 964, 1444]);
    }

    #[test]
    fn history_undo_redo_and_labels() {
        let mut p = project();
        let original = p.clone();
        let mut h = History::new();
        let r = region(&p, 0);
        let ns = notes(&p, 0);
        h.apply(
            &mut p,
            "Move Notes",
            Edit::MoveNotes {
                region: r,
                notes: ns.clone(),
                ticks: 480,
                keys: 0,
            },
        );
        h.apply(
            &mut p,
            "Delete Notes",
            Edit::DeleteNotes {
                region: r,
                notes: ns[..1].to_vec(),
            },
        );
        let after = p.clone();
        assert_eq!(h.undo_label(), Some("Delete Notes"));
        assert_eq!(h.undo(&mut p).as_deref(), Some("Delete Notes"));
        assert_eq!(h.undo(&mut p).as_deref(), Some("Move Notes"));
        assert_eq!(p, original);
        assert!(h.undo(&mut p).is_none());
        assert_eq!(h.redo(&mut p).as_deref(), Some("Move Notes"));
        assert_eq!(h.redo(&mut p).as_deref(), Some("Delete Notes"));
        assert_eq!(p, after);
        // A new edit clears the redo stack.
        h.undo(&mut p);
        h.apply(
            &mut p,
            "Quantize",
            Edit::Quantize {
                region: r,
                notes: ns,
                grid: 120,
                strength: 1.0,
            },
        );
        assert!(h.redo_label().is_none());
    }

    #[test]
    fn a_drag_is_one_undo_step() {
        let mut p = project();
        let original = p.clone();
        let mut h = History::new();
        let r = region(&p, 0);
        let ns = notes(&p, 0);
        for step in 1..=10 {
            h.preview(
                &mut p,
                "Move Notes",
                Edit::MoveNotes {
                    region: r,
                    notes: ns.clone(),
                    ticks: step * 48,
                    keys: 0,
                },
            );
        }
        h.commit();
        assert_eq!(
            p.tracks[0].regions[0].notes[0].start,
            7 + 480,
            "the last preview stands"
        );
        h.undo(&mut p);
        assert_eq!(p, original, "one undo removes the whole drag");
        assert!(h.undo_label().is_none());
    }

    #[test]
    fn cancelling_a_drag_restores_the_start() {
        let mut p = project();
        let original = p.clone();
        let mut h = History::new();
        let r = region(&p, 0);
        h.preview(
            &mut p,
            "Move",
            Edit::MoveRegions {
                regions: vec![r],
                ticks: 1920,
                tracks: 1,
            },
        );
        h.cancel(&mut p);
        assert_eq!(p, original);
        assert!(h.undo_label().is_none());
    }

    #[test]
    fn dirty_follows_the_saved_step_through_undo_and_redo() {
        let mut p = Project::from_song(&crate::song::Song::demo());
        let t = p.tracks[0].id;
        let mute = |on| Edit::SetTrack {
            track: t,
            field: TrackField::Mute(on),
        };
        let mut h = History::new();
        assert!(!h.is_dirty());
        h.apply(&mut p, "Mute", mute(true));
        assert!(h.is_dirty());
        h.mark_saved();
        assert!(!h.is_dirty());
        h.undo(&mut p);
        assert!(h.is_dirty(), "undo past the save");
        h.redo(&mut p);
        assert!(!h.is_dirty(), "redo back to it");
        h.undo(&mut p);
        h.apply(&mut p, "Mute", mute(false));
        assert!(h.is_dirty(), "a different branch");
        h.clear();
        assert!(!h.is_dirty());
    }
}
