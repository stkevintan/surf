//! Keep the JSON protocol pipe private to the host.
//!
//! The host speaks to the app over its stdout. Plugins print to stdout too:
//! Kontakt writes its own debug lines there. The client skips lines that are
//! not JSON, but a plugin line without a trailing newline would join the next
//! protocol message and lose it.
//!
//! [`take`] runs before any plugin is loaded. It duplicates the stdout pipe
//! into a handle only this module writes to, then points the process's
//! standard output at stderr (the host log). Whatever a plugin prints goes to
//! the log, and only `send` writes to the protocol pipe.

use std::fs::File;
use std::io::{BufWriter, Write};

pub type Out = BufWriter<File>;

/// Take the protocol pipe. On failure, falls back to sharing stdout as before.
pub fn take() -> Out {
    match imp::split() {
        Ok(file) => BufWriter::new(file),
        Err(err) => {
            eprintln!("helper: could not separate the protocol pipe ({err}); sharing stdout");
            BufWriter::new(imp::stdout_file())
        }
    }
}

/// Write one protocol line and flush it.
pub fn send(out: &mut Out, line: &[u8]) -> std::io::Result<()> {
    out.write_all(line)?;
    out.write_all(b"\n")?;
    out.flush()
}

#[cfg(unix)]
mod imp {
    use std::fs::File;
    use std::os::fd::{FromRawFd, RawFd};

    pub fn split() -> std::io::Result<File> {
        // SAFETY: plain descriptor calls on the process's own standard streams.
        unsafe {
            let protocol: RawFd = libc::dup(libc::STDOUT_FILENO);
            if protocol < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // The plugin must not inherit the protocol pipe into anything it spawns.
            libc::fcntl(protocol, libc::F_SETFD, libc::FD_CLOEXEC);
            if libc::dup2(libc::STDERR_FILENO, libc::STDOUT_FILENO) < 0 {
                let err = std::io::Error::last_os_error();
                libc::close(protocol);
                return Err(err);
            }
            Ok(File::from_raw_fd(protocol))
        }
    }

    pub fn stdout_file() -> File {
        // SAFETY: duplicates our own stdout descriptor.
        unsafe { File::from_raw_fd(libc::dup(libc::STDOUT_FILENO)) }
    }
}

#[cfg(windows)]
mod imp {
    use std::fs::File;
    use std::os::windows::io::{FromRawHandle, RawHandle};

    use windows::Win32::Foundation::{DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE};
    use windows::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    fn duplicate(handle: HANDLE) -> std::io::Result<HANDLE> {
        let mut copy = HANDLE::default();
        // SAFETY: duplicating one of our own handles into our own process.
        unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                handle,
                GetCurrentProcess(),
                &mut copy,
                0,
                false,
                DUPLICATE_SAME_ACCESS,
            )
        }
        .map_err(std::io::Error::other)?;
        Ok(copy)
    }

    pub fn split() -> std::io::Result<File> {
        // SAFETY: reading and replacing this process's standard handles.
        unsafe {
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE).map_err(std::io::Error::other)?;
            let protocol = duplicate(stdout)?;
            let stderr = GetStdHandle(STD_ERROR_HANDLE).map_err(std::io::Error::other)?;
            SetStdHandle(STD_OUTPUT_HANDLE, stderr).map_err(std::io::Error::other)?;
            // The C runtime caches its own stdout; plugins that use printf write
            // through that. Point it at stderr too.
            if libc_dup2_stderr_onto_stdout() != 0 {
                eprintln!("helper: could not redirect the C runtime's stdout");
            }
            Ok(File::from_raw_handle(protocol.0 as RawHandle))
        }
    }

    pub fn stdout_file() -> File {
        // SAFETY: duplicates our own stdout handle.
        unsafe {
            let stdout = GetStdHandle(STD_OUTPUT_HANDLE).expect("stdout handle");
            File::from_raw_handle(duplicate(stdout).expect("duplicate stdout").0 as RawHandle)
        }
    }

    unsafe extern "C" {
        fn _dup2(src: i32, dst: i32) -> i32;
    }

    fn libc_dup2_stderr_onto_stdout() -> i32 {
        // SAFETY: CRT descriptors 1 and 2 always exist in a console-subsystem process.
        unsafe { _dup2(2, 1) }
    }
}
