//! The system menu bar and the keyboard shortcuts behind it.
//!
//! Every menu command is a GPUI action. [`init`] binds the shortcuts (so
//! macOS shows them next to the items) and installs the menus; the window
//! handles the actions in `shell.rs`. On Windows and Linux GPUI 0.2 keeps
//! the menus but doesn't draw a menu bar, so the shortcuts still work there.
//!
//! Single-letter shortcuts (Space, Q, M, E, …) only apply while Musical
//! Typing is off: with it on, those letters play notes.

use gpui::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};

/// Key context the window sets while Musical Typing is on.
pub const TYPING_CONTEXT: &str = "Typing";
const NOT_TYPING: &str = "!Typing";

actions!(
    surf,
    [
        // App
        About,
        Quit,
        // File
        NewProject,
        OpenFile,
        LoadDemo,
        CloseWindow,
        SaveProject,
        SaveProjectAs,
        ExportWav,
        ExportMidi,
        // Edit
        Undo,
        Redo,
        Cut,
        Copy,
        Paste,
        Delete,
        SelectAll,
        Quantize,
        SplitAtPlayhead,
        JoinRegions,
        // Track
        NewTrack,
        DeleteTrack,
        ToggleMute,
        ToggleSolo,
        // Navigate
        TogglePlay,
        Stop,
        RewindBar,
        ForwardBar,
        ToggleRecord,
        ToggleLoop,
        ToggleMetronome,
        ToggleMusicalTyping,
        // View
        ToggleLibrary,
        ToggleInspector,
        ToggleToolbar,
        ToggleEditor,
        OpenPianoRoll,
        OpenMixer,
        OpenEventList,
        ViewTracks,
        ViewPerform,
        // Window
        Minimize,
        Zoom,
    ]
);

/// Whether the OS draws the menu bar itself. Elsewhere Surf draws its own
/// (see `views::menu_bar`). `SURF_IN_WINDOW_MENU=1` forces Surf's own, to
/// look at it on a Mac.
pub fn native_menu_bar() -> bool {
    cfg!(target_os = "macos") && std::env::var_os("SURF_IN_WINDOW_MENU").is_none()
}

/// Whether a menu item shows a checkmark: the View items for what is on
/// screen, and the Navigate toggles. None for items that aren't toggles.
/// Both menu bars read this, so they always agree.
pub fn checked(action: &dyn gpui::Action, ui: &crate::shell::Ui, playing: bool) -> Option<bool> {
    use crate::shell::{EditorTab, MainView};
    let a = action.as_any();
    let tab = |t| ui.show_editor && ui.tab == t;
    Some(if a.is::<ToggleLibrary>() {
        ui.show_library
    } else if a.is::<ToggleInspector>() {
        ui.show_inspector
    } else if a.is::<ToggleToolbar>() {
        ui.show_toolbar
    } else if a.is::<ToggleEditor>() {
        ui.show_editor
    } else if a.is::<OpenPianoRoll>() {
        tab(EditorTab::PianoRoll)
    } else if a.is::<OpenMixer>() {
        tab(EditorTab::Mixer)
    } else if a.is::<OpenEventList>() {
        tab(EditorTab::EventList)
    } else if a.is::<ViewTracks>() {
        ui.main == MainView::Tracks
    } else if a.is::<ViewPerform>() {
        ui.main == MainView::Perform
    } else if a.is::<ToggleMusicalTyping>() {
        ui.musical_typing
    } else if a.is::<ToggleMetronome>() {
        ui.metronome
    } else if a.is::<TogglePlay>() {
        playing
    } else {
        return None;
    })
}

/// The same menus in the form the in-window bar draws from.
pub fn owned_menus() -> Vec<gpui::OwnedMenu> {
    menus().into_iter().map(Menu::owned).collect()
}

pub fn init(cx: &mut App) {
    cx.bind_keys(key_bindings());
    // With no window to handle it, Quit still quits.
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.set_menus(menus());
}

fn key_bindings() -> Vec<KeyBinding> {
    // `secondary` is ⌘ on macOS and Ctrl elsewhere.
    vec![
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-n", NewProject, None),
        KeyBinding::new("secondary-o", OpenFile, None),
        KeyBinding::new("secondary-w", CloseWindow, None),
        KeyBinding::new("secondary-s", SaveProject, None),
        KeyBinding::new("secondary-shift-s", SaveProjectAs, None),
        KeyBinding::new("secondary-b", ExportWav, None),
        KeyBinding::new("secondary-shift-e", ExportMidi, None),
        KeyBinding::new("secondary-z", Undo, None),
        KeyBinding::new("secondary-shift-z", Redo, None),
        KeyBinding::new("secondary-x", Cut, None),
        KeyBinding::new("secondary-c", Copy, None),
        KeyBinding::new("secondary-v", Paste, None),
        KeyBinding::new("secondary-a", SelectAll, None),
        KeyBinding::new("secondary-t", SplitAtPlayhead, None),
        KeyBinding::new("secondary-j", JoinRegions, None),
        KeyBinding::new("secondary-alt-n", NewTrack, None),
        KeyBinding::new("secondary-backspace", DeleteTrack, None),
        KeyBinding::new("secondary-k", ToggleMusicalTyping, None),
        KeyBinding::new("secondary-m", Minimize, None),
        KeyBinding::new("backspace", Delete, Some(NOT_TYPING)),
        KeyBinding::new("delete", Delete, Some(NOT_TYPING)),
        KeyBinding::new("q", Quantize, Some(NOT_TYPING)),
        KeyBinding::new("m", ToggleMute, Some(NOT_TYPING)),
        KeyBinding::new("s", ToggleSolo, Some(NOT_TYPING)),
        KeyBinding::new("space", TogglePlay, Some(NOT_TYPING)),
        KeyBinding::new("enter", Stop, Some(NOT_TYPING)),
        KeyBinding::new(",", RewindBar, Some(NOT_TYPING)),
        KeyBinding::new(".", ForwardBar, Some(NOT_TYPING)),
        KeyBinding::new("r", ToggleRecord, Some(NOT_TYPING)),
        KeyBinding::new("c", ToggleLoop, Some(NOT_TYPING)),
        KeyBinding::new("k", ToggleMetronome, Some(NOT_TYPING)),
        KeyBinding::new("y", ToggleLibrary, Some(NOT_TYPING)),
        KeyBinding::new("i", ToggleInspector, Some(NOT_TYPING)),
        KeyBinding::new("e", ToggleEditor, Some(NOT_TYPING)),
        KeyBinding::new("p", OpenPianoRoll, Some(NOT_TYPING)),
        KeyBinding::new("x", OpenMixer, Some(NOT_TYPING)),
    ]
}

