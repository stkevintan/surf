#![allow(deprecated)]

//! VST2 instrument hosting (Windows `.dll`, macOS `.vst` bundle, Linux `.so`).
//!
//! Like the VST3 path, the plugin lives in the helper process. The UI process
//! only sees shared-memory audio and JSON commands.
//!
//! Loading, audio, MIDI and state are portable. The editor needs a native
//! parent window, which only exists on Windows so far; elsewhere
//! [`Vst2Plugin::open_editor`] returns an error.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use vst::api;
use vst::buffer::AudioBuffer;
use vst::host::{Host, PluginInstance, PluginLoader};
use vst::plugin::Plugin;

use surf_audio::Instrument;

pub const VST2_BLOCK: usize = 64;

struct SurfVst2Host {
    sample_rate: f32,
    directory: String,
}

impl Host for SurfVst2Host {
    fn get_info(&self) -> (isize, String, String) {
        (2400, "Surf".to_string(), "Surf".to_string())
    }

    fn get_block_size(&self) -> isize {
        VST2_BLOCK as isize
    }

    fn get_sample_rate(&self) -> f32 {
        self.sample_rate
    }

    fn get_directory(&self) -> Option<String> {
        Some(self.directory.clone())
    }

    fn can_do(&self, query: &str) -> bool {
        matches!(
            query,
            "send_vst_events" | "receive_vst_events" | "receive_sysex"
        )
    }

    fn get_current_process_level(&self) -> isize {
        2
    }

    fn get_automation_state(&self) -> isize {
        1
    }

    fn get_host_language(&self) -> isize {
        1
    }
}

pub struct Vst2Plugin {
    instance: Arc<Mutex<PluginInstance>>,
    pub path: PathBuf,
    pub name: String,
    pub sample_rate: u32,
    pub output_channels: usize,
    pub input_channels: usize,
    pub latency: u32,
    pub state_restored: bool,
}

impl Vst2Plugin {
    pub fn load(path: impl AsRef<Path>, sample_rate: u32) -> Result<Self> {
        Self::load_with_state(path, sample_rate, None)
    }

    pub fn load_with_state(
        path: impl AsRef<Path>,
        sample_rate: u32,
        state: Option<&[u8]>,
    ) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        // On macOS the plugin is a bundle; the loader needs the Mach-O inside it.
        let binary = crate::format::vst2_binary(&path);
        let directory = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let host = Arc::new(Mutex::new(SurfVst2Host {
            sample_rate: sample_rate as f32,
            directory,
        }));
        let mut loader = PluginLoader::load(&binary, host).map_err(|e| {
            anyhow::anyhow!("could not load VST2 plugin {}: {e}", binary.display())
        })?;
        let mut instance = loader
            .instance()
            .map_err(|e| anyhow::anyhow!("could not instantiate VST2 plugin: {e}"))?;
        instance.init();
        instance.set_sample_rate(sample_rate as f32);
        instance.set_block_size(VST2_BLOCK as i64);

        let mut state_restored = false;
        if let Some(bytes) = state {
            if !bytes.is_empty() {
                instance.get_parameter_object().load_bank_data(bytes);
                state_restored = true;
            }
        }
        instance.resume();

        let info = instance.get_info();
        let name = if info.name.trim().is_empty() {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("VST2 instrument")
                .to_string()
        } else {
            info.name
        };
        let output_channels = (info.outputs.max(0) as usize).max(2);
        let input_channels = (info.inputs.max(0) as usize).max(1);
        let latency = info.initial_delay.max(0) as u32;
        Ok(Self {
            instance: Arc::new(Mutex::new(instance)),
            path,
            name,
            sample_rate,
            output_channels,
            input_channels,
            latency,
            state_restored,
        })
    }

    pub fn instrument(&self) -> Vst2Instrument {
        Vst2Instrument {
            instance: self.instance.clone(),
            name: self.name.clone(),
            input_channels: self.input_channels,
            output_channels: self.output_channels,
            latency: self.latency,
            inputs: vec![vec![0.0; VST2_BLOCK]; self.input_channels],
            outputs: vec![vec![0.0; VST2_BLOCK]; self.output_channels],
            pending_midi: Vec::with_capacity(MAX_PENDING_MIDI),
            events: EventBatch::with_capacity(MAX_PENDING_MIDI),
        }
    }

    pub fn state(&self) -> Result<Vec<u8>> {
        let mut plugin = self
            .instance
            .lock()
            .map_err(|_| anyhow::anyhow!("VST2 plugin mutex poisoned"))?;
        Ok(plugin.get_parameter_object().get_bank_data())
    }

    pub fn load_state(&self, bytes: &[u8]) -> Result<()> {
        let mut plugin = self
            .instance
            .lock()
            .map_err(|_| anyhow::anyhow!("VST2 plugin mutex poisoned"))?;
        plugin.get_parameter_object().load_bank_data(bytes);
        Ok(())
    }

    pub fn open_editor(&self) -> Result<Vst2EditorWindow> {
        let mut plugin = self
            .instance
            .lock()
            .map_err(|_| anyhow::anyhow!("VST2 plugin mutex poisoned"))?;
        editor::open(&mut plugin, &self.name)
    }

}

