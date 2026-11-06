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

use rfd::FileDialog;

use surf_audio::render::{RenderOptions, render_timeline_to_wav};
use surf_audio::{AudioEngine, Command, EngineStatus, OutputEvent, Synth};
use surf_core::recorder::Recorder;
use surf_core::song::{Song, Timeline};
use surf_midi::{LiveEvent, MidiHub, NoteSink, PortChange};
use surf_plugin::client::{RenderNote, VstClient, helper_log_path};
use surf_plugin::scan::{self as vst_scan, PluginEntry};

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
    pub song: Option<Song>,
    pub timeline: Timeline,
    pub song_name: String,
    pub live_notes: Vec<LiveNote>,
    pub vst: Option<VstClient>,
    render_rx: Option<Receiver<anyhow::Result<surf_audio::render::RenderStats>>>,
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
}

impl Session {
    pub fn new() -> Self {
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
        // Start with a short demo loaded (paused) so the piano roll has
        // something to show before the user opens their own file.
        let demo = Song::demo();
        let timeline = demo.timeline();
        let song_name = demo.name.clone();
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
        Self {
            audio,
            midi,
            recorder: Recorder::new(),
            song: Some(demo),
            timeline,
            song_name,
            live_notes: Vec::new(),
            vst: None,
            render_rx: None,
            rendering: false,
            pending_vst: std::env::var_os("SURF_VST").map(PathBuf::from),
            plugin_scan,
            sustain: false,
            sustained_keys: HashSet::new(),
            loop_playback: false,
            volume: 0.9,
            status,
            engine,
        }
    }

    pub fn toggle_play(&mut self) {
        if self.song.is_none() {
            self.status = "Load a MIDI file before pressing play".to_string();
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
            if notes.is_empty() {
                self.status = "Recording stopped: no notes captured".to_string();
                return;
            }
            let name = format!("Take {}", self.song_name_or("Untitled"));
            let song = Song::from_recorded(name, &notes);
            self.timeline = song.timeline();
            self.song = Some(song);
            self.song_name = format!("Recorded take · {} notes", notes.len());
            self.audio.stop();
            self.status = format!("Recorded {} notes — press play or export MIDI", notes.len());
        } else {
            self.recorder.start(Instant::now());
            self.live_notes.clear();
            self.audio.stop();
            self.status = "Recording… play your MIDI keyboard or use Musical Typing".to_string();
        }
    }

    fn song_name_or(&self, fallback: &str) -> String {
        self.song
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_else(|| fallback.to_string())
    }

    pub fn load_demo(&mut self) {
        let song = Song::demo();
        self.timeline = song.timeline();
        self.song_name = song.name.clone();
        self.song = Some(song);
        self.audio.stop();
        self.audio.play(self.timeline.clone(), 0.0);
        self.status = "Demo loaded — 4 bars of Am–F–C–G".to_string();
    }

    pub fn load_midi_dialog(&mut self) {
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

    pub fn export_midi_dialog(&mut self) {
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

    pub fn export_wav_dialog(&mut self) {
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

    pub fn load_vst_dialog(&mut self) {
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

    pub fn load_vst_path(&mut self, path: PathBuf) {
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

    pub fn open_vst_editor(&mut self) {
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

    pub fn save_vst_state(&mut self) {
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

    pub fn unload_vst(&mut self) {
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
        self.song
            .as_ref()
            .and_then(|s| s.tempos.first())
            .map(|t| t.bpm)
            .unwrap_or(120.0)
    }

    pub fn time_signature(&self) -> (u8, u8) {
        self.song.as_ref().map(|s| s.time_signature).unwrap_or((4, 4))
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

fn vst_state_path_for(plugin_path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    plugin_path
        .to_string_lossy()
        .to_lowercase()
        .hash(&mut hasher);
    surf_platform::data_dir().join(format!("vst-{:016x}.state", hasher.finish()))
}


