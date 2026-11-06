//! Tracks area: track headers + timeline of regions.
//!
//! Pointer tool: click selects a region (shift adds), drag moves it (across
//! tracks too, ⌥ copies), drag its right edge to resize, drag empty space for
//! a marquee. Pencil draws an empty one-bar region, Scissors cuts at the
//! snap grid, Eraser deletes. The ruler seeks; double-click opens a region in
//! the Piano Roll; ⌘ + wheel zooms.

use gpui::{
    Bounds, Context, FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, ScrollWheelEvent, SharedString, Styled, TextRun,
    Window, canvas, div, fill, font as gfont, point, prelude::*, px, size as gsize, svg,
};

use surf_core::edit::{Edit, TrackField};
use surf_core::project::{Id, Project};

use crate::editing::{self, Focus, Snap, Tool};
use crate::icons;
use crate::shell::{Drag, EditorTab, MainView, SurfApp, on_click, pane_header};
use crate::theme::{color, font, radius, size};
use crate::views::{self, mix, with_alpha};
use crate::widgets::{Tone, label, msr, segment, segmented};

/// Width of the region edge that resizes instead of moving.
const EDGE: f32 = 6.0;
/// Pixels the pointer must travel before a click becomes a drag.
const DRAG_SLOP: f32 = 3.0;

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let ui = &app.ui;
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .child(
            pane_header()
                .child(tool_picker(app, cx))
                .child(label("Snap"))
                .child(snap_picker(app, cx))
                .child(div().flex_1())
                .child(view_switch(ui.main, cx))
                .child(label("Zoom"))
                .child(zoom_buttons(cx)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h(px(0.0))
                .child(headers(app, cx))
                .child(timeline(app, cx)),
        )
}

fn tool_picker(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let current = app.ui.edit.tracks_tool;
    segmented().children(Tool::TRACKS.iter().map(|t| {
        let t = *t;
        on_click(
            segment(t.label(), current == t).id(SharedString::from(format!("tt-{}", t.label()))),
            cx,
            move |a| a.ui.edit.tracks_tool = t,
        )
    }))
}

fn snap_picker(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let current = app.ui.edit.tracks_snap;
    segmented().children(Snap::TRACKS.iter().map(|sn| {
        let sn = *sn;
        on_click(
            segment(sn.label(), current == sn).id(SharedString::from(format!("ts-{}", sn.label()))),
            cx,
            move |a| a.ui.edit.tracks_snap = sn,
        )
    }))
}

pub fn view_switch(main: MainView, cx: &mut Context<SurfApp>) -> impl IntoElement {
    segmented()
        .child(on_click(
            segment("Tracks", main == MainView::Tracks).id("view-tracks"),
            cx,
            |a| a.ui.main = MainView::Tracks,
        ))
        .child(on_click(
            segment("Perform", main == MainView::Perform).id("view-perform"),
            cx,
            |a| a.ui.main = MainView::Perform,
        ))
}

fn zoom_buttons(cx: &mut Context<SurfApp>) -> impl IntoElement {
    segmented()
        .child(on_click(segment("−", false).id("zoom-out"), cx, |a| {
            a.ui.tracks_beat_w = (a.ui.tracks_beat_w / 1.25).max(size::TRACKS_BEAT_W_MIN)
        }))
        .child(on_click(segment("+", false).id("zoom-in"), cx, |a| {
            a.ui.tracks_beat_w = (a.ui.tracks_beat_w * 1.25).min(size::TRACKS_BEAT_W_MAX)
        }))
}