pub struct Vst2Instrument {
    instance: Arc<Mutex<PluginInstance>>,
    name: String,
    input_channels: usize,
    output_channels: usize,
    latency: u32,
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    /// MIDI waiting to be handed to the plugin on the next `process` call.
    ///
    /// VST2 requires `processEvents` immediately before `processReplacing` in the same
    /// cycle: the plugin only keeps those events for the next `process` and then drops
    /// them. Calling `processEvents` once per message (as this used to) meant each call
    /// replaced the previous one, so a chord sent in a single drain lost every note but
    /// the last, and anything sent in a cycle that never reached `process` vanished
    /// entirely. Queue here, flush in `process`.
    pending_midi: Vec<[u8; 3]>,
    /// The last batch handed to `processEvents`, kept alive through the `process` that
    /// follows it.
    events: EventBatch,
}

impl Vst2Instrument {
    fn send_midi(&mut self, data: [u8; 3]) {
        // Bound the queue so a stalled audio thread cannot grow it without limit.
        // 512 messages is far more than a human can play into one block.
        if self.pending_midi.len() < MAX_PENDING_MIDI {
            self.pending_midi.push(data);
        }
    }
}

/// Most MIDI messages queued for one block; also the [`EventBatch`] capacity.
const MAX_PENDING_MIDI: usize = 512;

/// Words before the pointer array in a `VstEvents` block.
///
/// `api::Events` declares `events: [*mut Event; 2]`, but the VST2 ABI treats it as a
/// variable-length array of `num_events` pointers, so the block is built by hand.
/// `num_events` is an i32 followed by an isize, so repr(C) pads it out to isize
/// alignment. Adding the two field sizes instead would place every pointer 4 bytes
/// early on x64.
const HEADER_WORDS: usize =
    std::mem::offset_of!(api::Events, events) / std::mem::size_of::<usize>();

/// Words in `api::Events` as declared, so a reference to the block never covers memory
/// outside it, even with fewer than two events.
const EVENTS_WORDS: usize = std::mem::size_of::<api::Events>() / std::mem::size_of::<usize>();

/// One block's `VstEvents` and the MIDI events it points to.
///
/// VST2 lets a plugin keep the `VstEvents*` it got in `processEvents` and read the
/// events in the following `processReplacing`; many do. So the block has to stay valid
/// until that `process` returns. It lives in the instrument and is only rebuilt by the
/// next [`fill`](Self::fill), after the previous `process` has returned.
///
/// Both buffers are allocated once at full capacity, so filling never reallocates on the
/// audio thread and never moves events a plugin may still point at.
struct EventBatch {
    events: Vec<api::MidiEvent>,
    /// The `VstEvents` header followed by one pointer per event, as `usize` words so
    /// the pointer array is aligned.
    block: Vec<usize>,
}

