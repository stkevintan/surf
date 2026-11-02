//! Plugin discovery for the `SCAN` / LOAD VST picker.
//!
//! This only walks the filesystem — no plugin is loaded, so a scan is cheap
//! enough to run synchronously on the UI thread. Loading (and the crash risk
//! that comes with it) still happens in `surf-vst-host.exe`.
//!
//! Standard VST3 folders are scanned recursively; the classic VST2 folders are
//! scanned for `.dll` at the top level only. Extra folders can be added with
//! `SURF_VST_SCAN` (`;`- or `,`-separated), which is how you reach a plugin
//! installed somewhere unusual (e.g. a portable Kontakt unpacked in your
//! Music folder).

use std::path::{Path, PathBuf};

/// Extra scan roots, separated by `;` or `,`.
pub const EXTRA_ROOTS_ENV: &str = "SURF_VST_SCAN";

const MAX_DEPTH: usize = 6;
const MAX_RESULTS: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginEntry {
    pub path: PathBuf,
    /// Display name, i.e. the file stem: `Kontakt 8`, `OmnivocalBeta`.
    pub name: String,
    /// `true` for a VST2 `.dll`, `false` for VST3.
    pub vst2: bool,
}

impl PluginEntry {
    fn new(path: PathBuf, vst2: bool) -> Option<Self> {
        let name = path.file_stem()?.to_string_lossy().trim().to_string();
        // `.vst3` has no stem at all, and a leading dot is a leftover/hidden
        // file rather than a plugin name.
        if name.is_empty() || name.starts_with('.') {
            return None;
        }
        Some(Self { path, name, vst2 })
    }
}

/// Every plugin found in the standard folders plus `SURF_VST_SCAN`.
///
/// Sorted by display name, de-duplicated by path, capped at [`MAX_RESULTS`].
pub fn scan_plugin_paths() -> Vec<PluginEntry> {
    let mut found = Vec::new();
    for Root { path, accept_dll } in scan_roots() {
        if path.is_dir() {
            walk(&path, 0, accept_dll, &mut found);
        }
    }
    finish(found)
}

/// Every plugin inside one folder (recursive), for the folder picker.
///
/// `.dll` is accepted here as well: the user pointed at a specific folder, so
/// a legacy VST2 plugin next to a VST3 one is not noise.
pub fn scan_folder(dir: &Path) -> Vec<PluginEntry> {
    let mut found = Vec::new();
    if dir.is_dir() {
        walk(dir, 0, true, &mut found);
    }
    finish(found)
}

/// Sort by display name, de-duplicate by path, cap the result count.
fn finish(mut found: Vec<PluginEntry>) -> Vec<PluginEntry> {
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found.dedup_by(|a, b| a.path == b.path);
    found.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.path.cmp(&b.path))
    });
    found.truncate(MAX_RESULTS);
    found
}

struct Root {
    path: PathBuf,
    /// VST2 `.dll` files are only collected from dedicated VST2 folders.
    accept_dll: bool,
}

