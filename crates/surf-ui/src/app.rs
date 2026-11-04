//! The GPUI application shell: transport, live/play views and the piano roll.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Instant;

use gpui::{
    AnyElement, ClickEvent, Context, FocusHandle, FontWeight, KeyDownEvent, KeyUpEvent,
    MouseButton, MouseDownEvent, Render, Window, div, prelude::*, px, rgb, rgba,
};
use rfd::FileDialog;

use surf_audio::render::{RenderOptions, render_timeline_to_wav};
use surf_audio::{AudioEngine, Command, Synth};
use surf_core::recorder::Recorder;
use surf_core::song::{Song, TimedNote, Timeline, key_name};
use surf_midi::{LiveEvent, MidiHub, NoteSink};
use surf_plugin::client::{RenderNote, VstClient, helper_log_path};
use surf_plugin::scan::{self as vst_scan, PluginEntry};

const PAD: f32 = 16.0;
const GAP: f32 = 12.0;
const SIDEBAR: f32 = 268.0;
const KEYBOARD_H: f32 = 146.0;
const TOPBAR_H: f32 = 64.0;
const TRANSPORT_H: f32 = 60.0;

const BG: u32 = 0x07090e;
const PANEL: u32 = 0x0d111a;
const PANEL_2: u32 = 0x111725;
const BORDER: u32 = 0x1c2433;
const TEXT: u32 = 0xe8edf6;
const MUTED: u32 = 0x7d879b;
const ACCENT: u32 = 0x5ce1ff;
const ACCENT_2: u32 = 0xff4d8d;
const GREEN: u32 = 0x5cf2a5;
const AMBER: u32 = 0xffc857;

const KEY_COLORS: [u32; 12] = [
    0x5ce1ff, 0x53d3d9, 0x63e6a0, 0x9be36a, 0xf4e36b, 0xffb35c, 0xff8a5c, 0xff5c8d, 0xe05cff,
    0xa45cff, 0x6f7cff, 0x4fb7ff,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Live,
    Play,
}

struct LiveNote {
    key: u8,
    velocity: u8,
    start: Instant,
    end: Option<Instant>,
}

#[derive(Clone, Copy, Default)]
struct KeyGeom {
    x: f32,
    w: f32,
    black: bool,
}

pub struct SurfApp {
    audio: AudioEngine,
    midi: MidiHub,
    recorder: Recorder,
    focus_handle: FocusHandle,
    mode: Mode,
    song: Option<Song>,
    timeline: Timeline,
    song_name: String,
    live_notes: Vec<LiveNote>,
    computer_notes: HashMap<String, u8>,
    vst: Option<VstClient>,
    render_rx: Option<Receiver<anyhow::Result<surf_audio::render::RenderStats>>>,
    rendering: bool,
    pending_vst: Option<PathBuf>,
    /// Results of the last `SCAN`, shown in the plugin picker.
    plugin_scan: Vec<PluginEntry>,
    plugins_open: bool,
    loop_playback: bool,
    volume: f32,
    status: String,
    last_peak: f32,
    /// Sustain pedal state, for the UI indicator and the held-note highlight.
    sustain: bool,
    /// Keys whose note-off arrived while the pedal was down. They sound until the
    /// pedal lifts, so the keyboard should keep showing them as ringing.
    sustained_keys: HashSet<u8>,
}

