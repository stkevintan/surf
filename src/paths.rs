//! Writable per-user directories.
//!
//! `%APPDATA%\Surf` is the right place for a normal desktop launch, but the DSH
//! shell runs with workspace-write sandboxing, which denies writes there.  Probe
//! writability once and fall back to a workspace-local `.surf/` directory (or
//! the executable directory for an installed build).

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
        if let Some(appdata) = std::env::var_os("APPDATA") {
            let dir = PathBuf::from(appdata).join("Surf");
            if writable_dir(&dir) {
                return dir;
            }
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
