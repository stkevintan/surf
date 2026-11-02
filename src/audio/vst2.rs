#![allow(deprecated)]

//! VST2 instrument hosting, used for portable Kontakt builds whose VST3
//! wrapper is not compatible with `vst3-host`.
//!
//! Like the VST3 path, the plugin lives in the helper process.  The UI process
//! only sees shared-memory audio and JSON commands.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use vst::api;
use vst::buffer::AudioBuffer;
use vst::editor::Editor;
use vst::host::{Host, PluginInstance, PluginLoader};
use vst::plugin::Plugin;
use windows::core::HSTRING;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow, HICON, HCURSOR, IsWindow,
    RegisterClassW, SW_SHOW, SetWindowPos, ShowWindow, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};

use super::Instrument;

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
        let directory = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let host = Arc::new(Mutex::new(SurfVst2Host {
            sample_rate: sample_rate as f32,
            directory,
        }));
        let mut loader = PluginLoader::load(&path, host)
            .map_err(|e| anyhow::anyhow!("could not load VST2 plugin {}: {e}", path.display()))?;
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
            pending_midi: Vec::with_capacity(64),
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
        let mut editor = plugin
            .get_editor()
            .context("VST2 plugin has no editor")?;
        let window = NativeWindow::new(&self.name)?;
        let parent = window.hwnd.0 as *mut c_void;
        if !editor.open(parent) {
            anyhow::bail!("VST2 editor refused to open");
        }
        let (w, h) = editor.size();
        window.resize(w, h);
        window.show();
        Ok(Vst2EditorWindow { window, editor })
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
}

impl Vst2Instrument {
    fn send_midi(&mut self, data: [u8; 3]) {
        // Bound the queue so a stalled audio thread cannot grow it without limit.
        // 512 messages is far more than a human can play into one block.
        if self.pending_midi.len() < 512 {
            self.pending_midi.push(data);
        }
    }

    /// Hand every queued message to the plugin as one `processEvents` batch.
    ///
    /// Takes the queue by value because the caller already holds the plugin lock, which
    /// borrows `self`. Must be called immediately before `process`.
    fn flush_midi(pending: &mut Vec<[u8; 3]>, plugin: &mut PluginInstance) {
        if pending.is_empty() {
            return;
        }
        // The plugin reads these during processEvents/process, so they only need to
        // outlive this function — but `api::Events` is a fixed two-pointer struct, so a
        // batch larger than that needs a backing allocation whose pointers stay valid
        // for the whole call.
        let mut events: Vec<Box<api::MidiEvent>> = pending
            .drain(..)
            .map(|data| {
                Box::new(api::MidiEvent {
                    event_type: api::EventType::Midi,
                    byte_size: std::mem::size_of::<api::MidiEvent>() as i32,
                    // All events land at the start of the block. Sample-accurate
                    // placement would need the arrival offset plumbed through the IPC
                    // ring; at 64-frame blocks this is under 1.4 ms of quantisation.
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
                })
            })
            .collect();

        let ptrs: Vec<*mut api::Event> = events
            .iter_mut()
            .map(|e| e.as_mut() as *mut api::MidiEvent as *mut api::Event)
            .collect();

        // `api::Events` declares `events: [*mut Event; 2]`, but the VST2 ABI treats it as
        // a variable-length array of `num_events` pointers. Build the real layout by
        // hand, taking the array offset from the struct itself: `num_events` is an i32
        // followed by an isize, so repr(C) pads it out to isize alignment. Adding the two
        // field sizes instead would place every pointer 4 bytes early on x64.
        let header = std::mem::offset_of!(api::Events, events);
        let mut raw = vec![0u8; header + ptrs.len() * std::mem::size_of::<*mut api::Event>()];
        // SAFETY: `raw` is sized for the header plus exactly `ptrs.len()` pointers, and
        // the writes below stay inside it. The plugin only reads it during this call.
        unsafe {
            let base = raw.as_mut_ptr();
            std::ptr::write_unaligned(base as *mut i32, ptrs.len() as i32);
            std::ptr::write_unaligned(base.add(std::mem::size_of::<i32>()) as *mut isize, 0);
            let array = base.add(header) as *mut *mut api::Event;
            for (i, ptr) in ptrs.iter().enumerate() {
                std::ptr::write_unaligned(array.add(i), *ptr);
            }
            plugin.process_events(&*(base as *const api::Events));
        }
    }
}

impl Instrument for Vst2Instrument {
    fn note_on(&mut self, key: u8, velocity: u8) {
        self.send_midi([0x90, key, velocity]);
    }

    fn note_off(&mut self, key: u8) {
        self.send_midi([0x80, key, 0]);
    }

    fn all_notes_off(&mut self) {
        self.send_midi([0xB0, 123, 0]);
    }