impl SurfApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut synth = Synth::new(48_000.0);
        synth.set_master_gain(0.9);
        let audio = AudioEngine::new(Box::new(synth));
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
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        // Start with a short demo loaded (paused) so the piano roll has
        // something to show before the user opens their own file.
        let demo = Song::demo();
        let timeline = demo.timeline();
        let song_name = demo.name.clone();
        let status = if midi.ports().is_empty() {
            format!(
                "{} · no MIDI input detected — load a MIDI file or use the computer keyboard",
                audio.status().output
            )
        } else {
            format!(
                "{} · MIDI: {}",
                audio.status().output,
                midi.ports().first().cloned().unwrap_or_default()
            )
        };
        // `SURF_OPEN_PLUGINS=1` opens the picker on the first frame, which is
        // handy for scripted smoke tests and for people who always start from
        // the same shortlist.
        let open_plugins = std::env::var_os("SURF_OPEN_PLUGINS").is_some();
        let plugin_scan = if open_plugins {
            vst_scan::scan_plugin_paths()
        } else {
            Vec::new()
        };
        let app = Self {
            audio,
            midi,
            recorder: Recorder::new(),
            focus_handle,
            mode: Mode::Play,
            song: Some(demo),
            timeline,
            song_name,
            live_notes: Vec::new(),
            computer_notes: HashMap::new(),
            vst: None,
            render_rx: None,
            rendering: false,
            pending_vst: std::env::var_os("SURF_VST").map(PathBuf::from),
            plugin_scan,
            plugins_open: open_plugins,
            sustain: false,
            sustained_keys: HashSet::new(),
            loop_playback: false,
            volume: 0.9,
            status,
            last_peak: 0.0,
        };
        app
    }

    fn set_mode(&mut self, mode: Mode) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        if mode == Mode::Live {
            self.audio.stop();
            self.status = "Live mode — play your MIDI keyboard or A–K".to_string();
        } else if self.song.is_none() {
            self.status = "Play mode: load a MIDI file first".to_string();
        } else {
            self.status = "Play mode — Space to play".to_string();
        }
    }

    fn toggle_play(&mut self) {
        if self.song.is_none() {
            self.status = "Load a MIDI file before pressing play".to_string();
            return;
        }
        self.mode = Mode::Play;
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

    fn stop_playback(&mut self) {
        self.audio.stop();
        self.status = "Stopped".to_string();
    }

    #[allow(dead_code)]
    fn restart(&mut self) {
        if self.song.is_none() {
            return;
        }
        self.audio.play(self.timeline.clone(), 0.0);
        self.mode = Mode::Play;
        self.status = "Playing from start".to_string();
    }

    fn seek_relative(&mut self, delta: f64) {
        let status = self.audio.status();
        let target = (status.position + delta).clamp(0.0, self.timeline.duration.max(0.0));
        self.audio.seek(target);
        self.status = format!("Seek {:.1}s", target);
    }

    fn toggle_loop(&mut self) {
        self.loop_playback = !self.loop_playback;
        self.audio.set_loop(self.loop_playback);
        self.status = if self.loop_playback {
            "Loop enabled".to_string()
        } else {
            "Loop disabled".to_string()
        };
    }

    fn toggle_record(&mut self) {
        if self.recorder.is_recording() {
            let notes = self.recorder.finish(Instant::now());
            if notes.is_empty() {
                self.status = "Recording stopped: no notes captured".to_string();
                return;
            }
            let name = format!("Take {}", self.song_name_or("Untitled"));
            let song = Song::from_recorded(name, &notes);
            self.timeline = song.timeline();
            self.song = Some(song);
            self.song_name = format!("Recorded take · {} notes", notes.len());
            self.mode = Mode::Play;
            self.audio.stop();
            self.status = format!("Recorded {} notes — press play or export MIDI", notes.len());
        } else {
            self.recorder.start(Instant::now());
            self.live_notes.clear();
            self.mode = Mode::Live;
            self.audio.stop();
            self.status = "Recording… play your MIDI keyboard".to_string();
        }
    }

    fn song_name_or(&self, fallback: &str) -> String {
        self.song
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| fallback.to_string())
    }

    fn load_demo(&mut self) {
        let song = Song::demo();
        self.timeline = song.timeline();
        self.song_name = song.name.clone();
        self.song = Some(song);
        self.mode = Mode::Play;
        self.audio.stop();
        self.audio.play(self.timeline.clone(), 0.0);
        self.status = "Demo loaded — 4 bars of Am–F–C–G".to_string();
    }

    fn load_midi_dialog(&mut self) {
        let Some(path) = FileDialog::new()
            .add_filter("MIDI", &["mid", "midi"])
            .set_title("Open a MIDI file")
            .pick_file()
        else {
            return;
        };
        match surf_midi::smf::load(&path) {
            Ok(song) => {
                self.timeline = song.timeline();
                self.song_name = song.name.clone();
                let notes = song.note_count();
                let duration = self.timeline.duration;
                self.song = Some(song);
                self.mode = Mode::Play;
                self.audio.stop();
                self.audio.play(self.timeline.clone(), 0.0);
                self.status = format!(
                    "Loaded {} · {} notes · {:.1}s",
                    self.song_name, notes, duration
                );
            }
            Err(err) => self.status = format!("Could not load MIDI: {err:#}"),
        }
    }

    fn export_midi_dialog(&mut self) {
        let Some(song) = &self.song else {
            self.status = "Nothing to export yet".to_string();
            return;
        };
        let Some(path) = FileDialog::new()
            .add_filter("MIDI", &["mid"])
            .set_file_name(&format!("{}.mid", song.name))
            .set_title("Export MIDI")
            .save_file()
        else {
            return;
        };
        match surf_midi::smf::save(song, &path) {
            Ok(()) => self.status = format!("MIDI exported to {}", path.display()),
            Err(err) => self.status = format!("MIDI export failed: {err:#}"),
        }
    }

    fn export_wav_dialog(&mut self) {
        if self.timeline.notes.is_empty() {
            self.status = "Nothing to render yet".to_string();
            return;
        }
        let Some(path) = FileDialog::new()
            .add_filter("WAV", &["wav"])
            .set_file_name("surf-export.wav")
            .set_title("Render audio to WAV")
            .save_file()
        else {
            return;
        };
        let timeline = self.timeline.clone();
        let sample_rate = self.audio.sample_rate();
        let options = RenderOptions {
            sample_rate,
            tail_secs: 3.0,
            gain: self.volume,
        };

        if let Some(vst) = &mut self.vst {
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
                .collect::<Vec<_>>();
            self.rendering = true;
            self.status = "Rendering WAV through the isolated VST helper…".to_string();
            let result = vst.render(
                &path,
                notes,
                timeline.duration,
                sample_rate,
                options.tail_secs,
            );
            self.rendering = false;
            match result {
                Ok((path, duration, peak)) => {
                    self.status = format!(
                        "WAV ready: {} · {:.1}s · peak {:.0}%",
                        path,
                        duration,
                        peak * 100.0
                    );
                }
                Err(err) => self.status = format!("VST helper render failed: {err:#}"),
            }
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.render_rx = Some(rx);
        self.rendering = true;
        self.status = "Rendering WAV with the built-in piano…".to_string();
        std::thread::spawn(move || {
            let result = render_timeline_to_wav(&timeline, &path, &options);
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

    fn load_vst_dialog(&mut self) {
        let vst2 = surf_plugin::format::VST2_EXTENSION;
        let Some(folder) = FileDialog::new()
            .set_title(format!("Pick a folder containing a .vst3 or .{vst2} plugin"))
            .pick_folder()
        else {
            return;
        };
        let mut found = vst_scan::scan_folder(&folder);
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
                self.plugins_open = true;
                self.status = format!(
                    "{} plugins in {} — pick one in the list",
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
    fn scan_plugins(&mut self) {
        self.plugin_scan = vst_scan::scan_plugin_paths();
        self.plugins_open = true;
        self.status = if self.plugin_scan.is_empty() {
            format!(
                "No .vst3 plugins found — add extra folders with {}",
                vst_scan::EXTRA_ROOTS_ENV
            )
        } else {
            format!(
                "Found {} plugin(s) — click one to load it in the helper",
                self.plugin_scan.len()
            )
        };
    }

    fn close_plugins(&mut self) {
        self.plugins_open = false;
        self.status = "Plugin picker closed".to_string();
    }

    fn load_scanned_plugin(&mut self, index: usize) {
        let Some(entry) = self.plugin_scan.get(index) else {
            return;
        };
        let path = entry.path.clone();
        self.plugins_open = false;
        self.load_vst_path(path);
    }

    fn load_vst_path(&mut self, path: PathBuf) {
        self.status = format!("Loading {} in helper…", path.display());
        let old = self.vst.take();
        let state_path = vst_state_path_for(&path);
        let use_state = std::env::var_os("SURF_NO_STATE").is_none() && state_path.exists();
        match VstClient::load(
            &path,
            self.audio.sample_rate(),
            if use_state {
                Some(state_path.as_path())
            } else {
                None
            },
        ) {
            Ok(vst) => {
                let name = vst.name.clone();
                self.audio.set_instrument(Box::new(vst.instrument()));
                self.status = if use_state {
                    format!(
                        "Loaded VST helper: {} (state from {})",
                        name,
                        state_path.display()
                    )
                } else {
                    format!("Loaded VST helper: {name}")
                };
                self.vst = Some(vst);
                drop(old);
                let has_editor = surf_plugin::format::detect(&path)
                    .is_none_or(surf_plugin::format::Format::has_editor);
                if !has_editor {
                    self.status.push_str(" · no editor on this platform yet");
                } else if std::env::var_os("SURF_NO_EDITOR").is_none() {
                    self.open_vst_editor();
                }
            }
            Err(err) => {
                self.vst = old;
                let tail = helper_log_tail(3);
                // Mirror the failure to stderr: the status bar truncates, and a terminal
                // transcript is what you actually paste into a bug report.
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
                self.status = if tail.is_empty() {
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
                };
            }
        }
    }

    fn open_vst_editor(&mut self) {
        let Some(vst) = &mut self.vst else {
            self.status = "Load a VST instrument first".to_string();
            return;
        };
        match vst.open_editor() {
            Ok(()) => {
                self.status =
                    "Plugin editor opened in helper — load your Kontakt instrument there"
                        .to_string();
            }
            Err(err) => self.status = format!("Could not open plugin editor: {err:#}"),
        }
    }

    fn save_vst_state(&mut self) {
        let Some(vst) = &mut self.vst else {
            self.status = "No VST instrument loaded".to_string();
            return;
        };
        let path = vst_state_path_for(&vst.path);
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                self.status = format!(
                    "Could not create VST state directory {}: {err}",
                    parent.display()
                );
                return;
            }
        }
        match vst.save_state(&path) {
            Ok(bytes) => {
                self.status = format!(
                    "VST state saved in helper ({} bytes) to {}",
                    bytes,
                    path.display()
                )
            }
            Err(err) => self.status = format!("Could not save VST state: {err:#}"),
        }
    }

    fn unload_vst(&mut self) {
        let old = self.vst.take();
        self.audio
            .set_instrument(Box::new(Synth::new(self.audio.sample_rate() as f32)));
        drop(old);
        self.status = "Using built-in piano".to_string();
    }

    /// If the helper aborts (Kontakt is native code), the UI process keeps
    /// running and falls back to the built-in piano.
    fn poll_vst_health(&mut self) {
        let Some(vst) = &mut self.vst else {
            return;
        };
        if vst.is_alive() {
            return;
        }
        let dead = self.vst.take();
        self.audio
            .set_instrument(Box::new(Synth::new(self.audio.sample_rate() as f32)));
        drop(dead);
        self.status = "VST helper crashed; switched to built-in piano".to_string();
    }

    fn drain_midi(&mut self) {
        for msg in self.midi.drain() {
            self.handle_live(msg.event, msg.at, false);
        }
    }

    fn handle_live(&mut self, event: LiveEvent, at: Instant, send_audio: bool) {
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

    fn key_to_note(key: &str) -> Option<u8> {
        Some(match key {
            "a" => 60,
            "w" => 61,
            "s" => 62,
            "e" => 63,
            "d" => 64,
            "f" => 65,
            "t" => 66,
            "g" => 67,
            "y" => 68,
            "h" => 69,
            "u" => 70,
            "j" => 71,
            "k" => 72,
            "o" => 73,
            "l" => 74,
            "p" => 75,
            ";" => 76,
            _ => return None,
        })
    }

    fn key_down(&mut self, event: &KeyDownEvent) {
        if event.is_held {
            return;
        }
        let key = event.keystroke.key.to_lowercase();
        if key == "escape" && self.plugins_open {
            self.close_plugins();
            return;
        }
        if event.keystroke.modifiers.control && key == "s" {
            self.save_vst_state();
            return;
        }
        if key == "space" {
            self.toggle_play();
            return;
        }
        // Shift acts as a sustain pedal, so the pedal path is testable without
        // hardware (and usable when the keyboard has no pedal jack).
        if key == "shift" && !self.sustain {
            self.handle_live(LiveEvent::Sustain(true), Instant::now(), true);
            return;
        }
        let Some(note) = Self::key_to_note(&key) else {
            return;
        };
        if self.computer_notes.insert(key, note).is_none() {
            self.handle_live(
                LiveEvent::NoteOn {
                    key: note,
                    velocity: 100,
                },
                Instant::now(),
                true,
            );
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent) {
        let key = event.keystroke.key.to_lowercase();
        if key == "shift" && self.sustain {
            self.handle_live(LiveEvent::Sustain(false), Instant::now(), true);
            return;
        }
        if let Some(note) = self.computer_notes.remove(&key) {
            self.handle_live(LiveEvent::NoteOff { key: note }, Instant::now(), true);
        }
    }
}

impl Render for SurfApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.request_animation_frame();
        // Load a startup instrument only after the first frame; doing it in the
        // constructor can block the platform from ever showing the window.
        if let Some(path) = self.pending_vst.take() {
            if path.exists() {
                self.load_vst_path(path);
            }
        }
        self.drain_midi();
        self.poll_vst_health();
        self.poll_render();
        let now = Instant::now();
        self.trim_live_notes(now);
        let status = self.audio.status();
        self.last_peak = status.peak;
        let duration = self.timeline.duration.max(status.duration);
        let position = status.position.clamp(0.0, duration.max(0.0));
        let playing = status.playing;
        if !playing
            && duration > 0.0
            && position >= duration - 0.05
            && self.status.starts_with("Playing")
        {
            self.status = "Finished — press play to restart".to_string();
        }

        let viewport = window.viewport_size();
        let vw = f32::from(viewport.width);
        let vh = f32::from(viewport.height);
        let content_w = (vw - PAD * 2.0 - SIDEBAR - GAP).max(420.0);
        let field_h =
            (vh - TOPBAR_H - TRANSPORT_H - KEYBOARD_H - 26.0 - PAD * 2.0 - GAP * 4.0).max(200.0);
        let speed = (field_h / 4.2).max(40.0);
        let keys = layout_keys(content_w);

        let mut active_keys: HashSet<u8> = HashSet::new();
        if self.mode == Mode::Play && !self.timeline.notes.is_empty() && !playing {
            for note in self.timeline.active_at(position) {
                active_keys.insert(note.key);
            }
        }
        if playing {
            for note in self.timeline.active_at(position) {
                active_keys.insert(note.key);
            }
        }
        for note in &self.live_notes {
            if note.end.is_none() {
                active_keys.insert(note.key);
            }
        }
        // Notes the pedal is holding after the key came up: still sounding, so the
        // keyboard should still show them.
        for key in &self.sustained_keys {
            active_keys.insert(*key);
        }

        let song_name = self.song_name.clone();
        let note_count = self.timeline.notes.len();
        let bpm = self
            .song
            .as_ref()
            .and_then(|s| s.tempos.first())
            .map(|t| t.bpm)
            .unwrap_or(120.0);
        let recording = self.recorder.is_recording();
        let record_secs = self.recorder.elapsed(now);
        let render_busy = self.rendering;
        let midi_connected = self.midi.selected().is_some();

        div()
            .flex()
            .flex_col()
            .size_full()
            .p(px(PAD))
            .gap(px(GAP))
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_weight(FontWeight::NORMAL)
            .track_focus(&self.focus_handle)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, _cx| {
                    window.focus(&this.focus_handle);
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.key_down(event);
                cx.notify();
            }))
            .on_key_up(cx.listener(|this, event: &KeyUpEvent, _, cx| {
                this.key_up(event);
                cx.notify();
            }))
            .child(self.top_bar(midi_connected, cx))
            .child(self.transport_bar(position, duration, playing, recording, record_secs, cx))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(GAP))
                    .flex_1()
                    .min_h(px(0.0))
                    .child(self.sidebar(song_name, note_count, bpm, render_busy, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(GAP))
                            .flex_1()
                            .min_w(px(0.0))
                            .child(self.visualizer(
                                position, playing, now, content_w, field_h, speed, &keys,
                            ))
                            .child(self.keyboard(&keys, &active_keys)),
                    ),
            )
            .child(self.status_bar(duration, position, cx))
            .when(self.plugins_open, |this| this.child(self.plugin_panel(cx)))
    }
}

