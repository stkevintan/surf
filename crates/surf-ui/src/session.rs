//! Everything behind the interface: audio engine, MIDI input, recording,
//! the loaded song, plugin instruments and file import/export.
//!
//! [`Session`] owns no pixels. The views in [`crate::shell`] read it to draw
//! and call its methods when the user acts; the status bar shows
//! [`Session::status`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use surf_audio::render::{RenderOptions, RenderStats, mix_stems, render_timeline_to_wav};
use surf_audio::{AudioEngine, Command, EngineStatus, OutputEvent, Synth};
use surf_core::document::{self, Document};
use surf_core::recorder::Recorder;
use surf_core::edit::{Edit, History, TrackField};
use surf_core::project::{Id, Project};
use surf_core::song::{Song, Timeline};
use surf_midi::{LiveEvent, MidiHub, NoteSink, PortChange};
use surf_plugin::client::{RenderNote, VstClient, helper_log_path};
use surf_plugin::scan::{self as vst_scan, PluginEntry};

use crate::rack::{self, Host, Rack, Source};

/// A note the user is playing (or just played), for the rising-note view.
pub struct LiveNote {
    pub key: u8,
    pub velocity: u8,
    pub start: Instant,
    pub end: Option<Instant>,
}

pub struct Session {
    pub audio: AudioEngine,
    pub midi: MidiHub,
    recorder: Recorder,
    /// The tracks, regions and notes being edited.
    pub project: Project,
    pub history: History,
    /// What the engine plays: the project's audible tracks, flattened.
    /// Rebuilt after every edit by [`Session::project_changed`].
    pub timeline: Timeline,
    pub live_notes: Vec<LiveNote>,
    /// The instruments the tracks play: built-in pianos and plugin hosts.
    pub rack: Rack,
    render_rx: Option<Receiver<anyhow::Result<RenderStats>>>,
    pub rendering: bool,
    pending_vst: Option<PathBuf>,
    /// Results of the last `SCAN`, shown in the plugin picker.
    pub plugin_scan: Vec<PluginEntry>,
    pub loop_playback: bool,
    pub volume: f32,
    pub status: String,
    /// Engine state sampled once per frame by [`Session::tick`].
    pub engine: EngineStatus,
    /// Sustain pedal state, for the UI indicator and the held-note highlight.
    pub sustain: bool,
    /// Keys whose note-off arrived while the pedal was down. They sound until the
    /// pedal lifts, so the keyboard should keep showing them as ringing.
    pub sustained_keys: HashSet<u8>,
    /// Track armed for recording (the R button); None means the first track.
    pub record_arm: Option<Id>,
    /// Track whose instrument live input plays: the selected track.
    input_track: Option<Id>,
    /// The `.surf` file the project was opened from or last saved to.
    pub project_path: Option<PathBuf>,
    /// Plugin states from the opened project, used when each plugin's host
    /// first starts.
    project_states: std::collections::BTreeMap<PathBuf, Vec<u8>>,
    /// The last recorded region, so the editor can open it.
    pub record_region: Option<Id>,
}

impl Session {
    pub fn new() -> Self {
        let audio = AudioEngine::new();
        // The spare piano, for tracks that find no slot of their own.
        audio.set_slot(0, Some(Box::new(piano(audio.sample_rate()))));
        // Live notes go straight from the MIDI driver thread to the audio
        // thread, so they sound without waiting for a UI frame.
        let audio_tx = audio.sender();
        let sink: NoteSink = std::sync::Arc::new(move |event| {
            let command = match event {
                LiveEvent::NoteOn { key, velocity } => Command::NoteOn { key, velocity },
                LiveEvent::NoteOff { key } => Command::NoteOff { key },
                LiveEvent::Sustain(down) => Command::Sustain(down),
            };
            let _ = audio_tx.send(command);
        });
        let midi = MidiHub::new(sink);
        // Start with a short demo loaded (paused) so the piano roll has
        // something to show before the user opens their own file.
        let project = Project::from_song(&Song::demo());
        let timeline = project.timeline();
        // The status bar already shows the output device and MIDI port.
        let status = if midi.ports().is_empty() {
            "No MIDI input · press ⌘K for Musical Typing, or open a MIDI file".to_string()
        } else {
            "Ready · play your keyboard, or press Space to play the demo".to_string()
        };
        // The Library lists installed plugins from the start. Scanning only
        // walks the file system, so it is cheap and cannot crash.
        let plugin_scan = vst_scan::scan_plugin_paths();
        let engine = audio.status();
        let mut session = Self {
            audio,
            midi,
            recorder: Recorder::new(),
            project,
            history: History::new(),
            timeline,
            live_notes: Vec::new(),
            rack: Rack::new(),
            render_rx: None,
            rendering: false,
            pending_vst: std::env::var_os("SURF_VST").map(PathBuf::from),
            plugin_scan,
            sustain: false,
            sustained_keys: HashSet::new(),
            record_arm: None,
            input_track: None,
            project_path: None,
            project_states: Default::default(),
            record_region: None,
            loop_playback: false,
            volume: 0.9,
            status,
            engine,
        };
        session.sync_instruments();
        session
    }

    pub fn toggle_play(&mut self) {
        if self.timeline.notes.is_empty() && self.timeline.duration <= 0.0 {
            self.status = "Nothing to play yet · open a MIDI file or record a take".to_string();
            return;
        }
        let status = self.audio.status();
        if status.playing {
            self.audio.pause();
            self.status = "Paused".to_string();
        } else if status.position >= self.timeline.duration - 0.05 || status.position <= 0.0 {
            self.audio.play(self.timeline.clone(), 0.0);
            self.status = "Playing".to_string();
        } else {
            self.audio.resume();
            self.status = "Playing".to_string();
        }
    }

    pub fn stop_playback(&mut self) {
        self.audio.stop();
        self.status = "Stopped".to_string();
    }

    pub fn seek_relative(&mut self, delta: f64) {
        let status = self.audio.status();
        let target = (status.position + delta).clamp(0.0, self.timeline.duration.max(0.0));
        self.audio.seek(target);
        self.status = format!("Seek {:.1}s", target);
    }

    pub fn toggle_loop(&mut self) {
        self.loop_playback = !self.loop_playback;
        self.audio.set_loop(self.loop_playback);
        self.status = if self.loop_playback {
            "Loop enabled".to_string()
        } else {
            "Loop disabled".to_string()
        };
    }

    pub fn toggle_record(&mut self) {
        if self.recorder.is_recording() {
            let notes = self.recorder.finish(Instant::now());
            self.audio.stop();
            if notes.is_empty() {
                self.status = "Recording stopped: no notes captured".to_string();
                return;
            }
            let take = Song::from_recorded("Take", &notes);
            self.add_take(&take);
        } else {
            self.recorder.start(Instant::now());
            self.live_notes.clear();
            self.audio.stop();
            self.status = "Recording… play your MIDI keyboard or use Musical Typing".to_string();
        }
    }

