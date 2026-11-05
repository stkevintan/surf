//! Named shared memory between two processes.
//!
//! The creator picks a name with [`make_name`], creates the region, and passes
//! the name to the other process, which opens it. Both see the same zeroed
//! bytes; synchronisation is the caller's job (Surf uses atomics in a header).

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

#[cfg(unix)]
pub use unix::SharedRegion;
#[cfg(windows)]
pub use windows::SharedRegion;

/// A name unique to this process and moment, valid on the current OS.
///
/// macOS limits POSIX shm names to 31 bytes including the leading `/`, so the
/// Unix form is kept short.
pub fn make_name() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    #[cfg(windows)]
    {
        format!("Local\\SurfVst-{pid}-{nanos}")
    }
    #[cfg(unix)]
    {
        // pid + low 32 bits of the clock, hex: at most 1 + 4 + 8 + 1 + 8 = 22 bytes.
        format!("/surf{pid:x}-{:x}", nanos as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_then_open_shares_bytes() {
        let name = make_name();
        let a = SharedRegion::create(&name, 4096).unwrap();
        let b = SharedRegion::open(&name, 4096).unwrap();
        unsafe {
            assert_eq!(*a.as_ptr().add(100), 0, "new regions start zeroed");
            *a.as_ptr().add(100) = 42;
            assert_eq!(*b.as_ptr().add(100), 42);
        }
    }

    /// The 31-byte limit is macOS's POSIX shm rule; Windows section names
    /// (`Local\...`) allow up to MAX_PATH and are deliberately longer.
    #[cfg(unix)]
    #[test]
    fn names_fit_the_posix_limit() {
        assert!(make_name().len() <= 31);
    }
}