impl SurfApp {
    fn top_bar(&self, midi_connected: bool, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(TOPBAR_H))
            .px(px(4.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_end()
                            .gap(px(3.0))
                            .h(px(26.0))
                            .child(div().w(px(4.0)).h(px(14.0)).rounded_full().bg(rgb(ACCENT)))
                            .child(div().w(px(4.0)).h(px(22.0)).rounded_full().bg(rgb(ACCENT)))
                            .child(
                                div()
                                    .w(px(4.0))
                                    .h(px(26.0))
                                    .rounded_full()
                                    .bg(rgb(ACCENT_2)),
                            )
                            .child(
                                div()
                                    .w(px(4.0))
                                    .h(px(18.0))
                                    .rounded_full()
                                    .bg(rgb(ACCENT_2)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(22.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child("SURF"),
                            )
                            .child(
                                div()
                                    .text_size(px(9.0))
                                    .text_color(rgb(MUTED))
                                    .font_weight(FontWeight::BOLD)
                                    .child("MIDI PIANO STUDIO"),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .p(px(4.0))
                    .rounded_full()
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .child(self.mode_tab("LIVE", Mode::Live, cx))
                    .child(self.mode_tab("PLAY", Mode::Play, cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(10.0))
                            .h(px(28.0))
                            .rounded_full()
                            .bg(rgb(PANEL_2))
                            .child(div().size(px(6.0)).rounded_full().bg(if midi_connected {
                                rgb(GREEN)
                            } else {
                                rgb(AMBER)
                            }))
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(rgb(MUTED))
                                    .child(if midi_connected { "MIDI IN" } else { "NO MIDI" }),
                            ),
                    )
                    .child(
                        // Sustain pedal. Lights up amber while the pedal is held, so
                        // a dead pedal is visibly distinguishable from one that works.
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(6.0))
                            .px(px(10.0))
                            .h(px(28.0))
                            .rounded_full()
                            .bg(if self.sustain {
                                rgb(AMBER)
                            } else {
                                rgb(PANEL_2)
                            })
                            .child(div().size(px(6.0)).rounded_full().bg(if self.sustain {
                                rgb(0x041016)
                            } else {
                                rgb(MUTED)
                            }))
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .font_weight(if self.sustain {
                                        FontWeight::BOLD
                                    } else {
                                        FontWeight::NORMAL
                                    })
                                    .text_color(if self.sustain {
                                        rgb(0x041016)
                                    } else {
                                        rgb(MUTED)
                                    })
                                    .child("PEDAL"),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(MUTED))
                            .child(format!("{} Hz", self.audio.sample_rate())),
                    ),
            )
    }