fn headers(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let p = &app.session.project;
    let selected = app.ui.edit.track;
    let armed = app.session.record_track();
    let recording = app.session.is_recording();
    let rows: Vec<gpui::AnyElement> = p
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let id = t.id;
            let tc = views::track_color(t);
            let on = selected == Some(id);
            let audible = p.audible(t);
            div()
                .id(SharedString::from(format!("th-{}", id.0)))
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(6.0))
                .h(px(size::LANE_H))
                .pr(px(8.0))
                .bg(if on { color::HEAD_SEL } else { color::HEAD })
                .hover(|d| d.bg(color::HOVER))
                .border_b_1()
                .border_l_3()
                .border_color(tc)
                .opacity(if audible { 1.0 } else { 0.55 })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
                    this.ui.edit.select_track(id);
                    this.ui.edit.focus = Focus::Tracks;
                    cx.notify();
                }))
                .child(
                    div()
                        .w(px(17.0))
                        .text_right()
                        .font_family(font::MONO)
                        .text_size(px(10.0))
                        .text_color(color::TEXT_TERTIARY)
                        .child((i + 1).to_string()),
                )
                .child(
                    div()
                        .size(px(24.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(radius::SM + 1.0))
                        .border_1()
                        .border_color(tc)
                        .child(
                            svg()
                                .path(icons::path("piano"))
                                .size(px(14.0))
                                .text_color(tc),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .flex_1()
                        .min_w(px(0.0))
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::MEDIUM)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(t.name.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(color::TEXT_TERTIARY)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(format!(
                                    "{} · Ch {}",
                                    views::instrument_name(&app.session),
                                    t.channel + 1
                                )),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(2.0))
                        .child(track_toggle(cx, id, "M", Tone::Mute, t.mute, |t| {
                            TrackField::Mute(!t.mute)
                        }))
                        .child(track_toggle(cx, id, "S", Tone::Solo, t.solo, |t| {
                            TrackField::Solo(!t.solo)
                        }))
                        .child(
                            msr(
                                "R",
                                Tone::Record,
                                armed == Some(id)
                                    && (recording || app.session.record_arm == Some(id)),
                            )
                            .id(SharedString::from(format!("arm-{}", id.0)))
                            .on_click(cx.listener(
                                move |this, _: &gpui::ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    let s = &mut this.session;
                                    s.record_arm = if s.record_arm == Some(id) {
                                        None
                                    } else {
                                        Some(id)
                                    };
                                    cx.notify();
                                },
                            )),
                        ),
                )
                .into_any_element()
        })
        .collect();

    div()
        .id("track-headers")
        .flex()
        .flex_col()
        .flex_none()
        .w(px(size::TRACK_HEADER_W))
        .bg(color::HEAD)
        .border_r_1()
        .border_color(color::LINE)
        .overflow_y_scroll()
        .child(
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(6.0))
                .h(px(size::RULER_TRACKS_H))
                .px(px(6.0))
                .bg(color::CHROME)
                .border_b_1()
                .border_color(color::LINE)
                .child(on_click(
                    div()
                        .id("new-track")
                        .size(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(radius::SM))
                        .border_1()
                        .border_color(color::LINE_STRONG)
                        .hover(|d| d.bg(color::HOVER))
                        .cursor_pointer()
                        .child(
                            svg()
                                .path(icons::path("plus"))
                                .size(px(12.0))
                                .text_color(color::TEXT_SECONDARY),
                        ),
                    cx,
                    |a| editing::new_track(&mut a.session, &mut a.ui.edit),
                ))
                .child(label("Tracks"))
                .child(div().flex_1())
                .child(label("Global ▸").opacity(0.4)),
        )
        .children(rows)
}

fn track_toggle(
    cx: &mut Context<SurfApp>,
    id: Id,
    letter: &'static str,
    tone: Tone,
    on: bool,
    field: fn(&surf_core::project::Track) -> TrackField,
) -> impl IntoElement {
    msr(letter, tone, on)
        .id(SharedString::from(format!("{letter}-{}", id.0)))
        .on_click(cx.listener(move |this, _: &gpui::ClickEvent, _, cx| {
            cx.stop_propagation();
            if let Some(t) = this.session.project.track(id) {
                let f = field(t);
                editing::set_track(&mut this.session, id, f);
            }
            cx.notify();
        }))
}

// ---- timeline geometry ----------------------------------------------------

/// Mapping between pointer positions and ticks / tracks.
#[derive(Clone, Copy)]
struct Geom {
    origin: Point<Pixels>,
    tick_w: f32,
}