fn menus() -> Vec<Menu> {
    vec![
        Menu {
            name: "Surf".into(),
            items: vec![
                MenuItem::action("About Surf", About),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Quit Surf", Quit),
            ],
        },
        Menu {
            name: "File".into(),
            items: vec![
                MenuItem::action("New", NewProject),
                MenuItem::action("Open…", OpenFile),
                MenuItem::action("Open Demo Song", LoadDemo),
                MenuItem::separator(),
                MenuItem::action("Close", CloseWindow),
                MenuItem::action("Save", SaveProject),
                MenuItem::action("Save As…", SaveProjectAs),
                MenuItem::separator(),
                MenuItem::action("Bounce WAV…", ExportWav),
                MenuItem::action("Export MIDI…", ExportMidi),
            ],
        },
        Menu {
            name: "Edit".into(),
            items: vec![
                MenuItem::os_action("Undo", Undo, OsAction::Undo),
                MenuItem::os_action("Redo", Redo, OsAction::Redo),
                MenuItem::separator(),
                MenuItem::os_action("Cut", Cut, OsAction::Cut),
                MenuItem::os_action("Copy", Copy, OsAction::Copy),
                MenuItem::os_action("Paste", Paste, OsAction::Paste),
                MenuItem::action("Delete", Delete),
                MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
                MenuItem::separator(),
                MenuItem::action("Quantize", Quantize),
                MenuItem::action("Split at Playhead", SplitAtPlayhead),
                MenuItem::action("Join Regions", JoinRegions),
            ],
        },
        Menu {
            name: "Track".into(),
            items: vec![
                MenuItem::action("New Track", NewTrack),
                MenuItem::action("Delete Track", DeleteTrack),
                MenuItem::separator(),
                MenuItem::action("Mute", ToggleMute),
                MenuItem::action("Solo", ToggleSolo),
            ],
        },
        Menu {
            name: "Navigate".into(),
            items: vec![
                MenuItem::action("Play / Pause", TogglePlay),
                MenuItem::action("Stop", Stop),
                MenuItem::action("Back One Bar", RewindBar),
                MenuItem::action("Forward One Bar", ForwardBar),
                MenuItem::separator(),
                MenuItem::action("Record", ToggleRecord),
                MenuItem::action("Cycle", ToggleLoop),
                MenuItem::action("Metronome", ToggleMetronome),
                MenuItem::separator(),
                MenuItem::action("Musical Typing", ToggleMusicalTyping),
            ],
        },
        Menu {
            name: "View".into(),
            items: vec![
                MenuItem::action("Library", ToggleLibrary),
                MenuItem::action("Inspector", ToggleInspector),
                MenuItem::action("Toolbar", ToggleToolbar),
                MenuItem::action("Editor", ToggleEditor),
                MenuItem::separator(),
                MenuItem::action("Piano Roll", OpenPianoRoll),
                MenuItem::action("Mixer", OpenMixer),
                MenuItem::action("Event List", OpenEventList),
                MenuItem::separator(),
                MenuItem::action("Tracks", ViewTracks),
                MenuItem::action("Perform", ViewPerform),
            ],
        },
        // macOS fills a menu named "Window" with its own items too.
        Menu {
            name: "Window".into(),
            items: vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_binding_parses() {
        // `KeyBinding::new` panics on a bad keystroke or context.
        assert!(key_bindings().len() > 30);
    }
}

/// Show `checked` on the native menu bar's items (macOS). GPUI 0.2 builds
/// the NSMenu without checkmarks, so this finds each item by its title and
/// sets its state directly. Items are matched by title within their menu,
/// which is unique in Surf's menus. A no-op where there's no native bar.
pub fn sync_native_checks(state: impl Fn(&dyn gpui::Action) -> Option<bool>) {
    #[cfg(target_os = "macos")]
    {
        use objc2::MainThreadMarker;
        use objc2_app_kit::{NSApplication, NSControlStateValueOff, NSControlStateValueOn};
        use objc2_foundation::NSString;

        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        let Some(bar) = NSApplication::sharedApplication(mtm).mainMenu() else {
            return;
        };
        for menu in menus() {
            let Some(top) = bar.itemWithTitle(&NSString::from_str(&menu.name)) else {
                continue;
            };
            let Some(sub) = top.submenu() else { continue };
            for item in &menu.items {
                let MenuItem::Action { name, action, .. } = item else {
                    continue;
                };
                let Some(on) = state(action.as_ref()) else {
                    continue;
                };
                if let Some(native) = sub.itemWithTitle(&NSString::from_str(name)) {
                    let want = if on { NSControlStateValueOn } else { NSControlStateValueOff };
                    if native.state() != want {
                        native.setState(want);
                    }
                }
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = state;
}
