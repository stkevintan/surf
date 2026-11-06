//! Editing state and commands shared by the Tracks area and the Piano Roll.
//!
//! Two selections, as in Logic: regions in the Tracks area, notes in the
//! editor. Keyboard commands (Delete, ⌘A, ⌘C/⌘X/⌘V, Q, ⌥↑/↓, ⌘T, ⌘J) act on the area
//! that was clicked last ([`Focus`]). Every change goes through
//! [`Session::apply`] or a drag's [`Session::preview`], so it can be undone.

use std::collections::BTreeSet;

use surf_core::edit::{Edit, TrackField};
use surf_core::project::{Id, Note, Project, Region, Track};

use crate::session::Session;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    Tracks,
    Roll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pointer,
    Pencil,
    Scissors,
    Eraser,
}

impl Tool {
    pub const TRACKS: [Tool; 4] = [Tool::Pointer, Tool::Pencil, Tool::Scissors, Tool::Eraser];
    pub const ROLL: [Tool; 3] = [Tool::Pointer, Tool::Pencil, Tool::Eraser];

    pub fn label(self) -> &'static str {
        match self {
            Tool::Pointer => "Pointer",
            Tool::Pencil => "Pencil",
            Tool::Scissors => "Scissors",
            Tool::Eraser => "Eraser",
        }
    }
}

/// Grid used by snapping and quantize, as a fraction of a whole note.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    Bar,
    Beat,
    Eighth,
    Sixteenth,
}

impl Snap {
    pub const TRACKS: [Snap; 3] = [Snap::Bar, Snap::Beat, Snap::Sixteenth];
    pub const ROLL: [Snap; 3] = [Snap::Beat, Snap::Eighth, Snap::Sixteenth];

    pub fn label(self) -> &'static str {
        match self {
            Snap::Bar => "Bar",
            Snap::Beat => "1/4",
            Snap::Eighth => "1/8",
            Snap::Sixteenth => "1/16",
        }
    }

    pub fn ticks(self, p: &Project) -> u64 {
        let beat = p.ticks_per_beat();
        match self {
            Snap::Bar => p.ticks_per_bar(),
            Snap::Beat => beat,
            Snap::Eighth => beat / 2,
            Snap::Sixteenth => beat / 4,
        }
        .max(1)
    }
}

/// What the user has selected and where they are working.
pub struct Editing {
    pub focus: Focus,
    pub track: Option<Id>,
    pub regions: BTreeSet<Id>,
    /// The region the Piano Roll and Event List show.
    pub edit_region: Option<Id>,
    pub notes: BTreeSet<Id>,
    pub tracks_tool: Tool,
    pub roll_tool: Tool,
    pub tracks_snap: Snap,
    pub roll_snap: Snap,
    pub quantize: Snap,
    /// 0..=100
    pub quantize_strength: u8,
    /// Velocity for new notes; follows the last note touched.
    pub new_velocity: u8,
    pub clipboard: Option<Clipboard>,
}

/// What ⌘C / ⌘X copied. Starts are relative to the earliest item, so a
/// paste lands at the playhead.
#[derive(Clone, Debug, PartialEq)]
pub enum Clipboard {
    Notes(Vec<Note>),
    /// Each region with its track offset from the topmost copied track.
    Regions(Vec<(usize, Region)>),
}

impl Default for Editing {
    fn default() -> Self {
        Self {
            focus: Focus::Tracks,
            track: None,
            regions: BTreeSet::new(),
            edit_region: None,
            notes: BTreeSet::new(),
            tracks_tool: Tool::Pointer,
            roll_tool: Tool::Pointer,
            tracks_snap: Snap::Beat,
            roll_snap: Snap::Sixteenth,
            quantize: Snap::Sixteenth,
            quantize_strength: 100,
            new_velocity: 90,
            clipboard: None,
        }
    }
}

