//! Windows backend: the WinMM MIDI API.
//!
//! WinMM is the native API here and keeps the dependency graph small. The
//! callback runs on a driver thread, parses the message and hands it to
//! [`Deliver`], which fans it out to the audio engine and the UI.

use std::mem::size_of;

use anyhow::Result;
use windows::Win32::Media::Audio::{
    CALLBACK_FUNCTION, HMIDIIN, MIDIINCAPSW, midiInClose, midiInGetDevCapsW, midiInGetNumDevs,
    midiInOpen, midiInReset, midiInStart, midiInStop,
};
use windows::Win32::Media::MM_MIM_DATA;

use super::{Deliver, parse_message};

struct CallbackCtx {
    deliver: Deliver,
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
    // SAFETY: `instance` points at the `CallbackCtx` kept alive by `Connection`.
    let ctx = unsafe { &*(instance as *const CallbackCtx) };
    let status = (param1 & 0xFF) as u8;
    let data1 = ((param1 >> 8) & 0xFF) as u8;
    let data2 = ((param1 >> 16) & 0xFF) as u8;
    if let Some(event) = parse_message(status, data1, data2) {
        (ctx.deliver)(event);
    }
}

pub(crate) struct Connection {
    handle: HMIDIIN,
    ctx: *mut CallbackCtx,
}

impl Drop for Connection {
    fn drop(&mut self) {
        unsafe {
            let _ = midiInStop(self.handle);
            let _ = midiInReset(self.handle);
            let _ = midiInClose(self.handle);
        }
        if !self.ctx.is_null() {
            // SAFETY: the pointer came from Box::into_raw in `open`, and the
            // device is closed above, so no callback can still be using it.
            unsafe {
                drop(Box::from_raw(self.ctx));
            }
        }
    }
}

fn device_name(index: u32) -> Option<String> {
    let mut caps = MIDIINCAPSW::default();
    let rc =
        unsafe { midiInGetDevCapsW(index as usize, &mut caps, size_of::<MIDIINCAPSW>() as u32) };
    if rc != 0 {
        return None;
    }
    let pname = unsafe { std::ptr::addr_of!(caps.szPname).read_unaligned() };
    Some(wide_to_string(&pname))
}

pub(crate) fn list_ports() -> Result<Vec<String>> {
    let count = unsafe { midiInGetNumDevs() };
    Ok((0..count).filter_map(device_name).collect())
}

pub(crate) fn open(index: usize, deliver: Deliver) -> Result<Connection> {
    let index = index as u32;
    if device_name(index).is_none() {
        anyhow::bail!("midiInGetDevCapsW failed for device {index}");
    }
    let ctx = Box::into_raw(Box::new(CallbackCtx { deliver }));
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
    Ok(Connection { handle, ctx })
}

fn wide_to_string(wide: &[u16]) -> String {
    let end = wide.iter().position(|c| *c == 0).unwrap_or(wide.len());
    String::from_utf16_lossy(&wide[..end])
}
