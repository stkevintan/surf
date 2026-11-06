//! Headless tests of the editors' pointer handling.
//!
//! They drive the same `pointer_down` / `pointer_move` / `pointer_up` the
//! views wire to GPUI, with real mouse events at computed positions, and
//! check the project and the undo stack. The canvases' painted bounds are set
//! by hand, since nothing is painted.

use gpui::{
    Bounds, Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, Pixels, Point, TestAppContext,
    point, px, size,
};

use surf_core::project::Project;
use surf_core::song::Song;

use crate::editing::{Focus, Tool};
use crate::session::Session;
use crate::shell::SurfApp;
use crate::theme::size as sz;
use crate::views::{piano_roll, tracks};

const ROLL_ORIGIN: (f32, f32) = (300.0, 500.0);
const TRACKS_ORIGIN: (f32, f32) = (300.0, 100.0);

fn app(cx: &mut TestAppContext) -> SurfApp {
    let focus = cx.update(|cx| cx.focus_handle());
    let mut song = Song::demo();
    // Two tracks: bass on channel 2.
    for n in song.notes.iter_mut().filter(|n| n.key < 55) {
        n.channel = 1;
    }
    let mut session = Session::new();
    session.set_project(Project::from_song(&song));
    let mut app = SurfApp::with_session(session, focus);
    app.ui.edit.reconcile(&app.session.project);
    app.ui.roll_scroll_x = 0.0;
    app.ui.roll_top_key = 80;
    app.ui.roll_bounds.set(Bounds::new(
        point(px(ROLL_ORIGIN.0), px(ROLL_ORIGIN.1)),
        size(px(1000.0), px(300.0)),
    ));
    app.ui.tracks_bounds.set(Bounds::new(
        point(px(TRACKS_ORIGIN.0), px(TRACKS_ORIGIN.1)),
        size(px(1000.0), px(300.0)),
    ));
    app
}

/// Window position of a note's body in the piano roll (region at tick 0).
fn roll_pos(app: &SurfApp, tick: u64, key: u8, dx: f32) -> Point<Pixels> {
    let tick_w = app.ui.roll_beat_w / app.session.project.ticks_per_beat() as f32;
    let x = sz::ROLL_KEYS_W + tick as f32 * tick_w + dx;
    let y = sz::RULER_EDITOR_H
        + (app.ui.roll_top_key as f32 - key as f32) * sz::ROLL_ROW_H
        + sz::ROLL_ROW_H / 2.0;
    point(px(ROLL_ORIGIN.0 + x), px(ROLL_ORIGIN.1 + y))
}

fn down(at: Point<Pixels>, modifiers: Modifiers, clicks: usize) -> MouseDownEvent {
    MouseDownEvent {
        button: MouseButton::Left,
        position: at,
        modifiers,
        click_count: clicks,
        first_mouse: false,
    }
}

fn drag(at: Point<Pixels>) -> MouseMoveEvent {
    MouseMoveEvent {
        position: at,
        pressed_button: Some(MouseButton::Left),
        modifiers: Modifiers::default(),
    }
}

/// The first note whose row is inside the test viewport (top key 80, a
/// 300 px tall roll shows about 15 rows), with no other note on its row
/// within a beat, so clicks hit it unambiguously.
fn first_note(app: &SurfApp) -> surf_core::project::Note {
    let grid_h = 300.0 - sz::RULER_EDITOR_H - sz::ROLL_VELOCITY_H;
    let rows = (grid_h / sz::ROLL_ROW_H) as i32;
    let top = app.ui.roll_top_key as i32;
    let notes = &app.session.project.tracks[0].regions[0].notes;
    notes
        .iter()
        .find(|n| {
            let row = top - n.key as i32;
            (1..rows - 2).contains(&row)
                && !notes
                    .iter()
                    .any(|m| m.id != n.id && m.key == n.key && m.start.abs_diff(n.start) < 960)
        })
        .expect("a note in view")
        .clone()
}