impl EventBatch {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            events: Vec::with_capacity(capacity),
            block: Vec::with_capacity((HEADER_WORDS + capacity).max(EVENTS_WORDS)),
        }
    }

    /// Replace the batch with `messages` and return the block for `processEvents`.
    ///
    /// Messages beyond the capacity are dropped rather than reallocating.
    fn fill(&mut self, messages: impl Iterator<Item = [u8; 3]>) -> *const api::Events {
        let capacity = self.events.capacity();
        self.events.clear();
        self.events.extend(messages.take(capacity).map(midi_event));

        self.block.clear();
        self.block.resize(HEADER_WORDS, 0);
        // SAFETY: the block holds at least HEADER_WORDS words, and `num_events` is the
        // i32 at offset 0, which is the start of the first word on either endianness.
        unsafe {
            std::ptr::write(self.block.as_mut_ptr() as *mut i32, self.events.len() as i32);
        }
        for event in &mut self.events {
            self.block.push(event as *mut api::MidiEvent as usize);
        }
        if self.block.len() < EVENTS_WORDS {
            self.block.resize(EVENTS_WORDS, 0);
        }
        self.block.as_ptr() as *const api::Events
    }
}

fn midi_event(data: [u8; 3]) -> api::MidiEvent {
    api::MidiEvent {
        event_type: api::EventType::Midi,
        byte_size: std::mem::size_of::<api::MidiEvent>() as i32,
        // All events land at the start of the block. Sample-accurate placement would
        // need the arrival offset plumbed through the IPC ring; at 64-frame blocks this
        // is under 1.4 ms of quantisation.
        delta_frames: 0,
        flags: 0,
        note_length: 0,
        note_offset: 0,
        midi_data: data,
        _midi_reserved: 0,
        detune: 0,
        note_off_velocity: 0,
        _reserved1: 0,
        _reserved2: 0,
    }
}

impl Instrument for Vst2Instrument {
    fn note_on(&mut self, channel: u8, key: u8, velocity: u8) {
        self.send_midi([0x90 | (channel & 15), key, velocity]);
    }

    fn note_off(&mut self, channel: u8, key: u8) {
        self.send_midi([0x80 | (channel & 15), key, 0]);
    }

    fn all_notes_off(&mut self) {
        // Tracks sharing the plugin play on different channels.
        for channel in 0..16u8 {
            self.send_midi([0xB0 | channel, 64, 0]);
            self.send_midi([0xB0 | channel, 123, 0]);
        }
    }

    fn set_sustain(&mut self, channel: u8, down: bool) {
        self.send_midi([0xB0 | (channel & 15), 64, if down { 127 } else { 0 }]);
    }

    fn control_change(&mut self, channel: u8, controller: u8, value: u8) {
        self.send_midi([0xB0 | (channel & 15), controller & 127, value & 127]);
    }

    fn process(&mut self, out: &mut [f32], frames: usize) {
        let frames = frames.min(VST2_BLOCK).min(out.len() / 2);
        if frames == 0 {
            return;
        }
        out[..frames * 2].fill(0.0);
        for input in &mut self.inputs {
            input[..frames].fill(0.0);
        }
        for output in &mut self.outputs {
            output[..frames].fill(0.0);
        }
        let Ok(mut plugin) = self.instance.lock() else {
            return;
        };
        // VST2 contract: events must arrive in the same cycle as the process call that
        // consumes them, and immediately before it. The plugin may read them during
        // that process call, so the batch stays in `self.events` until the next fill.
        if !self.pending_midi.is_empty() {
            let block = self.events.fill(self.pending_midi.drain(..));
            // SAFETY: `block` points into `self.events`, which is not touched again
            // until the next `fill`, after this block's `process` below has returned.
            plugin.process_events(unsafe { &*block });
        }
        let input_ptrs: Vec<*const f32> = self.inputs.iter().map(|b| b.as_ptr()).collect();
        let mut output_ptrs: Vec<*mut f32> =
            self.outputs.iter_mut().map(|b| b.as_mut_ptr()).collect();
        let mut buffer = unsafe {
            AudioBuffer::from_raw(
                self.input_channels,
                self.output_channels,
                input_ptrs.as_ptr(),
                output_ptrs.as_mut_ptr(),
                frames,
            )
        };
        plugin.process(&mut buffer);
        for ch in 0..2.min(self.outputs.len()) {
            for i in 0..frames {
                out[i * 2 + ch] = self.outputs[ch][i];
            }
        }
    }

    fn latency_samples(&self) -> u32 {
        self.latency
    }

    fn name(&self) -> &str {
        &self.name
    }
}

pub use editor::Vst2EditorWindow;

#[cfg(windows)]
mod editor;

#[cfg(not(windows))]
mod editor {
    use anyhow::Result;
    use vst::host::PluginInstance;