    fn mode_tab(
        &self,
        label: &'static str,
        target: Mode,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.mode == target;
        div()
            .id(label)
            .flex()
            .items_center()
            .justify_center()
            .h(px(30.0))
            .px(px(18.0))
            .rounded_full()
            .bg(if active { rgb(ACCENT) } else { rgb(PANEL_2) })
            .text_color(if active { rgb(0x041016) } else { rgb(MUTED) })
            .text_size(px(11.0))
            .font_weight(FontWeight::BOLD)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.set_mode(target);
                cx.notify();
            }))
            .child(label)
    }

    fn transport_bar(
        &self,
        position: f64,
        duration: f64,
        playing: bool,
        recording: bool,
        record_secs: f64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let progress = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let track_w = 230.0;
        div()
            .flex()
            .flex_row()
            .items_center()
            .h(px(TRANSPORT_H))
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(self.transport_button(
                        "play",
                        if playing { "II" } else { ">" },
                        playing,
                        ACCENT,
                        |this| this.toggle_play(),
                        cx,
                    ))
                    .child(self.transport_button(
                        "stop",
                        "[]",
                        false,
                        MUTED,
                        |this| this.stop_playback(),
                        cx,
                    ))
                    .child(self.transport_button(
                        "back",
                        "-5",
                        false,
                        MUTED,
                        |this| this.seek_relative(-5.0),
                        cx,
                    ))
                    .child(self.transport_button(
                        "forward",
                        "+5",
                        false,
                        MUTED,
                        |this| this.seek_relative(5.0),
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(5.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgb(TEXT))
                                    .font_weight(FontWeight::BOLD)
                                    .child(format!(
                                        "{} / {}",
                                        fmt_time(position),
                                        fmt_time(duration)
                                    )),
                            )
                            .child(div().text_size(px(9.0)).text_color(rgb(MUTED)).child(
                                if self.loop_playback {
                                    "LOOP ON"
                                } else {
                                    "LOOP OFF"
                                },
                            )),
                    )
                    .child(
                        div()
                            .relative()
                            .w(px(track_w))
                            .h(px(5.0))
                            .rounded_full()
                            .bg(rgb(PANEL_2))
                            .child(
                                div()
                                    .absolute()
                                    .left_0()
                                    .top_0()
                                    .h(px(5.0))
                                    .w(px(track_w * progress as f32))
                                    .rounded_full()
                                    .bg(rgb(ACCENT)),
                            ),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .child(self.text_button("OPEN MIDI", false, |this| this.load_midi_dialog(), cx))
                    .child(self.text_button(
                        "EXPORT MIDI",
                        false,
                        |this| this.export_midi_dialog(),
                        cx,
                    ))
                    .child(self.text_button(
                        if recording { "STOP REC" } else { "RECORD" },
                        recording,
                        |this| this.toggle_record(),
                        cx,
                    ))
                    .child(self.transport_button(
                        "loop",
                        "@",
                        self.loop_playback,
                        ACCENT_2,
                        |this| this.toggle_loop(),
                        cx,
                    )),
            )
            .when(recording, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.0))
                        .px(px(10.0))
                        .h(px(28.0))
                        .rounded_full()
                        .bg(rgb(0x2a101a))
                        .child(div().size(px(7.0)).rounded_full().bg(rgb(ACCENT_2)))
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(ACCENT_2))
                                .font_weight(FontWeight::BOLD)
                                .child(format!("REC {}", fmt_time(record_secs))),
                        ),
                )
            })
    }

    fn transport_button(
        &self,
        id: &'static str,
        glyph: &'static str,
        active: bool,
        accent: u32,
        action: fn(&mut Self),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .size(px(34.0))
            .rounded_full()
            .bg(if active { rgb(accent) } else { rgb(PANEL) })
            .border_1()
            .border_color(if active { rgb(accent) } else { rgb(BORDER) })
            .text_size(px(11.0))
            .font_weight(FontWeight::BOLD)
            .text_color(if active { rgb(0x041016) } else { rgb(TEXT) })
            .cursor_pointer()
            .hover(|style| style.bg(rgb(PANEL_2)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                action(this);
                cx.notify();
            }))
            .child(glyph)
    }

    fn text_button(
        &self,
        label: &'static str,
        active: bool,
        action: fn(&mut Self),
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(label)
            .flex()
            .items_center()
            .justify_center()
            .h(px(30.0))
            .px(px(12.0))
            .rounded_md()
            .bg(if active { rgb(ACCENT_2) } else { rgb(PANEL) })
            .border_1()
            .border_color(if active { rgb(ACCENT_2) } else { rgb(BORDER) })
            .text_size(px(9.0))
            .font_weight(FontWeight::BOLD)
            .text_color(if active { rgb(0x14040a) } else { rgb(TEXT) })
            .cursor_pointer()
            .hover(|style| style.bg(rgb(PANEL_2)))
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                action(this);
                cx.notify();
            }))
            .child(label)
    }

    fn sidebar(
        &mut self,
        song_name: String,
        note_count: usize,
        bpm: f64,
        render_busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let instrument = self
            .vst
            .as_ref()
            .map(|v| v.name.clone())
            .unwrap_or_else(|| "Surf Grand (built-in)".to_string());
        let loaded = self.vst.is_some();
        let ports = self.midi.ports();
        let selected = self.midi.selected();
        let midi_error = self.midi.error().map(|s| s.to_string());
        let duration = self.timeline.duration;

        let mut port_rows: Vec<AnyElement> = Vec::new();
        for (index, name) in ports.iter().enumerate().take(5) {
            let active = selected == Some(index);
            port_rows.push(
                div()
                    .id(("midi-port", index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded_md()
                    .bg(if active { rgb(0x122430) } else { rgb(0x0a0e16) })
                    .text_size(px(10.0))
                    .text_color(if active { rgb(ACCENT) } else { rgb(MUTED) })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        if let Err(err) = this.midi.connect(index) {
                            this.status = format!("MIDI connect failed: {err}");
                        } else {
                            this.status = "MIDI input connected".to_string();
                        }
                        cx.notify();
                    }))
                    .child(div().size(px(5.0)).rounded_full().bg(if active {
                        rgb(GREEN)
                    } else {
                        rgb(0x39445a)
                    }))
                    .child(div().flex_1().overflow_hidden().child(name.clone()))
                    .into_any_element(),
            );
        }
        if port_rows.is_empty() {
            port_rows.push(
                div()
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child("No MIDI inputs — plug in a keyboard, then rescan.")
                    .into_any_element(),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap(px(GAP))
            .w(px(SIDEBAR))
            .min_w(px(SIDEBAR))
            .child(
                panel()
                    .child(section_label("NOW PLAYING"))
                    .child(
                        div()
                            .text_size(px(15.0))
                            .font_weight(FontWeight::BOLD)
                            .overflow_hidden()
                            .child(song_name),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(12.0))
                            .child(stat("NOTES", &note_count.to_string()))
                            .child(stat("LENGTH", &fmt_time(duration)))
                            .child(stat("BPM", &format!("{bpm:.0}"))),
                    ),
            )
            .child(
                panel()
                    .child(section_label("INSTRUMENT"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .child(div().size(px(8.0)).rounded_full().bg(if loaded {
                                rgb(ACCENT)
                            } else {
                                rgb(GREEN)
                            }))
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child(instrument),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap(px(6.0))
                            .child(self.text_button(
                                if loaded { "EDITOR" } else { "LOAD VST" },
                                false,
                                |this| {
                                    if this.vst.is_some() {
                                        this.open_vst_editor();
                                    } else {
                                        this.load_vst_dialog();
                                    }
                                },
                                cx,
                            ))
                            .child(self.text_button("SCAN", false, |this| this.scan_plugins(), cx))
                            .when(loaded, |this| {
                                this.child(self.text_button(
                                    "SAVE STATE",
                                    false,
                                    |this| this.save_vst_state(),
                                    cx,
                                ))
                                .child(self.text_button(
                                    "UNLOAD",
                                    false,
                                    |this| this.unload_vst(),
                                    cx,
                                ))
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(8.0))
                            .text_color(rgb(MUTED))
                            .child("Kontakt: load .nki, then SAVE STATE."),
                    ),
            )
            .child(
                panel()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(section_label("MIDI INPUT"))
                            .child(
                                div()
                                    .id("rescan-midi")
                                    .text_size(px(9.0))
                                    .text_color(rgb(ACCENT))
                                    .cursor_pointer()
                                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                        this.midi.refresh();
                                        cx.notify();
                                    }))
                                    .child("RESCAN"),
                            ),
                    )
                    .children(port_rows)
                    .when_some(midi_error, |this, err| {
                        this.child(div().text_size(px(9.0)).text_color(rgb(AMBER)).child(err))
                    }),
            )
            .child(
                panel().child(section_label("RENDER & EXPORT")).child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap(px(6.0))
                        .child(self.text_button(
                            if render_busy {
                                "RENDERING…"
                            } else {
                                "EXPORT WAV"
                            },
                            render_busy,
                            |this| {
                                if !this.rendering {
                                    this.export_wav_dialog();
                                }
                            },
                            cx,
                        ))
                        .child(self.text_button("DEMO", false, |this| this.load_demo(), cx)),
                ),
            )
    }

    /// Modal list of the plugins found by `SCAN` or by the folder picker.
    ///
    /// Clicking a row loads it through `surf-vst-host`, exactly like
    /// `LOAD VST` does; `Esc` or `CLOSE` dismisses the list.
    fn plugin_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, entry) in self.plugin_scan.iter().enumerate() {
            let name = entry.name.clone();
            let format = if entry.vst2 { "VST2" } else { "VST3" };
            let folder = entry
                .path
                .parent()
                .map(|parent| parent.display().to_string())
                .unwrap_or_default();
            rows.push(
                div()
                    .id(("scanned-plugin", index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded_md()
                    .bg(rgb(0x0a0e16))
                    .cursor_pointer()
                    .hover(|style| style.bg(rgb(0x16212f)))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.load_scanned_plugin(index);
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_hidden()
                            .child(div().text_size(px(11.0)).child(name))
                            .child(
                                div()
                                    .text_size(px(8.0))
                                    .text_color(rgb(MUTED))
                                    .overflow_hidden()
                                    .child(folder),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(8.0))
                            .font_weight(FontWeight::BOLD)
                            .text_color(rgb(MUTED))
                            .child(format),
                    )
                    .into_any_element(),
            );
        }
        if rows.is_empty() {
            rows.push(
                div()
                    .px(px(10.0))
                    .py(px(12.0))
                    .text_size(px(10.0))
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "Nothing found. Point {} at your plugin folder and press SCAN again.",
                        vst_scan::EXTRA_ROOTS_ENV
                    ))
                    .into_any_element(),
            );
        }

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000b4))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.0))
                    .w(px(560.0))
                    .max_h(px(520.0))
                    .p(px(14.0))
                    .rounded_lg()
                    .bg(rgb(PANEL_2))
                    .border_1()
                    .border_color(rgb(BORDER))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(section_label("VST PLUGINS"))
                            .child(self.text_button(
                                "CLOSE",
                                false,
                                |this| this.close_plugins(),
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .id("plugin-scan-list")
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .flex_1()
                            .min_h(px(0.0))
                            .overflow_y_scroll()
                            .children(rows),
                    ),
            )
    }

    fn visualizer(
        &self,
        position: f64,
        playing: bool,
        now: Instant,
        width: f32,
        height: f32,
        speed: f32,
        keys: &[KeyGeom; 128],
    ) -> impl IntoElement {
        let mut grid = Vec::new();
        let mut x = 0.0;
        while x < width {
            grid.push(
                div()
                    .absolute()
                    .left(px(x))
                    .top_0()
                    .w(px(1.0))
                    .h(px(height))
                    .bg(rgb(0x0c111b))
                    .into_any_element(),
            );
            x += 64.0;
        }
        // Octave separators at every C.
        for key in 21u8..=108 {
            if key % 12 == 0 {
                let geom = keys[key as usize];
                grid.push(
                    div()
                        .absolute()
                        .left(px(geom.x))
                        .top_0()
                        .w(px(1.0))
                        .h(px(height))
                        .bg(rgb(0x172033))
                        .into_any_element(),
                );
            }
        }

        // Horizontal beat/bar grid in Play mode.  Bar lines are brighter.
        if self.mode == Mode::Play {
            let bpm = self
                .song
                .as_ref()
                .and_then(|s| s.tempos.first())
                .map(|t| t.bpm)
                .unwrap_or(120.0)
                .max(1.0);
            let beat_secs = 60.0 / bpm;
            let first = ((position / beat_secs).floor() as i64 - 8).max(0);
            for step in 0..24 {
                let beat_index = first + step;
                let beat_time = beat_index as f64 * beat_secs;
                let y = height - (beat_time - position) as f32 * speed;
                if y < 0.0 || y > height {
                    continue;
                }
                let bar = beat_index % 4 == 0;
                grid.push(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .top(px(y))
                        .h(px(1.0))
                        .bg(rgb(if bar { 0x1d2a40 } else { 0x0f1624 }))
                        .into_any_element(),
                );
            }
        }

        let mut notes = Vec::new();
        if self.mode == Mode::Play && !self.timeline.notes.is_empty() {
            for note in visible_notes(&self.timeline, position, speed, height) {
                let geom = keys[note.key as usize];
                let (x, w) = note_geometry(geom);
                let note_h = (note.duration() as f32 * speed).max(5.0);
                let bottom = height - (note.start as f32 - position as f32) * speed;
                let top = bottom - note_h;
                if bottom < 0.0 || top > height {
                    continue;
                }
                let active = note.is_active(position);
                notes.push(note_block(
                    note.key,
                    note.velocity,
                    x,
                    top,
                    w,
                    note_h.min(height),
                    active,
                    "fall",
                ));
            }
        } else if self.mode == Mode::Live {
            let now_secs = now.elapsed().as_secs_f32();
            let _ = now_secs;
            for note in &self.live_notes {
                let start = note.start.elapsed().as_secs_f32();
                let (top, h) = match note.end {
                    None => {
                        let h = (start * speed).clamp(3.0, height);
                        (height - h, h)
                    }
                    Some(end) => {
                        let duration = end.duration_since(note.start).as_secs_f32();
                        let age = now.duration_since(end).as_secs_f32();
                        let h = (duration * speed).max(4.0);
                        let bottom = height - age * speed;
                        (bottom - h, h)
                    }
                };
                if top > height || top + h < 0.0 {
                    continue;
                }
                let geom = keys[note.key as usize];
                let (x, w) = note_geometry(geom);
                notes.push(note_block(
                    note.key,
                    note.velocity,
                    x,
                    top,
                    w,
                    h.min(height),
                    note.end.is_none(),
                    "rise",
                ));
            }
        }

        div()
            .relative()
            .w_full()
            .h(px(height))
            .bg(rgb(0x06080d))
            .border_1()
            .border_color(rgb(BORDER))
            .rounded_lg()
            .overflow_hidden()
            .children(grid)
            .children(notes)
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(2.0))
                    .bg(rgb(ACCENT))
                    .opacity(if playing { 0.9 } else { 0.45 }),
            )
            .when(
                self.mode == Mode::Play && self.timeline.notes.is_empty(),
                |this| {
                    this.child(empty_hint(
                        "DROP IN A MIDI FILE",
                        "Open MIDI, load the demo, or record a take in LIVE mode",
                    ))
                },
            )
            .when(
                self.mode == Mode::Live && self.live_notes.is_empty(),
                |this| {
                    this.child(empty_hint(
                        "PLAY YOUR MIDI KEYBOARD",
                        "Notes rise from the keys · press RECORD to capture a take",
                    ))
                },
            )
    }

    fn keyboard(&self, keys: &[KeyGeom; 128], active: &HashSet<u8>) -> impl IntoElement {
        let mut whites = Vec::new();
        let mut blacks = Vec::new();
        for key in 21u8..=108 {
            let geom = keys[key as usize];
            let is_active = active.contains(&key);
            if geom.black {
                blacks.push(
                    div()
                        .absolute()
                        .left(px(geom.x))
                        .top_0()
                        .w(px(geom.w))
                        .h(px(KEYBOARD_H * 0.63))
                        .rounded_b_sm()
                        .bg(if is_active {
                            rgb(ACCENT_2)
                        } else {
                            rgb(0x10141d)
                        })
                        .border_1()
                        .border_color(if is_active {
                            rgb(ACCENT_2)
                        } else {
                            rgb(0x05070b)
                        })
                        .into_any_element(),
                );
            } else {
                whites.push(
                    div()
                        .absolute()
                        .left(px(geom.x))
                        .top_0()
                        .w(px(geom.w))
                        .h(px(KEYBOARD_H))
                        .rounded_b_sm()
                        .bg(if is_active {
                            rgb(ACCENT)
                        } else {
                            rgb(0xeef2f9)
                        })
                        .border_1()
                        .border_color(rgb(0x0b0e15))
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .right_0()
                                .bottom(px(5.0))
                                .text_center()
                                .text_size(px(8.0))
                                .text_color(if is_active {
                                    rgb(0x041016)
                                } else {
                                    rgb(0x6d778c)
                                })
                                .child(if key % 12 == 0 {
                                    key_name(key)
                                } else {
                                    String::new()
                                }),
                        )
                        .into_any_element(),
                );
            }
        }

        div()
            .relative()
            .w_full()
            .h(px(KEYBOARD_H))
            .rounded_lg()
            .overflow_hidden()
            .bg(rgb(0x0d1119))
            .border_1()
            .border_color(rgb(BORDER))
            .children(whites)
            .children(blacks)
    }

    fn status_bar(
        &self,
        duration: f64,
        position: f64,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let peak_pct = (self.last_peak * 100.0).clamp(0.0, 100.0);
        div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .h(px(26.0))
            .px(px(2.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .flex_1()
                    .min_w(px(0.0))
                    .overflow_hidden()
                    .child(div().size(px(5.0)).rounded_full().bg(rgb(GREEN)))
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(rgb(MUTED))
                            .overflow_hidden()
                            .child(self.status.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(
                        div()
                            .text_size(px(9.0))
                            .text_color(rgb(MUTED))
                            .child(format!("{:.1}s / {:.1}s", position, duration)),
                    )
                    .child(
                        div()
                            .w(px(50.0))
                            .h(px(3.0))
                            .rounded_full()
                            .bg(rgb(PANEL_2))
                            .child(
                                div()
                                    .h(px(3.0))
                                    .w(px(50.0 * peak_pct / 100.0))
                                    .rounded_full()
                                    .bg(rgb(if peak_pct > 90.0 { ACCENT_2 } else { GREEN })),
                            ),
                    ),
            )
    }
}

fn panel() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .p(px(10.0))
        .bg(rgb(PANEL))
        .border_1()
        .border_color(rgb(BORDER))
        .rounded_lg()
}

