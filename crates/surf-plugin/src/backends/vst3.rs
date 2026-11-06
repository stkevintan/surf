//! VST3 instrument hosting (in-process).
//!
//! The audio thread owns a [`VstInstrument`] which shares the actual plugin
//! instance with the UI through a mutex.  That lets the UI open the plugin's
//! native editor (Kontakt, Pianoteq, ...) while audio keeps flowing.  A lock
//! held by the editor can briefly starve the audio callback, which is one more
//! reason this only ever runs inside the plugin host process.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use vst3_host::audio::AudioBuffers;
use vst3_host::host::Vst3Host;
use vst3_host::midi::MidiChannel;
use vst3_host::plugin::{Plugin, ProcessMode};
use vst3_host::window::PluginWindow;

use surf_audio::Instrument;
use surf_audio::render::{RenderOptions, RenderStats, render_with_instrument};
use surf_core::song::Timeline;

/// The engine renders 64-frame chunks; matching the plugin block size keeps
/// every sample accounted for (VST3 `numSamples` follows the buffer length).
pub const VST_BLOCK: usize = 64;

pub struct VstPlugin {
    pub plugin: Arc<Mutex<Plugin>>,
    pub path: PathBuf,
    pub name: String,
    pub sample_rate: u32,
    /// Whether state was applied before the processor was activated.
    pub state_restored: bool,
}

impl VstPlugin {
    pub fn load(path: impl AsRef<Path>, sample_rate: u32) -> Result<Self> {
        Self::load_with_state(path, sample_rate, None)
    }

    pub fn load_with_state(
        path: impl AsRef<Path>,
        sample_rate: u32,
        state: Option<&[u8]>,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut host = Vst3Host::builder()
            .sample_rate(sample_rate as f64)
            .block_size(VST_BLOCK)
            .build()
            .context("could not create VST3 host")?;
        let mut plugin = host
            .load_plugin(&path)
            .with_context(|| format!("could not load VST3 plugin {}", path.display()))?;
        // Kontakt wants the saved NKI/multi state before the processor is
        // activated.  Applying it after start_processing() can leave the editor
        // looking correct while the audio path is still on the default state.
        let mut state_restored = false;
        if let Some(state) = state {
            match plugin.load_state(state) {
                Ok(()) => state_restored = true,
                Err(err) => eprintln!("VST3 state restore failed: {err}"),
            }
        }
        plugin
            .start_processing()
            .context("plugin refused to start processing")?;
        let mut name = plugin.info().name.clone();
        if name.trim().is_empty() {
            name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("VST3 instrument")
                .to_string();
        }
        Ok(Self {
            plugin: Arc::new(Mutex::new(plugin)),
            path,
            name,
            sample_rate,
            state_restored,
        })
    }

    pub fn instrument(&self) -> VstInstrument {
        VstInstrument::new(self.plugin.clone(), self.name.clone(), self.sample_rate)
    }

    pub fn state(&self) -> Result<Vec<u8>> {
        let plugin = self
            .plugin
            .lock()
            .map_err(|_| anyhow::anyhow!("plugin mutex poisoned"))?;
        Ok(plugin.save_state()?)
    }

    pub fn load_state(&self, bytes: &[u8]) -> Result<()> {
        let mut plugin = self
            .plugin
            .lock()
            .map_err(|_| anyhow::anyhow!("plugin mutex poisoned"))?;
        Ok(plugin.load_state(bytes)?)
    }

    pub fn open_editor(&self) -> Result<PluginWindow> {
        let mut window = PluginWindow::new(self.plugin.clone());
        window.open().context("could not open plugin editor")?;
        Ok(window)
    }

    pub fn has_editor(&self) -> bool {
        self.plugin.lock().map(|p| p.has_editor()).unwrap_or(false)
    }

    /// Which parameter the plugin maps a MIDI controller to, if any.
    ///
    /// VST3 instruments receive controllers (sustain = CC 64) as parameter changes
    /// via `IMidiMapping`, not as events; this is how the host finds the target.
    /// Used by `examples/pedal_probe.rs`.
    pub fn midi_cc_parameter(&self, bus: i32, channel: i16, cc: u16) -> Option<u32> {
        self.plugin
            .lock()
            .ok()
            .and_then(|p| p.midi_cc_to_parameter(bus, channel, cc))
    }
}

fn midi_channel(channel: u8) -> MidiChannel {
    MidiChannel::from_index(channel & 15).unwrap_or(MidiChannel::Ch1)
}