impl Editing {
    /// Make the selection valid for `p`: drop ids that no longer exist and
    /// pick a track and region to edit if none is chosen.
    pub fn reconcile(&mut self, p: &Project) {
        if self.track.is_none_or(|t| p.track(t).is_none()) {
            self.track = p.tracks.first().map(|t| t.id);
        }
        self.regions.retain(|r| p.find_region(*r).is_some());
        if self.edit_region.is_none_or(|r| p.find_region(r).is_none()) {
            self.edit_region = self
                .track
                .and_then(|t| p.track(t))
                .and_then(|t| t.regions.first())
                .or_else(|| p.tracks.iter().find_map(|t| t.regions.first()))
                .map(|r| r.id);
            self.notes.clear();
        }
        if let Some((_, r)) = self.edit_region.and_then(|id| p.find_region(id)) {
            self.notes.retain(|n| r.note(*n).is_some());
        } else {
            self.notes.clear();
        }
    }

    pub fn select_track(&mut self, track: Id) {
        self.track = Some(track);
    }

    /// Select one region (or add it with shift) and open it in the editor.
    pub fn select_region(&mut self, p: &Project, region: Id, add: bool) {
        if !add {
            self.regions.clear();
        }
        if add && self.regions.contains(&region) {
            self.regions.remove(&region);
        } else {
            self.regions.insert(region);
        }
        if let Some((t, _)) = p.find_region(region) {
            self.track = Some(t.id);
        }
        if self.edit_region != Some(region) {
            self.edit_region = Some(region);
            self.notes.clear();
        }
    }

    pub fn edit_region<'a>(&self, p: &'a Project) -> Option<(&'a Track, &'a Region)> {
        self.edit_region.and_then(|id| p.find_region(id))
    }

    /// Notes the next note command acts on: the selection, else all notes in
    /// the edited region.
    pub fn target_notes(&self, p: &Project) -> Vec<Id> {
        let Some((_, r)) = self.edit_region(p) else {
            return Vec::new();
        };
        if self.notes.is_empty() {
            r.notes.iter().map(|n| n.id).collect()
        } else {
            r.notes
                .iter()
                .filter(|n| self.notes.contains(&n.id))
                .map(|n| n.id)
                .collect()
        }
    }
}

// ---- commands -------------------------------------------------------------

pub fn delete(s: &mut Session, e: &mut Editing) {
    if let Some((label, edit)) = removal(e) {
        s.apply(&label, edit);
        clear_selection(s, e);
    }
}

/// The edit that removes the selection of the focused area, with its label.
fn removal(e: &Editing) -> Option<(String, Edit)> {
    match e.focus {
        Focus::Roll => {
            let region = e.edit_region?;
            if e.notes.is_empty() {
                return None;
            }
            let notes: Vec<Id> = e.notes.iter().copied().collect();
            let label = plural("Delete Note", notes.len());
            Some((label, Edit::DeleteNotes { region, notes }))
        }
        Focus::Tracks => {
            if e.regions.is_empty() {
                return None;
            }
            let edits = e
                .regions
                .iter()
                .map(|r| Edit::RemoveRegion { region: *r })
                .collect();
            let label = plural("Delete Region", e.regions.len());
            Some((label, Edit::Batch(edits)))
        }
    }
}

fn clear_selection(s: &Session, e: &mut Editing) {
    match e.focus {
        Focus::Roll => e.notes.clear(),
        Focus::Tracks => e.regions.clear(),
    }
    e.reconcile(&s.project);
}

pub fn select_all(s: &Session, e: &mut Editing) {
    match e.focus {
        Focus::Roll => {
            if let Some((_, r)) = e.edit_region(&s.project) {
                e.notes = r.notes.iter().map(|n| n.id).collect();
            }
        }
        Focus::Tracks => {
            e.regions = s
                .project
                .tracks
                .iter()
                .flat_map(|t| t.regions.iter().map(|r| r.id))
                .collect();
        }
    }
}