fn section_label(text: &'static str) -> impl IntoElement {
    div()
        .text_size(px(9.0))
        .font_weight(FontWeight::BOLD)
        .text_color(rgb(0x5a6579))
        .child(text)
}

fn stat(label: &'static str, value: &str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(1.0))
        .child(
            div()
                .text_size(px(8.0))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(0x5a6579))
                .child(label),
        )
        .child(
            div()
                .text_size(px(12.0))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(TEXT))
                .child(value.to_string()),
        )
}

fn empty_hint(title: &'static str, subtitle: &'static str) -> impl IntoElement {
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(7.0))
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(0x66718a))
                .child(title),
        )
        .child(
            div()
                .text_size(px(10.0))
                .text_color(rgb(0x3f4a5e))
                .child(subtitle),
        )
}

fn note_block(
    key: u8,
    velocity: u8,
    x: f32,
    top: f32,
    width: f32,
    height: f32,
    active: bool,
    direction: &'static str,
) -> AnyElement {
    let color = KEY_COLORS[(key % 12) as usize];
    let black = is_black(key);
    let base = rgb(color);
    let alpha = if active { 1.0 } else { 0.82 };
    let velocity_alpha = 0.55 + (velocity as f32 / 127.0) * 0.45;
    let mut fill = base;
    fill.a = alpha * velocity_alpha;
    let mut border = gpui::white();
    border.a = if active { 0.85 } else { 0.22 };
    div()
        .absolute()
        .left(px(x))
        .top(px(top))
        .w(px(width))
        .h(px(height.max(4.0)))
        .rounded_sm()
        .bg(fill)
        .border_1()
        .border_color(border)
        .when(black, |this| this.opacity(0.92))
        .when(active, |this| this.shadow_lg())
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(px((height * 0.16).clamp(1.0, 4.0)))
                .rounded_t_sm()
                .bg(gpui::white())
                .opacity(0.22),
        )
        .when(direction == "fall" && active, |this| {
            this.child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom(px(-2.0))
                    .h(px(2.0))
                    .bg(gpui::white())
                    .opacity(0.7),
            )
        })
        .into_any_element()
}

