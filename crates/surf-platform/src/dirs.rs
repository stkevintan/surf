//! Writable per-user directories.
//!
//! The OS convention comes first: `%APPDATA%\Surf` on Windows,
//! `~/Library/Application Support/Surf` on macOS, `$XDG_DATA_HOME/surf` (or
//! `~/.local/share/surf`) on Linux. A sandboxed shell may deny writes there, so
//! writability is probed once, with fallbacks to a workspace-local `.surf/`,
//! the executable directory, and finally the temp directory.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Directory for state files and helper logs.
pub fn data_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Some(dir) = std::env::var_os("SURF_STATE_DIR") {
            let dir = PathBuf::from(dir);
            if writable_dir(&dir) {
                return dir;
            }
        }
        if let Some(dir) = platform_data_dir()
            && writable_dir(&dir)
        {
            return dir;
        }
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        if cwd.join("Cargo.toml").exists() && writable_dir(&cwd.join(".surf")) {
            return cwd.join(".surf");
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                let dir = parent.join("surf-state");
                if writable_dir(&dir) {
                    return dir;
                }
            }
        }
        let dir = std::env::temp_dir().join("surf-state");
        let _ = std::fs::create_dir_all(&dir);
        dir
    })
    .clone()
}

fn platform_data_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("Surf"))
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|home| PathBuf::from(home).join("Library/Application Support/Surf"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
            .map(|dir| dir.join("surf"))
    }
}

fn writable_dir(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".write-probe-{}", std::process::id()));
    match std::fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = std::fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}