/// Copy the selection of the focused area to the clipboard.
pub fn copy(s: &mut Session, e: &mut Editing) -> bool {
    let p = &s.project;
    let clip = match e.focus {
        Focus::Roll => {
            let mut notes: Vec<Note> = e
                .edit_region(p)
                .map(|(_, r)| {
                    r.notes
                        .iter()
                        .filter(|n| e.notes.contains(&n.id))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            let Some(first) = notes.iter().map(|n| n.start).min() else {
                s.status = "Select notes to copy".to_string();
                return false;
            };
            for n in &mut notes {
                n.start -= first;
            }
            Clipboard::Notes(notes)
        }
        Focus::Tracks => {
            let mut regions: Vec<(usize, Region)> = p
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(ti, t)| t.regions.iter().map(move |r| (ti, r)))
                .filter(|(_, r)| e.regions.contains(&r.id))
                .map(|(ti, r)| (ti, r.clone()))
                .collect();
            let (Some(top), Some(first)) = (
                regions.iter().map(|(t, _)| *t).min(),
                regions.iter().map(|(_, r)| r.start).min(),
            ) else {
                s.status = "Select regions to copy".to_string();
                return false;
            };
            for (t, r) in &mut regions {
                *t -= top;
                r.start -= first;
            }
            Clipboard::Regions(regions)
        }
    };
    let (n, what) = match &clip {
        Clipboard::Notes(n) => (n.len(), "note"),
        Clipboard::Regions(r) => (r.len(), "region"),
    };
    s.status = format!("Copied {n} {what}{}", if n == 1 { "" } else { "s" });
    e.clipboard = Some(clip);
    true
}

/// Copy the selection, then remove it as one undo step.
pub fn cut(s: &mut Session, e: &mut Editing) {
    if !copy(s, e) {
        return;
    }
    if let Some((_, edit)) = removal(e) {
        s.apply("Cut", edit);
        clear_selection(s, e);
    }
}

/// Paste the clipboard at the playhead, snapped to the grid: notes into the
/// edited region, regions onto the selected track and the ones below it.
/// The pasted items get fresh ids and become the selection.
pub fn paste(s: &mut Session, e: &mut Editing) {
    let playhead = s.project.sec_to_tick(s.position());
    match e.clipboard.clone() {
        None => s.status = "Nothing to paste".to_string(),
        Some(Clipboard::Notes(notes)) => {
            let Some((_, r)) = e.edit_region(&s.project) else {
                s.status = "Open a region in the Piano Roll to paste notes".to_string();
                return;
            };
            let (region, region_start, region_len) = (r.id, r.start, r.len);
            let grid = e.roll_snap.ticks(&s.project);
            let at = playhead.saturating_sub(region_start) / grid * grid;
            let copies: Vec<Note> = notes
                .into_iter()
                .map(|mut n| {
                    n.id = s.project.alloc();
                    n.start += at;
                    n
                })
                .collect();
            let ids = copies.iter().map(|n| n.id).collect();
            let mut edits = vec![Edit::AddNotes {
                region,
                notes: copies.clone(),
            }];
            // Grow the region, in whole bars, so the pasted notes play.
            let end = copies.iter().map(|n| n.end()).max().unwrap_or(0);
            if end > region_len {
                let bar = s.project.ticks_per_bar().max(1);
                let len = end.div_ceil(bar) * bar;
                edits.push(Edit::ResizeRegion { region, len });
            }
            s.apply("Paste", Edit::Batch(edits));
            e.focus = Focus::Roll;
            e.notes = ids;
        }
        Some(Clipboard::Regions(regions)) => {
            let p = &mut s.project;
            if p.tracks.is_empty() {
                return;
            }
            let anchor = e
                .track
                .and_then(|t| p.tracks.iter().position(|x| x.id == t))
                .unwrap_or(0);
            let grid = e.tracks_snap.ticks(p);
            let at = playhead / grid * grid;
            let mut edits = Vec::new();
            let mut ids = Vec::new();
            for (dt, mut r) in regions {
                let target = (anchor + dt).min(p.tracks.len() - 1);
                r.id = p.alloc();
                r.start += at;
                for n in &mut r.notes {
                    n.id = p.alloc();
                }
                ids.push(r.id);
                edits.push(Edit::InsertRegion {
                    track: p.tracks[target].id,
                    index: usize::MAX,
                    region: r,
                });
            }
            s.apply("Paste", Edit::Batch(edits));
            e.focus = Focus::Tracks;
            e.regions = ids.iter().copied().collect();
            if let Some(first) = ids.first() {
                e.edit_region = Some(*first);
                e.notes.clear();
            }
        }
    }
}

/// Quantize the selected notes (Roll), or every note of the selected regions
/// (Tracks), to the quantize grid.
pub fn quantize(s: &mut Session, e: &Editing) {
    let grid = e.quantize.ticks(&s.project);
    let strength = e.quantize_strength as f32 / 100.0;
    let regions: Vec<(Id, Vec<Id>)> = match e.focus {
        Focus::Roll => e
            .edit_region
            .map(|r| (r, e.target_notes(&s.project)))
            .into_iter()
            .collect(),
        Focus::Tracks => e
            .regions
            .iter()
            .filter_map(|id| s.project.find_region(*id))
            .map(|(_, r)| (r.id, r.notes.iter().map(|n| n.id).collect()))
            .collect(),
    };
    let count: usize = regions.iter().map(|(_, n)| n.len()).sum();
    if count == 0 {
        s.status = "Select notes or regions to quantize".to_string();
        return;
    }
    let edits = regions
        .into_iter()
        .map(|(region, notes)| Edit::Quantize {
            region,
            notes,
            grid,
            strength,
        })
        .collect();
    s.apply("Quantize", Edit::Batch(edits));
    s.status = format!(
        "Quantized {count} note{} to {}{}",
        if count == 1 { "" } else { "s" },
        e.quantize.label(),
        if e.quantize_strength < 100 {
            format!(" at {}%", e.quantize_strength)
        } else {
            String::new()
        }
    );
}

pub fn transpose(s: &mut Session, e: &Editing, semitones: i32) {
    let Some(region) = e.edit_region else { return };
    let notes = e.target_notes(&s.project);
    if notes.is_empty() || e.focus != Focus::Roll {
        return;
    }
    s.apply(
        "Transpose",
        Edit::MoveNotes {
            region,
            notes,
            ticks: 0,
            keys: semitones,
        },
    );
}

/// Split selected regions at the playhead.
pub fn split_at_playhead(s: &mut Session, e: &mut Editing) {
    let at = s.project.sec_to_tick(s.position());
    let mut edits = Vec::new();
    let candidates: Vec<Id> = if e.regions.is_empty() {
        e.track
            .and_then(|t| s.project.track(t))
            .map(|t| t.regions.iter().map(|r| r.id).collect())
            .unwrap_or_default()
    } else {
        e.regions.iter().copied().collect()
    };
    for id in candidates {
        let Some((_, r)) = s.project.find_region(id) else {
            continue;
        };
        if at <= r.start || at >= r.end() {
            continue;
        }
        let cut = at - r.start;
        let moving = r.notes.iter().filter(|n| n.start >= cut).count();
        let new_region = s.project.alloc();
        let new_notes = (0..moving).map(|_| s.project.alloc()).collect();
        edits.push(Edit::SplitRegion {
            region: id,
            at,
            new_region,
            new_notes,
        });
    }
    if edits.is_empty() {
        s.status = "Put the playhead inside a region to split it".to_string();
        return;
    }
    s.apply("Split Region", Edit::Batch(edits));
}

pub fn split_region_at(s: &mut Session, e: &mut Editing, region: Id, at: u64) {
    let Some((_, r)) = s.project.find_region(region) else {
        return;
    };
    if at <= r.start || at >= r.end() {
        return;
    }
    let cut = at - r.start;
    let moving = r.notes.iter().filter(|n| n.start >= cut).count();
    let new_region = s.project.alloc();
    let new_notes = (0..moving).map(|_| s.project.alloc()).collect();
    s.apply(
        "Split Region",
        Edit::SplitRegion {
            region,
            at,
            new_region,
            new_notes,
        },
    );
    e.regions = [region, new_region].into();
}

pub fn join(s: &mut Session, e: &mut Editing) {
    let ids: Vec<Id> = e.regions.iter().copied().collect();
    let tracks: BTreeSet<Id> = ids
        .iter()
        .filter_map(|r| s.project.find_region(*r))
        .map(|(t, _)| t.id)
        .collect();
    if ids.len() < 2 || tracks.len() != 1 {
        s.status = "Select two or more regions on one track to join".to_string();
        return;
    }
    let first = ids
        .iter()
        .min_by_key(|r| s.project.find_region(**r).map(|(_, r)| r.start))
        .copied();
    s.apply("Join Regions", Edit::JoinRegions { regions: ids });
    e.regions = first.into_iter().collect();
    e.edit_region = first;
    e.notes.clear();
}

pub fn new_track(s: &mut Session, e: &mut Editing) {
    let p = &mut s.project;
    let index = e
        .track
        .and_then(|t| p.tracks.iter().position(|x| x.id == t))
        .map_or(p.tracks.len(), |i| i + 1);
    let used: BTreeSet<u8> = p.tracks.iter().map(|t| t.channel).collect();
    let channel = (0..16u8)
        .find(|c| !used.contains(c) && *c != 9)
        .unwrap_or(0);
    let id = p.alloc();
    let track = Track {
        id,
        name: format!("Inst {}", p.tracks.len() + 1),
        color: p.tracks.len(),
        channel,
        mute: false,
        solo: false,
        volume: 1.0,
        regions: Vec::new(),
    };
    s.apply("New Track", Edit::AddTrack { index, track });
    e.track = Some(id);
    e.regions.clear();
    s.status = format!(
        "New track on MIDI channel {} · draw a region with the Pencil",
        channel + 1
    );
}

pub fn set_track(s: &mut Session, track: Id, field: TrackField) {
    let label = match &field {
        TrackField::Name(_) => "Rename Track",
        TrackField::Mute(_) => "Mute",
        TrackField::Solo(_) => "Solo",
        TrackField::Volume(_) => "Volume",
        TrackField::Channel(_) => "MIDI Channel",
    };
    s.apply(label, Edit::SetTrack { track, field });
}

/// An empty one-bar region at `tick` (snapped to bars) on `track`.
pub fn create_region(s: &mut Session, e: &mut Editing, track: Id, tick: u64) {
    let bar = s.project.ticks_per_bar().max(1);
    let start = tick / bar * bar;
    let Some(t) = s.project.track(track) else {
        return;
    };
    let index = t.regions.len();
    let name = t.name.clone();
    let id = s.project.alloc();
    let region = Region {
        id,
        name,
        start,
        len: bar,
        notes: Vec::new(),
    };
    s.apply(
        "Create Region",
        Edit::InsertRegion {
            track,
            index,
            region,
        },
    );
    e.select_region(&s.project, id, false);
}

/// A new note in the edited region; returns its id.
pub fn create_note(s: &mut Session, e: &mut Editing, start: u64, key: u8, len: u64) -> Option<Id> {
    let region = e.edit_region?;
    let id = s.project.alloc();
    let note = Note {
        id,
        key,
        velocity: e.new_velocity,
        start,
        len: len.max(1),
    };
    s.apply(
        "Create Note",
        Edit::AddNotes {
            region,
            notes: vec![note],
        },
    );
    e.notes = [id].into();
    Some(id)
}

fn plural(what: &str, n: usize) -> String {
    if n == 1 {
        what.to_string()
    } else {
        format!("{what}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use surf_core::song::Song;

    fn editing_for(p: &Project) -> Editing {
        let mut e = Editing::default();
        e.reconcile(p);
        e
    }

    #[test]
    fn reconcile_picks_a_region_and_drops_stale_ids() {
        let mut p = Project::from_song(&Song::demo());
        let mut e = editing_for(&p);
        let r = p.tracks[0].regions[0].id;
        assert_eq!(e.edit_region, Some(r));
        e.notes.insert(p.tracks[0].regions[0].notes[0].id);
        e.regions.insert(r);
        p.tracks[0].regions.clear();
        e.reconcile(&p);
        assert!(e.regions.is_empty() && e.notes.is_empty() && e.edit_region.is_none());
    }

    #[test]
    fn target_notes_are_the_selection_or_everything() {
        let p = Project::from_song(&Song::demo());
        let mut e = editing_for(&p);
        let all = p.tracks[0].regions[0].notes.len();
        assert_eq!(e.target_notes(&p).len(), all);
        e.notes.insert(p.tracks[0].regions[0].notes[3].id);
        assert_eq!(e.target_notes(&p), vec![p.tracks[0].regions[0].notes[3].id]);
    }

    #[test]
    fn snap_sizes_follow_the_project() {
        let p = Project::from_song(&Song::demo());
        assert_eq!(Snap::Beat.ticks(&p), 480);
        assert_eq!(Snap::Sixteenth.ticks(&p), 120);
        assert_eq!(Snap::Bar.ticks(&p), 1920);
    }
}