fn visible_notes<'a>(
    timeline: &'a Timeline,
    position: f64,
    speed: f32,
    height: f32,
) -> impl Iterator<Item = &'a TimedNote> {
    let lookahead = height as f64 / speed as f64 + 0.2;
    timeline
        .notes
        .iter()
        .filter(move |note| note.end > position - 0.2 && note.start < position + lookahead)
}

fn note_geometry(geom: KeyGeom) -> (f32, f32) {
    if geom.black {
        let w = geom.w * 0.88;
        (geom.x + (geom.w - w) * 0.5, w)
    } else {
        let w = geom.w * 0.86;
        (geom.x + (geom.w - w) * 0.5, w)
    }
}

fn layout_keys(width: f32) -> [KeyGeom; 128] {
    let mut keys = [KeyGeom::default(); 128];
    let white_count = (21u8..=108).filter(|k| !is_black(*k)).count() as f32;
    let white_w = width / white_count;
    let black_w = white_w * 0.62;
    let mut x = 0.0;
    for key in 21u8..=108 {
        if is_black(key) {
            keys[key as usize] = KeyGeom {
                x: x - black_w * 0.5,
                w: black_w,
                black: true,
            };
        } else {
            keys[key as usize] = KeyGeom {
                x,
                w: white_w,
                black: false,
            };
            x += white_w;
        }
    }
    keys
}

fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

fn fmt_time(secs: f64) -> String {
    let secs = secs.max(0.0);
    let minutes = (secs / 60.0).floor() as u64;
    let seconds = secs - minutes as f64 * 60.0;
    format!("{minutes:02}:{seconds:04.1}")
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

fn vst_state_path_for(plugin_path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    plugin_path
        .to_string_lossy()
        .to_lowercase()
        .hash(&mut hasher);
    surf_platform::data_dir().join(format!("vst-{:016x}.state", hasher.finish()))
}


