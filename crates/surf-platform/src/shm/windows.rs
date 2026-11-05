//! Windows shared memory: a pagefile-backed file mapping.

use std::ffi::c_void;
use std::ptr;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, FILE_MAP_ALL_ACCESS, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
    OpenFileMappingW, PAGE_READWRITE, UnmapViewOfFile,
};
use windows::core::HSTRING;

pub struct SharedRegion {
    ptr: *mut u8,
    size: usize,
    handle: HANDLE,
}

// The mapping is shared across processes; access is synchronised by the
// caller (atomics in a header).
unsafe impl Send for SharedRegion {}
unsafe impl Sync for SharedRegion {}

impl SharedRegion {
    /// Create a new zero-filled region.
    pub fn create(name: &str, size: usize) -> Result<Self> {
        let name = HSTRING::from(name);
        let handle = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                size as u32,
                &name,
            )
        }
        .context("CreateFileMappingW failed")?;
        let region = Self::map(handle, size)?;
        unsafe { ptr::write_bytes(region.ptr, 0, size) };
        Ok(region)
    }

    /// Open a region another process created.
    pub fn open(name: &str, size: usize) -> Result<Self> {
        let name = HSTRING::from(name);
        let handle = unsafe { OpenFileMappingW(FILE_MAP_ALL_ACCESS.0, false, &name) }
            .context("OpenFileMappingW failed")?;
        Self::map(handle, size)
    }

    fn map(handle: HANDLE, size: usize) -> Result<Self> {
        let view = unsafe { MapViewOfFile(handle, FILE_MAP_ALL_ACCESS, 0, 0, size) };
        if view.Value.is_null() {
            unsafe {
                let _ = CloseHandle(handle);
            }
            anyhow::bail!("MapViewOfFile failed");
        }
        Ok(Self {
            ptr: view.Value as *mut u8,
            size,
            handle,
        })
    }

    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }

    pub fn len(&self) -> usize {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.size == 0
    }
}

impl Drop for SharedRegion {
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
