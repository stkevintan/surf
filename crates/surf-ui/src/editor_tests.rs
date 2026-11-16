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

use crate::editing::{self, Focus, Tool};
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

/// Put the playhead at `secs` without a running engine.
fn set_playhead(app: &mut SurfApp, secs: f64) {
    app.session.engine.position = secs;
    app.session.engine.duration = app.session.engine.duration.max(secs + 1.0);
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

/// Review M22: grabbing a short note's edge and moving a pixel or two leaves it
/// as it was, instead of snapping it up to a whole grid step, and records no
/// edit.
#[gpui::test]
fn a_tiny_resize_leaves_a_short_note_alone(cx: &mut TestAppContext) {
    let mut app = app(cx);
    // Zoomed in, so a half-grid note is wide enough to grab by its edge.
    app.ui.roll_beat_w = sz::ROLL_BEAT_W_MAX;
    let n = first_note(&app);
    let grid = app.ui.edit.roll_snap.ticks(&app.session.project);
    let short = (grid / 2).max(1);
    let r = &mut app.session.project.tracks[0].regions[0];
    r.notes.iter_mut().find(|m| m.id == n.id).unwrap().len = short;
    let n = r.note(n.id).unwrap().clone();
    let label = app.session.history.undo_label().map(str::to_owned);
    let edge = roll_pos(&app, n.end(), n.key, -1.0);
    piano_roll::pointer_down(&mut app, &down(edge, Modifiers::default(), 1));
    assert!(
        matches!(app.ui.drag, Some(crate::shell::Drag::ResizeNotes { .. })),
        "grabbed by its edge"
    );
    // Out past the threshold (a preview is applied), then back within it.
    for dx in [1.0, 1.5, 40.0, 1.0] {
        piano_roll::pointer_move(&mut app, &drag(point(edge.x + px(dx), edge.y)));
    }
    piano_roll::pointer_up(&mut app);
    let after = app.session.project.tracks[0].regions[0]
        .note(n.id)
        .unwrap()
        .len;
    assert_eq!(after, short, "still {short} ticks");
    assert_eq!(
        app.session.history.undo_label().map(str::to_owned),
        label,
        "jitter is not an edit"
    );
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

/// Review M19: with the lanes scrolled, a click lands on the lane and the time
/// drawn under the pointer, not on the unscrolled one.
#[gpui::test]
fn clicks_follow_the_tracks_scroll(cx: &mut TestAppContext) {
    let mut app = app(cx);
    // Enough tracks to scroll past the 300 px view.
    for _ in 0..8 {
        editing::new_track(&mut app.session, &mut app.ui.edit);
    }
    let n = app.session.project.tracks.len();
    assert!(n as f32 * sz::LANE_H > 300.0 - sz::RULER_TRACKS_H);
    // Scroll down one lane with the wheel; it stops at the last lane.
    tracks::scroll_by(&mut app, 0.0, -sz::LANE_H, false, false);
    assert_eq!(app.ui.tracks_scroll_y, sz::LANE_H);
    tracks::scroll_by(&mut app, 0.0, -100_000.0, false, false);
    let max = n as f32 * sz::LANE_H - (300.0 - sz::RULER_TRACKS_H);
    assert_eq!(app.ui.tracks_scroll_y, max, "stops at the last lane");
    tracks::scroll_by(&mut app, 0.0, 100_000.0, false, false);
    assert_eq!(app.ui.tracks_scroll_y, 0.0);

    // Three lanes down: the top visible lane is track 4, a new empty one.
    tracks::scroll_by(&mut app, 0.0, -3.0 * sz::LANE_H, false, false);
    let at = point(
        px(TRACKS_ORIGIN.0 + 20.0),
        px(TRACKS_ORIGIN.1 + sz::RULER_TRACKS_H + 10.0),
    );
    tracks::pointer_down(&mut app, &down(at, Modifiers::default(), 1));
    tracks::pointer_up(&mut app, at);
    let fourth = app.session.project.tracks[3].id;
    assert_eq!(app.ui.edit.track, Some(fourth), "the lane drawn there");

    // Scrolled a bar to the right, the Pencil draws in bar 2, not bar 1.
    let bar = app.session.project.ticks_per_bar();
    let beats = bar as f32 / app.session.project.ticks_per_beat() as f32;
    let one_bar = beats * app.ui.tracks_beat_w;
    tracks::scroll_by(&mut app, 0.0, -one_bar, false, true);
    app.ui.edit.tracks_tool = Tool::Pencil;
    let at = point(px(TRACKS_ORIGIN.0 + 2.0), at.y);
    tracks::pointer_down(&mut app, &down(at, Modifiers::default(), 1));
    tracks::pointer_up(&mut app, at);
    let made = app.session.project.track(fourth).unwrap().regions.clone();
    assert_eq!(made.len(), 1, "drawn on the fourth track");
    assert_eq!(made[0].start, bar, "in bar 2");
}

/// Review: the wheel does not scroll the lanes under a region being dragged,
/// which would drop it on another track than the one under the pointer.
#[gpui::test]
fn the_wheel_waits_for_a_region_drag(cx: &mut TestAppContext) {
    let mut app = app(cx);
    for _ in 0..8 {
        editing::new_track(&mut app.session, &mut app.ui.edit);
    }
    let start = point(
        px(TRACKS_ORIGIN.0 + 20.0),
        px(TRACKS_ORIGIN.1 + sz::RULER_TRACKS_H + 20.0),
    );
    tracks::pointer_down(&mut app, &down(start, Modifiers::default(), 1));
    assert!(matches!(
        app.ui.drag,
        Some(crate::shell::Drag::MoveRegions { .. })
    ));
    tracks::scroll_by(&mut app, 0.0, -sz::LANE_H, false, false);
    tracks::scroll_by(&mut app, 0.0, -100.0, false, true);
    assert_eq!((app.ui.tracks_scroll_x, app.ui.tracks_scroll_y), (0.0, 0.0));
    tracks::pointer_up(&mut app, start);
    tracks::scroll_by(&mut app, 0.0, -sz::LANE_H, false, false);
    assert_eq!(app.ui.tracks_scroll_y, sz::LANE_H, "scrolls again after");
}

/// A replaced project starts at bar 1 and its first track, wherever the last
/// one was scrolled to.
#[gpui::test]
fn a_new_project_resets_the_tracks_scroll(cx: &mut TestAppContext) {
    let mut app = app(cx);
    app.ui.tracks_scroll_x = 5_000.0;
    app.ui.tracks_scroll_y = 200.0;
    app.after_project_replaced();
    assert_eq!((app.ui.tracks_scroll_x, app.ui.tracks_scroll_y), (0.0, 0.0));
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

#[gpui::test]
fn pasted_notes_land_one_grid_step_after_the_copied_ones(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let region = &app.session.project.tracks[0].regions[0];
    let picked: Vec<_> = region.notes.iter().skip(4).take(3).cloned().collect();
    let count = region.notes.len();
    app.ui.edit.focus = Focus::Roll;
    app.ui.edit.notes = picked.iter().map(|n| n.id).collect();
    editing::copy(&mut app.session, &mut app.ui.edit);
    // The playhead doesn't matter for notes.
    let far = app.session.project.tick_to_sec(5000);
    set_playhead(&mut app, far);
    let grid = app.ui.edit.roll_snap.ticks(&app.session.project);

    for step in 1..=2 {
        editing::paste(&mut app.session, &mut app.ui.edit);
        let notes = &app.session.project.tracks[0].regions[0].notes;
        assert_eq!(notes.len(), count + 3 * step as usize);
        assert_eq!(app.ui.edit.notes.len(), 3, "the pasted notes are selected");
        for n in &picked {
            let pasted = notes
                .iter()
                .find(|m| {
                    app.ui.edit.notes.contains(&m.id)
                        && m.key == n.key
                        && m.start == n.start + step * grid
                })
                .expect("a copy one step further on per paste");
            assert_ne!(pasted.id, n.id, "copies get fresh ids");
        }
    }
    assert_eq!(app.session.history.undo_label(), Some("Paste"));
    app.session.undo();
    app.session.undo();
    assert_eq!(app.session.project.tracks[0].regions[0].notes.len(), count);
}

#[gpui::test]
fn cut_notes_paste_back_where_they_were(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let before = app.session.project.clone();
    let n = first_note(&app);
    app.ui.edit.focus = Focus::Roll;
    app.ui.edit.notes = [n.id].into();
    editing::cut(&mut app.session, &mut app.ui.edit);
    let region = &app.session.project.tracks[0].regions[0];
    assert!(region.note(n.id).is_none());
    assert_eq!(app.session.history.undo_label(), Some("Cut"));

    editing::paste(&mut app.session, &mut app.ui.edit);
    let region = &app.session.project.tracks[0].regions[0];
    let back: Vec<_> = region
        .notes
        .iter()
        .map(|m| (m.key, m.start, m.len, m.velocity))
        .collect();
    let orig: Vec<_> = before.tracks[0].regions[0]
        .notes
        .iter()
        .map(|m| (m.key, m.start, m.len, m.velocity))
        .collect();
    assert_eq!(back, orig, "cut then paste puts the note back");
    app.session.undo();
    app.session.undo();
    assert_eq!(app.session.project.tracks, before.tracks);
}

#[gpui::test]
fn pasting_regions_keeps_their_track_layout(cx: &mut TestAppContext) {
    let mut app = app(cx);
    let p = &app.session.project;
    let (a, b) = (
        p.tracks[0].regions[0].clone(),
        p.tracks[1].regions[0].clone(),
    );
    app.ui.edit.focus = Focus::Tracks;
    app.ui.edit.regions = [a.id, b.id].into();
    editing::copy(&mut app.session, &mut app.ui.edit);

    let end = app.session.project.end();
    app.ui.edit.track = Some(app.session.project.tracks[0].id);
    let at = app.session.project.tick_to_sec(end);
    set_playhead(&mut app, at);
    editing::paste(&mut app.session, &mut app.ui.edit);

    let p = &app.session.project;
    let bar = p.ticks_per_bar();
    let at = end / bar * bar;
    let first = a.start.min(b.start);
    let copy_a = p.tracks[0]
        .regions
        .iter()
        .find(|r| r.start == at + a.start - first);
    let copy_b = p.tracks[1]
        .regions
        .iter()
        .find(|r| r.start == at + b.start - first);
    let (copy_a, copy_b) = (
        copy_a.expect("copy on track 1"),
        copy_b.expect("copy on track 2"),
    );
    assert_ne!(copy_a.id, a.id);
    assert_eq!(copy_a.notes.len(), a.notes.len());
    assert_eq!(copy_b.notes.len(), b.notes.len());
    assert_eq!(app.ui.edit.regions, [copy_a.id, copy_b.id].into());
}

/// A real window with the menu bindings, focused, with two tracks.
fn window(cx: &mut TestAppContext) -> (gpui::Entity<SurfApp>, &mut gpui::VisualTestContext) {
    cx.update(crate::menu::init);
    let (view, vcx) = cx.add_window_view(|window, cx| {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let mut song = Song::demo();
        for n in song.notes.iter_mut().filter(|n| n.key < 55) {
            n.channel = 1;
        }
        let mut session = Session::new();
        session.set_project(Project::from_song(&song));
        let mut app = SurfApp::with_session(session, focus);
        app.ui.edit.reconcile(&app.session.project);
        app
    });
    vcx.run_until_parked();
    (view, vcx)
}

/// A shortcut runs its menu action once: ⌘N-style chords and bare letters
/// alike go through the action, not also through the old key handler.
#[gpui::test]
fn shortcuts_run_their_menu_action_once(cx: &mut TestAppContext) {
    let (view, vcx) = window(cx);
    let tracks = |vcx: &mut gpui::VisualTestContext| {
        view.read_with(vcx, |a, _| a.session.project.tracks.len())
    };
    let n = tracks(vcx);
    vcx.simulate_keystrokes("secondary-alt-n");
    assert_eq!(tracks(vcx), n + 1, "New Track once");
    vcx.simulate_keystrokes("secondary-z");
    assert_eq!(tracks(vcx), n, "one undo undoes it");
    vcx.simulate_keystrokes("secondary-shift-z");
    assert_eq!(tracks(vcx), n + 1);

    let shown = view.read_with(vcx, |a, _| a.ui.show_inspector);
    vcx.simulate_keystrokes("i");
    assert_eq!(view.read_with(vcx, |a, _| a.ui.show_inspector), !shown);
}

/// With Musical Typing on, letters that are shortcuts play notes instead.
#[gpui::test]
fn musical_typing_letters_are_notes_not_shortcuts(cx: &mut TestAppContext) {
    let (view, vcx) = window(cx);
    vcx.simulate_keystrokes("secondary-k");
    assert!(view.read_with(vcx, |a, _| a.ui.musical_typing));
    let editor = view.read_with(vcx, |a, _| a.ui.show_editor);
    // E is D#3 in Musical Typing, and Toggle Editor otherwise.
    vcx.simulate_keystrokes("e");
    view.read_with(vcx, |a, _| {
        assert_eq!(a.ui.show_editor, editor, "E did not toggle the editor");
        assert!(a.ui.typing_keys.contains_key("e"), "E is a held note");
    });
    // ⌘ chords still work while typing.
    vcx.simulate_keystrokes("secondary-k");
    assert!(!view.read_with(vcx, |a, _| a.ui.musical_typing));
    vcx.simulate_keystrokes("e");
    assert_eq!(view.read_with(vcx, |a, _| a.ui.show_editor), !editor);
}

fn key(key: &str, shift: bool) -> gpui::Keystroke {
    gpui::Keystroke {
        modifiers: Modifiers {
            shift,
            ..Default::default()
        },
        key: key.into(),
        key_char: None,
    }
}

fn press(vcx: &mut gpui::VisualTestContext, k: &str, shift: bool) {
    vcx.simulate_event(gpui::KeyDownEvent {
        keystroke: key(k, shift),
        is_held: false,
    });
}

fn release(vcx: &mut gpui::VisualTestContext, k: &str) {
    vcx.simulate_event(gpui::KeyUpEvent {
        keystroke: key(k, false),
    });
}

/// Review M20: Shift is a modifier (Shift-click, ⇧⌘S), never the sustain pedal.
#[gpui::test]
fn shift_is_not_the_sustain_pedal(cx: &mut TestAppContext) {
    let (view, vcx) = window(cx);
    press(vcx, "shift", true);
    assert!(!view.read_with(vcx, |a, _| a.session.sustain), "Shift down");
    // A pedal held on a keyboard is not lifted by letting go of Shift.
    view.update(vcx, |a, _| {
        a.session.handle_live(
            surf_midi::LiveEvent::Sustain(true),
            std::time::Instant::now(),
            true,
        )
    });
    release(vcx, "shift");
    assert!(
        view.read_with(vcx, |a, _| a.session.sustain),
        "hardware pedal kept"
    );
}

/// Musical Typing's pedal is Tab: down while held, and lifting it lifts only the
/// pedal it put down.
#[gpui::test]
fn tab_is_the_musical_typing_pedal(cx: &mut TestAppContext) {
    let (view, vcx) = window(cx);
    vcx.simulate_keystrokes("secondary-k");
    press(vcx, "tab", false);
    assert!(view.read_with(vcx, |a, _| a.session.sustain), "Tab down");
    release(vcx, "tab");
    assert!(!view.read_with(vcx, |a, _| a.session.sustain), "Tab up");
    // A hardware pedal already down: Tab neither takes it over nor lifts it.
    view.update(vcx, |a, _| {
        a.session.handle_live(
            surf_midi::LiveEvent::Sustain(true),
            std::time::Instant::now(),
            true,
        )
    });
    press(vcx, "tab", false);
    release(vcx, "tab");
    assert!(
        view.read_with(vcx, |a, _| a.session.sustain),
        "hardware pedal kept"
    );
    // Tab first, then the hardware pedal: letting go of Tab keeps the pedal.
    let hw = |vcx: &mut gpui::VisualTestContext, down: bool| {
        view.update(vcx, |a, _| {
            a.session.handle_live(
                surf_midi::LiveEvent::Sustain(down),
                std::time::Instant::now(),
                true,
            )
        })
    };
    hw(vcx, false);
    press(vcx, "tab", false);
    hw(vcx, true);
    release(vcx, "tab");
    assert!(
        view.read_with(vcx, |a, _| a.session.sustain),
        "pedal still held"
    );
    hw(vcx, false);
    assert!(!view.read_with(vcx, |a, _| a.session.sustain), "both up");
}

/// Review M21: with Musical Typing on (recording turns it on), R still stops
/// recording and the transport keys still work.
#[gpui::test]
fn transport_keys_work_while_typing(cx: &mut TestAppContext) {
    let (view, vcx) = window(cx);
    vcx.simulate_keystrokes("secondary-k");
    assert!(view.read_with(vcx, |a, _| a.ui.musical_typing));
    vcx.simulate_keystrokes("r");
    assert!(
        view.read_with(vcx, |a, _| a.session.is_recording()),
        "R starts"
    );
    vcx.simulate_keystrokes("r");
    assert!(
        !view.read_with(vcx, |a, _| a.session.is_recording()),
        "R stops"
    );
    // Other letters stay notes: M plays nothing, mutes nothing.
    let muted = view.read_with(vcx, |a, _| a.session.project.tracks[0].mute);
    vcx.simulate_keystrokes("m");
    assert_eq!(
        view.read_with(vcx, |a, _| a.session.project.tracks[0].mute),
        muted
    );
}

/// The in-window menu bar is built from the same menus macOS gets, and its
/// items carry the shortcut from the keymap.
#[gpui::test]
fn the_in_window_menu_bar_mirrors_the_native_one(cx: &mut TestAppContext) {
    let (view, vcx) = window(cx);
    let menus = view.read_with(vcx, |a, _| a.ui.menus.clone());
    let names: Vec<String> = menus.iter().map(|m| m.name.to_string()).collect();
    assert_eq!(
        names,
        vec![
            "Surf", "File", "Edit", "Track", "Navigate", "View", "Window"
        ]
    );

    // Every command in the menus has a handler, so none of them is dead.
    let actions: Vec<Box<dyn gpui::Action>> = menus
        .iter()
        .flat_map(|m| &m.items)
        .filter_map(|item| match item {
            gpui::OwnedMenuItem::Action { action, .. } => Some(action.boxed_clone()),
            _ => None,
        })
        .collect();
    assert!(actions.len() > 30);
    for action in &actions {
        assert!(
            vcx.update(|window, cx| window.is_action_available(action.as_ref(), cx)),
            "{} has no handler",
            action.name()
        );
    }

    // Items show the shortcut the keymap holds, and nothing when an item
    // has none (not the action's name, which GPUI falls back to).
    let mut shortcut = |action: &dyn gpui::Action| {
        vcx.update(|window, _| crate::views::menu_bar::shortcut(action, window))
    };
    let save = shortcut(&crate::menu::SaveProject);
    assert!(
        save.to_lowercase().ends_with('s'),
        "Save shortcut: {save:?}"
    );
    assert_eq!(shortcut(&crate::menu::OpenEventList), "");

    // The bar draws, with a menu open, and Escape closes it.
    view.update(vcx, |app, cx| {
        app.ui.menu_open = Some(1);
        cx.notify();
    });
    vcx.run_until_parked();
    vcx.simulate_keystrokes("escape");
    assert_eq!(view.read_with(vcx, |a, _| a.ui.menu_open), None);
}

/// View items are checked for what's on screen; non-toggles have no check.
#[gpui::test]
fn view_menu_checks_follow_what_is_shown(cx: &mut TestAppContext) {
    use crate::menu::{self, checked};
    use crate::shell::{EditorTab, MainView};
    let (view, vcx) = window(cx);
    let state = |vcx: &mut gpui::VisualTestContext, action: &dyn gpui::Action| {
        view.read_with(vcx, |a, _| checked(action, &a.ui, a.session.engine.playing))
    };
    // Defaults from the design: Inspector open, Library closed, Piano Roll.
    assert_eq!(state(vcx, &menu::ToggleInspector), Some(true));
    assert_eq!(state(vcx, &menu::ToggleLibrary), Some(false));
    assert_eq!(state(vcx, &menu::OpenPianoRoll), Some(true));
    assert_eq!(state(vcx, &menu::OpenMixer), Some(false));
    assert_eq!(state(vcx, &menu::ViewTracks), Some(true));
    assert_eq!(state(vcx, &menu::SaveProject), None, "not a toggle");

    // Toggling through the shortcut flips the check.
    vcx.simulate_keystrokes("y");
    assert_eq!(state(vcx, &menu::ToggleLibrary), Some(true));
    // Opening the Mixer moves the tab check off Piano Roll.
    view.update(vcx, |a, _| a.open_tab(EditorTab::Mixer, false));
    assert_eq!(state(vcx, &menu::OpenMixer), Some(true));
    assert_eq!(state(vcx, &menu::OpenPianoRoll), Some(false));
    // A closed editor checks no tab.
    view.update(vcx, |a, _| a.ui.show_editor = false);
    assert_eq!(state(vcx, &menu::OpenMixer), Some(false));
    view.update(vcx, |a, _| a.ui.main = MainView::Perform);
    assert_eq!(state(vcx, &menu::ViewPerform), Some(true));
    assert_eq!(state(vcx, &menu::ViewTracks), Some(false));
}

/// Review: dragging a note during playback must not audition it. The
/// audition shares the playing note's voice, so its note-off on release
/// would cut that note. Stopped, the drag still auditions.
#[gpui::test]
fn dragging_a_note_auditions_only_when_stopped(cx: &mut TestAppContext) {
    let drag_once = |app: &mut SurfApp| {
        let n = first_note(app);
        let start = roll_pos(app, n.start, n.key, 6.0);
        piano_roll::pointer_down(app, &down(start, Modifiers::default(), 1));
        let heard = app.ui.mouse_key;
        piano_roll::pointer_move(
            app,
            &drag(point(start.x, start.y - px(2.0 * sz::ROLL_ROW_H))),
        );
        let heard_moving = app.ui.mouse_key;
        piano_roll::pointer_up(app);
        (heard, heard_moving)
    };

    let mut stopped = app(cx);
    let (down_key, move_key) = drag_once(&mut stopped);
    assert!(
        down_key.is_some() && move_key.is_some(),
        "stopped: auditioned"
    );

    let mut playing = app(cx);
    playing.session.engine.playing = true;
    let (down_key, move_key) = drag_once(&mut playing);
    assert_eq!((down_key, move_key), (None, None), "playing: no audition");
    assert_eq!(playing.session.history.undo_label(), Some("Move Notes"));
}
