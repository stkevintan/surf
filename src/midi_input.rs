//! Live MIDI input via the Windows WinMM MIDI API.
//!
//! `midir` would be the portable choice, but its ALSA dependency conflicts with
//! `cpal`'s ALSA link on Linux in the same lockfile.  WinMM is the native API on
//! the target platform and keeps the dependency graph small.

use std::mem::size_of;
use std::time::Instant;

use anyhow::Result;
use crossbeam_channel::{Receiver, Sender, unbounded};
use windows::Win32::Media::Audio::{
    CALLBACK_FUNCTION, HMIDIIN, MIDIINCAPSW, midiInClose, midiInGetDevCapsW, midiInGetNumDevs,
    midiInOpen, midiInReset, midiInStart, midiInStop,
};
use windows::Win32::Media::MM_MIM_DATA;

use crate::audio::Command;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LiveEvent {
    NoteOn { key: u8, velocity: u8 },
    NoteOff { key: u8 },
    Sustain(bool),
}

#[derive(Clone, Copy, Debug)]
pub struct TimedMidi {
    pub at: Instant,
    pub event: LiveEvent,
}

struct CallbackCtx {
    tx: Sender<TimedMidi>,
    audio_tx: Sender<Command>,
}

unsafe extern "system" fn midi_callback(
    _handle: HMIDIIN,
    message: u32,
    instance: usize,
    param1: usize,
    _param2: usize,
) {
    if message != MM_MIM_DATA || instance == 0 {
        return;
    }
    // SAFETY: `instance` points at the `CallbackCtx` kept alive by `MidiIn`.
    let ctx = unsafe { &*(instance as *const CallbackCtx) };
    let status = (param1 & 0xFF) as u8;
    let data1 = ((param1 >> 8) & 0xFF) as u8;
    let data2 = ((param1 >> 16) & 0xFF) as u8;
    let Some(event) = parse_message(status, data1, data2) else {
        return;
    };
    let at = Instant::now();
    match event {
        LiveEvent::NoteOn { key, velocity } => {
            let _ = ctx.audio_tx.send(Command::NoteOn { key, velocity });
        }
        LiveEvent::NoteOff { key } => {
            let _ = ctx.audio_tx.send(Command::NoteOff { key });
        }
        LiveEvent::Sustain(down) => {
            let _ = ctx.audio_tx.send(Command::Sustain(down));
        }
    }
    let _ = ctx.tx.send(TimedMidi { at, event });
}

struct MidiIn {
    handle: HMIDIIN,
    ctx: *mut CallbackCtx,
    _name: String,
}

impl Drop for MidiIn {
    fn drop(&mut self) {
        unsafe {
            let _ = midiInStop(self.handle);
            let _ = midiInReset(self.handle);
            let _ = midiInClose(self.handle);
        }
        if !self.ctx.is_null() {
            // SAFETY: the pointer came from Box::into_raw in `open`.
            unsafe {
                drop(Box::from_raw(self.ctx));
            }
        }
    }
}

fn open_device(index: u32, tx: Sender<TimedMidi>, audio_tx: Sender<Command>) -> Result<MidiIn> {
    let mut caps = MIDIINCAPSW::default();
    let rc =
        unsafe { midiInGetDevCapsW(index as usize, &mut caps, size_of::<MIDIINCAPSW>() as u32) };
    if rc != 0 {
        anyhow::bail!("midiInGetDevCapsW failed with code {rc}");
    }
    let pname = unsafe { std::ptr::addr_of!(caps.szPname).read_unaligned() };
    let name = wide_to_string(&pname);
    let ctx = Box::into_raw(Box::new(CallbackCtx { tx, audio_tx }));
    let mut handle = HMIDIIN(std::ptr::null_mut());
    let rc = unsafe {
        midiInOpen(
            &mut handle,
            index,
            Some(midi_callback as *const () as usize),
            Some(ctx as usize),
            CALLBACK_FUNCTION,
        )
    };
    if rc != 0 {
        // SAFETY: `ctx` was just allocated above and is not shared yet.
        unsafe {
            drop(Box::from_raw(ctx));
        }
        anyhow::bail!("midiInOpen failed with code {rc}");
    }
    let rc = unsafe { midiInStart(handle) };
    if rc != 0 {
        unsafe {
            let _ = midiInClose(handle);
            drop(Box::from_raw(ctx));
        }
        anyhow::bail!("midiInStart failed with code {rc}");
    }
    Ok(MidiIn {
        handle,
        ctx,
        _name: name,
    })
}

pub struct MidiHub {
    devices: Vec<String>,
    connection: Option<MidiIn>,
    selected: Option<usize>,
    rx: Receiver<TimedMidi>,
    tx: Sender<TimedMidi>,
    audio_tx: Sender<Command>,
    error: Option<String>,
}

impl MidiHub {
    pub fn new(audio_tx: Sender<Command>) -> Self {
        let (tx, rx) = unbounded();
        let mut hub = Self {
            devices: Vec::new(),
            connection: None,
            selected: None,
            rx,
            tx,
            audio_tx,
            error: None,
        };
        hub.refresh();
        hub
    }

    pub fn refresh(&mut self) {
        let count = unsafe { midiInGetNumDevs() };
        let mut devices = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut caps = MIDIINCAPSW::default();
            let rc = unsafe {
                midiInGetDevCapsW(index as usize, &mut caps, size_of::<MIDIINCAPSW>() as u32)
            };
            if rc == 0 {
                let pname = unsafe { std::ptr::addr_of!(caps.szPname).read_unaligned() };
                devices.push(wide_to_string(&pname));
            }
        }
        self.devices = devices;
        if self.selected.is_some_and(|i| i >= self.devices.len()) {
            self.disconnect();
        }
        if self.connection.is_none() && !self.devices.is_empty() {
            let _ = self.connect(0);
        }
    }

    pub fn ports(&self) -> Vec<String> {
        self.devices.clone()
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn connect(&mut self, index: usize) -> Result<()> {
        if index >= self.devices.len() {
            anyhow::bail!("no MIDI input at index {index}");
        }
        self.connection = None;
        self.selected = None;
        match open_device(index as u32, self.tx.clone(), self.audio_tx.clone()) {
            Ok(connection) => {
                self.connection = Some(connection);
                self.selected = Some(index);
                self.error = None;
                Ok(())
            }
            Err(err) => {
                self.error = Some(err.to_string());
                Err(err)
            }
        }
    }

    pub fn disconnect(&mut self) {
        self.connection = None;
        self.selected = None;
    }

    pub fn drain(&self) -> Vec<TimedMidi> {
        self.rx.try_iter().collect()
    }
}

fn parse_message(status_byte: u8, data1: u8, data2: u8) -> Option<LiveEvent> {
    let status = status_byte & 0xF0;
    let key = data1 & 0x7F;
    let value = data2 & 0x7F;
    match status {
        0x90 => {
            if value == 0 {
                Some(LiveEvent::NoteOff { key })
            } else {
                Some(LiveEvent::NoteOn {
                    key,
                    velocity: value,
                })
            }
        }
        0x80 => Some(LiveEvent::NoteOff { key }),
        0xB0 if key == 64 => Some(LiveEvent::Sustain(value >= 64)),
        _ => None,
    }
}

fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}

/// Open a specific port by name; useful for settings persistence.
pub fn connect_by_name(hub: &mut MidiHub, name: &str) -> Result<()> {
    if let Some(index) = hub.ports().iter().position(|p| p == name) {
        hub.connect(index)
    } else {
        Err(anyhow::anyhow!("MIDI input {name} is not connected"))
    }
}