    /// Placeholder so the host's editor bookkeeping compiles everywhere.
    pub struct Vst2EditorWindow;

    impl Vst2EditorWindow {
        pub fn idle(&mut self) {}

        pub fn closed_by_user(&self) -> bool {
            false
        }
    }

    pub fn open(_plugin: &mut PluginInstance, _name: &str) -> Result<Vst2EditorWindow> {
        anyhow::bail!(
            "VST2 editors are not supported on this platform yet; audio, MIDI and state still work"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `VstEvents` block must be laid out the way the VST2 ABI reads it.
    ///
    /// `api::Events` declares `events: [*mut Event; 2]`, but the ABI treats that as a
    /// variable-length array of `num_events` pointers. `EventBatch` therefore builds the
    /// block by hand, and a wrong offset there would hand Kontakt garbage pointers — so
    /// assert the layout this code depends on rather than trusting the struct definition.
    #[test]
    fn events_header_layout_matches_abi() {
        // `num_events: i32` is followed by `_reserved: isize`, so repr(C) pads the i32 out
        // to isize alignment: the pointer array starts at 2 * size_of::<isize>(), not at
        // size_of::<i32>() + size_of::<isize>(). `EventBatch` must use `offset_of` rather
        // than adding field sizes by hand — on x64 that mistake puts every pointer 4
        // bytes early and hands the plugin garbage.
        let offset = std::mem::offset_of!(api::Events, events);
        assert_eq!(
            offset,
            2 * std::mem::size_of::<isize>(),
            "pointer array offset must account for alignment padding"
        );
        assert_ne!(
            offset,
            std::mem::size_of::<i32>() + std::mem::size_of::<isize>(),
            "naive field-size addition must not be mistaken for the real offset"
        );
    }

    /// Read a block back the way a plugin walks `VstEvents`.
    unsafe fn read_block(block: *const api::Events) -> Vec<[u8; 3]> {
        unsafe {
            let base = block as *const u8;
            let num = std::ptr::read_unaligned(base as *const i32);
            let array = base.add(std::mem::offset_of!(api::Events, events))
                as *const *const api::MidiEvent;
            (0..num as usize)
                .map(|i| (*std::ptr::read_unaligned(array.add(i))).midi_data)
                .collect()
        }
    }

    /// Three messages queued in one cycle must all reach one batch. The old code called
    /// `process_events` once per message, so a chord lost every note but the last.
    #[test]
    fn batched_events_all_survive() {
        let messages = [[0x90u8, 60, 100], [0x90, 64, 100], [0x90, 67, 100]];
        let mut batch = EventBatch::with_capacity(MAX_PENDING_MIDI);
        let block = batch.fill(messages.into_iter());
        assert_eq!(unsafe { read_block(block) }, messages);
    }

    /// A plugin may keep the `VstEvents*` from `processEvents` and read it in the
    /// following `process`. The block must still hold the same events after the call
    /// that built it has returned and other allocations have come and gone.
    #[test]
    fn block_outlives_process_events() {
        let mut batch = EventBatch::with_capacity(MAX_PENDING_MIDI);
        let block = batch.fill([[0x90u8, 60, 100], [0x80, 60, 0]].into_iter());
        let churn: Vec<Vec<u8>> = (0..64).map(|i| vec![0xAA; 64 + i]).collect();
        drop(churn);
        assert_eq!(unsafe { read_block(block) }, [[0x90, 60, 100], [0x80, 60, 0]]);
    }

    /// Filling never reallocates, so events never move under a plugin, and a single
    /// event still leaves the block as large as `api::Events`.
    #[test]
    fn fill_keeps_its_buffers() {
        let mut batch = EventBatch::with_capacity(4);
        let events = batch.events.as_ptr();
        let block = batch.block.as_ptr();
        batch.fill(std::iter::repeat([0x90, 60, 100]).take(10));
        assert_eq!(batch.events.len(), 4, "extra messages are dropped, not reallocated");
        batch.fill(std::iter::once([0x80, 60, 0]));
        assert_eq!(batch.events.as_ptr(), events);
        assert_eq!(batch.block.as_ptr(), block);
        assert!(batch.block.len() * std::mem::size_of::<usize>() >= std::mem::size_of::<api::Events>());
    }
}