    /// Put a recorded take on the record track as a new region, starting at
    /// bar 1 (the transport always records from the start for now).
    fn add_take(&mut self, take: &Song) {
        let Some(track) = self.record_track() else { return };
        let bar = self.project.ticks_per_bar().max(1);
        // The recorder works at 480 PPQ; rescale to the project's resolution.
        let scale = self.project.ppq.max(1) as f64 / take.ppq.max(1) as f64;
        let mut notes = Vec::new();
        for n in &take.notes {
            notes.push(surf_core::project::Note {
                id: self.project.alloc(),
                key: n.key,
                velocity: n.velocity,
                start: (n.start_tick as f64 * scale).round() as u64,
                len: ((n.duration_ticks as f64 * scale).round() as u64).max(1),
            });
        }
        let end = notes.iter().map(|n| n.end()).max().unwrap_or(bar);
        let index = self.project.track(track).map_or(0, |t| t.regions.len());
        let takes = self
            .project
            .tracks
            .iter()
            .flat_map(|t| &t.regions)
            .filter(|r| r.name.starts_with("Take"))
            .count();
        let region = surf_core::project::Region {
            id: self.project.alloc(),
            name: format!("Take {}", takes + 1),
            start: 0,
            len: end.div_ceil(bar) * bar,
            notes,
        };
        let count = region.notes.len();
        let id = region.id;
        self.history.apply(&mut self.project, "Record", Edit::InsertRegion { track, index, region });
        self.project_changed();
        self.record_region = Some(id);
        self.status = format!("Recorded {count} notes into a new take");
    }

    /// Track that recording goes to: the armed one, else the first.
    pub fn record_track(&self) -> Option<Id> {
        self.record_arm.or_else(|| self.project.tracks.first().map(|t| t.id))
    }

    pub fn load_demo(&mut self) {
        self.set_project(Project::from_song(&Song::demo()));
        self.audio.play(self.timeline.clone(), 0.0);
        self.status = "Demo loaded — 4 bars of Am–F–C–G".to_string();
    }

    /// Replace the whole project (a new file). Clears undo history.
    ///
    /// The new tracks keep the instruments already loaded: a track takes
    /// the plugin of the old track on its MIDI channel, else the first old
    /// track's. Opening a file doesn't unload Kontakt.
    pub fn set_project(&mut self, mut project: Project) {
        self.audio.stop();
        let fallback = self.project.tracks.first().and_then(|t| t.instrument.clone());
        for t in &mut project.tracks {
            if t.instrument.is_none() {
                t.instrument = self
                    .project
                    .tracks
                    .iter()
                    .find(|old| old.channel == t.channel)
                    .map_or(fallback.clone(), |old| old.instrument.clone());
            }
        }
        self.project = project;
        self.history.clear();
        self.record_arm = None;
        self.record_region = None;
        self.project_path = None;
        self.project_changed();
    }

    /// Call after every change to the project, so playback hears it.
    pub fn project_changed(&mut self) {
        let position = self.position();
        let playing = self.engine.playing;
        self.sync_instruments();
        let timeline = self.project.timeline();
        // Mute, solo and volume changes reach the engine without restarting
        // the song; only a change in what plays does.
        if playing && timeline != self.timeline {
            self.audio.play(timeline.clone(), position);
        }
        self.timeline = timeline;
    }

    /// Give every track the instrument it names: start plugin hosts that
    /// are new, stop ones no track uses, add or drop built-in pianos, then
    /// send the engine each track's route and gain.
    pub fn sync_instruments(&mut self) {
        let sample_rate = self.audio.sample_rate();
        let wanted: Vec<Source> = self
            .project
            .tracks
            .iter()
            .map(|t| rack::wanted(t, &self.rack.failed))
            .collect();

        // Drop what no track wants any more.
        for slot in 1..self.rack.slots.len() {
            let Some(source) = self.rack.slots[slot].clone() else {
                continue;
            };
            if wanted.contains(&source) {
                continue;
            }
            self.audio.set_slot(slot as u8, None);
            self.rack.slots[slot] = None;
            if let Source::Plugin(path) = source
                && let Some(i) = self.rack.hosts.iter().position(|h| h.path == path)
            {
                let mut host = self.rack.hosts.remove(i);
                // Park its state so undo can bring it back as it was.
                let _ = host.client.save_state(&rack::parked_state_path(&path));
                drop(host);
            }
            self.rack.sent_cc7.retain(|(s, _), _| *s as usize != slot);
        }

        // Add what is new, in track order.
        let mut seen: Vec<Source> = Vec::new();
        for source in wanted {
            if seen.contains(&source) || self.rack.slots.contains(&Some(source.clone())) {
                seen.push(source);
                continue;
            }
            seen.push(source.clone());
            let Some(slot) = self.rack.free_slot() else {
                // The spare piano plays it.
                continue;
            };
            match &source {
                Source::Piano(_) => {
                    self.audio.set_slot(slot, Some(Box::new(piano(sample_rate))));
                    self.rack.slots[slot as usize] = Some(source);
                }
                Source::Plugin(path) => {
                    if let Some(client) = self.start_host(path) {
                        self.audio.set_slot(slot, Some(Box::new(client.instrument())));
                        self.rack.hosts.push(Host {
                            path: path.clone(),
                            client,
                            slot,
                        });
                        self.rack.slots[slot as usize] = Some(source);
                    }
                }
            }
        }
        self.send_plan();
    }

    /// Routes, slot gains and channel volumes for the current project.
    fn send_plan(&mut self) {
        let plan = rack::plan(&self.project, &self.rack.slots, &self.rack.failed);
        self.audio.send(Command::SetRoutes(plan.routes));
        for (slot, gain) in plan.gains {
            self.audio.send(Command::SetSlotGain { slot, gain });
        }
        for (slot, channel, value) in plan.cc7 {
            if self.rack.sent_cc7.insert((slot, channel), value) != Some(value) {
                self.audio.send(Command::ControlChange {
                    slot,
                    channel,
                    controller: 7,
                    value,
                });
            }
        }
        // Live input plays the record-armed track, else the selected one.
        let input = self
            .record_arm
            .or(self.input_track)
            .and_then(|id| self.project.tracks.iter().position(|t| t.id == id))
            .unwrap_or(0);
        self.audio.send(Command::SetInput(input as u16));
    }

