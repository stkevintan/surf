//! Plugin discovery for the `SCAN` / LOAD VST picker.
//!
//! This only walks the filesystem — no plugin is loaded, so a scan is cheap
//! enough to run synchronously on the UI thread. Loading (and the crash risk
//! that comes with it) still happens in `surf-vst-host`.
//!
//! Standard VST3 and VST2 folders are scanned recursively. VST2 plugins (see
//! [`crate::format`] for what that means on each OS) are only collected from
//! the VST2 folders. Extra folders can be added with
//! `SURF_VST_SCAN` (`;`- or `,`-separated), which is how you reach a plugin
//! installed somewhere unusual (e.g. a portable Kontakt unpacked in your
//! Music folder).

use std::path::{Path, PathBuf};

use crate::format::{self, Format};

/// Extra scan roots, separated by `;` or `,`.
pub const EXTRA_ROOTS_ENV: &str = "SURF_VST_SCAN";

const MAX_DEPTH: usize = 6;
const MAX_RESULTS: usize = 400;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginEntry {
    pub path: PathBuf,
    /// Display name, i.e. the file stem: `Kontakt 8`, `OmnivocalBeta`.
    pub name: String,
    /// `true` for a VST2 plugin, `false` for VST3.
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
    for Root { path, accept_vst2 } in scan_roots() {
        if path.is_dir() {
            walk(&path, 0, accept_vst2, &mut found);
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
    /// VST2 plugins are only collected from dedicated VST2 folders. On Windows a
    /// stray `.dll` in a VST3 folder is a support library, not a plugin.
    accept_vst2: bool,
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
                accept_vst2: false,
            });
            roots.push(Root {
                path: base.join("VSTPlugins"),
                accept_vst2: true,
            });
            roots.push(Root {
                path: base.join("Steinberg").join("VSTPlugins"),
                accept_vst2: true,
            });
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            roots.push(Root {
                path: PathBuf::from(local)
                    .join("Programs")
                    .join("Common")
                    .join("VST3"),
                accept_vst2: false,
            });
        }
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").ok().map(PathBuf::from);
        for (dir, accept_vst2) in [("VST3", false), ("VST", true)] {
            roots.push(Root {
                path: PathBuf::from("/Library/Audio/Plug-Ins").join(dir),
                accept_vst2,
            });
            if let Some(home) = &home {
                roots.push(Root {
                    path: home.join("Library/Audio/Plug-Ins").join(dir),
                    accept_vst2,
                });
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        let mut push = |path: PathBuf, accept_vst2| roots.push(Root { path, accept_vst2 });
        for path in ["/usr/lib/vst3", "/usr/local/lib/vst3"] {
            push(PathBuf::from(path), false);
        }
        for path in ["/usr/lib/vst", "/usr/local/lib/vst"] {
            push(PathBuf::from(path), true);
        }
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            push(home.join(".vst3"), false);
            push(home.join(".vst"), true);
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
                // A user-supplied folder may hold VST2 plugins as well as VST3.
                accept_vst2: true,
                path,
            });
        }
    }

    roots
}

fn walk(dir: &Path, depth: usize, accept_vst2: bool, out: &mut Vec<PluginEntry>) {
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
        match format::detect(&path) {
            // A `.vst3` is either a bundle directory or a single-file module;
            // both are plugins, and neither needs recursing into.
            Some(Format::Vst3) => {
                if let Some(plugin) = PluginEntry::new(path, false) {
                    out.push(plugin);
                }
            }
            // A VST2 plugin is a file on Windows and Linux and a bundle
            // directory on macOS, so accept either shape.
            Some(Format::Vst2) if accept_vst2 => {
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
        walk(&subdir, depth + 1, accept_vst2, out);
        if out.len() >= MAX_RESULTS {
            return;
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    // Build paths with `join` rather than Windows literals: on macOS and Linux a
    // backslash is an ordinary file-name character, not a separator.
    fn plugin(dir: &str, file: &str) -> PathBuf {
        Path::new("plugins").join(dir).join(file)
    }

    #[test]
    fn entry_name_is_the_file_stem() {
        let entry = PluginEntry::new(plugin("VST3", "Kontakt 8.vst3"), false).unwrap();
        assert_eq!(entry.name, "Kontakt 8");
        assert!(!entry.vst2);
    }

    #[test]
    fn bundle_directory_keeps_its_stem() {
        // A bundle on disk is a directory, but the name comes from the same place.
        let entry = PluginEntry::new(plugin("VST3", "OmnivocalBeta.vst3"), false).unwrap();
        assert_eq!(entry.name, "OmnivocalBeta");
    }

    #[test]
    fn vst2_entries_are_flagged() {
        let file = format!("TruePianos.{}", format::VST2_EXTENSION);
        let entry = PluginEntry::new(plugin("VST", &file), true).unwrap();
        assert!(entry.vst2);
        assert_eq!(entry.name, "TruePianos");
    }

    #[test]
    fn names_without_a_stem_are_skipped() {
        // `Path::file_stem` of a dot-file is the whole name, which is not a
        // plugin name; neither is an empty one.
        assert!(PluginEntry::new(plugin("VST3", ".vst3"), false).is_none());
        assert!(PluginEntry::new(plugin("VST3", ".hidden.vst3"), false).is_none());
    }

    #[test]
    fn walk_finds_both_formats_in_a_folder() {
        let root = std::env::temp_dir().join(format!("surf-scan-{}", std::process::id()));
        let vst2 = root.join(format!("Old Synth.{}", format::VST2_EXTENSION));
        std::fs::create_dir_all(root.join("Vendor").join("New Synth.vst3")).unwrap();
        std::fs::write(&vst2, b"").unwrap();
        std::fs::write(root.join("notes.txt"), b"").unwrap();
        let found = scan_folder(&root);
        let names: Vec<_> = found.iter().map(|p| (p.name.as_str(), p.vst2)).collect();
        assert_eq!(names, vec![("New Synth", false), ("Old Synth", true)]);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
