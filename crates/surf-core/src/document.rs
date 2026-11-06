//! The `.surf` project file: the [`Project`] plus each plugin's state.
//!
//! JSON, so it diffs and survives hand edits. Plugin states are opaque blobs
//! (Kontakt's runs to megabytes) and go in as base64, one per plugin, since
//! tracks that share a plugin share its state. Plugin paths are stored as
//! they were; a project opened on another machine plays the piano on tracks
//! whose plugin isn't installed there.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde::{Deserialize, Serialize};

use crate::project::Project;

/// Written as `format` so a newer file is refused instead of misread.
pub const FORMAT: &str = "surf-project";
pub const VERSION: u32 = 1;
pub const EXTENSION: &str = "surf";

#[derive(Debug, PartialEq)]
pub struct Document {
    pub project: Project,
    /// Plugin path → its state blob.
    pub plugin_states: BTreeMap<PathBuf, Vec<u8>>,
}

#[derive(Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    project: Project,
    #[serde(default)]
    plugins: Vec<PluginState>,
}

#[derive(Serialize, Deserialize)]
struct PluginState {
    path: PathBuf,
    /// Base64 of the plugin's state blob.
    state: String,
}

impl Document {
    pub fn to_json(&self) -> Result<String> {
        let file = File {
            format: FORMAT.to_string(),
            version: VERSION,
            project: self.project.clone(),
            plugins: self
                .plugin_states
                .iter()
                .map(|(path, state)| PluginState {
                    path: path.clone(),
                    state: B64.encode(state),
                })
                .collect(),
        };
        Ok(serde_json::to_string_pretty(&file)?)
    }

    pub fn from_json(text: &str) -> Result<Self> {
        let file: File = serde_json::from_str(text).context("not a Surf project")?;
        if file.format != FORMAT {
            bail!("not a Surf project (format {:?})", file.format);
        }
        if file.version > VERSION {
            bail!(
                "this project was saved by a newer Surf (version {}); update Surf to open it",
                file.version
            );
        }
        let mut plugin_states = BTreeMap::new();
        for p in file.plugins {
            let state = B64
                .decode(p.state.as_bytes())
                .with_context(|| format!("bad state for {}", p.path.display()))?;
            plugin_states.insert(p.path, state);
        }
        let mut project = file.project;
        project.repair();
        Ok(Self {
            project,
            plugin_states,
        })
    }

    /// Write atomically: to a temp file next to `path`, then rename, so a
    /// crash mid-save never leaves a half-written project.
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = self.to_json()?;
        let tmp = path.with_extension(format!("{EXTENSION}.tmp"));
        std::fs::write(&tmp, text).with_context(|| format!("could not write {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("could not replace {}", path.display()))?;
        Ok(())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        Self::from_json(&text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::song::Song;

    fn doc() -> Document {
        let mut project = Project::from_song(&Song::demo());
        project.tracks[0].instrument = Some("/Library/Audio/Plug-Ins/VST3/Kontakt 8.vst3".into());
        project.tracks[0].volume = 0.7;
        project.tracks[0].mute = true;
        let mut plugin_states = BTreeMap::new();
        plugin_states.insert(
            PathBuf::from("/Library/Audio/Plug-Ins/VST3/Kontakt 8.vst3"),
            vec![0, 1, 2, 255, 128],
        );
        Document {
            project,
            plugin_states,
        }
    }

    #[test]
    fn round_trips_the_project_and_plugin_states() {
        let d = doc();
        let back = Document::from_json(&d.to_json().unwrap()).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn ids_keep_counting_after_a_load() {
        let mut back = Document::from_json(&doc().to_json().unwrap()).unwrap();
        let fresh = back.project.alloc();
        let used = back
            .project
            .tracks
            .iter()
            .flat_map(|t| {
                std::iter::once(t.id).chain(
                    t.regions
                        .iter()
                        .flat_map(|r| std::iter::once(r.id).chain(r.notes.iter().map(|n| n.id))),
                )
            })
            .max()
            .unwrap();
        assert!(fresh > used);
    }

    #[test]
    fn refuses_other_files_and_newer_versions() {
        assert!(Document::from_json("{\"hello\": 1}").is_err());
        let newer = doc()
            .to_json()
            .unwrap()
            .replace("\"version\": 1", "\"version\": 99");
        let err = Document::from_json(&newer).unwrap_err().to_string();
        assert!(err.contains("newer Surf"), "{err}");
    }

    #[test]
    fn saves_and_loads_a_file() {
        let path = std::env::temp_dir().join(format!("surf-doc-{}.surf", std::process::id()));
        let d = doc();
        d.save(&path).unwrap();
        assert_eq!(Document::load(&path).unwrap(), d);
        std::fs::remove_file(path).ok();
    }
}