    /// Start a plugin host, restoring the state saved for the plugin (or
    /// parked when its last track let it go). A failure is remembered, so
    /// the tracks fall back to the piano instead of retrying every frame.
    fn start_host(&mut self, path: &Path) -> Option<VstClient> {
        let parked = rack::parked_state_path(path);
        let saved = vst_state_path_for(path);
        // The opened project's state wins over the last one saved for the
        // plugin; it goes through the parked file, which the host reads.
        if let Some(blob) = self.project_states.remove(path) {
            let _ = std::fs::write(&parked, blob);
        }
        let state = if parked.exists() {
            Some(parked)
        } else if std::env::var_os("SURF_NO_STATE").is_none() && saved.exists() {
            Some(saved)
        } else {
            None
        };
        match VstClient::load(path, self.audio.sample_rate(), state.as_deref()) {
            Ok(client) => {
                self.status = match &state {
                    Some(state) => format!(
                        "Loaded {} (state from {})",
                        client.name,
                        state.display()
                    ),
                    None => format!("Loaded {}", client.name),
                };
                let has_editor = surf_plugin::format::detect(path)
                    .is_none_or(surf_plugin::format::Format::has_editor);
                if !has_editor {
                    self.status.push_str(" · no editor on this platform yet");
                }
                Some(client)
            }
            Err(err) => {
                self.rack.failed.insert(path.to_path_buf());
                self.status = load_failure(path, &err);
                None
            }
        }
    }

    /// Arm a track for recording; live input then plays its instrument.
    pub fn set_record_arm(&mut self, track: Option<Id>) {
        self.record_arm = track;
        self.send_plan();
    }

    /// The track live input plays: the one selected in the editor.
    pub fn set_input_track(&mut self, track: Option<Id>) {
        if self.input_track != track {
            self.input_track = track;
            self.send_plan();
        }
    }

    /// Apply an edit as one undo step and refresh playback.
    pub fn apply(&mut self, label: &str, edit: Edit) {
        self.history.apply(&mut self.project, label, edit);
        self.project_changed();
    }

    /// Apply a drag's current state; see [`History::preview`].
    pub fn preview(&mut self, label: &str, edit: Edit) {
        self.history.preview(&mut self.project, label, edit);
        self.timeline = self.project.timeline();
        // A fader drag is heard while it moves.
        self.send_plan();
    }

    /// End a drag started with [`Session::preview`].
    pub fn commit(&mut self) {
        self.history.commit();
        self.project_changed();
    }

    pub fn undo(&mut self) {
        self.status = match self.history.undo(&mut self.project) {
            Some(label) => format!("Undo {label}"),
            None => "Nothing to undo".to_string(),
        };
        self.project_changed();
    }

    pub fn redo(&mut self) {
        self.status = match self.history.redo(&mut self.project) {
            Some(label) => format!("Redo {label}"),
            None => "Nothing to redo".to_string(),
        };
        self.project_changed();
    }

