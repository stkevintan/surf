//! The Logic-style window, as specified in `docs/design/README.md`:
//!
//! ```text
//! control bar   (area toggles · transport · LCD · count-in/metronome · MIDI · CPU)
//! toolbar       (edit commands · planned features · export)
//! [library] [inspector] │ Tracks / Perform
//!                       │ ═══ splitter ═══
//!                       │ editor tabs: Mixer · Piano Roll · Event List · …
//! status bar
//! ```
//!
//! The song is still the old single note list, so it is shown as one track
//! with one region; the multi-track model comes with `surf-core` in a later
//! step. Everything that does not exist yet is drawn in place and dimmed, so
//! later work fills in slots instead of moving the layout.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use gpui::{
    AnyElement, Bounds, ClickEvent, Context, FocusHandle, KeyDownEvent, KeyUpEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Window, div, prelude::*,
    px,
};

use surf_midi::LiveEvent;

use surf_core::project::Id;

use crate::editing::{self, Editing, Focus};
use crate::session::Session;
use crate::theme::{color, font, size};
use crate::views;

/// The big area above the editor.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MainView {
    Tracks,
    Perform,
}

/// Tabs of the bottom editor.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditorTab {
    Mixer,
    PianoRoll,
    EventList,
}

/// UI-only state: what is shown and how. Not part of the project file.
pub struct Ui {
    pub show_library: bool,
    pub show_inspector: bool,
    pub show_toolbar: bool,
    pub show_editor: bool,
    pub editor_max: bool,
    pub editor_h: f32,
    pub main: MainView,
    pub tab: EditorTab,
    pub region_open: bool,
    pub track_open: bool,
    /// Tracks-area zoom: pixels per beat.
    pub tracks_beat_w: f32,
    /// Piano-roll zoom and scroll.
    pub roll_beat_w: f32,
    pub roll_top_key: u8,
    pub roll_scroll_x: f32,
    pub library_query: String,
    pub musical_typing: bool,
    pub typing_octave: i8,
    pub metronome: bool,
    pub count_in: bool,
    /// Computer keys currently held for Musical Typing, and the note each plays.
    pub typing_keys: HashMap<String, u8>,
    /// Splitter drag in progress: (pointer y at start, editor height at start).
    pub splitter_drag: Option<(f32, f32)>,
    /// Which channel-strip fader is being dragged.
    pub fader_drag: Option<&'static str>,
    /// Where each fader was painted last frame, for pointer mapping.
    pub fader_bounds: Rc<RefCell<HashMap<&'static str, Bounds<Pixels>>>>,
    /// Painted bounds of the canvases that handle pointer input.
    pub tracks_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub roll_bounds: Rc<Cell<Bounds<Pixels>>>,
    pub perform_bounds: Rc<Cell<Bounds<Pixels>>>,
    /// Key held down with the mouse on an on-screen keyboard.
    pub mouse_key: Option<u8>,
    /// Selection, focus, tools and snap for the editors.
    pub edit: Editing,
    /// Pointer drag in the Tracks area or Piano Roll.
    pub drag: Option<Drag>,
    /// Rubber-band selection rectangle in window coordinates.
    pub marquee: Option<(Point<Pixels>, Point<Pixels>)>,
    /// Pencil hover cell in the piano roll: (tick, key).
    pub pencil_hover: Option<(u64, u8)>,
}

/// What a pointer drag is doing, with the state it started from.
#[derive(Clone, Debug)]
pub enum Drag {
    /// Moving notes (or copying, with ⌥). The lead note's start tick and key
    /// anchor snapping, so the dragged note lands on the grid.
    MoveNotes {
        x0: f32,
        y0: f32,
        lead_tick: u64,
        lead_key: u8,
        copy: bool,
        notes: Vec<Id>,
    },
    ResizeNotes {
        x0: f32,
        notes: Vec<Id>,
    },
    Velocity {
        y0: f32,
        base: Vec<(Id, u8)>,
    },
    MoveRegions {
        x0: f32,
        y0: f32,
        lead_tick: u64,
        copy: bool,
        regions: Vec<Id>,
    },
    ResizeRegion {
        x0: f32,
        region: Id,
        len0: u64,
    },
    Marquee {
        tracks: bool,
    },
    Scrub {
        tracks: bool,
    },
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            // Design defaults: Inspector open, Library closed, editor on Piano Roll.
            show_library: false,
            show_inspector: true,
            show_toolbar: true,
            show_editor: true,
            editor_max: false,
            editor_h: size::EDITOR_H,
            main: MainView::Tracks,
            tab: EditorTab::PianoRoll,
            region_open: true,
            track_open: false,
            tracks_beat_w: size::TRACKS_BEAT_W,
            roll_beat_w: size::ROLL_BEAT_W,
            roll_top_key: 86,
            roll_scroll_x: 0.0,
            library_query: String::new(),
            musical_typing: false,
            typing_octave: 0,
            metronome: false,
            count_in: false,
            typing_keys: HashMap::new(),
            splitter_drag: None,
            fader_drag: None,
            fader_bounds: Rc::default(),
            tracks_bounds: Rc::default(),
            roll_bounds: Rc::default(),
            perform_bounds: Rc::default(),
            mouse_key: None,
            edit: Editing::default(),
            drag: None,
            marquee: None,
            pencil_hover: None,
        }
    }
}

