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
    // Two names made in one process within the clock's resolution (a
    // microsecond on macOS) would otherwise be the same.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    #[cfg(windows)]
    {
        format!("Local\\SurfVst-{pid}-{n}-{nanos}")
    }
    #[cfg(unix)]
    {
        // pid, counter and low 24 bits of the clock, hex: at most
        // 5 + 8 + 1 + 8 + 1 + 6 = 29 bytes.
        format!("/surf{pid:x}-{n:x}-{:x}", nanos as u32 & 0xff_ffff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Names made back to back in one process differ (CI made two tests'
    /// names equal on macOS, whose clock ticks in microseconds).
    #[test]
    fn names_made_at_once_differ() {
        let names: std::collections::HashSet<String> = (0..1000).map(|_| make_name()).collect();
        assert_eq!(names.len(), 1000);
    }

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

    /// Review M12: a name that exists is not created again, and a region is not
    /// mapped larger than it is (on Unix that would SIGBUS on first touch).
    #[test]
    fn create_is_exclusive_and_open_checks_the_size() {
        let name = make_name();
        let _a = SharedRegion::create(&name, 4096).unwrap();
        assert!(SharedRegion::create(&name, 8192).is_err(), "name taken");
        assert!(SharedRegion::open(&name, 1 << 20).is_err(), "too small");
        assert!(SharedRegion::open(&name, 4096).is_ok());
    }

    /// The 31-byte limit is macOS's POSIX shm rule; Windows section names
    /// (`Local\...`) allow up to MAX_PATH and are deliberately longer.
    #[cfg(unix)]
    #[test]
    fn names_fit_the_posix_limit() {
        assert!(make_name().len() <= 31);
        let worst = format!("/surf{:x}-{:x}-{:x}", u32::MAX, u32::MAX, 0xff_ffff);
        assert!(worst.len() <= 31, "{worst}");
    }
}