pub struct VstInstrument {
    plugin: Arc<Mutex<Plugin>>,
    buffers: AudioBuffers,
    name: String,
    failed: bool,
}

impl VstInstrument {
    pub fn new(plugin: Arc<Mutex<Plugin>>, name: String, sample_rate: u32) -> Self {
        let output_channels = plugin
            .lock()
            .map(|p| p.output_channel_count().max(2))
            .unwrap_or(2);
        let buffers = AudioBuffers::new(0, output_channels, VST_BLOCK, sample_rate as f64);
        Self {
            plugin,
            buffers,
            name,
            failed: false,
        }
    }

    fn silence(&mut self, out: &mut [f32], frames: usize) {
        out[..frames * 2].fill(0.0);
    }
}

impl Instrument for VstInstrument {
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        if let Ok(mut plugin) = self.plugin.lock() {
            if let Err(err) = plugin.send_midi_note(key, velocity, midi_channel(channel)) {
                eprintln!("VST note_on failed: {err}");
            }
        }
    }

    fn note_off(&mut self, channel: u8, key: u8) {
        if let Ok(mut plugin) = self.plugin.lock() {
            if let Err(err) = plugin.send_midi_note_off(key, midi_channel(channel)) {
                eprintln!("VST note_off failed: {err}");
            }
        }
    }

    fn all_notes_off(&mut self) {
        if let Ok(mut plugin) = self.plugin.lock() {
            let _ = plugin.midi_panic();
        }
    }

    fn set_sustain(&mut self, channel: u8, down: bool) {
        if let Ok(mut plugin) = self.plugin.lock() {
            let _ = plugin.send_midi_cc(64, if down { 127 } else { 0 }, midi_channel(channel));
        }
    }

    fn control_change(&mut self, channel: u8, controller: u8, value: u8) {
        if let Ok(mut plugin) = self.plugin.lock() {
            let _ = plugin.send_midi_cc(controller & 127, value & 127, midi_channel(channel));
        }
    }

    fn process(&mut self, out: &mut [f32], frames: usize) {
        if self.failed {
            self.silence(out, frames);
            return;
        }
        let frames = frames.min(VST_BLOCK);
        let Ok(mut plugin) = self.plugin.lock() else {
            self.silence(out, frames);
            return;
        };
        for channel in &mut self.buffers.outputs {
            channel[..frames].fill(0.0);
        }
        let result = plugin.process_audio(&mut self.buffers);
        drop(plugin);
        match result {
            Ok(()) => {
                let left = self.buffers.outputs.first();
                let right = self.buffers.outputs.get(1).or(left);
                for frame in 0..frames {
                    let l = left.map(|c| c[frame]).unwrap_or(0.0);
                    let r = right.map(|c| c[frame]).unwrap_or(l);
                    out[frame * 2] = l;
                    out[frame * 2 + 1] = r;
                }
            }
            Err(err) => {
                eprintln!("VST process failed: {err}");
                self.failed = true;
                self.silence(out, frames);
            }
        }
    }

    fn latency_samples(&self) -> u32 {
        self.plugin.lock().map(|p| p.latency_samples()).unwrap_or(0)
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Render a timeline through a fresh offline instance of a VST3 plugin.
pub fn render_vst_to_wav(
    plugin_path: impl AsRef<Path>,
    state: Option<&[u8]>,
    timeline: &Timeline,
    output: impl AsRef<Path>,
    sample_rate: u32,
    options: &RenderOptions,
    controls: &[(u8, u8, u8)],
) -> Result<RenderStats> {
    let plugin_path = plugin_path.as_ref();
    let mut host = Vst3Host::builder()
        .sample_rate(sample_rate as f64)
        .block_size(VST_BLOCK)
        .build()
        .context("could not create offline VST3 host")?;
    let mut plugin = host
        .load_plugin(plugin_path)
        .with_context(|| format!("could not load {}", plugin_path.display()))?;
    plugin
        .set_process_mode(ProcessMode::Offline)
        .context("plugin does not support offline mode")?;
    if let Some(state) = state {
        plugin
            .load_state(state)
            .context("could not restore the plugin state for export")?;
    }
    plugin.start_processing()?;
    let name = plugin.info().name.clone();
    let mut instrument = VstInstrument::new(Arc::new(Mutex::new(plugin)), name, sample_rate);
    for &(channel, controller, value) in controls {
        instrument.control_change(channel, controller, value);
    }
    render_with_instrument(timeline, &mut instrument, output, options)
}