pub struct SurfApp {
    pub session: Session,
    pub ui: Ui,
    focus: FocusHandle,
    started: Instant,
}

impl SurfApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        if std::env::var_os("SURF_UI").is_none() {
            window.focus(&focus);
        }
        Self::with_session(Session::new(), focus)
    }

    /// Build the app around a session, without touching a window. Used by
    /// [`SurfApp::new`] and by headless tests of the editors.
    pub fn with_session(session: Session, focus: FocusHandle) -> Self {
        let mut app = Self {
            session,
            ui: Ui::default(),
            focus,
            started: Instant::now(),
        };
        app.center_roll_on_song();
        // `SURF_UI=perform,mixer,library,...` opens specific views at startup,
        // for screenshots and smoke tests.
        if let Ok(spec) = std::env::var("SURF_UI") {
            for part in spec.split(',').map(str::trim) {
                match part {
                    "perform" => app.ui.main = MainView::Perform,
                    "mixer" => app.ui.tab = EditorTab::Mixer,
                    "events" => app.ui.tab = EditorTab::EventList,
                    "library" => app.ui.show_library = true,
                    "no-editor" => app.ui.show_editor = false,
                    "no-inspector" => app.ui.show_inspector = false,
                    "track" => app.ui.track_open = true,
                    "play" => app.session.toggle_play(),
                    // Two tracks from the demo (bass notes on channel 2) and a
                    // note selection, so editing state shows in screenshots.
                    "duet" => {
                        let mut song = surf_core::song::Song::demo();
                        for n in song.notes.iter_mut().filter(|n| n.key < 55) {
                            n.channel = 1;
                        }
                        app.session
                            .set_project(surf_core::project::Project::from_song(&song));
                        app.ui.edit.reconcile(&app.session.project);
                        let ids: Vec<_> = app.session.project.tracks[0].regions[0]
                            .notes
                            .iter()
                            .take(4)
                            .map(|n| n.id)
                            .collect();
                        app.ui.edit.notes = ids.into_iter().collect();
                        app.ui
                            .edit
                            .regions
                            .insert(app.session.project.tracks[0].regions[0].id);
                        app.center_roll_on_song();
                    }
                    _ => {}
                }
            }
        }
        app
    }

    /// Scroll the piano roll so the song's notes are in view.
    pub fn center_roll_on_song(&mut self) {
        self.ui.edit.reconcile(&self.session.project);
        let region = self
            .ui
            .edit
            .edit_region(&self.session.project)
            .map(|(_, r)| r);
        let keys = region
            .into_iter()
            .flat_map(|r| r.notes.iter().map(|n| n.key));
        let (lo, hi) = keys.fold((127u8, 0u8), |(lo, hi), k| (lo.min(k), hi.max(k)));
        if lo <= hi {
            // Rows that fit in the default editor height.
            let grid_h =
                size::EDITOR_H - size::EDITOR_BAR_H - size::RULER_EDITOR_H - size::ROLL_VELOCITY_H;
            let rows = (grid_h / size::ROLL_ROW_H).floor() as u16;
            let span = (hi - lo) as u16 + 1;
            // Fit the whole range if it fits; otherwise start just above the top note.
            let top = if span + 2 <= rows {
                hi as u16 + (rows - span) / 2
            } else {
                hi as u16 + 2
            };
            self.ui.roll_top_key = top.min(127) as u8;
        }
        self.ui.roll_scroll_x = 0.0;
    }

    // ---- shortcuts (Logic defaults; see docs/design/README.md §5) ----

    fn key_down(&mut self, event: &KeyDownEvent) {
        let ks = &event.keystroke;
        let key = ks.key.to_lowercase();
        let cmd = ks.modifiers.secondary();

        if self.ui.musical_typing && !cmd {
            if let Some(note) = typing_note(&key, self.ui.typing_octave) {
                if !event.is_held && self.ui.typing_keys.insert(key, note).is_none() {
                    self.session.handle_live(
                        LiveEvent::NoteOn {
                            key: note,
                            velocity: 100,
                        },
                        Instant::now(),
                        true,
                    );
                }
                return;
            }
            match key.as_str() {
                "z" => {
                    self.ui.typing_octave = (self.ui.typing_octave - 1).max(-3);
                    self.session.status =
                        format!("Musical Typing octave C{}", 3 + self.ui.typing_octave);
                    return;
                }
                "x" => {
                    self.ui.typing_octave = (self.ui.typing_octave + 1).min(4);
                    self.session.status =
                        format!("Musical Typing octave C{}", 3 + self.ui.typing_octave);
                    return;
                }
                _ => {}
            }
        }
        if event.is_held {
            return;
        }
        let alt = ks.modifiers.alt;
        let shift = ks.modifiers.shift;
        let tracks_focus = self.ui.edit.focus == Focus::Tracks;
        match (cmd, key.as_str()) {
            (true, "z") if shift => self.session.redo(),
            (true, "z") => self.session.undo(),
            (true, "a") => editing::select_all(&self.session, &mut self.ui.edit),
            (true, "c") => {
                editing::copy(&mut self.session, &mut self.ui.edit);
            }
            (true, "x") => editing::cut(&mut self.session, &mut self.ui.edit),
            (true, "v") => editing::paste(&mut self.session, &mut self.ui.edit),
            (true, "t") => editing::split_at_playhead(&mut self.session, &mut self.ui.edit),
            (true, "j") => editing::join(&mut self.session, &mut self.ui.edit),
            (false, "backspace" | "delete") => {
                editing::delete(&mut self.session, &mut self.ui.edit)
            }
            (false, "q") => editing::quantize(&mut self.session, &self.ui.edit),
            (false, "up") if alt => {
                editing::transpose(&mut self.session, &self.ui.edit, if shift { 12 } else { 1 })
            }
            (false, "down") if alt => editing::transpose(
                &mut self.session,
                &self.ui.edit,
                if shift { -12 } else { -1 },
            ),
            (false, "up") if tracks_focus => self.select_adjacent_track(-1),
            (false, "down") if tracks_focus => self.select_adjacent_track(1),
            (false, "escape") => {
                if self.ui.drag.take().is_some() {
                    self.session.history.cancel(&mut self.session.project);
                    self.session.project_changed();
                }
                self.ui.marquee = None;
            }
            (false, "m") => self.toggle_selected_track(true),
            (false, "s") => self.toggle_selected_track(false),
            (true, "k") => self.toggle_typing(),
            (true, "s") => self.session.save_vst_state(),
            // ⌘O is handled in render, which has the context a dialog needs.
            (false, "space") => self.session.toggle_play(),
            (false, "enter") => self.session.seek(0.0),
            (false, ",") => self.seek_bars(-1.0),
            (false, ".") => self.seek_bars(1.0),
            (false, "r") => self.session.toggle_record(),
            (false, "c") => self.session.toggle_loop(),
            (false, "k") => self.toggle_metronome(),
            (false, "i") => self.ui.show_inspector = !self.ui.show_inspector,
            (false, "y") => self.ui.show_library = !self.ui.show_library,
            (false, "e") => self.ui.show_editor = !self.ui.show_editor,
            (false, "p") => self.open_tab(EditorTab::PianoRoll, true),
            (false, "x") => self.open_tab(EditorTab::Mixer, true),
            // Shift doubles as a sustain pedal when no pedal is plugged in.
            (false, "shift") if !self.session.sustain => {
                self.session
                    .handle_live(LiveEvent::Sustain(true), Instant::now(), true)
            }
            _ => {}
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent) {
        let key = event.keystroke.key.to_lowercase();
        if key == "shift" && self.session.sustain {
            self.session
                .handle_live(LiveEvent::Sustain(false), Instant::now(), true);
            return;
        }
        if let Some(note) = self.ui.typing_keys.remove(&key) {
            self.session
                .handle_live(LiveEvent::NoteOff { key: note }, Instant::now(), true);
        }
    }

    fn select_adjacent_track(&mut self, step: i32) {
        let p = &self.session.project;
        if p.tracks.is_empty() {
            return;
        }
        let i = self
            .ui
            .edit
            .track
            .and_then(|t| p.tracks.iter().position(|x| x.id == t))
            .unwrap_or(0) as i32;
        let next = (i + step).clamp(0, p.tracks.len() as i32 - 1) as usize;
        self.ui.edit.track = Some(p.tracks[next].id);
    }

    /// M / S on the selected track.
    fn toggle_selected_track(&mut self, mute: bool) {
        let Some(id) = self.ui.edit.track else { return };
        let Some(t) = self.session.project.track(id) else {
            return;
        };
        let field = if mute {
            surf_core::edit::TrackField::Mute(!t.mute)
        } else {
            surf_core::edit::TrackField::Solo(!t.solo)
        };
        editing::set_track(&mut self.session, id, field);
    }

    pub fn toggle_typing(&mut self) {
        self.ui.musical_typing = !self.ui.musical_typing;
        self.session.status = if self.ui.musical_typing {
            "Musical Typing on · A W S E D F T G Y H U J K O L P ; play C3–E4, Z/X change octave"
                .to_string()
        } else {
            for (_, note) in self.ui.typing_keys.drain() {
                self.session
                    .handle_live(LiveEvent::NoteOff { key: note }, Instant::now(), true);
            }
            "Musical Typing off · letters are shortcuts again".to_string()
        };
    }

    pub fn toggle_metronome(&mut self) {
        self.ui.metronome = !self.ui.metronome;
        self.session.status = "Metronome is planned · the button shows where it goes".to_string();
    }

    /// Open an editor tab; `toggle` closes the editor if that tab is already showing.
    pub fn open_tab(&mut self, tab: EditorTab, toggle: bool) {
        if toggle && self.ui.show_editor && self.ui.tab == tab {
            self.ui.show_editor = false;
        } else {
            self.ui.tab = tab;
            self.ui.show_editor = true;
        }
    }

    pub fn seek_bars(&mut self, bars: f64) {
        let (beats_per_bar, _) = self.session.time_signature();
        let bar_secs = 60.0 / self.session.bpm().max(1.0) * beats_per_bar.max(1) as f64;
        let now = self.session.position();
        let current = (now / bar_secs + 1e-6).floor();
        let target = if bars < 0.0 && now - current * bar_secs > 0.05 {
            current
        } else {
            current + bars
        };
        self.session.seek(target.max(0.0) * bar_secs);
    }

    pub fn planned(&mut self, what: &str) {
        self.session.status =
            format!("{what} · planned. The control is here so the layout won't change later.");
    }

    pub fn elapsed(&self) -> f32 {
        self.started.elapsed().as_secs_f32()
    }
}