    fn set_sustain(&mut self, down: bool) {
        self.send_midi([0xB0, 64, if down { 127 } else { 0 }]);
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
        // consumes them, and immediately before it.
        Self::flush_midi(&mut self.pending_midi, &mut plugin);
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

unsafe extern "system" fn vst2_wndproc(
    hwnd: HWND,
    msg: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

struct NativeWindow {
    hwnd: HWND,
    class: HSTRING,
}

impl NativeWindow {
    fn new(title: &str) -> Result<Self> {
        let class = HSTRING::from("SurfVst2Editor");
        let instance = unsafe { GetModuleHandleW(None)? };
        let wc = WNDCLASSW {
            style: Default::default(),
            lpfnWndProc: Some(vst2_wndproc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: instance.into(),
            hIcon: HICON::default(),
            hCursor: HCURSOR::default(),
            hbrBackground: HBRUSH::default(),
            lpszMenuName: windows::core::PCWSTR::null(),
            lpszClassName: windows::core::PCWSTR(class.as_ptr()),
        };
        unsafe {
            let _ = RegisterClassW(&wc);
        }
        let title = HSTRING::from(title);
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                &class,
                &title,
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                900,
                700,
                None,
                None,
                Some(instance.into()),
                None,
            )?
        };
        Ok(Self { hwnd, class })
    }

    fn resize(&self, width: i32, height: i32) {
        if width <= 0 || height <= 0 {
            return;
        }
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                width,
                height,
                Default::default(),
            );
        }
    }

    fn show(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
    }
}

impl Drop for NativeWindow {
    fn drop(&mut self) {
        unsafe {
            if IsWindow(Some(self.hwnd)).as_bool() {
                let _ = DestroyWindow(self.hwnd);
            }
        }
    }
}

pub struct Vst2EditorWindow {
    window: NativeWindow,
    editor: Box<dyn Editor>,
}

impl Vst2EditorWindow {
    pub fn idle(&mut self) {
        self.editor.idle();
    }
}

impl Drop for Vst2EditorWindow {
    fn drop(&mut self) {
        if self.editor.is_open() {
            self.editor.close();
        }
        let _ = &self.window.class;
    }
}

#[cfg(test)]
mod tests {
    use vst::api;

    /// The `VstEvents` block must be laid out the way the VST2 ABI reads it.
    ///
    /// `api::Events` declares `events: [*mut Event; 2]`, but the ABI treats that as a
    /// variable-length array of `num_events` pointers. `flush_midi` therefore builds the
    /// block by hand, and a wrong offset there would hand Kontakt garbage pointers — so
    /// assert the layout this code depends on rather than trusting the struct definition.
    #[test]
    fn events_header_layout_matches_abi() {
        // `num_events: i32` is followed by `_reserved: isize`, so repr(C) pads the i32 out
        // to isize alignment: the pointer array starts at 2 * size_of::<isize>(), not at
        // size_of::<i32>() + size_of::<isize>(). `flush_midi` must use `offset_of` rather
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

    /// Build the block exactly as `flush_midi` does and read it back as the plugin would.
    ///
    /// This is the regression guard for the dropped-notes bug: three messages queued in
    /// one cycle must all survive into a single batch. The old code called
    /// `process_events` once per message with `num_events: 1`, so a chord lost every note
    /// but the last.
    #[test]
    fn batched_events_all_survive() {
        let messages = [[0x90u8, 60, 100], [0x90, 64, 100], [0x90, 67, 100]];

        let mut events: Vec<Box<api::MidiEvent>> = messages
            .iter()
            .map(|data| {
                Box::new(api::MidiEvent {
                    event_type: api::EventType::Midi,
                    byte_size: std::mem::size_of::<api::MidiEvent>() as i32,
                    delta_frames: 0,
                    flags: 0,
                    note_length: 0,
                    note_offset: 0,
                    midi_data: *data,
                    _midi_reserved: 0,
                    detune: 0,
                    note_off_velocity: 0,
                    _reserved1: 0,
                    _reserved2: 0,
                })
            })
            .collect();
        let ptrs: Vec<*mut api::Event> = events
            .iter_mut()
            .map(|e| e.as_mut() as *mut api::MidiEvent as *mut api::Event)
            .collect();

        let header = std::mem::offset_of!(api::Events, events);
        let mut raw = vec![0u8; header + ptrs.len() * std::mem::size_of::<*mut api::Event>()];
        // SAFETY: `raw` is sized for the header plus exactly `ptrs.len()` pointers.
        unsafe {
            let base = raw.as_mut_ptr();
            std::ptr::write_unaligned(base as *mut i32, ptrs.len() as i32);
            std::ptr::write_unaligned(base.add(std::mem::size_of::<i32>()) as *mut isize, 0);
            let array = base.add(header) as *mut *mut api::Event;
            for (i, ptr) in ptrs.iter().enumerate() {
                std::ptr::write_unaligned(array.add(i), *ptr);
            }

            // Now read it back the way a plugin walks VstEvents.
            let num = std::ptr::read_unaligned(base as *const i32);
            assert_eq!(num, 3, "all three notes must be in one batch");
            let array = base.add(header) as *const *mut api::Event;
            for (i, expected) in messages.iter().enumerate() {
                let event = std::ptr::read_unaligned(array.add(i)) as *const api::MidiEvent;
                assert_eq!(
                    (*event).midi_data, *expected,
                    "event {i} must keep its own MIDI bytes"
                );
            }
        }
    }
}