    /// Open a MIDI file and start playing it.
    ///
    /// File pickers live in the shell: GPUI's prompts are asynchronous, and a
    /// blocking native dialog run from an event handler re-enters GPUI and
    /// panics ("RefCell already borrowed").
    pub fn load_midi_path(&mut self, path: &Path) {
        let is_midi = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("mid") || e.eq_ignore_ascii_case("midi"));
        if !is_midi {
            self.status = format!("{} is not a .surf, .mid or .midi file", path.display());
            return;
        }
        match surf_midi::smf::load(path) {
            Ok(song) => {
                let notes = song.note_count();
                self.set_project(Project::from_song(&song));
                self.audio.play(self.timeline.clone(), 0.0);
                self.status = format!(
                    "Loaded {} · {} notes on {} track{} · {:.1}s",
                    self.project.name,
                    notes,
                    self.project.tracks.len(),
                    if self.project.tracks.len() == 1 { "" } else { "s" },
                    self.timeline.duration
                );
            }
            Err(err) => self.status = format!("Could not load MIDI: {err:#}"),
        }
    }

    /// Save the project and every plugin's state to `path` (a `.surf` file).
    pub fn save_project_to(&mut self, path: PathBuf) {
        let mut plugin_states = std::collections::BTreeMap::new();
        let mut failed = Vec::new();
        for host in &mut self.rack.hosts {
            let tmp = rack::parked_state_path(&host.path).with_extension("save");
            match host.client.save_state(&tmp).and_then(|_| Ok(std::fs::read(&tmp)?)) {
                Ok(blob) => {
                    plugin_states.insert(host.path.clone(), blob);
                }
                Err(err) => failed.push(format!("{}: {err:#}", host.client.name)),
            }
            let _ = std::fs::remove_file(&tmp);
        }
        // Plugins no track plays right now, or that failed to load, keep the
        // state they were opened with.
        for t in &self.project.tracks {
            if let Some(p) = &t.instrument
                && !plugin_states.contains_key(p)
            {
                if let Some(blob) = self.project_states.get(p) {
                    plugin_states.insert(p.clone(), blob.clone());
                } else if let Ok(blob) = std::fs::read(rack::parked_state_path(p)) {
                    plugin_states.insert(p.clone(), blob);
                }
            }
        }
        let doc = Document {
            project: self.project.clone(),
            plugin_states,
        };
        match doc.save(&path) {
            Ok(()) => {
                self.history.mark_saved();
                self.status = format!("Saved {}", path.display());
                if !failed.is_empty() {
                    self.status
                        .push_str(&format!(" · without plugin state for {}", failed.join(", ")));
                }
                self.project_path = Some(path);
            }
            Err(err) => self.status = format!("Could not save: {err:#}"),
        }
    }

    /// Save to the file the project came from; false if it has none yet.
    pub fn save_project(&mut self) -> bool {
        match self.project_path.clone() {
            Some(path) => {
                self.save_project_to(path);
                true
            }
            None => false,
        }
    }

    /// Open a `.surf` project: its tracks, their instruments, and each
    /// plugin's saved sound.
    pub fn open_project(&mut self, path: &Path) {
        let doc = match Document::load(path) {
            Ok(doc) => doc,
            Err(err) => {
                self.status = format!("Could not open {}: {err:#}", path.display());
                return;
            }
        };
        self.audio.stop();
        // Every plugin restarts from the project's state.
        let hosts: Vec<u8> = self.rack.hosts.iter().map(|h| h.slot).collect();
        for slot in hosts {
            self.audio.set_slot(slot, None);
            self.rack.slots[slot as usize] = None;
        }
        self.rack.hosts.clear();
        self.rack.sent_cc7.clear();
        self.rack.failed.clear();
        self.project_states = doc.plugin_states;
        for p in self.project_states.keys() {
            let _ = std::fs::remove_file(rack::parked_state_path(p));
        }
        self.project = doc.project;
        self.history.clear();
        self.record_arm = None;
        self.record_region = None;
        self.project_path = Some(path.to_path_buf());
        self.status = format!("Opening {}…", path.display());
        self.project_changed();
        let missing: Vec<String> = self
            .project
            .tracks
            .iter()
            .filter_map(|t| t.instrument.as_ref())
            .filter(|p| self.rack.failed.contains(*p))
            .map(|p| {
                p.file_stem()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        self.status = if missing.is_empty() {
            format!(
                "Opened {} · {} track{}",
                self.project.name,
                self.project.tracks.len(),
                if self.project.tracks.len() == 1 { "" } else { "s" }
            )
        } else {
            format!(
                "Opened {} · {} not available here, playing the piano instead",
                self.project.name,
                missing.join(", ")
            )
        };
    }

    /// Open either kind of file the Open dialog offers.
    pub fn open_path(&mut self, path: &Path) {
        let is_project = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case(document::EXTENSION));
        if is_project {
            self.open_project(path);
        } else {
            self.load_midi_path(path);
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    /// Suggested file name for an export, without extension.
    pub fn export_stem(&self) -> String {
        let name = if self.project.name.is_empty() { "surf-export" } else { self.project.name.as_str() };
        let clean: String = name
            .chars()
            .map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' })
            .collect();
        if clean.trim().is_empty() { "surf-export".to_string() } else { clean }
    }

    pub fn can_export_midi(&mut self) -> bool {
        let any = self.project.tracks.iter().any(|t| t.regions.iter().any(|r| !r.notes.is_empty()));
        if !any {
            self.status = "Nothing to export yet".to_string();
        }
        any
    }

    pub fn export_midi_to(&mut self, path: &Path) {
        let song = self.project.to_song();
        match surf_midi::smf::save(&song, path) {
            Ok(()) => self.status = format!("MIDI exported to {}", path.display()),
            Err(err) => self.status = format!("MIDI export failed: {err:#}"),
        }
    }

    pub fn can_export_wav(&mut self) -> bool {
        if self.timeline.notes.is_empty() {
            self.status = "Nothing to render yet".to_string();
            return false;
        }
        true
    }

    pub fn export_wav_to(&mut self, path: PathBuf) {
        if !self.can_export_wav() {
            return;
        }
        let sample_rate = self.audio.sample_rate();
        let options = RenderOptions {
            sample_rate,
            tail_secs: 3.0,
            gain: 1.0,
        };
        let plan = rack::plan(&self.project, &self.rack.slots, &self.rack.failed);
        let gain_of = |slot: u8| {
            plan.gains
                .iter()
                .find(|(s, _)| *s == slot)
                .map_or(1.0, |(_, g)| *g)
        };

        // One stem per slot, from the notes routed to it.
        let mut by_slot: std::collections::BTreeMap<u8, Timeline> = Default::default();
        for n in &self.timeline.notes {
            let slot = plan.routes.get(n.track as usize).map_or(0, |r| r.slot);
            let mut n = *n;
            if let Some(r) = plan.routes.get(n.track as usize) {
                n.channel = r.channel;
            }
            by_slot
                .entry(slot)
                .or_insert_with(|| Timeline {
                    notes: Vec::new(),
                    duration: self.timeline.duration,
                })
                .notes
                .push(n);
        }
        let stem_dir = std::env::temp_dir().join(format!("surf-stems-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&stem_dir);
        let mut pianos = Vec::new();
        let mut stems = Vec::new();
        for (slot, timeline) in by_slot {
            let stem = stem_dir.join(format!("slot-{slot}.wav"));
            let host = self.rack.hosts.iter_mut().find(|h| h.slot == slot);
            match host {
                Some(host) => {
                    // Plugins render in their own host, here and now: the
                    // helper is busy until the file is written.
                    self.status = format!("Rendering {} in its plugin host…", host.client.name);
                    let notes = timeline
                        .notes
                        .iter()
                        .map(|n| RenderNote {
                            key: n.key,
                            velocity: n.velocity,
                            channel: n.channel,
                            start: n.start,
                            end: n.end,
                        })
                        .collect();
                    let controls = plan
                        .cc7
                        .iter()
                        .filter(|(s, _, _)| *s == slot)
                        .map(|&(_, channel, value)| (channel, 7, value))
                        .collect();
                    if let Err(err) = host.client.render(
                        &stem,
                        notes,
                        timeline.duration,
                        sample_rate,
                        options.tail_secs,
                        controls,
                    ) {
                        self.status = format!("{} render failed: {err:#}", host.client.name);
                        let _ = std::fs::remove_dir_all(&stem_dir);
                        return;
                    }
                    stems.push((stem, gain_of(slot)));
                }
                None => pianos.push((stem, timeline, gain_of(slot))),
            }
        }

        // The built-in pianos render off the UI thread, then everything mixes.
        let (tx, rx) = mpsc::channel();
        self.render_rx = Some(rx);
        self.rendering = true;
        self.status = "Rendering WAV…".to_string();
        let master = self.volume;
        std::thread::spawn(move || {
            let result = (|| {
                for (stem, timeline, gain) in pianos {
                    render_timeline_to_wav(&timeline, &stem, &options)?;
                    stems.push((stem, gain));
                }
                mix_stems(&stems, &path, sample_rate, master)
            })();
            let _ = std::fs::remove_dir_all(&stem_dir);
            let _ = tx.send(result);
        });
    }

    fn poll_render(&mut self) {
        let Some(rx) = self.render_rx.take() else {
            return;
        };
        match rx.try_recv() {
            Ok(Ok(stats)) => {
                self.rendering = false;
                self.status = format!(
                    "WAV ready: {} · {:.1}s · peak {:.0}%",
                    stats.path,
                    stats.duration,
                    stats.peak * 100.0
                );
            }
            Ok(Err(err)) => {
                self.rendering = false;
                self.status = format!("WAV render failed: {err:#}");
            }
            Err(TryRecvError::Empty) => self.render_rx = Some(rx),
            Err(TryRecvError::Disconnected) => {
                self.rendering = false;
                self.status = "WAV render thread stopped unexpectedly".to_string();
            }
        }
    }

    /// Load the plugin in `folder`, or list its plugins if there are several.
    pub fn load_vst_folder(&mut self, folder: &Path) {
        let vst2 = surf_plugin::format::VST2_EXTENSION;
        let mut found = vst_scan::scan_folder(folder);
        match found.len() {
            0 => {
                self.status = format!(
                    "No .vst3 or .{vst2} plugin in {} — press SCAN to search the standard folders",
                    folder.display()
                );
            }
            1 => {
                let entry = found.remove(0);
                self.load_vst_path(entry.path);
            }
            _ => {
                self.plugin_scan = found;
                self.status = format!(
                    "{} plugins in {} — pick one in the Library",
                    self.plugin_scan.len(),
                    folder.display()
                );
            }
        }
    }

    /// Walk the standard VST folders and show what is installed.
    ///
    /// Only the filesystem is touched here — no plugin is loaded — so this can
    /// run on the UI thread without risking a Kontakt abort.
    pub fn scan_plugins(&mut self) {
        self.plugin_scan = vst_scan::scan_plugin_paths();
        self.status = if self.plugin_scan.is_empty() {
            format!(
                "No .vst3 plugins found — add extra folders with {}",
                vst_scan::EXTRA_ROOTS_ENV
            )
        } else {
            format!(
                "Found {} plugin(s) — click one in the Library to load it",
                self.plugin_scan.len()
            )
        };
    }

    pub fn load_scanned_plugin(&mut self, index: usize) {
        let Some(entry) = self.plugin_scan.get(index) else {
            return;
        };
        let path = entry.path.clone();
        self.load_vst_path(path);
    }

    /// The track the instrument commands act on: the selected one, else the
    /// first.
    fn instrument_track(&self) -> Option<Id> {
        self.input_track
            .filter(|id| self.project.track(*id).is_some())
            .or_else(|| self.project.tracks.first().map(|t| t.id))
    }

    /// Put a plugin on the selected track (undoable). Another track already
    /// on the same plugin shares its host.
    pub fn load_vst_path(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        let Some(track) = self.instrument_track() else {
            self.status = "Add a track first".to_string();
            return;
        };
        // Picking a plugin again is how to retry one that failed.
        self.rack.failed.remove(&path);
        // Start the host first: if it fails, the track keeps its instrument
        // and nothing lands on the undo stack.
        if self.rack.host(&path).is_none() {
            let Some(slot) = self.rack.free_slot() else {
                self.status = "Every instrument slot is in use".to_string();
                return;
            };
            let Some(client) = self.start_host(&path) else {
                return;
            };
            self.audio.set_slot(slot, Some(Box::new(client.instrument())));
            self.rack.hosts.push(Host {
                path: path.clone(),
                client,
                slot,
            });
            self.rack.slots[slot as usize] = Some(Source::Plugin(path.clone()));
        }
        let status = std::mem::take(&mut self.status);
        self.set_instrument(track, Some(path.clone()));
        self.status = status;
        let shared = self
            .project
            .tracks
            .iter()
            .filter(|t| t.instrument.as_ref() == Some(&path))
            .count();
        if shared > 1 {
            self.status.push_str(&format!(
                " · shared by {shared} tracks, each on its own MIDI channel"
            ));
        }
        let editor = surf_plugin::format::detect(&path)
            .is_none_or(surf_plugin::format::Format::has_editor);
        if editor && shared == 1 && std::env::var_os("SURF_NO_EDITOR").is_none() {
            self.open_vst_editor();
        }
    }

    pub fn set_instrument(&mut self, track: Id, instrument: Option<PathBuf>) {
        let same = self
            .project
            .track(track)
            .is_some_and(|t| t.instrument == instrument);
        if !same {
            self.apply(
                "Change Instrument",
                Edit::SetTrack {
                    track,
                    field: TrackField::Instrument(instrument),
                },
            );
        }
    }

    /// The selected track's plugin host, if it plays one.
    fn track_host(&mut self) -> Option<&mut Host> {
        let track = self.instrument_track()?;
        let path = self.project.track(track)?.instrument.clone()?;
        self.rack.host_mut(&path)
    }

    pub fn open_vst_editor(&mut self) {
        let Some(host) = self.track_host() else {
            self.status = "This track plays the built-in piano".to_string();
            return;
        };
        let status = match host.client.open_editor() {
            Ok(()) => format!(
                "{} editor opened — tracks sharing it play on their own MIDI channels",
                host.client.name
            ),
            Err(err) => format!("Could not open plugin editor: {err:#}"),
        };
        self.status = status;
    }

    pub fn save_vst_state(&mut self) {
        let Some(host) = self.track_host() else {
            self.status = "This track plays the built-in piano".to_string();
            return;
        };
        let path = vst_state_path_for(&host.path);
        if let Some(parent) = path.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            self.status = format!(
                "Could not create VST state directory {}: {err}",
                parent.display()
            );
            return;
        }
        self.status = match host.client.save_state(&path) {
            Ok(bytes) => format!(
                "VST state saved in helper ({} bytes) to {}",
                bytes,
                path.display()
            ),
            Err(err) => format!("Could not save VST state: {err:#}"),
        };
    }

    /// Put the selected track back on the built-in piano.
    pub fn unload_vst(&mut self) {
        if let Some(track) = self.instrument_track() {
            self.set_instrument(track, None);
            self.status = "Using built-in piano".to_string();
        }
    }

    /// If a helper aborts (Kontakt is native code), the UI keeps running and
    /// the tracks on that plugin fall back to the built-in piano.
    fn poll_vst_health(&mut self) {
        let dead: Vec<PathBuf> = self
            .rack
            .hosts
            .iter_mut()
            .filter_map(|h| (!h.client.is_alive()).then(|| h.path.clone()))
            .collect();
        if dead.is_empty() {
            return;
        }
        for path in &dead {
            if let Some(i) = self.rack.hosts.iter().position(|h| &h.path == path) {
                let host = self.rack.hosts.remove(i);
                self.audio.set_slot(host.slot, None);
                self.rack.slots[host.slot as usize] = None;
                drop(host);
            }
            self.rack.failed.insert(path.clone());
        }
        self.sync_instruments();
        self.status = format!(
            "{} crashed; its tracks play the built-in piano · pick it again to retry",
            dead[0]
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
    }

    /// React to MIDI keyboards and audio outputs that come and go while the
    /// app runs.
    fn poll_devices(&mut self) {
        match self.midi.poll() {
            Some(PortChange::Connected(name)) => self.status = format!("MIDI: connected {name}"),
            Some(PortChange::Disconnected(name)) => {
                self.status = format!("MIDI: {name} was unplugged")
            }
            None => {}
        }
        for event in self.audio.drain_output_events() {
            self.status = match event {
                OutputEvent::Rerouted { device } => format!("Audio output switched to {device}"),
                OutputEvent::Lost => "Audio output device lost — sound is paused".to_string(),
                OutputEvent::Error(message) => format!("Audio output: {message}"),
                OutputEvent::Glitch => continue,
            };
        }
    }

    fn drain_midi(&mut self) {
        for msg in self.midi.drain() {
            self.handle_live(msg.event, msg.at, false);
        }
    }

    pub fn handle_live(&mut self, event: LiveEvent, at: Instant, send_audio: bool) {
        match event {
            LiveEvent::NoteOn { key, velocity } => {
                if self.recorder.is_recording() {
                    self.recorder.note_on(key, velocity, at);
                }
                self.live_notes.push(LiveNote {
                    key,
                    velocity,
                    start: at,
                    end: None,
                });
                if send_audio {
                    self.audio.note_on(key, velocity);
                }
            }
            LiveEvent::NoteOff { key } => {
                if self.recorder.is_recording() {
                    self.recorder.note_off(key, at);
                }
                if let Some(note) = self
                    .live_notes
                    .iter_mut()
                    .rev()
                    .find(|n| n.key == key && n.end.is_none())
                {
                    note.end = Some(at);
                }
                // The key is up but the pedal keeps the note sounding, so remember
                // it for the keyboard highlight until the pedal lifts.
                if self.sustain {
                    self.sustained_keys.insert(key);
                }
                if send_audio {
                    self.audio.note_off(key);
                }
            }
            LiveEvent::Sustain(down) => {
                self.sustain = down;
                if !down {
                    // Pedal up: everything it was holding stops ringing.
                    self.sustained_keys.clear();
                }
                if self.recorder.is_recording() {
                    self.recorder.sustain(down, at);
                }
                if send_audio {
                    self.audio.sustain(down);
                }
            }
        }
    }

    fn trim_live_notes(&mut self, now: Instant) {
        self.live_notes.retain(|n| {
            n.end
                .map(|end| now.duration_since(end).as_secs_f32() < 4.0)
                .unwrap_or(true)
        });
        if self.live_notes.len() > 512 {
            let excess = self.live_notes.len() - 512;
            self.live_notes.drain(0..excess);
        }
    }

    /// Advance per-frame state: drain MIDI, react to devices and plugin
    /// crashes, finish background renders, sample the engine.
    pub fn tick(&mut self) {
        if let Some(path) = self.pending_vst.take()
            && path.exists()
        {
            self.load_vst_path(path);
            self.history.clear();
        }
        self.drain_midi();
        self.poll_devices();
        self.poll_vst_health();
        self.poll_render();
        self.trim_live_notes(Instant::now());
        self.engine = self.audio.status();
        let duration = self.timeline.duration.max(self.engine.duration);
        if !self.engine.playing
            && duration > 0.0
            && self.engine.position >= duration - 0.05
            && self.status.starts_with("Playing")
        {
            self.status = "Finished — press play to restart".to_string();
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.is_recording()
    }

    pub fn record_secs(&self) -> f64 {
        self.recorder.elapsed(Instant::now())
    }

    /// Seconds into the song, clamped to its length.
    pub fn position(&self) -> f64 {
        let duration = self.timeline.duration.max(self.engine.duration);
        self.engine.position.clamp(0.0, duration.max(0.0))
    }

    /// Keys sounding right now: song notes under the playhead, held live
    /// notes, and notes the sustain pedal is holding.
    pub fn active_keys(&self) -> HashSet<u8> {
        let position = self.position();
        let mut keys: HashSet<u8> = self.timeline.active_at(position).map(|n| n.key).collect();
        keys.extend(self.live_notes.iter().filter(|n| n.end.is_none()).map(|n| n.key));
        keys.extend(self.sustained_keys.iter().copied());
        keys
    }

    /// Tempo at the start of the song.
    pub fn bpm(&self) -> f64 {
        self.project.tempos.first().map(|t| t.bpm).unwrap_or(120.0)
    }

    pub fn time_signature(&self) -> (u8, u8) {
        self.project.time_signature
    }

    pub fn seek(&mut self, secs: f64) {
        let target = secs.clamp(0.0, self.timeline.duration.max(0.0));
        self.audio.seek(target);
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.5);
        self.audio.set_volume(self.volume);
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// Last `lines` non-empty lines of the helper log, for the status bar.
///
/// Kontakt writes its own diagnostics to the helper's stderr (in the system
/// code page, not UTF-8), so read bytes and decode lossily.
/// Lines that show up on *successful* loads too, so they say nothing about a failure.
///
/// Kontakt logs all of these while loading fine (verified against helper logs that end in
/// `plugin loaded`). Surfacing them as "helper says: ..." actively misleads, because they
/// crowd out the line that does matter. Keep this list tight — only add a pattern once a
/// successful load has been seen emitting it.
const HELPER_LOG_NOISE: &[&str] = &[
    "Failed to read install_state",
    "cannot resolve resource:",
    "Failed to locate 'Win32_ComputerSystemProduct'",
    "DoVMLock:",
    "DoVMUnLock:",
];

/// The last `lines` interesting lines of the helper log, newest last, newline-separated.
///
/// Drops the per-run banner and the known-harmless plugin chatter (see [`HELPER_LOG_NOISE`]).
/// If *everything* was noise we fall back to the raw tail rather than returning nothing — an
/// unexpected message is better than silence.
fn helper_log_tail(lines: usize) -> String {
    let Ok(bytes) = std::fs::read(helper_log_path()) else {
        return String::new();
    };
    let text = decode_helper_log(&bytes);

    let is_candidate = |line: &&str| {
        let line = line.trim();
        !line.is_empty() && !line.starts_with("--- helper for ")
    };
    let interesting: Vec<&str> = text
        .lines()
        .filter(is_candidate)
        .filter(|line| !HELPER_LOG_NOISE.iter().any(|noise| line.contains(noise)))
        .collect();
    let pool = if interesting.is_empty() {
        text.lines().filter(is_candidate).collect()
    } else {
        interesting
    };

    let start = pool.len().saturating_sub(lines);
    pool[start..].join("\n")
}

/// Decode the helper log, which mixes our own ASCII with whatever encoding the plugin used.
///
/// Native Instruments writes Windows error strings in the system ANSI codepage (GBK on this
/// machine), so a plain `from_utf8_lossy` turns "拒绝访问" into a row of replacement
/// characters. Decode per line: valid UTF-8 is kept as-is, and anything else falls back to
/// cp936/GBK, which covers the NI logger's output. Undecodable bytes still become U+FFFD.
fn decode_helper_log(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for (i, raw) in bytes.split(|&b| b == b'\n').enumerate() {
        if i > 0 {
            out.push('\n');
        }
        let raw = raw.strip_suffix(b"\r").unwrap_or(raw);
        match std::str::from_utf8(raw) {
            Ok(line) => out.push_str(line),
            Err(_) => out.push_str(&decode_gbk_lossy(raw)),
        }
    }
    out
}

/// Minimal GBK/cp936 decoder: ASCII passes through, two-byte sequences in the GBK lead/trail
/// range are decoded via the OS, and stray bytes become U+FFFD.
///
/// Uses `MultiByteToWideChar` so we get the real codepage tables instead of shipping our own.
#[cfg(windows)]
fn decode_gbk_lossy(bytes: &[u8]) -> String {
    use windows::Win32::Globalization::MultiByteToWideChar;
    const CP_GBK: u32 = 936;
    if bytes.is_empty() {
        return String::new();
    }
    // SAFETY: both calls pass a valid slice and honour the length the first call returns.
    unsafe {
        let needed = MultiByteToWideChar(CP_GBK, Default::default(), bytes, None);
        if needed <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut wide = vec![0u16; needed as usize];
        let written = MultiByteToWideChar(CP_GBK, Default::default(), bytes, Some(&mut wide));
        if written <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        String::from_utf16_lossy(&wide[..written as usize])
    }
}

#[cfg(not(windows))]
fn decode_gbk_lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn piano(sample_rate: u32) -> Synth {
    let mut synth = Synth::new(sample_rate as f32);
    synth.set_master_gain(0.9);
    synth
}

/// Status line for a plugin that would not load, mirrored to stderr: the
/// status bar truncates, and a terminal transcript is what goes into a bug
/// report.
fn load_failure(path: &Path, err: &anyhow::Error) -> String {
    let tail = helper_log_tail(3);
    eprintln!("---- VST helper load failed ----");
    eprintln!("plugin: {}", path.display());
    eprintln!("error : {err:#}");
    if !tail.is_empty() {
        eprintln!("helper log tail:");
        for line in tail.lines() {
            eprintln!("  {line}");
        }
    }
    eprintln!("full log: {}", helper_log_path().display());
    eprintln!("--------------------------------");
    if tail.is_empty() {
        format!(
            "VST helper load failed for {}: {err:#} (helper log: {})",
            path.display(),
            helper_log_path().display()
        )
    } else {
        format!(
            "VST helper load failed: {err:#} · helper says: {} (log: {})",
            tail.replace('\n', " | "),
            helper_log_path().display()
        )
    }
}

fn vst_state_path_for(plugin_path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    plugin_path
        .to_string_lossy()
        .to_lowercase()
        .hash(&mut hasher);
    surf_platform::data_dir().join(format!("vst-{:016x}.state", hasher.finish()))
}



#[cfg(test)]
mod tests {
    use super::*;

    /// The path the file picker hands back is loaded, played and named.
    #[test]
    fn loading_a_midi_path_replaces_the_song() {
        let dir = std::env::temp_dir().join(format!("surf-session-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("Two Notes.mid");
        let mut song = Song::empty();
        song.name = "Two Notes".into();
        song.notes = vec![
            surf_core::song::Note { key: 60, velocity: 100, channel: 0, start_tick: 0, duration_ticks: 480 },
            surf_core::song::Note { key: 67, velocity: 90, channel: 0, start_tick: 480, duration_ticks: 480 },
        ];
        surf_midi::smf::save(&song, &path).unwrap();

        let mut session = Session::new();
        session.load_midi_path(&path);
        assert_eq!(session.project.name, "Two Notes", "status: {}", session.status);
        assert_eq!(session.timeline.notes.len(), 2);

        // A file that is not MIDI is refused with a message, not loaded.
        let txt = dir.join("notes.txt");
        std::fs::write(&txt, b"hi").unwrap();
        session.load_midi_path(&txt);
        assert_eq!(session.timeline.notes.len(), 2);
        assert!(session.status.contains("is not a .surf"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An edit reaches playback, and undo puts playback back.
    #[test]
    fn edits_and_undo_refresh_what_plays() {
        let mut s = Session::new();
        let region = s.project.tracks[0].regions[0].id;
        let first = s.project.tracks[0].regions[0].notes[0].clone();
        let before = s.timeline.notes.len();
        s.apply("Delete Note", Edit::DeleteNotes { region, notes: vec![first.id] });
        assert_eq!(s.timeline.notes.len(), before - 1);
        assert_eq!(s.history.undo_label(), Some("Delete Note"));
        s.undo();
        assert_eq!(s.timeline.notes.len(), before);
        assert!(s.status.contains("Undo Delete Note"));
        s.redo();
        assert_eq!(s.timeline.notes.len(), before - 1);
    }

    /// Muting a track takes its notes out of playback (undoably).
    #[test]
    fn mute_changes_playback() {
        let mut s = Session::new();
        let t = s.project.tracks[0].id;
        let before = s.timeline.notes.len();
        assert!(before > 0);
        s.apply("Mute", Edit::SetTrack { track: t, field: surf_core::edit::TrackField::Mute(true) });
        assert_eq!(s.timeline.notes.len(), 0);
        s.undo();
        assert_eq!(s.timeline.notes.len(), before);
    }

    /// A recorded take lands as a new region on the record track.
    #[test]
    fn recording_adds_a_take_region() {
        let mut s = Session::new();
        let track = s.project.tracks[0].id;
        let regions = s.project.tracks[0].regions.len();
        let mut take = Song::empty();
        take.notes.push(surf_core::song::Note { key: 64, velocity: 100, channel: 0, start_tick: 0, duration_ticks: 240 });
        s.add_take(&take);
        let t = s.project.track(track).unwrap();
        assert_eq!(t.regions.len(), regions + 1);
        let new = t.regions.last().unwrap();
        assert_eq!(new.name, "Take 1");
        assert_eq!(new.notes.len(), 1);
        assert_eq!(s.record_region, Some(new.id));
        s.undo();
        assert_eq!(s.project.track(track).unwrap().regions.len(), regions);
    }

    fn two_tracks() -> Session {
        let mut s = Session::new();
        let mut song = Song::demo();
        for n in song.notes.iter_mut().filter(|n| n.key < 55) {
            n.channel = 1;
        }
        s.set_project(Project::from_song(&song));
        s
    }

    fn pianos(s: &Session) -> Vec<Id> {
        s.rack
            .slots
            .iter()
            .flatten()
            .filter_map(|src| match src {
                Source::Piano(id) => Some(*id),
                Source::Plugin(_) => None,
            })
            .collect()
    }

    /// Each track plays its own piano, so its fader is a real gain stage;
    /// deleting a track frees its slot, and undo brings it back.
    #[test]
    fn every_track_gets_its_own_instrument_slot() {
        let mut s = two_tracks();
        let ids: Vec<Id> = s.project.tracks.iter().map(|t| t.id).collect();
        assert_eq!(pianos(&s), ids);
        let plan = rack::plan(&s.project, &s.rack.slots, &s.rack.failed);
        assert_ne!(plan.routes[0].slot, plan.routes[1].slot);
        assert_eq!(plan.routes[1].channel, 1);

        s.apply("Delete Track", Edit::RemoveTrack { track: ids[1] });
        assert_eq!(pianos(&s), vec![ids[0]]);
        s.undo();
        assert_eq!(pianos(&s).len(), 2);
    }

    /// Volume, mute and solo change the mix, not the notes that play.
    #[test]
    fn mixing_does_not_touch_the_timeline() {
        let mut s = two_tracks();
        let before = s.timeline.clone();
        let t = s.project.tracks[0].id;
        s.apply("Volume", Edit::SetTrack { track: t, field: TrackField::Volume(0.4) });
        assert_eq!(s.timeline, before, "velocities stay as written");
        let plan = rack::plan(&s.project, &s.rack.slots, &s.rack.failed);
        assert!(plan.gains.contains(&(plan.routes[0].slot, 0.4)));
    }

    /// A plugin that will not load leaves the track as it was, with no undo
    /// step, and says why.
    #[test]
    fn a_plugin_that_fails_to_load_changes_nothing() {
        let mut s = two_tracks();
        let before = s.project.clone();
        let label = s.history.undo_label().map(str::to_string);
        s.load_vst_path(PathBuf::from("/nonexistent/Missing.vst3"));
        assert_eq!(s.project, before);
        assert_eq!(s.history.undo_label().map(str::to_string), label);
        assert!(s.status.contains("failed"), "status: {}", s.status);
        assert!(s.rack.hosts.is_empty());
    }

    /// Two tracks on one real Kontakt share a single host on their own
    /// channels, and export renders through it. Needs Kontakt installed:
    /// `cargo test -p surf-ui kontakt -- --ignored`.
    #[test]
    #[ignore]
    fn tracks_share_one_kontakt_host() {
        let kontakt = PathBuf::from("/Library/Audio/Plug-Ins/VST3/Kontakt 8.vst3");
        if !kontakt.exists() {
            return;
        }
        // SAFETY: set before any thread reads it.
        unsafe { std::env::set_var("SURF_NO_EDITOR", "1") };
        let mut s = two_tracks();
        let ids: Vec<Id> = s.project.tracks.iter().map(|t| t.id).collect();
        for id in &ids {
            s.set_input_track(Some(*id));
            s.load_vst_path(kontakt.clone());
            assert!(s.rack.host(&kontakt).is_some(), "status: {}", s.status);
        }
        assert_eq!(s.rack.hosts.len(), 1, "one host for both tracks");
        assert!(pianos(&s).is_empty());
        let plan = rack::plan(&s.project, &s.rack.slots, &s.rack.failed);
        assert_eq!(plan.routes[0].slot, plan.routes[1].slot);
        assert_eq!((plan.routes[0].channel, plan.routes[1].channel), (0, 1));
        assert_eq!(plan.cc7.len(), 2);

        let out = std::env::temp_dir().join(format!("surf-kontakt-{}.wav", std::process::id()));
        s.export_wav_to(out.clone());
        while s.rendering {
            s.poll_render();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.status.starts_with("WAV ready"), "status: {}", s.status);
        assert!(out.exists());
        std::fs::remove_file(&out).ok();

        // Back to the piano on one track: the host stays for the other.
        s.set_input_track(Some(ids[0]));
        s.unload_vst();
        assert_eq!(s.rack.hosts.len(), 1);
        s.set_input_track(Some(ids[1]));
        s.unload_vst();
        assert!(s.rack.hosts.is_empty(), "no track uses it: the host stops");
        s.undo();
        assert_eq!(s.rack.hosts.len(), 1, "undo brings it back");

        // Saved and reopened, both tracks are on one Kontakt again, started
        // from the state in the file. (Undo put only track 2 back.)
        s.set_input_track(Some(ids[0]));
        s.load_vst_path(kontakt.clone());
        let path = temp_project("kontakt");
        s.save_project_to(path.clone());
        let doc = Document::load(&path).unwrap();
        assert!(doc.plugin_states.get(&kontakt).is_some_and(|b| !b.is_empty()));
        let mut t = Session::new();
        t.open_project(&path);
        assert!(t.status.starts_with("Opened"), "status: {}", t.status);
        assert_eq!(t.rack.hosts.len(), 1);
        assert!(t.project.tracks.iter().all(|tr| tr.instrument.as_ref() == Some(&kontakt)));
        std::fs::remove_file(path).ok();
    }

    fn temp_project(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("surf-{tag}-{}.surf", std::process::id()))
    }

    /// Save, edit, reopen: the file brings back the tracks and mix as saved,
    /// with a clean undo stack.
    #[test]
    fn a_saved_project_opens_as_it_was() {
        let path = temp_project("roundtrip");
        let mut s = two_tracks();
        let t = s.project.tracks[1].id;
        s.apply("Volume", Edit::SetTrack { track: t, field: TrackField::Volume(0.3) });
        assert!(s.is_dirty());
        s.save_project_to(path.clone());
        assert!(!s.is_dirty(), "status: {}", s.status);
        let saved = s.project.clone();

        s.apply("Mute", Edit::SetTrack { track: t, field: TrackField::Mute(true) });
        assert!(s.is_dirty());
        s.open_path(&path);
        assert_eq!(s.project, saved, "status: {}", s.status);
        assert!(!s.is_dirty());
        assert_eq!(s.history.undo_label(), None);
        assert_eq!(s.project_path.as_deref(), Some(path.as_path()));
        assert_eq!(pianos(&s).len(), 2);

        // ⌘S goes back to the same file.
        s.apply("Mute", Edit::SetTrack { track: t, field: TrackField::Mute(true) });
        assert!(s.save_project());
        assert!(Document::load(&path).unwrap().project.tracks[1].mute);
        std::fs::remove_file(path).ok();
    }

    /// A project whose plugin isn't installed opens on the piano, keeps the
    /// plugin on the track, and saves its state back untouched.
    #[test]
    fn a_missing_plugin_plays_the_piano_and_keeps_its_state() {
        let path = temp_project("missing");
        let ghost = PathBuf::from("/nonexistent/Ghost.vst3");
        let mut project = two_tracks().project;
        project.tracks[0].instrument = Some(ghost.clone());
        let doc = Document {
            project,
            plugin_states: [(ghost.clone(), vec![7, 7, 7])].into(),
        };
        doc.save(&path).unwrap();

        let mut s = Session::new();
        s.open_project(&path);
        assert!(s.status.contains("Ghost not available"), "status: {}", s.status);
        assert_eq!(s.project.tracks[0].instrument.as_ref(), Some(&ghost));
        assert!(s.rack.failed.contains(&ghost));
        s.save_project_to(path.clone());
        let back = Document::load(&path).unwrap();
        assert_eq!(back.plugin_states.get(&ghost), Some(&vec![7, 7, 7]));
        std::fs::remove_file(path).ok();
    }

    /// Export writes every track, on its own channel.
    #[test]
    fn export_round_trips_tracks() {
        let dir = std::env::temp_dir().join(format!("surf-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut s = Session::new();
        let mut song = Song::demo();
        for n in song.notes.iter_mut().filter(|n| n.key < 55) {
            n.channel = 1;
        }
        s.set_project(Project::from_song(&song));
        assert_eq!(s.project.tracks.len(), 2);
        let path = dir.join("out.mid");
        s.export_midi_to(&path);
        let back = Project::from_song(&surf_midi::smf::load(&path).unwrap());
        assert_eq!(back.tracks.len(), 2);
        let count = |p: &Project| p.tracks.iter().map(|t| t.regions[0].notes.len()).collect::<Vec<_>>();
        assert_eq!(count(&back), count(&s.project));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