/// Musical Typing: letters to notes, Logic layout. C3 = MIDI 60.
fn typing_note(key: &str, octave: i8) -> Option<u8> {
    let offset = match key {
        "a" => 0,
        "w" => 1,
        "s" => 2,
        "e" => 3,
        "d" => 4,
        "f" => 5,
        "t" => 6,
        "g" => 7,
        "y" => 8,
        "h" => 9,
        "u" => 10,
        "j" => 11,
        "k" => 12,
        "o" => 13,
        "l" => 14,
        "p" => 15,
        ";" => 16,
        _ => return None,
    };
    let note = 60 + octave as i16 * 12 + offset;
    (0..=127).contains(&note).then_some(note as u8)
}

impl Render for SurfApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Pause the frame loop while a file picker is up; see `dialogs`.
        // Closing the picker notifies, which restarts it.
        if !crate::dialogs::is_open() {
            window.request_animation_frame();
        }
        self.session.tick();
        self.ui.edit.reconcile(&self.session.project);
        // Live input and the instrument commands follow the selected track.
        self.session.set_input_track(self.ui.edit.track);
        if let Some(region) = self.session.record_region.take() {
            self.ui
                .edit
                .select_region(&self.session.project, region, false);
        }

        let main_view: AnyElement = match self.ui.main {
            MainView::Tracks => views::tracks::render(self, cx).into_any_element(),
            MainView::Perform => views::perform::render(self, cx).into_any_element(),
        };

        div()
            .id("surf-root")
            .flex()
            .flex_col()
            .size_full()
            .bg(color::CHROME)
            .text_color(color::TEXT_PRIMARY)
            .font_family(font::UI)
            .text_size(px(font::BODY))
            .track_focus(&self.focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, _| window.focus(&this.focus)),
            )
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, _, cx| {
                let ks = &e.keystroke;
                if ks.modifiers.secondary() && ks.key.eq_ignore_ascii_case("o") && !e.is_held {
                    open_midi(cx);
                } else {
                    this.key_down(e);
                }
                cx.notify();
            }))
            .on_key_up(cx.listener(|this, e: &KeyUpEvent, _, cx| {
                this.key_up(e);
                cx.notify();
            }))
            // Splitter drag is tracked on the root so the pointer may leave the bar.
            .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, window, cx| {
                if let Some((y0, h0)) = this.ui.splitter_drag {
                    let total = f32::from(window.viewport_size().height);
                    let y = f32::from(e.position.y);
                    let max = (total - 260.0).max(size::EDITOR_MIN_H);
                    this.ui.editor_h = (h0 + (y0 - y)).clamp(size::EDITOR_MIN_H, max);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| {
                    this.ui.splitter_drag = None;
                    // A fader released outside its strip still ends its drag.
                    if this.ui.fader_drag.take().is_some() {
                        this.session.commit();
                    }
                }),
            )
            .child(views::control_bar::render(self, cx))
            .when(self.ui.show_toolbar, |this| {
                this.child(views::toolbar::render(self, cx))
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h(px(0.0))
                    .border_t_1()
                    .border_color(color::LINE)
                    .when(self.ui.show_library, |this| {
                        this.child(views::library::render(self, cx))
                    })
                    .when(self.ui.show_inspector, |this| {
                        this.child(views::inspector::render(self, cx))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w(px(0.0))
                            .bg(color::PANEL)
                            .when(!(self.ui.show_editor && self.ui.editor_max), |this| {
                                this.child(main_view)
                            })
                            .when(self.ui.show_editor, |this| {
                                this.when(!self.ui.editor_max, |this| this.child(splitter(cx)))
                                    .child(views::editor::render(self, cx))
                            }),
                    ),
            )
            .child(views::status_bar::render(self, cx))
    }
}

