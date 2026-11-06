//! Shared-memory audio/MIDI transport between the UI process and a plugin
//! host process.
//!
//! Layout is a fixed header followed by two SPSC rings:
//!
//! ```text
//! +----------------------+--------------------------+-------------------+
//! | atomics / metadata   | stereo f32 audio ring    | MIDI message ring |
//! +----------------------+--------------------------+-------------------+
//! ```
//!
//! The helper writes audio/MIDI; the UI reads audio and writes MIDI.  Indices
//! are monotonic framecounts; only the producer advances write, only the
//! consumer advances read.

use std::mem::size_of;
use std::ptr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use anyhow::Result;
use surf_platform::shm::SharedRegion;

pub const AUDIO_CAPACITY: usize = 16_384; // stereo frames (~341 ms @ 48 kHz)
pub const MIDI_CAPACITY: usize = 1_024;

const MAGIC: u64 = 0x5355_5246_5653_5431; // "SURFVST1"
const HEADER_SIZE: usize = 64;
const AUDIO_OFFSET: usize = HEADER_SIZE;
const MIDI_OFFSET: usize = AUDIO_OFFSET + AUDIO_CAPACITY * 2 * size_of::<f32>();
const TOTAL_SIZE: usize = MIDI_OFFSET + MIDI_CAPACITY * size_of::<MidiMsg>();

