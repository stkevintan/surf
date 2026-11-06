//! Which instrument each track plays, and the engine slots behind them.
//!
//! Every track on the built-in piano gets its own piano in its own slot, so
//! its fader is a real gain stage. Tracks that name the same plugin share one
//! `surf-vst-host` (Kontakt multi style) and are told apart by MIDI channel;
//! their audio comes back already mixed, so a shared plugin's tracks are
//! balanced with CC 7 (channel volume) instead.
//!
//! Slot 0 is a spare piano that tracks fall back to when every other slot is
//! taken. [`plan`] is the pure part: given what each slot holds, where does
//! each track go and how loud is each slot.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use surf_audio::{MAX_SLOTS, Route};
use surf_core::project::{Id, Project, Track};
use surf_plugin::client::VstClient;

/// What an engine slot holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// A built-in piano for one track.
    Piano(Id),
    /// A plugin host, shared by every track that names this plugin.
    Plugin(PathBuf),
}

/// A running plugin host.
pub struct Host {
    pub path: PathBuf,
    pub client: VstClient,
    pub slot: u8,
}

#[derive(Default)]
pub struct Rack {
    /// Slot contents, by slot index. Slot 0 is the spare piano.
    pub slots: Vec<Option<Source>>,
    pub hosts: Vec<Host>,
    /// Plugins that failed to load or crashed this session. Their tracks play
    /// the piano until the user picks the plugin again.
    pub failed: HashSet<PathBuf>,
    /// CC 7 last sent per (slot, channel), so it is only sent on change.
    pub sent_cc7: BTreeMap<(u8, u8), u8>,
}

impl Rack {
    pub fn new() -> Self {
        Self {
            slots: vec![None; MAX_SLOTS],
            ..Default::default()
        }
    }

    pub fn host(&self, path: &Path) -> Option<&Host> {
        self.hosts.iter().find(|h| h.path == path)
    }

    pub fn host_mut(&mut self, path: &Path) -> Option<&mut Host> {
        self.hosts.iter_mut().find(|h| h.path == path)
    }

    /// The first slot nothing uses (never the spare).
    pub fn free_slot(&self) -> Option<u8> {
        (1..self.slots.len())
            .find(|i| self.slots[*i].is_none())
            .map(|i| i as u8)
    }
}

/// What a track should play, given the plugins that failed.
pub fn wanted(track: &Track, failed: &HashSet<PathBuf>) -> Source {
    match &track.instrument {
        Some(path) if !failed.contains(path) => Source::Plugin(path.clone()),
        _ => Source::Piano(track.id),
    }
}

/// Where each track plays and how loud each slot is.
#[derive(Debug, Default, PartialEq)]
pub struct Plan {
    /// By track index, as the timeline numbers them.
    pub routes: Vec<Route>,
    /// Gain of every slot in use.
    pub gains: Vec<(u8, f32)>,
    /// (slot, channel, value) for tracks on a shared plugin.
    pub cc7: Vec<(u8, u8, u8)>,
}

/// CC 7 for a track volume: 1.0 is 100, the General MIDI default.
pub fn cc7(volume: f32) -> u8 {
    (volume * 100.0).round().clamp(0.0, 127.0) as u8
}

pub fn plan(p: &Project, slots: &[Option<Source>], failed: &HashSet<PathBuf>) -> Plan {
    let slot_of = |track: &Track| {
        let source = wanted(track, failed);
        slots
            .iter()
            .position(|s| s.as_ref() == Some(&source))
            .unwrap_or(0) as u8
    };
    let routes: Vec<Route> = p
        .tracks
        .iter()
        .map(|t| Route {
            slot: slot_of(t),
            channel: t.channel,
        })
        .collect();

    let mut users: BTreeMap<u8, Vec<usize>> = BTreeMap::new();
    for (i, r) in routes.iter().enumerate() {
        users.entry(r.slot).or_default().push(i);
    }
    let mut plan = Plan {
        routes,
        ..Default::default()
    };
    for (slot, tracks) in users {
        match tracks.as_slice() {
            // One track: the slot's gain is its fader. The spare piano is
            // shared by whoever fell back to it, so it stays at unity.
            [one] if slot != 0 => plan.gains.push((slot, p.tracks[*one].volume)),
            _ => {
                plan.gains.push((slot, 1.0));
                if matches!(slots.get(slot as usize), Some(Some(Source::Plugin(_)))) {
                    for i in tracks {
                        let t = &p.tracks[i];
                        plan.cc7.push((slot, t.channel, cc7(t.volume)));
                    }
                }
            }
        }
    }
    plan
}

