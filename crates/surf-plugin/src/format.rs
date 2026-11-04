//! Which plugin format a path is, and where its loadable binary lives.
//!
//! Scan, the host and the UI all ask this module, so the rules live in one
//! place:
//!
//! | Format | Windows | macOS | Linux |
//! |---|---|---|---|
//! | VST3 | `.vst3` file or bundle | `.vst3` bundle | `.vst3` bundle |
//! | VST2 | `.dll` | `.vst` bundle | `.so` |

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Vst3,
    Vst2,
}

/// The file extension a VST2 plugin has on this OS (lowercase, no dot).
pub const VST2_EXTENSION: &str = if cfg!(windows) {
    "dll"
} else if cfg!(target_os = "macos") {
    "vst"
} else {
    "so"
};

impl Format {
    /// Whether the host can open this format's editor window on this OS.
    ///
    /// VST2 editors need a native parent window, which only exists on Windows
    /// so far. VST3 editors go through `vst3-host`.
    pub fn has_editor(self) -> bool {
        match self {
            Format::Vst3 => true,
            Format::Vst2 => cfg!(windows),
        }
    }
}

/// Lowercase extension, if any.
pub fn extension(path: &Path) -> Option<String> {
    path.extension()
        .map(|ext| ext.to_string_lossy().to_lowercase())
}

/// The format a path names, judged by its extension. `None` if it is neither.
pub fn detect(path: &Path) -> Option<Format> {
    match extension(path)?.as_str() {
        "vst3" => Some(Format::Vst3),
        ext if ext == VST2_EXTENSION => Some(Format::Vst2),
        _ => None,
    }
}

/// The file `dlopen`/`LoadLibrary` should open for a VST2 plugin.
///
/// On macOS a `.vst` is a bundle directory and the real Mach-O binary is
/// `Contents/MacOS/<CFBundleExecutable>`. The executable name usually matches
/// the bundle name, but not always, so `Info.plist` is checked first. On
/// Windows and Linux the plugin path is already the binary.
pub fn vst2_binary(path: &Path) -> PathBuf {
    if !path.is_dir() {
        return path.to_path_buf();
    }
    let macos = path.join("Contents").join("MacOS");
    if let Some(name) = bundle_executable(&path.join("Contents").join("Info.plist")) {
        let candidate = macos.join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    if let Some(stem) = path.file_stem() {
        let candidate = macos.join(stem);
        if candidate.is_file() {
            return candidate;
        }
    }
    // Last resort: the only file in Contents/MacOS.
    if let Ok(entries) = std::fs::read_dir(&macos) {
        let files: Vec<_> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file())
            .collect();
        if let [only] = files.as_slice() {
            return only.clone();
        }
    }
    path.to_path_buf()
}

/// `CFBundleExecutable` from an XML `Info.plist`. Binary plists are not
/// parsed; the caller falls back to the bundle name for those.
fn bundle_executable(plist: &Path) -> Option<String> {
    let text = std::fs::read_to_string(plist).ok()?;
    let key = text.find("<key>CFBundleExecutable</key>")?;
    let rest = &text[key..];
    let start = rest.find("<string>")? + "<string>".len();
    let end = rest[start..].find("</string>")?;
    let name = rest[start..start + end].trim();
    (!name.is_empty()).then(|| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_extension_case_insensitively() {
        assert_eq!(detect(Path::new("Kontakt 8.vst3")), Some(Format::Vst3));
        assert_eq!(detect(Path::new("Kontakt 8.VST3")), Some(Format::Vst3));
        let vst2 = format!("TruePianos.{}", VST2_EXTENSION.to_uppercase());
        assert_eq!(detect(Path::new(&vst2)), Some(Format::Vst2));
        assert_eq!(detect(Path::new("readme.txt")), None);
        assert_eq!(detect(Path::new("VST3")), None);
    }

    #[test]
    fn only_this_os_vst2_extension_counts() {
        for ext in ["dll", "vst", "so"] {
            let expected = (ext == VST2_EXTENSION).then_some(Format::Vst2);
            assert_eq!(detect(Path::new(&format!("x.{ext}"))), expected, "{ext}");
        }
    }

    #[test]
    fn a_plain_file_is_its_own_binary() {
        let file = Path::new("/no/such/dir/plugin.so");
        assert_eq!(vst2_binary(file), file);
    }

    #[test]
    fn bundle_binary_comes_from_info_plist() {
        let root = std::env::temp_dir().join(format!("surf-format-{}", std::process::id()));
        let bundle = root.join("Bundle Name.vst");
        let macos = bundle.join("Contents").join("MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        std::fs::write(macos.join("RealBinary"), b"").unwrap();
        std::fs::write(
            bundle.join("Contents").join("Info.plist"),
            "<plist><dict>\n  <key>CFBundleExecutable</key>\n  <string>RealBinary</string>\n</dict></plist>",
        )
        .unwrap();
        assert_eq!(vst2_binary(&bundle), macos.join("RealBinary"));

        // Without a usable plist, fall back to the only file present.
        std::fs::remove_file(bundle.join("Contents").join("Info.plist")).unwrap();
        assert_eq!(vst2_binary(&bundle), macos.join("RealBinary"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