#[gpui::test]
fn dragging_a_note_moves_it_on_the_grid_as_one_undo_step(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let before = app.session.project.clone();
    let n = first_note(&app);
    let beat_w = app.ui.roll_beat_w;
    let start = roll_pos(&app, n.start, n.key, 6.0);
    piano_roll::pointer_down(&mut app, &down(start, Modifiers::default(), 1));
    assert_eq!(app.ui.edit.focus, Focus::Roll);
    assert!(app.ui.edit.notes.contains(&n.id), "click selects the note");
    // Several moves, ending one beat right and two rows up.
    for step in 1..=5 {
        let t = step as f32 / 5.0;
        piano_roll::pointer_move(
            &mut app,
            &drag(point(
                start.x + px(beat_w * t),
                start.y - px(2.0 * sz::ROLL_ROW_H * t),
            )),
        );
    }
    piano_roll::pointer_up(&mut app);

    let moved = app.session.project.tracks[0].regions[0]
        .note(n.id)
        .unwrap()
        .clone();
    let beat = app.session.project.ticks_per_beat();
    let grid = app.ui.edit.roll_snap.ticks(&app.session.project);
    assert_eq!(moved.key, n.key + 2);
    assert_eq!(moved.start % grid, 0, "lands on the grid");
    assert!((moved.start as i64 - (n.start + beat) as i64).abs() <= grid as i64);
    assert_eq!(app.session.history.undo_label(), Some("Move Notes"));
    app.session.undo();
    assert_eq!(app.session.project, before, "one ⌘Z undoes the whole drag");
}

#[gpui::test]
fn dragging_a_note_edge_resizes_it(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let n = first_note(&app);
    let tick_w = app.ui.roll_beat_w / app.session.project.ticks_per_beat() as f32;
    let edge = roll_pos(&app, n.end(), n.key, -2.0);
    piano_roll::pointer_down(&mut app, &down(edge, Modifiers::default(), 1));
    piano_roll::pointer_move(&mut app, &drag(point(edge.x + px(480.0 * tick_w), edge.y)));
    piano_roll::pointer_up(&mut app);
    let after = app.session.project.tracks[0].regions[0]
        .note(n.id)
        .unwrap()
        .len;
    assert_eq!(after, n.len + 480, "one beat longer");
    assert_eq!(app.session.history.undo_label(), Some("Resize Notes"));
}

#[gpui::test]
fn pencil_adds_a_note_and_double_click_removes_it(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let count = app.session.project.tracks[0].regions[0].notes.len();
    app.ui.edit.roll_tool = Tool::Pencil;
    // An empty cell: key 79 at beat 3 (the demo has nothing there).
    let at = roll_pos(&app, 3 * 480 + 10, 79, 0.0);
    piano_roll::pointer_down(&mut app, &down(at, Modifiers::default(), 1));
    piano_roll::pointer_up(&mut app);
    let notes = &app.session.project.tracks[0].regions[0].notes;
    assert_eq!(notes.len(), count + 1);
    let added = notes
        .iter()
        .find(|n| n.key == 79 && n.start == 3 * 480)
        .expect("note on the grid cell");
    assert_eq!(added.len, app.ui.edit.roll_snap.ticks(&app.session.project));

    app.ui.edit.roll_tool = Tool::Pointer;
    piano_roll::pointer_down(&mut app, &down(at, Modifiers::default(), 2));
    assert_eq!(app.session.project.tracks[0].regions[0].notes.len(), count);
}

#[gpui::test]
fn velocity_lane_drag_changes_selected_notes(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let n = first_note(&app);
    app.ui.edit.notes = [n.id].into();
    let tick_w = app.ui.roll_beat_w / app.session.project.ticks_per_beat() as f32;
    let x = ROLL_ORIGIN.0 + sz::ROLL_KEYS_W + n.start as f32 * tick_w + 1.0;
    let y = ROLL_ORIGIN.1 + 300.0 - 20.0;
    piano_roll::pointer_down(
        &mut app,
        &down(point(px(x), px(y)), Modifiers::default(), 1),
    );
    piano_roll::pointer_move(&mut app, &drag(point(px(x), px(y - 30.0))));
    piano_roll::pointer_up(&mut app);
    let v = app.session.project.tracks[0].regions[0]
        .note(n.id)
        .unwrap()
        .velocity;
    assert!(
        v > n.velocity,
        "dragging up raises velocity ({} -> {v})",
        n.velocity
    );
    assert_eq!(app.session.history.undo_label(), Some("Change Velocity"));
}