/// Where a plugin's state is parked while no track uses it, so undoing a
/// change of instrument brings the plugin back as it was.
pub fn parked_state_path(plugin: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    plugin.hash(&mut hasher);
    std::env::temp_dir().join(format!(
        "surf-{}-vst-{:016x}.state",
        std::process::id(),
        hasher.finish()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use surf_core::song::Song;

    fn project(n: usize) -> Project {
        let mut song = Song::demo();
        for (i, note) in song.notes.iter_mut().enumerate() {
            note.channel = (i % n) as u8;
        }
        Project::from_song(&song)
    }

    fn kontakt() -> PathBuf {
        PathBuf::from("/plugins/Kontakt 8.vst3")
    }

    #[test]
    fn each_piano_track_has_its_own_slot_and_gain() {
        let mut p = project(2);
        p.tracks[1].volume = 0.5;
        let mut slots = vec![None; MAX_SLOTS];
        slots[1] = Some(Source::Piano(p.tracks[0].id));
        slots[2] = Some(Source::Piano(p.tracks[1].id));
        let plan = plan(&p, &slots, &HashSet::new());
        assert_eq!(
            plan.routes,
            vec![
                Route { slot: 1, channel: 0 },
                Route { slot: 2, channel: 1 }
            ]
        );
        assert_eq!(plan.gains, vec![(1, 1.0), (2, 0.5)]);
        assert!(plan.cc7.is_empty());
    }

    #[test]
    fn tracks_on_one_plugin_share_its_slot_and_balance_with_cc7() {
        let mut p = project(3);
        for t in &mut p.tracks[1..] {
            t.instrument = Some(kontakt());
        }
        p.tracks[2].volume = 0.5;
        let mut slots = vec![None; MAX_SLOTS];
        slots[1] = Some(Source::Piano(p.tracks[0].id));
        slots[2] = Some(Source::Plugin(kontakt()));
        let plan = plan(&p, &slots, &HashSet::new());
        assert_eq!(plan.routes[1], Route { slot: 2, channel: 1 });
        assert_eq!(plan.routes[2], Route { slot: 2, channel: 2 });
        assert_eq!(plan.gains, vec![(1, 1.0), (2, 1.0)]);
        assert_eq!(plan.cc7, vec![(2, 1, 100), (2, 2, 50)]);
    }

    #[test]
    fn one_track_on_a_plugin_uses_the_slot_gain() {
        let mut p = project(1);
        p.tracks[0].instrument = Some(kontakt());
        p.tracks[0].volume = 1.2;
        let mut slots = vec![None; MAX_SLOTS];
        slots[3] = Some(Source::Plugin(kontakt()));
        let plan = plan(&p, &slots, &HashSet::new());
        assert_eq!(plan.routes, vec![Route { slot: 3, channel: 0 }]);
        assert_eq!(plan.gains, vec![(3, 1.2)]);
        assert!(plan.cc7.is_empty());
    }

    #[test]
    fn a_failed_plugin_or_a_full_rack_falls_back_to_a_piano() {
        let mut p = project(2);
        p.tracks[0].instrument = Some(kontakt());
        let mut slots = vec![None; MAX_SLOTS];
        slots[1] = Some(Source::Piano(p.tracks[0].id));
        let failed: HashSet<PathBuf> = [kontakt()].into();
        let plan = plan(&p, &slots, &failed);
        assert_eq!(plan.routes[0].slot, 1, "its own piano while Kontakt is down");
        assert_eq!(plan.routes[1].slot, 0, "no slot of its own: the spare");
        assert!(plan.gains.contains(&(0, 1.0)));
    }
}
