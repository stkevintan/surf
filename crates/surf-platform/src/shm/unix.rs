//! POSIX shared memory: `shm_open` + `ftruncate` + `mmap`.

use std::ffi::CString;
use std::ptr;

use anyhow::{Context, Result};

pub struct SharedRegion {
    ptr: *mut u8,
    size: usize,
    /// Set for the creator, which removes the name when it is dropped.
    owned_name: Option<CString>,
}

// The mapping is shared across processes; access is synchronised by the
// caller (atomics in a header).
unsafe impl Send for SharedRegion {}
unsafe impl Sync for SharedRegion {}

impl SharedRegion {
    /// Create a new zero-filled region. Fails if the name already exists.
    pub fn create(name: &str, size: usize) -> Result<Self> {
        let cname = CString::new(name).context("shared memory name contains a NUL byte")?;
        // SAFETY: valid C string; flags and mode are plain integers.
        let fd = unsafe {
            libc::shm_open(
                cname.as_ptr(),
                libc::O_CREAT | libc::O_EXCL | libc::O_RDWR,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("shm_open (create) failed");
        }
        // SAFETY: `fd` is a fresh shm descriptor owned here.
        if unsafe { libc::ftruncate(fd, size as libc::off_t) } != 0 {
            let err = std::io::Error::last_os_error();
            unsafe {
                libc::close(fd);
                libc::shm_unlink(cname.as_ptr());
            }
            return Err(err).context("ftruncate on shared memory failed");
        }
        let ptr = map(fd, size);
        // The mapping keeps the object alive; the descriptor is no longer needed.
        unsafe { libc::close(fd) };
        let ptr = match ptr {
            Ok(ptr) => ptr,
            Err(err) => {
                unsafe { libc::shm_unlink(cname.as_ptr()) };
                return Err(err);
            }
        };
        // ftruncate already zero-fills, but be explicit: the header relies on it.
        unsafe { ptr::write_bytes(ptr, 0, size) };
        Ok(Self {
            ptr,
            size,
            owned_name: Some(cname),
        })
    }

    /// Open a region another process created.
    pub fn open(name: &str, size: usize) -> Result<Self> {
        let cname = CString::new(name).context("shared memory name contains a NUL byte")?;
        // SAFETY: valid C string.
        let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0 as libc::c_uint) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("shm_open (open) failed");
        }
        let ptr = map(fd, size);
        unsafe { libc::close(fd) };
        Ok(Self {
            ptr: ptr?,
            size,
            owned_name: None,
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

fn map(fd: libc::c_int, size: usize) -> Result<*mut u8> {
    // SAFETY: `fd` is a valid shm descriptor at least `size` bytes long.
    let ptr = unsafe {
        libc::mmap(
            ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return Err(std::io::Error::last_os_error()).context("mmap of shared memory failed");
    }
    Ok(ptr as *mut u8)
}

impl Drop for SharedRegion {
    fn drop(&mut self) {
        // SAFETY: `ptr`/`size` came from a successful mmap.
        unsafe {
            libc::munmap(self.ptr as *mut libc::c_void, self.size);
        }
        if let Some(name) = &self.owned_name {
            // Unlinking only removes the name; a process that already opened it
            // keeps its mapping.
            unsafe {
                libc::shm_unlink(name.as_ptr());
            }
        }
    }
}