impl Geom {
    fn of(app: &SurfApp) -> Self {
        let p = &app.session.project;
        Self {
            origin: app.ui.tracks_bounds.get().origin,
            tick_w: app.ui.tracks_beat_w / p.ticks_per_beat() as f32,
        }
    }
    fn local(&self, pos: Point<Pixels>) -> (f32, f32) {
        (
            f32::from(pos.x - self.origin.x),
            f32::from(pos.y - self.origin.y),
        )
    }
    fn tick_at(&self, x: f32) -> u64 {
        (x / self.tick_w).max(0.0) as u64
    }
    fn track_at(&self, y: f32) -> Option<usize> {
        let y = y - size::RULER_TRACKS_H;
        (y >= 0.0).then(|| (y / size::LANE_H) as usize)
    }
}

enum Hit {
    Ruler,
    Region { id: Id, edge: bool },
    Lane(Id),
    Nothing,
}

fn hit(p: &Project, g: &Geom, x: f32, y: f32) -> Hit {
    if y < size::RULER_TRACKS_H {
        return Hit::Ruler;
    }
    let Some(ti) = g.track_at(y) else {
        return Hit::Nothing;
    };
    let Some(t) = p.tracks.get(ti) else {
        return Hit::Nothing;
    };
    let tick = x / g.tick_w;
    for r in t.regions.iter().rev() {
        let (a, b) = (r.start as f32, r.end() as f32);
        if tick >= a && tick <= b {
            let edge = (b - tick) * g.tick_w < EDGE && (b - a) * g.tick_w > EDGE * 3.0;
            return Hit::Region { id: r.id, edge };
        }
    }
    Hit::Lane(t.id)
}

fn snap(tick: f64, grid: u64) -> i64 {
    ((tick / grid as f64).round() * grid as f64) as i64
}

// ---- timeline -------------------------------------------------------------