fn splitter(cx: &mut Context<SurfApp>) -> impl IntoElement {
    div()
        .id("splitter")
        .flex_none()
        .h(px(size::SPLITTER))
        .w_full()
        .bg(color::LINE)
        .cursor_row_resize()
        .flex()
        .justify_center()
        .items_center()
        .child(
            div()
                .w(px(34.0))
                .h(px(3.0))
                .rounded_full()
                .bg(color::TEXT_DISABLED),
        )
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, _| {
                this.ui.splitter_drag = Some((f32::from(e.position.y), this.ui.editor_h));
            }),
        )
}

/// A pane header: dark bar, 30 px.
pub fn pane_header() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(8.0))
        .h(px(size::PANE_HEADER_H))
        .px(px(8.0))
        .bg(color::CHROME)
        .border_b_1()
        .border_color(color::LINE)
        .text_color(color::TEXT_SECONDARY)
}

/// Open a MIDI file through the platform picker.
pub fn open_midi(cx: &mut Context<SurfApp>) {
    crate::dialogs::open(cx, "Open", false, |app, path| {
        app.session.load_midi_path(&path);
        app.center_roll_on_song();
    });
}

pub fn export_midi(app: &mut SurfApp, cx: &mut Context<SurfApp>) {
    if app.session.can_export_midi() {
        crate::dialogs::save(cx, app.session.export_stem(), "mid", |app, path| {
            app.session.export_midi_to(&path)
        });
    }
}

