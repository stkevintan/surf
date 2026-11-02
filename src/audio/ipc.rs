//! Shared-memory audio/MIDI transport between the UI process and the VST3
//! helper process.
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

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use anyhow::{Context, Result};
use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};

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
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct MidiMsg {
    pub kind: u32,
    pub a: u32,
    pub b: u32,
    pub _pad: u32,
}

impl MidiMsg {
    pub fn note_on(key: u8, velocity: u8) -> Self {
        Self {
            kind: MidiKind::NoteOn as u32,
            a: key as u32,
            b: velocity as u32,
            _pad: 0,
        }
    }

    pub fn note_off(key: u8) -> Self {
        Self {
            kind: MidiKind::NoteOff as u32,
            a: key as u32,
            b: 0,
            _pad: 0,
        }
    }

    pub fn sustain(down: bool) -> Self {
        Self {
            kind: MidiKind::Sustain as u32,
            a: down as u32,
            b: 0,
            _pad: 0,
        }
    }

    pub fn all_notes_off() -> Self {
        Self {
            kind: MidiKind::AllNotesOff as u32,
            a: 0,
            b: 0,
            _pad: 0,
        }
    }
}

pub struct SharedMap {
    ptr: *mut u8,
    handle: HANDLE,
    _private: (),
}

// The mapping is shared by the UI and helper process; access is synchronised
// with the atomics in the header.
unsafe impl Send for SharedMap {}
unsafe impl Sync for SharedMap {}

impl SharedMap {
    pub fn create(name: &str) -> Result<Self> {
        let name = HSTRING::from(name);
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                TOTAL_SIZE as u32,
                &name,
            )
        }
        .context("CreateFileMappingW failed")?;
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, TOTAL_SIZE) };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            anyhow::bail!("MapViewOfFile failed");
        }
        let map = Self {
            ptr: view.Value as *mut u8,
            handle,
            _private: (),
        };
        unsafe {
            ptr::write_bytes(map.ptr, 0, TOTAL_SIZE);
        }
        map.atomic_u64(OFF_MAGIC).store(MAGIC, Ordering::Release);
        map.set_helper_alive(false);
        map.atomic_u32(OFF_MAIN_ALIVE).store(1, Ordering::Release);
        Ok(map)
    }

    pub fn open(name: &str) -> Result<Self> {
        let name = HSTRING::from(name);
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &name) }
            .context("OpenFileMappingW failed")?;
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, TOTAL_SIZE) };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            anyhow::bail!("MapViewOfFile failed");
        }
        let map = Self {
            ptr: view.Value as *mut u8,
            handle,
            _private: (),
        };
        if map.atomic_u64(OFF_MAGIC).load(Ordering::Acquire) != MAGIC {
            anyhow::bail!("shared memory magic mismatch");
        }
        Ok(map)
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

impl Drop for SharedMap {
    fn drop(&mut self) {
        unsafe {
            let view = MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ptr as *mut c_void,
            };
            let _ = UnmapViewOfFile(view);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Generate a per-instance local mapping name.
pub fn make_name() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("Local\\SurfVst-{pid}-{nanos}")
}