fn scan_roots() -> Vec<Root> {
    let mut roots = Vec::new();

    #[cfg(target_os = "windows")]
    {
        for env in ["ProgramFiles", "ProgramFiles(x86)"] {
            let Ok(base) = std::env::var(env) else {
                continue;
            };
            let base = PathBuf::from(base);
            roots.push(Root {
                path: base.join("Common Files").join("VST3"),
                accept_dll: false,
            });
            roots.push(Root {
                path: base.join("VSTPlugins"),
                accept_dll: true,
            });
            roots.push(Root {
                path: base.join("Steinberg").join("VSTPlugins"),
                accept_dll: true,
            });
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            roots.push(Root {
                path: PathBuf::from(local).join("Programs").join("Common").join("VST3"),
                accept_dll: false,
            });
        }
    }

    #[cfg(target_os = "macos")]
    {
        roots.push(Root {
            path: PathBuf::from("/Library/Audio/Plug-Ins/VST3"),
            accept_dll: false,
        });
        if let Ok(home) = std::env::var("HOME") {
            roots.push(Root {
                path: PathBuf::from(home).join("Library/Audio/Plug-Ins/VST3"),
                accept_dll: false,
            });
        }
    }

    #[cfg(target_os = "linux")]
    {
        for path in ["/usr/lib/vst3", "/usr/local/lib/vst3"] {
            roots.push(Root {
                path: PathBuf::from(path),
                accept_dll: false,
            });
        }
        if let Ok(home) = std::env::var("HOME") {
            roots.push(Root {
                path: PathBuf::from(home).join(".vst3"),
                accept_dll: false,
            });
        }
    }

    if let Ok(extra) = std::env::var(EXTRA_ROOTS_ENV) {
        for part in extra.split([';', ',']) {
            let part = part.trim();
            if part.is_empty() {
                continue;
            }
            let path = PathBuf::from(part);
            roots.push(Root {
                // A user-supplied folder may hold VST2 `.dll` as well as VST3.
                accept_dll: true,
                path,
            });
        }
    }

    roots
}

fn walk(dir: &Path, depth: usize, accept_dll: bool, out: &mut Vec<PluginEntry>) {
    if depth > MAX_DEPTH || out.len() >= MAX_RESULTS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        // Skip junctions/symlinks: one dangling entry (e.g. an old portable
        // install) must not abort the whole scan, and links can loop.
        let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }

        let path = entry.path();
        let is_dir = meta.is_dir();
        match extension(&path).as_deref() {
            // A `.vst3` is either a bundle directory or a single-file module;
            // both are plugins, and neither needs recursing into.
            Some("vst3") => {
                if let Some(plugin) = PluginEntry::new(path, false) {
                    out.push(plugin);
                }
            }
            Some("dll") if accept_dll && !is_dir => {
                if let Some(plugin) = PluginEntry::new(path, true) {
                    out.push(plugin);
                }
            }
            _ if is_dir => subdirs.push(path),
            _ => {}
        }
        if out.len() >= MAX_RESULTS {
            return;
        }
    }

    // Breadth-first would be nicer for very deep trees, but plugin folders are
    // shallow; depth-first keeps the code simple.
    for subdir in subdirs {
        walk(&subdir, depth + 1, accept_dll, out);
        if out.len() >= MAX_RESULTS {
            return;
        }
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_name_is_the_file_stem() {
        let entry = PluginEntry::new(PathBuf::from(r"C:\VST3\Kontakt 8.vst3"), false).unwrap();
        assert_eq!(entry.name, "Kontakt 8");
        assert!(!entry.vst2);
    }

    #[test]
    fn bundle_directory_keeps_its_stem() {
        // A bundle on disk is a directory, but the name comes from the same place.
        let entry = PluginEntry::new(PathBuf::from(r"C:\VST3\OmnivocalBeta.vst3"), false).unwrap();
        assert_eq!(entry.name, "OmnivocalBeta");
    }

    #[test]
    fn vst2_entries_are_flagged() {
        let entry = PluginEntry::new(PathBuf::from(r"C:\VST\TruePianos.dll"), true).unwrap();
        assert!(entry.vst2);
        assert_eq!(entry.name, "TruePianos");
    }

    #[test]
    fn names_without_a_stem_are_skipped() {
        // `Path::file_stem` of a dot-file is the whole name, which is not a
        // plugin name; neither is an empty one.
        assert!(PluginEntry::new(PathBuf::from(r"C:\VST3\.vst3"), false).is_none());
        assert!(PluginEntry::new(PathBuf::from(r"C:\VST3\.hidden.vst3"), false).is_none());
    }

    #[test]
    fn extensions_are_matched_case_insensitively() {
        assert_eq!(extension(Path::new(r"C:\VST3\Foo.VST3")).as_deref(), Some("vst3"));
        assert_eq!(extension(Path::new(r"C:\VST\Bar.DLL")).as_deref(), Some("dll"));
        assert_eq!(extension(Path::new(r"C:\VST3\VST3")), None);
    }
}