const OFF_MAGIC: usize = 0;
const OFF_SAMPLE_RATE: usize = 8;
const OFF_CHANNELS: usize = 12;
const OFF_AUDIO_WRITE: usize = 16;
const OFF_AUDIO_READ: usize = 24;
const OFF_MIDI_WRITE: usize = 32;
const OFF_MIDI_READ: usize = 40;
const OFF_HELPER_ALIVE: usize = 48;
const OFF_MAIN_ALIVE: usize = 52;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum MidiKind {
    NoteOn = 1,
    NoteOff = 2,
    Sustain = 3,
    AllNotesOff = 4,
    PitchBend = 5,
    ControlChange = 6,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MidiMsg {
    pub kind: u32,
    pub a: u32,
    pub b: u32,
    /// MIDI channel 0..=15. Was padding before channels were sent, so an
    /// older UI simply plays everything on channel 1.
    pub channel: u32,
}

impl MidiMsg {
    pub fn note_on(channel: u8, key: u8, velocity: u8) -> Self {
        Self::new(MidiKind::NoteOn, channel, key as u32, velocity as u32)
    }

    pub fn note_off(channel: u8, key: u8) -> Self {
        Self::new(MidiKind::NoteOff, channel, key as u32, 0)
    }

    pub fn sustain(channel: u8, down: bool) -> Self {
        Self::new(MidiKind::Sustain, channel, down as u32, 0)
    }

    pub fn all_notes_off() -> Self {
        Self::new(MidiKind::AllNotesOff, 0, 0, 0)
    }

    pub fn control_change(channel: u8, controller: u8, value: u8) -> Self {
        Self::new(
            MidiKind::ControlChange,
            channel,
            controller as u32,
            value as u32,
        )
    }

    fn new(kind: MidiKind, channel: u8, a: u32, b: u32) -> Self {
        Self {
            kind: kind as u32,
            a,
            b,
            channel: (channel & 15) as u32,
        }
    }
}

pub struct SharedMap {
    region: SharedRegion,
    ptr: *mut u8,
}

// The mapping is shared by the UI and helper process; access is synchronised
// with the atomics in the header.
unsafe impl Send for SharedMap {}
unsafe impl Sync for SharedMap {}

impl SharedMap {
    pub fn create(name: &str) -> Result<Self> {
        let region = SharedRegion::create(name, TOTAL_SIZE)?;
        let map = Self {
            ptr: region.as_ptr(),
            region,
        };
        map.atomic_u64(OFF_MAGIC).store(MAGIC, Ordering::Release);
        map.set_helper_alive(false);
        map.atomic_u32(OFF_MAIN_ALIVE).store(1, Ordering::Release);
        Ok(map)
    }

    pub fn open(name: &str) -> Result<Self> {
        let region = SharedRegion::open(name, TOTAL_SIZE)?;
        let map = Self {
            ptr: region.as_ptr(),
            region,
        };
        if map.atomic_u64(OFF_MAGIC).load(Ordering::Acquire) != MAGIC {
            anyhow::bail!("shared memory magic mismatch");
        }
        Ok(map)
    }

    /// Size of the mapping, for diagnostics.
    pub fn size(&self) -> usize {
        self.region.len()
    }

    pub fn sample_rate(&self) -> u32 {
        self.atomic_u32(OFF_SAMPLE_RATE).load(Ordering::Acquire)
    }

    pub fn set_sample_rate(&self, sample_rate: u32) {
        self.atomic_u32(OFF_SAMPLE_RATE)
            .store(sample_rate, Ordering::Release);
        self.atomic_u32(OFF_CHANNELS).store(2, Ordering::Release);
    }

    pub fn set_helper_alive(&self, alive: bool) {
        self.atomic_u32(OFF_HELPER_ALIVE)
            .store(alive as u32, Ordering::Release);
    }

    pub fn helper_alive(&self) -> bool {
        self.atomic_u32(OFF_HELPER_ALIVE).load(Ordering::Acquire) != 0
    }

    pub fn set_main_alive(&self, alive: bool) {
        self.atomic_u32(OFF_MAIN_ALIVE)
            .store(alive as u32, Ordering::Release);
    }

    pub fn main_alive(&self) -> bool {
        self.atomic_u32(OFF_MAIN_ALIVE).load(Ordering::Acquire) != 0
    }

    /// Frames currently sitting in the audio ring.
    ///
    /// This is live-play latency: every queued frame is sound the user has already
    /// triggered but cannot hear yet. Useful for diagnosing a backlog that the
    /// consumer never drains.
    pub fn audio_backlog(&self) -> u64 {
        let write = self.atomic_u64(OFF_AUDIO_WRITE).load(Ordering::Acquire);
        let read = self.atomic_u64(OFF_AUDIO_READ).load(Ordering::Acquire);
        write.wrapping_sub(read)
    }

    /// Drop all but `keep` frames of queued audio.
    ///
    /// The helper produces on a wall-clock schedule while the consumer drains one
    /// output buffer per callback; any excess becomes permanent latency, since
    /// nothing else ever shortens the queue. Skipping stale frames trades a tiny
    /// discontinuity for staying responsive, which is the right trade for live
    /// playing.
    pub fn trim_audio_backlog(&self, keep: u64) {
        let write = self.atomic_u64(OFF_AUDIO_WRITE).load(Ordering::Acquire);
        let read = self.atomic_u64(OFF_AUDIO_READ).load(Ordering::Relaxed);
        let queued = write.wrapping_sub(read);
        if queued > keep {
            self.atomic_u64(OFF_AUDIO_READ)
                .store(write.wrapping_sub(keep), Ordering::Release);
        }
    }

    /// Push interleaved stereo frames. Returns false if the ring is full.
    pub fn push_audio(&self, frames: &[f32]) -> bool {
        let count = frames.len() / 2;
        if count == 0 || count > AUDIO_CAPACITY {
            return false;
        }
        let write = self.atomic_u64(OFF_AUDIO_WRITE).load(Ordering::Relaxed);
        let read = self.atomic_u64(OFF_AUDIO_READ).load(Ordering::Acquire);
        if write.wrapping_sub(read) + count as u64 > AUDIO_CAPACITY as u64 {
            return false;
        }
        let ring = unsafe { self.ptr.add(AUDIO_OFFSET) as *mut f32 };
        let start = (write as usize % AUDIO_CAPACITY) * 2;
        let first = (AUDIO_CAPACITY * 2 - start).min(frames.len());
        unsafe {
            ptr::copy_nonoverlapping(frames.as_ptr(), ring.add(start), first);
            if first < frames.len() {
                ptr::copy_nonoverlapping(frames.as_ptr().add(first), ring, frames.len() - first);
            }
        }
        self.atomic_u64(OFF_AUDIO_WRITE)
            .store(write.wrapping_add(count as u64), Ordering::Release);
        true
    }

    /// Pop interleaved stereo frames. Returns the number of complete frames.
    pub fn pop_audio(&self, out: &mut [f32]) -> usize {
        let want = out.len() / 2;
        if want == 0 {
            return 0;
        }
        let read = self.atomic_u64(OFF_AUDIO_READ).load(Ordering::Relaxed);
        let write = self.atomic_u64(OFF_AUDIO_WRITE).load(Ordering::Acquire);
        let available = write.wrapping_sub(read) as usize;
        let count = want.min(available).min(AUDIO_CAPACITY);
        if count == 0 {
            return 0;
        }
        let ring = unsafe { self.ptr.add(AUDIO_OFFSET) as *const f32 };
        let start = (read as usize % AUDIO_CAPACITY) * 2;
        let first = (AUDIO_CAPACITY * 2 - start).min(count * 2);
        unsafe {
            ptr::copy_nonoverlapping(ring.add(start), out.as_mut_ptr(), first);
            if first < count * 2 {
                ptr::copy_nonoverlapping(ring, out.as_mut_ptr().add(first), count * 2 - first);
            }
        }
        self.atomic_u64(OFF_AUDIO_READ)
            .store(read.wrapping_add(count as u64), Ordering::Release);
        count
    }

    pub fn push_midi(&self, msg: MidiMsg) -> bool {
        let write = self.atomic_u64(OFF_MIDI_WRITE).load(Ordering::Relaxed);
        let read = self.atomic_u64(OFF_MIDI_READ).load(Ordering::Acquire);
        if write.wrapping_sub(read) >= MIDI_CAPACITY as u64 {
            return false;
        }
        let ring = unsafe { self.ptr.add(MIDI_OFFSET) as *mut MidiMsg };
        unsafe {
            ring.add(write as usize % MIDI_CAPACITY).write(msg);
        }
        self.atomic_u64(OFF_MIDI_WRITE)
            .store(write.wrapping_add(1), Ordering::Release);
        true
    }

    pub fn pop_midi(&self) -> Option<MidiMsg> {
        let read = self.atomic_u64(OFF_MIDI_READ).load(Ordering::Relaxed);
        let write = self.atomic_u64(OFF_MIDI_WRITE).load(Ordering::Acquire);
        if read == write {
            return None;
        }
        let ring = unsafe { self.ptr.add(MIDI_OFFSET) as *const MidiMsg };
        let msg = unsafe { ring.add(read as usize % MIDI_CAPACITY).read() };
        self.atomic_u64(OFF_MIDI_READ)
            .store(read.wrapping_add(1), Ordering::Release);
        Some(msg)
    }

    fn atomic_u64(&self, offset: usize) -> &AtomicU64 {
        unsafe { &*(self.ptr.add(offset) as *const AtomicU64) }
    }

    fn atomic_u32(&self, offset: usize) -> &AtomicU32 {
        unsafe { &*(self.ptr.add(offset) as *const AtomicU32) }
    }
}

pub use surf_platform::shm::make_name;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    /// Two mappings of one region, as the UI and the host process would hold.
    fn pair() -> (SharedMap, SharedMap) {
        let name = make_name();
        let creator = SharedMap::create(&name).unwrap();
        let opener = SharedMap::open(&name).unwrap();
        (creator, opener)
    }

    #[test]
    fn header_is_shared_between_mappings() {
        let (ui, host) = pair();
        ui.set_sample_rate(44_100);
        assert_eq!(host.sample_rate(), 44_100);
        assert!(!ui.helper_alive());
        host.set_helper_alive(true);
        assert!(ui.helper_alive());
        assert!(host.main_alive(), "creator marks the main process alive");
    }

    #[test]
    fn midi_crosses_the_mapping_in_order() {
        let (ui, host) = pair();
        assert!(ui.push_midi(MidiMsg::note_on(2, 60, 100)));
        assert!(ui.push_midi(MidiMsg::sustain(2, true)));
        assert!(ui.push_midi(MidiMsg::note_off(2, 60)));
        assert!(ui.push_midi(MidiMsg::control_change(5, 7, 90)));
        let kinds: Vec<_> = std::iter::from_fn(|| host.pop_midi())
            .map(|m| (m.kind, m.channel, m.a, m.b))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (MidiKind::NoteOn as u32, 2, 60, 100),
                (MidiKind::Sustain as u32, 2, 1, 0),
                (MidiKind::NoteOff as u32, 2, 60, 0),
                (MidiKind::ControlChange as u32, 5, 7, 90),
            ]
        );
    }

    /// The host's real pattern (64-frame blocks, topped up to a cushion) against
    /// the UI's (480-frame device buffers), on two threads, across the wrap
    /// point several times. Every sample must arrive once, in order.
    #[test]
    fn audio_streams_intact_across_threads_and_wraps() {
        let (ui, host) = pair();
        let host = Arc::new(host);
        let total_frames: u64 = AUDIO_CAPACITY as u64 * 4 + 37;

        let producer = {
            let host = host.clone();
            std::thread::spawn(move || {
                let mut next = 0u64;
                let mut block = [0.0f32; 128];
                while next < total_frames {
                    if host.audio_backlog() + 64 > 4_096 {
                        std::thread::yield_now();
                        continue;
                    }
                    let n = (total_frames - next).min(64) as usize;
                    for i in 0..n {
                        // Left carries the frame index, right its negative.
                        block[i * 2] = (next + i as u64) as f32;
                        block[i * 2 + 1] = -((next + i as u64) as f32);
                    }
                    assert!(host.push_audio(&block[..n * 2]));
                    next += n as u64;
                }
            })
        };

        let mut seen = 0u64;
        let mut buf = vec![0.0f32; 480 * 2];
        while seen < total_frames {
            let got = ui.pop_audio(&mut buf) as u64;
            for i in 0..got {
                let expected = (seen + i) as f32;
                assert_eq!(buf[i as usize * 2], expected);
                assert_eq!(buf[i as usize * 2 + 1], -expected);
            }
            seen += got;
            if got == 0 {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
        assert_eq!(ui.audio_backlog(), 0);
    }

    #[test]
    fn full_ring_rejects_instead_of_overwriting() {
        let (ui, host) = pair();
        let block = vec![0.5f32; 1024 * 2];
        for _ in 0..AUDIO_CAPACITY / 1024 {
            assert!(host.push_audio(&block));
        }
        assert!(!host.push_audio(&block), "a full ring must refuse more");
        ui.trim_audio_backlog(512);
        assert_eq!(ui.audio_backlog(), 512);
        assert!(host.push_audio(&block));
    }
}