#[gpui::test]
fn marquee_selects_notes_under_it(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let a = roll_pos(&app, 0, 80, 1.0);
    let b = roll_pos(&app, 4 * 480, 60, 0.0);
    piano_roll::pointer_down(
        &mut app,
        &down(point(a.x, a.y - px(4.0)), Modifiers::default(), 1),
    );
    piano_roll::pointer_move(&mut app, &drag(b));
    piano_roll::pointer_up(&mut app);
    let p = &app.session.project;
    let want: Vec<_> = p.tracks[0].regions[0]
        .notes
        .iter()
        .filter(|n| n.start < 4 * 480 && (60..=80).contains(&n.key))
        .map(|n| n.id)
        .collect();
    assert!(!want.is_empty());
    for id in &want {
        assert!(app.ui.edit.notes.contains(id));
    }
}

#[gpui::test]
fn dragging_a_region_to_the_next_track_moves_it(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let r = app.session.project.tracks[0].regions[0].clone();
    let bar = app.session.project.ticks_per_bar();
    let tick_w = app.ui.tracks_beat_w / app.session.project.ticks_per_beat() as f32;
    let start = point(
        px(TRACKS_ORIGIN.0 + 20.0),
        px(TRACKS_ORIGIN.1 + sz::RULER_TRACKS_H + 20.0),
    );
    tracks::pointer_down(&mut app, &down(start, Modifiers::default(), 1));
    assert!(app.ui.edit.regions.contains(&r.id));
    tracks::pointer_move(
        &mut app,
        &drag(point(
            start.x + px(bar as f32 * tick_w),
            start.y + px(sz::LANE_H),
        )),
    );
    tracks::pointer_up(&mut app, start);
    let p = &app.session.project;
    assert!(p.tracks[0].region(r.id).is_none(), "left track 1");
    let moved = p.tracks[1].region(r.id).expect("now on track 2");
    assert_eq!(moved.start, bar, "one bar later, snapped");
    assert_eq!(moved.notes, r.notes, "notes travel with the region");
    app.session.undo();
    assert!(app.session.project.tracks[0].region(r.id).is_some());
}

#[gpui::test]
fn option_drag_copies_a_region(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let bar = app.session.project.ticks_per_bar();
    let len = app.session.project.tracks[0].regions[0].len;
    let tick_w = app.ui.tracks_beat_w / app.session.project.ticks_per_beat() as f32;
    let start = point(
        px(TRACKS_ORIGIN.0 + 20.0),
        px(TRACKS_ORIGIN.1 + sz::RULER_TRACKS_H + 20.0),
    );
    let alt = Modifiers {
        alt: true,
        ..Default::default()
    };
    tracks::pointer_down(&mut app, &down(start, alt, 1));
    tracks::pointer_move(
        &mut app,
        &drag(point(start.x + px(len as f32 * tick_w), start.y)),
    );
    tracks::pointer_up(&mut app, start);
    let regions = &app.session.project.tracks[0].regions;
    assert_eq!(regions.len(), 2, "original stays, copy added");
    assert_eq!(regions[1].start, len.div_ceil(bar) * bar);
    assert_ne!(
        regions[0].notes[0].id, regions[1].notes[0].id,
        "copies get fresh ids"
    );
    assert_eq!(app.session.history.undo_label(), Some("Copy Region"));
}

#[gpui::test]
fn scissors_split_and_join_put_it_back(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let r = app.session.project.tracks[0].regions[0].clone();
    let tick_w = app.ui.tracks_beat_w / app.session.project.ticks_per_beat() as f32;
    app.ui.edit.tracks_tool = Tool::Scissors;
    let at = point(
        px(TRACKS_ORIGIN.0 + 2.0 * 1920.0 * tick_w),
        px(TRACKS_ORIGIN.1 + sz::RULER_TRACKS_H + 20.0),
    );
    tracks::pointer_down(&mut app, &down(at, Modifiers::default(), 1));
    tracks::pointer_up(&mut app, at);
    assert_eq!(app.session.project.tracks[0].regions.len(), 2);
    assert_eq!(app.session.project.tracks[0].regions[1].start, 2 * 1920);
    crate::editing::join(&mut app.session, &mut app.ui.edit);
    let joined = &app.session.project.tracks[0].regions;
    assert_eq!(joined.len(), 1);
    let keys = |r: &surf_core::project::Region| {
        r.notes.iter().map(|n| (n.start, n.key)).collect::<Vec<_>>()
    };
    assert_eq!(keys(&joined[0]), keys(&r));
}