pub fn export_wav(app: &mut SurfApp, cx: &mut Context<SurfApp>) {
    if app.session.can_export_wav() {
        crate::dialogs::save(cx, app.session.export_stem(), "wav", |app, path| {
            app.session.export_wav_to(path)
        });
    }
}

pub fn pick_plugin_folder(cx: &mut Context<SurfApp>) {
    crate::dialogs::open(cx, "Choose", true, |app, path| {
        app.session.load_vst_folder(&path)
    });
}

/// Like [`on_click`], for actions that also need the context (dialogs).
pub fn on_click_cx<E: InteractiveElement + StatefulInteractiveElement>(
    el: E,
    cx: &mut Context<SurfApp>,
    f: impl Fn(&mut SurfApp, &mut Context<SurfApp>) + 'static,
) -> E {
    el.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
        f(this, cx);
        cx.notify();
    }))
}

/// Wire a click on `el` to a closure on the app, then repaint.
pub fn on_click<E: InteractiveElement + StatefulInteractiveElement>(
    el: E,
    cx: &mut Context<SurfApp>,
    f: impl Fn(&mut SurfApp) + 'static,
) -> E {
    el.on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
        f(this);
        cx.notify();
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn musical_typing_follows_logic_octaves() {
        assert_eq!(typing_note("a", 0), Some(60)); // C3
        assert_eq!(typing_note(";", 0), Some(76)); // E4
        assert_eq!(typing_note("a", -1), Some(48));
        assert_eq!(typing_note("a", 6), None);
        assert_eq!(typing_note("q", 0), None);
    }
}
