//! File pickers, through GPUI's own asynchronous prompts.
//!
//! A blocking picker (such as `rfd`) runs a nested native event loop. Opened
//! from a GPUI event handler, it delivers events back into GPUI while the app
//! is already borrowed for that handler, and GPUI panics with "RefCell already
//! borrowed". GPUI's prompts open the panel asynchronously and hand the result
//! back on the main thread once the handler has returned.

use std::path::{Path, PathBuf};

use gpui::{Context, PathPromptOptions};

use crate::shell::SurfApp;

/// Ask for an existing file or folder, then run `then` with it on the app.
pub fn open(
    cx: &mut Context<SurfApp>,
    title: &'static str,
    folders: bool,
    then: impl FnOnce(&mut SurfApp, PathBuf) + 'static,
) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: !folders,
        directories: folders,
        multiple: false,
        prompt: Some(title.into()),
    });
    cx.spawn(async move |this, cx| {
        let picked = match rx.await {
            Ok(Ok(Some(mut paths))) if !paths.is_empty() => Ok(paths.remove(0)),
            Ok(Ok(_)) | Err(_) => return,
            Ok(Err(err)) => Err(err),
        };
        let _ = this.update(cx, |app, cx| {
            match picked {
                Ok(path) => then(app, path),
                Err(err) => app.session.status = format!("Could not open the file picker: {err:#}"),
            }
            cx.notify();
        });
    })
    .detach();
}

/// Ask where to save `stem.ext`, then run `then` with the chosen path.
/// Adds the extension if the user typed a name without one.
pub fn save(
    cx: &mut Context<SurfApp>,
    stem: String,
    ext: &'static str,
    then: impl FnOnce(&mut SurfApp, PathBuf) + 'static,
) {
    let dir = default_dir();
    let rx = cx.prompt_for_new_path(&dir, Some(&format!("{stem}.{ext}")));
    cx.spawn(async move |this, cx| {
        let picked = match rx.await {
            Ok(Ok(Some(path))) => Ok(with_extension(path, ext)),
            Ok(Ok(None)) | Err(_) => return,
            Ok(Err(err)) => Err(err),
        };
        let _ = this.update(cx, |app, cx| {
            match picked {
                Ok(path) => then(app, path),
                Err(err) => app.session.status = format!("Could not open the save dialog: {err:#}"),
            }
            cx.notify();
        });
    })
    .detach();
}

/// What the user chose when asked about unsaved changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsaved {
    Save,
    Discard,
    Cancel,
}

/// Ask "Save changes to …?" with Save / Don't Save / Cancel, then run
/// `then` with the answer on the app. Callers ask only when
/// [`crate::session::Session::is_dirty`].
pub fn confirm_unsaved(
    window: &mut gpui::Window,
    cx: &mut Context<SurfApp>,
    name: &str,
    then: impl FnOnce(&mut SurfApp, Unsaved, &mut Context<SurfApp>) + 'static,
) {
    let rx = window.prompt(
        gpui::PromptLevel::Warning,
        &format!("Save changes to “{name}”?"),
        Some("Your changes will be lost if you don't save them."),
        &["Save", "Don't Save", "Cancel"],
        cx,
    );
    cx.spawn(async move |this, cx| {
        let answer = match rx.await {
            Ok(0) => Unsaved::Save,
            Ok(1) => Unsaved::Discard,
            _ => Unsaved::Cancel,
        };
        let _ = this.update(cx, |app, cx| {
            then(app, answer, cx);
            cx.notify();
        });
    })
    .detach();
}

fn default_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| {
            let music = home.join("Music");
            if music.is_dir() { music } else { home }
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

fn with_extension(path: PathBuf, ext: &str) -> PathBuf {
    let has = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(ext));
    if has { path } else { append_ext(&path, ext) }
}

fn append_ext(path: &Path, ext: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_paths_get_the_extension_once() {
        assert_eq!(
            with_extension(PathBuf::from("/a/take"), "mid"),
            PathBuf::from("/a/take.mid")
        );
        assert_eq!(
            with_extension(PathBuf::from("/a/take.MID"), "mid"),
            PathBuf::from("/a/take.MID")
        );
        assert_eq!(
            with_extension(PathBuf::from("/a/v1.2"), "wav"),
            PathBuf::from("/a/v1.2.wav")
        );
    }
}
