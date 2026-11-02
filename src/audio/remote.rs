//! UI-side view of a VST3 hosted in `surf-vst-host.exe`.
//!
//! The audio callback only touches the shared-memory ring; it never calls into
//! the plugin process.

use std::sync::Arc;

use super::ipc::SharedMap;
use super::Instrument;

pub struct RemoteInstrument {
    map: Arc<SharedMap>,
    name: String,
}

impl RemoteInstrument {
    pub fn new(map: Arc<SharedMap>, name: impl Into<String>) -> Self {
        Self {
            map,
            name: name.into(),
        }
    }

    pub fn helper_alive(&self) -> bool {
        self.map.helper_alive()
    }
}

impl Instrument for RemoteInstrument {
    fn note_on(&mut self, key: u8, velocity: u8) {
        let _ = self
            .map
            .push_midi(super::ipc::MidiMsg::note_on(key, velocity));
    }

    fn note_off(&mut self, key: u8) {
        let _ = self.map.push_midi(super::ipc::MidiMsg::note_off(key));
    }

    fn all_notes_off(&mut self) {
        let _ = self
            .map
            .push_midi(super::ipc::MidiMsg::all_notes_off());
    }

    fn set_sustain(&mut self, down: bool) {
        let _ = self
            .map
            .push_midi(super::ipc::MidiMsg::sustain(down));
    }

    fn process(&mut self, out: &mut [f32], frames: usize) {
        let frames = frames.min(out.len() / 2);
        out[..frames * 2].fill(0.0);
        // Bound a runaway backlog, but stay clear of the cushion the helper keeps
        // (512 frames by default): trimming into the producer's working set would
        // clip the queue every callback and chop up the audio. This only fires when
        // something genuinely ran away -- e.g. the consumer was blocked by a long GUI
        // operation while the helper kept producing.
        const MAX_BACKLOG: u64 = 1_536; // ~32 ms @ 48 kHz
        self.map.trim_audio_backlog(MAX_BACKLOG);
        // A short underrun is normal while the helper is being scheduled;
        // leave the remainder silent rather than repeating stale audio.
        let _ = self.map.pop_audio(&mut out[..frames * 2]);
    }

    fn latency_samples(&self) -> u32 {
        // The helper owns the plugin latency; the real-time path is
        // intentionally not latency-compensated yet.
        0
    }

    fn name(&self) -> &str {
        &self.name
    }
}