fn timeline(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let s = &app.session;
    let p = s.project.clone();
    let tick_w = app.ui.tracks_beat_w / p.ticks_per_beat() as f32;
    let per_bar = p.time_signature.0.max(1) as f64;
    let beat_w = app.ui.tracks_beat_w;
    let playhead = p.sec_to_tick(s.position()) as f32;
    let looping = s.loop_playback;
    let recording = s.is_recording();
    let record_track = s.record_track();
    let selected_regions = app.ui.edit.regions.clone();
    let edit_region = app.ui.edit.edit_region;
    let selected_track = app.ui.edit.track;
    let marquee = app.ui.marquee;
    let bounds_cell = app.ui.tracks_bounds.clone();

    let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut Window, cx: &mut gpui::App| {
        bounds_cell.set(bounds);
        let o = bounds.origin;
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        let ruler = size::RULER_TRACKS_H;
        let x_of = |tick: f32| o.x + px(tick * tick_w);

        window.paint_quad(fill(bounds, color::LANE));
        for (i, t) in p.tracks.iter().enumerate() {
            let y = o.y + px(ruler + i as f32 * size::LANE_H);
            if selected_track == Some(t.id) {
                window.paint_quad(fill(
                    Bounds::new(point(o.x, y), gsize(px(w), px(size::LANE_H))),
                    color::LANE_SEL,
                ));
            }
            window.paint_quad(fill(
                Bounds::new(
                    point(o.x, y + px(size::LANE_H - 1.0)),
                    gsize(px(w), px(1.0)),
                ),
                color::LINE_ROW,
            ));
        }
        // Grid: bars brighter than beats; beats hidden when zoomed far out.
        let total_beats = (w / beat_w).ceil() as i64 + 1;
        let step = if beat_w < 8.0 { per_bar as i64 } else { 1 };
        let mut b = 0;
        while b <= total_beats {
            let bar = b as f64 % per_bar == 0.0;
            window.paint_quad(fill(
                Bounds::new(
                    point(o.x + px(b as f32 * beat_w), o.y + px(ruler)),
                    gsize(px(1.0), px(h - ruler)),
                ),
                if bar {
                    color::GRID_BAR
                } else {
                    color::GRID_BEAT
                },
            ));
            b += step;
        }
        let end_x = x_of(p.end() as f32);
        if end_x < o.x + px(w) {
            window.paint_quad(fill(
                Bounds::new(
                    point(end_x, o.y + px(ruler)),
                    gsize(o.x + px(w) - end_x, px(h - ruler)),
                ),
                color::GRID_OUTSIDE,
            ));
        }
        // Regions
        for (i, t) in p.tracks.iter().enumerate() {
            let tc = views::track_color(t);
            let audible = p.audible(t);
            let ry = o.y + px(ruler + i as f32 * size::LANE_H + 2.0);
            let rh = size::LANE_H - 5.0;
            for r in &t.regions {
                let x = x_of(r.start as f32);
                let rw = (r.len as f32 * tick_w - 1.0).max(4.0);
                let sel = selected_regions.contains(&r.id);
                let alpha = if audible { 1.0 } else { 0.45 };
                window.paint_quad(
                    fill(
                        Bounds::new(point(x + px(0.5), ry), gsize(px(rw), px(rh))),
                        with_alpha(tc, if sel { 0.3 } else { 0.15 } * alpha),
                    )
                    .corner_radii(px(radius::SM))
                    .border_widths(px(if sel { 1.5 } else { 1.0 }))
                    .border_color(with_alpha(tc, if sel { 1.0 } else { 0.5 } * alpha)),
                );
                window.with_content_mask(
                    Some(gpui::ContentMask {
                        bounds: Bounds::new(point(x, ry), gsize(px(rw), px(rh))),
                    }),
                    |window| {
                        let marker = if edit_region == Some(r.id) {
                            " ●"
                        } else {
                            ""
                        };
                        text(
                            window,
                            cx,
                            &format!("{}{marker}", r.name.to_uppercase()),
                            x + px(6.0),
                            ry + px(2.0),
                            10.5,
                            with_alpha(tc, alpha),
                            font::LABEL,
                            true,
                        );
                        let (lo, hi) = r
                            .notes
                            .iter()
                            .fold((127u8, 0u8), |(lo, hi), n| (lo.min(n.key), hi.max(n.key)));
                        let span = (hi.saturating_sub(lo) as f32 + 2.0).max(1.0);
                        let nh = ((rh - 20.0) / span).clamp(1.5, 3.0);
                        for n in r.notes.iter().filter(|n| n.start < r.len) {
                            let nx = x_of((r.start + n.start) as f32);
                            let ny = ry
                                + px(16.0 + (hi as f32 + 1.0 - n.key as f32) / span * (rh - 20.0));
                            window.paint_quad(fill(
                                Bounds::new(
                                    point(nx, ny),
                                    gsize(px((n.len as f32 * tick_w - 1.0).max(1.5)), px(nh)),
                                ),
                                with_alpha(tc, if sel { 1.0 } else { 0.8 } * alpha),
                            ));
                        }
                    },
                );
            }
            if recording && record_track == Some(t.id) {
                let rw = (playhead * tick_w).max(2.0);
                window.paint_quad(
                    fill(
                        Bounds::new(point(o.x, ry), gsize(px(rw), px(rh))),
                        with_alpha(color::RECORD, 0.4),
                    )
                    .corner_radii(px(radius::SM))
                    .border_widths(px(1.0))
                    .border_color(color::RECORD),
                );
            }
        }
        if let Some((a, b)) = marquee {
            let tl = point(a.x.min(b.x), a.y.min(b.y));
            let br = point(a.x.max(b.x), a.y.max(b.y));
            window.paint_quad(
                fill(Bounds::from_corners(tl, br), color::GRID_MARQUEE)
                    .border_widths(px(1.0))
                    .border_color(with_alpha(color::ACCENT_FG, 0.7)),
            );
        }
        ruler_paint(
            window,
            cx,
            o,
            w,
            ruler,
            beat_w,
            per_bar,
            looping.then(|| p.end() as f64 / p.ticks_per_beat() as f64),
        );
        let phx = x_of(playhead);
        window.paint_quad(fill(
            Bounds::new(point(phx, o.y), gsize(px(1.0), px(h))),
            color::ACCENT_FG,
        ));
    };

    div()
        .id("tracks-timeline")
        .relative()
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .overflow_hidden()
        .cursor(cursor_for(app))
        .child(canvas(|_, _, _| {}, paint).size_full())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                pointer_down(this, e);
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(|this, e: &MouseMoveEvent, _, cx| {
            if pointer_move(this, e) {
                cx.notify();
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, e: &MouseUpEvent, _, cx| {
                pointer_up(this, e.position);
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, window, cx| {
            if e.modifiers.secondary() {
                let dy = f32::from(e.delta.pixel_delta(window.line_height()).y);
                let factor = if dy > 0.0 { 1.1 } else { 1.0 / 1.1 };
                this.ui.tracks_beat_w = (this.ui.tracks_beat_w * factor)
                    .clamp(size::TRACKS_BEAT_W_MIN, size::TRACKS_BEAT_W_MAX);
                cx.notify();
            }
        }))
}

fn cursor_for(app: &SurfApp) -> gpui::CursorStyle {
    use gpui::CursorStyle;
    match (&app.ui.drag, app.ui.edit.tracks_tool) {
        (Some(Drag::ResizeRegion { .. }), _) => CursorStyle::ResizeLeftRight,
        (Some(Drag::MoveRegions { .. }), _) => CursorStyle::ClosedHand,
        (_, Tool::Pencil | Tool::Scissors) => CursorStyle::Crosshair,
        (_, Tool::Eraser) => CursorStyle::OperationNotAllowed,
        _ => CursorStyle::Arrow,
    }
}

pub(crate) fn pointer_down(app: &mut SurfApp, e: &MouseDownEvent) {
    app.ui.edit.focus = Focus::Tracks;
    let g = Geom::of(app);
    let (x, y) = g.local(e.position);
    let p = &app.session.project;
    let grid = app.ui.edit.tracks_snap.ticks(p);
    match hit(p, &g, x, y) {
        Hit::Ruler => {
            app.session.seek(p.tick_to_sec(g.tick_at(x)));
            app.ui.drag = Some(Drag::Scrub { tracks: true });
        }
        Hit::Region { id, edge } => {
            let tool = app.ui.edit.tracks_tool;
            match tool {
                Tool::Eraser => {
                    app.session
                        .apply("Delete Region", Edit::RemoveRegion { region: id });
                    app.ui.edit.regions.remove(&id);
                }
                Tool::Scissors => {
                    let at = snap(g.tick_at(x) as f64, grid).max(0) as u64;
                    editing::split_region_at(&mut app.session, &mut app.ui.edit, id, at);
                }
                Tool::Pointer | Tool::Pencil => {
                    if e.click_count == 2 {
                        app.ui.edit.select_region(p, id, false);
                        app.open_tab(EditorTab::PianoRoll, false);
                        app.center_roll_on_song();
                        return;
                    }
                    let add = e.modifiers.shift;
                    if add || !app.ui.edit.regions.contains(&id) {
                        app.ui.edit.select_region(p, id, add);
                    } else {
                        app.ui.edit.edit_region = Some(id);
                    }
                    let lead_tick = p.find_region(id).map_or(0, |(_, r)| r.start);
                    app.ui.drag = Some(if edge {
                        Drag::ResizeRegion {
                            x0: x,
                            region: id,
                            len0: p.find_region(id).map_or(0, |(_, r)| r.len),
                        }
                    } else {
                        Drag::MoveRegions {
                            x0: x,
                            y0: y,
                            lead_tick,
                            copy: e.modifiers.alt,
                            regions: app.ui.edit.regions.iter().copied().collect(),
                        }
                    });
                }
            }
        }
        Hit::Lane(track) => {
            app.ui.edit.select_track(track);
            if app.ui.edit.tracks_tool == Tool::Pencil {
                editing::create_region(&mut app.session, &mut app.ui.edit, track, g.tick_at(x));
            } else {
                if !e.modifiers.shift {
                    app.ui.edit.regions.clear();
                }
                app.ui.marquee = Some((e.position, e.position));
                app.ui.drag = Some(Drag::Marquee { tracks: true });
            }
        }
        Hit::Nothing => {
            app.ui.edit.regions.clear();
            app.ui.marquee = Some((e.position, e.position));
            app.ui.drag = Some(Drag::Marquee { tracks: true });
        }
    }
}

/// Returns whether anything changed.
pub(crate) fn pointer_move(app: &mut SurfApp, e: &MouseMoveEvent) -> bool {
    if e.pressed_button != Some(MouseButton::Left) {
        if app.ui.drag.is_some() {
            pointer_up(app, e.position);
            return true;
        }
        return false;
    }
    let g = Geom::of(app);
    let (x, y) = g.local(e.position);
    let grid = app.ui.edit.tracks_snap.ticks(&app.session.project);
    let Some(drag) = app.ui.drag.clone() else {
        return false;
    };
    match drag {
        Drag::Scrub { tracks: true } => {
            let t = app.session.project.tick_to_sec(g.tick_at(x));
            app.session.seek(t);
        }
        Drag::Marquee { tracks: true } => {
            let Some((a, _)) = app.ui.marquee else {
                return false;
            };
            app.ui.marquee = Some((a, e.position));
            let (ax, ay) = g.local(a);
            let (t0, t1) = (g.tick_at(ax.min(x)), g.tick_at(ax.max(x)));
            let (y0, y1) = (ay.min(y), ay.max(y));
            let p = &app.session.project;
            let mut picked = Vec::new();
            for (i, t) in p.tracks.iter().enumerate() {
                let top = size::RULER_TRACKS_H + i as f32 * size::LANE_H;
                if top + size::LANE_H < y0 || top > y1 {
                    continue;
                }
                picked.extend(
                    t.regions
                        .iter()
                        .filter(|r| r.end() > t0 && r.start < t1)
                        .map(|r| r.id),
                );
            }
            app.ui.edit.regions = picked.into_iter().collect();
        }
        Drag::MoveRegions {
            x0,
            y0,
            lead_tick,
            copy,
            regions,
        } => {
            if (x - x0).abs() < DRAG_SLOP && (y - y0).abs() < DRAG_SLOP {
                return false;
            }
            let target = snap(lead_tick as f64 + ((x - x0) / g.tick_w) as f64, grid);
            let ticks = target - lead_tick as i64;
            let tracks = ((y - y0) / size::LANE_H).round() as i32;
            let edit = if copy {
                copy_regions(&mut app.session.project, &regions, ticks, tracks)
            } else {
                Edit::MoveRegions {
                    regions,
                    ticks,
                    tracks,
                }
            };
            app.session
                .preview(if copy { "Copy Region" } else { "Move Region" }, edit);
        }
        Drag::ResizeRegion { x0, region, len0 } => {
            let delta = ((x - x0) / g.tick_w) as f64;
            let len = snap(len0 as f64 + delta, grid).max(grid as i64) as u64;
            app.session
                .preview("Resize Region", Edit::ResizeRegion { region, len });
        }
        _ => return false,
    }
    true
}

pub(crate) fn pointer_up(app: &mut SurfApp, _pos: Point<Pixels>) {
    match app.ui.drag.take() {
        Some(Drag::MoveRegions { .. } | Drag::ResizeRegion { .. }) => {
            app.session.commit();
            if let Some(label) = app.session.history.undo_label() {
                app.session.status = format!("{label} · ⌘Z to undo");
            }
        }
        Some(Drag::Marquee { tracks: true }) => {
            let n = app.ui.edit.regions.len();
            if n > 0 {
                app.session.status =
                    format!("{n} region{} selected", if n == 1 { "" } else { "s" });
            }
        }
        _ => {}
    }
    app.ui.marquee = None;
}

/// ⌥-drag: duplicates of `regions` placed at the offset, with fresh ids.
/// Allocating ids on each preview is fine: only the committed preview's
/// regions end up in the project.
fn copy_regions(p: &mut Project, regions: &[Id], ticks: i64, tracks: i32) -> Edit {
    let mut edits = Vec::new();
    let n_tracks = p.tracks.len() as i32;
    for id in regions {
        let Some((ti, ri)) = p.tracks.iter().enumerate().find_map(|(ti, t)| {
            t.regions
                .iter()
                .position(|r| r.id == *id)
                .map(|ri| (ti, ri))
        }) else {
            continue;
        };
        let src = p.tracks[ti].regions[ri].clone();
        let target = (ti as i32 + tracks).clamp(0, n_tracks - 1) as usize;
        let mut copy = src.clone();
        copy.id = p.alloc();
        copy.start = (src.start as i64 + ticks).max(0) as u64;
        for n in &mut copy.notes {
            n.id = p.alloc();
        }
        let track = p.tracks[target].id;
        let index = p.tracks[target].regions.len();
        edits.push(Edit::InsertRegion {
            track,
            index,
            region: copy,
        });
    }
    Edit::Batch(edits)
}

/// Ruler shared by the tracks area and the piano roll.
#[allow(clippy::too_many_arguments)]
pub fn ruler_paint(
    window: &mut Window,
    cx: &mut gpui::App,
    o: gpui::Point<Pixels>,
    w: f32,
    h: f32,
    beat_w: f32,
    per_bar: f64,
    cycle_beats: Option<f64>,
) {
    window.paint_quad(fill(Bounds::new(o, gsize(px(w), px(h))), color::CHROME));
    window.paint_quad(fill(
        Bounds::new(point(o.x, o.y + px(h - 1.0)), gsize(px(w), px(1.0))),
        color::LINE,
    ));
    if let Some(end) = cycle_beats {
        window.paint_quad(
            fill(
                Bounds::new(
                    point(o.x, o.y + px(3.0)),
                    gsize(px(end as f32 * beat_w), px(9.0)),
                ),
                color::CYCLE,
            )
            .corner_radii(px(2.0)),
        );
    }
    let bar_w = beat_w * per_bar as f32;
    let label_every = if bar_w < 30.0 {
        4
    } else if bar_w < 50.0 {
        2
    } else {
        1
    };
    let bars = (w / bar_w).ceil() as i64 + 1;
    for bar in 0..=bars {
        let x = o.x + px(bar as f32 * bar_w);
        window.paint_quad(fill(
            Bounds::new(point(x, o.y + px(h - 12.0)), gsize(px(1.0), px(11.0))),
            color::TEXT_DISABLED,
        ));
        if bar % label_every == 0 {
            text(
                window,
                cx,
                &(bar + 1).to_string(),
                x + px(3.0),
                o.y + px(h - 15.0),
                10.0,
                color::TEXT_SECONDARY,
                font::MONO,
                false,
            );
        }
        if beat_w >= 10.0 {
            for q in 1..per_bar as i64 {
                let qx = x + px(q as f32 * beat_w);
                window.paint_quad(fill(
                    Bounds::new(point(qx, o.y + px(h - 5.0)), gsize(px(1.0), px(4.0))),
                    color::LINE_STRONG,
                ));
            }
        }
    }
}

/// Draw one line of text inside a canvas.
#[allow(clippy::too_many_arguments)]
pub fn text(
    window: &mut Window,
    cx: &mut gpui::App,
    s: &str,
    x: Pixels,
    y: Pixels,
    size_px: f32,
    c: gpui::Rgba,
    family: &'static str,
    bold: bool,
) {
    if s.is_empty() {
        return;
    }
    let mut f = gfont(family);
    if bold {
        f.weight = FontWeight::SEMIBOLD;
    }
    let run = TextRun {
        len: s.len(),
        font: f,
        color: c.into(),
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let line = window.text_system().shape_line(
        SharedString::from(s.to_string()),
        px(size_px),
        &[run],
        None,
    );
    let _ = line.paint(point(x, y), px(size_px * 1.2), window, cx);
}

#[allow(dead_code)]
fn darker(c: gpui::Rgba) -> gpui::Rgba {
    mix(
        c,
        gpui::Rgba {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        },
        0.45,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapping_rounds_to_the_nearest_grid_line() {
        assert_eq!(snap(0.0, 480), 0);
        assert_eq!(snap(239.0, 480), 0);
        assert_eq!(snap(241.0, 480), 480);
        assert_eq!(snap(-250.0, 480), -480);
    }
}
