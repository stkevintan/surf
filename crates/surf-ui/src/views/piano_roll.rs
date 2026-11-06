//! Piano Roll: edits the notes of one region.
//!
//! Pointer: click selects a note (shift adds), drag moves it (⌥ copies,
//! snapped to the grid), drag its right edge to change length, drag empty
//! space for a marquee. Pencil adds a note and drags out its length; Eraser
//! deletes. Drag a stem in the velocity lane to change velocity (all
//! selected notes move together). Double-click adds or removes a note.
//! The ruler seeks; click a key to hear it; wheel scrolls, ⌘+wheel zooms.

use std::time::Instant;

use gpui::{
    Bounds, Context, CursorStyle, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, Pixels, Point, ScrollWheelEvent, SharedString, Styled, Window,
    canvas, div, fill, point, prelude::*, px, size as gsize,
};

use surf_core::edit::Edit;
use surf_core::project::{Id, Project};
use surf_midi::LiveEvent;

use crate::editing::{self, Focus, Snap, Tool};
use crate::shell::{Drag, SurfApp, on_click};
use crate::theme::{color, font, radius, size};
use crate::views::{self, is_black, mix, note_name, tracks, velocity_color, with_alpha};
use crate::widgets::{StyleExt, button, label, segment, segmented};

const EDGE: f32 = 6.0;
const DRAG_SLOP: f32 = 2.0;
const WHITE: gpui::Rgba = gpui::Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};
const BLACK: gpui::Rgba = gpui::Rgba {
    r: 0.0,
    g: 0.0,
    b: 0.0,
    a: 1.0,
};

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h(px(0.0))
        .child(local_inspector(app, cx))
        .child(grid(app, cx))
}

/// Bar above the roll: tool and grid.
pub fn bar(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let tool = app.ui.edit.roll_tool;
    let grid = app.ui.edit.roll_snap;
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(label("Tool"))
        .child(segmented().children(Tool::ROLL.iter().map(|t| {
            let t = *t;
            on_click(
                segment(t.label(), tool == t).id(SharedString::from(format!("rt-{}", t.label()))),
                cx,
                move |a| a.ui.edit.roll_tool = t,
            )
        })))
        .child(label("Grid"))
        .child(segmented().children(Snap::ROLL.iter().map(|g| {
            let g = *g;
            on_click(
                segment(g.label(), grid == g).id(SharedString::from(format!("rg-{}", g.label()))),
                cx,
                move |a| a.ui.edit.roll_snap = g,
            )
        })))
}

fn local_inspector(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let p = &app.session.project;
    let e = &app.ui.edit;
    let region = e.edit_region(p);
    let (tc, name) = region
        .map(|(t, r)| (views::track_color(t), format!("{} · {}", t.name, r.name)))
        .unwrap_or((color::TEXT_DISABLED, "No region".to_string()));
    let selected: Vec<u8> = region
        .map(|(_, r)| {
            r.notes
                .iter()
                .filter(|n| e.notes.contains(&n.id))
                .map(|n| n.velocity)
                .collect()
        })
        .unwrap_or_default();
    let velocity = match selected.len() {
        0 => "–".to_string(),
        1 => selected[0].to_string(),
        n => format!(
            "{} avg",
            selected.iter().map(|v| *v as u32).sum::<u32>() / n as u32
        ),
    };
    let strength = e.quantize_strength;
    let grid = e.quantize;

    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(10.0))
        .w(px(size::LOCAL_INSPECTOR_W))
        .p(px(10.0))
        .bg(color::PANEL)
        .border_r_1()
        .border_color(color::LINE)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.0))
                .child(div().flex_none().size(px(10.0)).rounded(px(3.0)).bg(tc))
                .child(
                    div()
                        .text_size(px(12.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(name),
                ),
        )
        .child(
            caption("Time Quantize")
                .child(segmented().children(Snap::ROLL.iter().map(|g| {
                    let g = *g;
                    on_click(
                        segment(g.label(), grid == g)
                            .id(SharedString::from(format!("q-{}", g.label()))),
                        cx,
                        move |a| a.ui.edit.quantize = g,
                    )
                })))
                .child(on_click(
                    button("Quantize (Q)").id("roll-quantize"),
                    cx,
                    |a| {
                        a.ui.edit.focus = Focus::Roll;
                        editing::quantize(&mut a.session, &a.ui.edit)
                    },
                )),
        )
        .child(
            caption(format!("Strength {strength}%")).child(segmented().children(
                [25u8, 50, 75, 100].iter().map(|v| {
                    let v = *v;
                    on_click(
                        segment(format!("{v}"), strength == v)
                            .px(px(7.0))
                            .id(SharedString::from(format!("qs-{v}"))),
                        cx,
                        move |a| a.ui.edit.quantize_strength = v,
                    )
                }),
            )),
        )
        .child(
            caption(format!("Velocity {velocity}")).child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(4.0))
                    .child(vel_step(cx, "−8", -8))
                    .child(vel_step(cx, "−1", -1))
                    .child(vel_step(cx, "+1", 1))
                    .child(vel_step(cx, "+8", 8)),
            ),
        )
        .child(
            caption("Scale Quantize · planned")
                .when_planned(true)
                .child(crate::widgets::field("Off")),
        )
        .child(
            caption("Articulation · planned")
                .when_planned(true)
                .child(crate::widgets::field("None")),
        )
}

fn caption(text: impl Into<SharedString>) -> gpui::Div {
    div().flex().flex_col().gap(px(5.0)).child(
        div()
            .text_size(px(10.5))
            .text_color(color::TEXT_TERTIARY)
            .child(text.into()),
    )
}

fn vel_step(cx: &mut Context<SurfApp>, text: &'static str, delta: i32) -> impl IntoElement {
    on_click(
        button(text)
            .id(SharedString::from(format!("vel{text}")))
            .px(px(6.0)),
        cx,
        move |a| {
            let Some((_, r)) = a.ui.edit.edit_region(&a.session.project) else {
                return;
            };
            let region = r.id;
            let velocities: Vec<(Id, u8)> = r
                .notes
                .iter()
                .filter(|n| a.ui.edit.notes.contains(&n.id))
                .map(|n| (n.id, (n.velocity as i32 + delta).clamp(1, 127) as u8))
                .collect();
            if velocities.is_empty() {
                a.session.status = "Select notes to change their velocity".to_string();
                return;
            }
            a.session
                .apply("Change Velocity", Edit::SetVelocity { region, velocities });
        },
    )
}

// ---- geometry -------------------------------------------------------------

#[derive(Clone, Copy)]
struct Geom {
    origin: Point<Pixels>,
    h: f32,
    tick_w: f32,
    top_key: u8,
    scroll_x: f32,
    region_start: u64,
}

impl Geom {
    fn of(app: &SurfApp) -> Self {
        let b = app.ui.roll_bounds.get();
        let p = &app.session.project;
        Self {
            origin: b.origin,
            h: f32::from(b.size.height),
            tick_w: app.ui.roll_beat_w / p.ticks_per_beat() as f32,
            top_key: app.ui.roll_top_key,
            scroll_x: app.ui.roll_scroll_x,
            region_start: app.ui.edit.edit_region(p).map_or(0, |(_, r)| r.start),
        }
    }
    fn local(&self, pos: Point<Pixels>) -> (f32, f32) {
        (
            f32::from(pos.x - self.origin.x),
            f32::from(pos.y - self.origin.y),
        )
    }
    /// Region-relative tick under local x (may be negative left of the region).
    fn tick_at(&self, x: f32) -> f64 {
        ((x - size::ROLL_KEYS_W + self.scroll_x) / self.tick_w) as f64 - self.region_start as f64
    }
    fn x_of(&self, region_tick: u64) -> f32 {
        size::ROLL_KEYS_W + (self.region_start + region_tick) as f32 * self.tick_w - self.scroll_x
    }
    fn key_at(&self, y: f32) -> i32 {
        self.top_key as i32 - ((y - size::RULER_EDITOR_H) / size::ROLL_ROW_H).floor() as i32
    }
}

enum Area {
    Ruler,
    Keys(u8),
    Grid,
    Velocity,
}

fn area(g: &Geom, x: f32, y: f32) -> Area {
    if y < size::RULER_EDITOR_H {
        Area::Ruler
    } else if y >= g.h - size::ROLL_VELOCITY_H {
        Area::Velocity
    } else if x < size::ROLL_KEYS_W {
        Area::Keys(g.key_at(y).clamp(0, 127) as u8)
    } else {
        Area::Grid
    }
}

/// The note under (x, y) in the grid, and whether it was its right edge.
fn note_at(p: &Project, region: Id, g: &Geom, x: f32, y: f32) -> Option<(Id, bool)> {
    let (_, r) = p.find_region(region)?;
    let key = g.key_at(y);
    r.notes.iter().rev().find_map(|n| {
        let (a, b) = (g.x_of(n.start), g.x_of(n.end()));
        (n.key as i32 == key && x >= a && x <= b)
            .then_some((n.id, b - x < EDGE && b - a > EDGE * 2.0))
    })
}

/// The note whose velocity stem is nearest to x, within a few pixels.
fn stem_at(
    p: &Project,
    region: Id,
    g: &Geom,
    x: f32,
    selected: &std::collections::BTreeSet<Id>,
) -> Option<Id> {
    let (_, r) = p.find_region(region)?;
    r.notes
        .iter()
        .map(|n| (n, (g.x_of(n.start) - x).abs()))
        .filter(|(_, d)| *d < 6.0)
        // Prefer selected notes when stems overlap.
        .min_by(|a, b| {
            (!selected.contains(&a.0.id), a.1)
                .partial_cmp(&(!selected.contains(&b.0.id), b.1))
                .unwrap()
        })
        .map(|(n, _)| n.id)
}

fn snap(tick: f64, grid: u64) -> i64 {
    ((tick / grid as f64).round() * grid as f64) as i64
}

fn floor_snap(tick: f64, grid: u64) -> u64 {
    ((tick.max(0.0) / grid as f64).floor() * grid as f64) as u64
}

// ---- grid -----------------------------------------------------------------

fn grid(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let s = &app.session;
    let p = &s.project;
    let e = &app.ui.edit;
    let beat_w = app.ui.roll_beat_w;
    let tick_w = beat_w / p.ticks_per_beat() as f32;
    let top_key = app.ui.roll_top_key;
    let scroll_x = app.ui.roll_scroll_x;
    let per_bar = p.time_signature.0.max(1) as f64;
    let grid_ticks = e.roll_snap.ticks(p);
    let playhead = p.sec_to_tick(s.position()) as f32;
    let active = s.active_keys();
    let region = e
        .edit_region(p)
        .map(|(t, r)| (views::track_color(t), r.clone()));
    let selected = e.notes.clone();
    let marquee = app
        .ui
        .marquee
        .filter(|_| matches!(app.ui.drag, Some(Drag::Marquee { tracks: false })));
    let hover = app
        .ui
        .pencil_hover
        .filter(|_| e.roll_tool == Tool::Pencil && app.ui.drag.is_none());
    let bounds_cell = app.ui.roll_bounds.clone();

    let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut Window, cx: &mut gpui::App| {
        bounds_cell.set(bounds);
        let o = bounds.origin;
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        let keys_w = size::ROLL_KEYS_W;
        let ruler = size::RULER_EDITOR_H;
        let vel_h = size::ROLL_VELOCITY_H;
        let row = size::ROLL_ROW_H;
        let grid_h = h - ruler - vel_h;
        let gx = o.x + px(keys_w);
        let gy = o.y + px(ruler);
        let region_start = region.as_ref().map_or(0, |(_, r)| r.start);
        let x_abs = |tick: f32| gx + px(tick * tick_w - scroll_x);
        let x_of = |rt: u64| x_abs((region_start + rt) as f32);
        let y_of = |key: u8| gy + px((top_key as f32 - key as f32) * row);
        let rows = (grid_h / row).ceil() as i32 + 1;
        let grid_mask = gpui::ContentMask {
            bounds: Bounds::new(point(gx, gy), gsize(px(w - keys_w), px(grid_h))),
        };

        window.paint_quad(fill(bounds, color::LANE));
        window.with_content_mask(Some(grid_mask.clone()), |window| {
            for i in 0..rows {
                let key = top_key as i32 - i;
                if !(0..=127).contains(&key) {
                    continue;
                }
                let key = key as u8;
                let y = gy + px(i as f32 * row);
                window.paint_quad(fill(
                    Bounds::new(point(gx, y), gsize(px(w), px(row))),
                    if is_black(key) {
                        color::PANEL
                    } else {
                        color::HEAD
                    },
                ));
                if key.is_multiple_of(12) {
                    window.paint_quad(fill(
                        Bounds::new(point(gx, y + px(row - 1.0)), gsize(px(w), px(1.0))),
                        color::GRID_OCTAVE,
                    ));
                }
            }
            // Outside the region
            if let Some((_, r)) = &region {
                let a = x_abs(r.start as f32);
                let b = x_abs(r.end() as f32);
                if a > gx {
                    window.paint_quad(fill(
                        Bounds::new(point(gx, gy), gsize(a - gx, px(grid_h))),
                        color::GRID_OUTSIDE,
                    ));
                }
                window.paint_quad(fill(
                    Bounds::new(point(b, gy), gsize(px(w), px(grid_h))),
                    color::GRID_OUTSIDE,
                ));
            }
            // Grid lines at the snap value
            let first = (scroll_x / tick_w / grid_ticks as f32).floor().max(0.0) as u64;
            let last = ((scroll_x + w) / tick_w / grid_ticks as f32).ceil() as u64;
            let ticks_per_beat = beat_w / tick_w;
            for i in first..=last {
                let t = (i * grid_ticks) as f32;
                let beats = t / ticks_per_beat;
                let c = if (beats as f64 % per_bar).abs() < 1e-4 {
                    color::GRID_BAR
                } else if beats.fract().abs() < 1e-4 {
                    color::GRID_BEAT
                } else {
                    color::GRID_SUB
                };
                window.paint_quad(fill(
                    Bounds::new(point(x_abs(t), gy), gsize(px(1.0), px(grid_h))),
                    c,
                ));
            }
            // Notes
            if let Some((_, r)) = &region {
                for n in &r.notes {
                    let x = x_of(n.start);
                    let y = y_of(n.key);
                    let nw = f32::from(x_of(n.end()) - x - px(1.0)).max(3.0);
                    if y < gy - px(row) || y > gy + px(grid_h) {
                        continue;
                    }
                    let c = velocity_color(n.velocity);
                    let sel = selected.contains(&n.id);
                    let out = n.start >= r.len;
                    let sounding = playhead >= (r.start + n.start) as f32
                        && playhead < (r.start + n.end()) as f32;
                    let body = if sel {
                        mix(c, WHITE, 0.45)
                    } else if sounding {
                        mix(c, WHITE, 0.25)
                    } else {
                        c
                    };
                    let alpha = if out { 0.35 } else { 1.0 };
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                point(x + px(0.5), y + px(1.0)),
                                gsize(px(nw), px(row - 2.0)),
                            ),
                            with_alpha(body, alpha),
                        )
                        .corner_radii(px(radius::XS))
                        .border_widths(px(1.0))
                        .border_color(if sel {
                            WHITE
                        } else {
                            with_alpha(mix(c, BLACK, 0.5), alpha)
                        }),
                    );
                    window.paint_quad(fill(
                        Bounds::new(
                            point(x + px(2.0), y + px(row / 2.0 - 0.5)),
                            gsize(
                                px(((nw - 4.0) * n.velocity as f32 / 127.0).max(1.0)),
                                px(1.0),
                            ),
                        ),
                        with_alpha(BLACK, 0.35),
                    ));
                    if nw > 30.0 && row >= 11.0 {
                        tracks::text(
                            window,
                            cx,
                            &note_name(n.key),
                            x + px(4.0),
                            y + px(0.5),
                            8.5,
                            with_alpha(BLACK, 0.7),
                            font::MONO,
                            false,
                        );
                    }
                }
            }
            // Pencil ghost
            if let Some((t, key)) = hover {
                let y = y_of(key);
                window.paint_quad(
                    fill(
                        Bounds::new(
                            point(x_of(t) + px(0.5), y + px(1.0)),
                            gsize(px(grid_ticks as f32 * tick_w - 1.0), px(row - 2.0)),
                        ),
                        with_alpha(WHITE, 0.08),
                    )
                    .corner_radii(px(radius::XS))
                    .border_widths(px(1.0))
                    .border_color(with_alpha(WHITE, 0.6)),
                );
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
        });
        // Velocity lane
        let vy = o.y + px(h - vel_h);
        window.paint_quad(fill(
            Bounds::new(point(o.x, vy), gsize(px(w), px(vel_h))),
            color::PANEL,
        ));
        window.paint_quad(fill(
            Bounds::new(point(o.x, vy), gsize(px(w), px(1.0))),
            color::LINE,
        ));
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds: Bounds::new(point(gx, vy), gsize(px(w - keys_w), px(vel_h))),
            }),
            |window| {
                for v in [32.0, 64.0, 96.0] {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(gx, vy + px(vel_h - 4.0 - (vel_h - 12.0) * v / 127.0)),
                            gsize(px(w), px(1.0)),
                        ),
                        color::GRID_SUB,
                    ));
                }
                if let Some((_, r)) = &region {
                    for n in &r.notes {
                        let x = x_of(n.start);
                        let stem = (vel_h - 12.0) * n.velocity as f32 / 127.0;
                        let c = if selected.contains(&n.id) {
                            WHITE
                        } else {
                            velocity_color(n.velocity)
                        };
                        window.paint_quad(fill(
                            Bounds::new(
                                point(x, vy + px(vel_h - 4.0 - stem)),
                                gsize(px(2.0), px(stem)),
                            ),
                            c,
                        ));
                        window.paint_quad(
                            fill(
                                Bounds::new(
                                    point(x - px(2.0), vy + px(vel_h - 7.0 - stem)),
                                    gsize(px(6.0), px(6.0)),
                                ),
                                c,
                            )
                            .corner_radii(px(3.0)),
                        );
                    }
                }
            },
        );
        window.paint_quad(fill(
            Bounds::new(point(o.x, vy), gsize(px(keys_w), px(vel_h))),
            color::CHROME,
        ));
        tracks::text(
            window,
            cx,
            "Velocity",
            o.x + px(8.0),
            vy + px(7.0),
            10.0,
            color::TEXT_TERTIARY,
            font::UI,
            false,
        );
        // Keyboard column
        window.paint_quad(fill(
            Bounds::new(point(o.x, gy), gsize(px(keys_w), px(grid_h))),
            color::FIELD,
        ));
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds: Bounds::new(point(o.x, gy), gsize(px(keys_w), px(grid_h))),
            }),
            |window| {
                for black_pass in [false, true] {
                    for i in 0..rows {
                        let key = top_key as i32 - i;
                        if !(0..=127).contains(&key) {
                            continue;
                        }
                        let key = key as u8;
                        if is_black(key) != black_pass {
                            continue;
                        }
                        let y = gy + px(i as f32 * row);
                        let on = active.contains(&key);
                        if black_pass {
                            window.paint_quad(fill(
                                Bounds::new(point(o.x, y), gsize(px(keys_w * 0.62), px(row))),
                                if on {
                                    color::KEY_BLACK_ACTIVE
                                } else {
                                    color::KEY_BLACK
                                },
                            ));
                        } else {
                            window.paint_quad(fill(
                                Bounds::new(point(o.x, y), gsize(px(keys_w - 1.0), px(row - 0.5))),
                                if on {
                                    color::KEY_WHITE_ACTIVE
                                } else {
                                    color::KEY_WHITE
                                },
                            ));
                            if key.is_multiple_of(12) {
                                window.paint_quad(fill(
                                    Bounds::new(
                                        point(o.x, y + px(row - 1.0)),
                                        gsize(px(keys_w), px(1.0)),
                                    ),
                                    color::TEXT_SECONDARY,
                                ));
                                tracks::text(
                                    window,
                                    cx,
                                    &note_name(key),
                                    o.x + px(keys_w - 22.0),
                                    y + px(0.5),
                                    9.0,
                                    color::TEXT_TERTIARY,
                                    font::MONO,
                                    false,
                                );
                            }
                        }
                    }
                }
            },
        );
        // Ruler in project time, with the region extent under it
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds: Bounds::new(point(gx, o.y), gsize(px(w - keys_w), px(ruler))),
            }),
            |window| {
                tracks::ruler_paint(
                    window,
                    cx,
                    point(gx - px(scroll_x), o.y),
                    w + scroll_x,
                    ruler,
                    beat_w,
                    per_bar,
                    None,
                );
                if let Some((tc, r)) = &region {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(x_abs(r.start as f32), o.y + px(ruler - 3.0)),
                            gsize(px(r.len as f32 * tick_w), px(2.0)),
                        ),
                        *tc,
                    ));
                }
            },
        );
        window.paint_quad(fill(
            Bounds::new(point(o.x, o.y), gsize(px(keys_w), px(ruler))),
            color::CHROME,
        ));
        if let Some((tc, _)) = &region {
            window.paint_quad(fill(
                Bounds::new(point(o.x + px(6.0), o.y + px(8.0)), gsize(px(8.0), px(8.0))),
                *tc,
            ));
        } else {
            tracks::text(
                window,
                cx,
                "Select a region to edit its notes",
                gx + px(16.0),
                gy + px(16.0),
                12.0,
                color::TEXT_TERTIARY,
                font::UI,
                false,
            );
        }
        let phx = x_abs(playhead);
        if phx >= gx {
            window.paint_quad(fill(
                Bounds::new(point(phx, o.y), gsize(px(1.0), px(h))),
                color::ACCENT_FG,
            ));
        }
    };

    div()
        .id("piano-roll")
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
            cx.listener(|this, _: &MouseUpEvent, _, cx| {
                pointer_up(this);
                cx.notify();
            }),
        )
        .on_scroll_wheel(cx.listener(|this, e: &ScrollWheelEvent, window, cx| {
            let d = e.delta.pixel_delta(window.line_height());
            let (dx, dy) = (f32::from(d.x), f32::from(d.y));
            if e.modifiers.secondary() {
                let factor = if dy > 0.0 { 1.1 } else { 1.0 / 1.1 };
                this.ui.roll_beat_w = (this.ui.roll_beat_w * factor)
                    .clamp(size::ROLL_BEAT_W_MIN, size::ROLL_BEAT_W_MAX);
            } else if e.modifiers.shift || dx.abs() > dy.abs() {
                let delta = if dx.abs() > dy.abs() { dx } else { dy };
                this.ui.roll_scroll_x = (this.ui.roll_scroll_x - delta).max(0.0);
            } else {
                let rows = (dy / size::ROLL_ROW_H).round() as i32;
                this.ui.roll_top_key = (this.ui.roll_top_key as i32 + rows).clamp(30, 127) as u8;
            }
            cx.notify();
        }))
}

fn cursor_for(app: &SurfApp) -> CursorStyle {
    match (&app.ui.drag, app.ui.edit.roll_tool) {
        (Some(Drag::ResizeNotes { .. }), _) => CursorStyle::ResizeLeftRight,
        (Some(Drag::MoveNotes { .. }), _) => CursorStyle::ClosedHand,
        (Some(Drag::Velocity { .. }), _) => CursorStyle::ResizeUpDown,
        (_, Tool::Pencil) => CursorStyle::Crosshair,
        (_, Tool::Eraser) => CursorStyle::OperationNotAllowed,
        _ => CursorStyle::Arrow,
    }
}

fn audition(app: &mut SurfApp, key: u8, velocity: u8) {
    if let Some(old) = app.ui.mouse_key.take() {
        app.session
            .handle_live(LiveEvent::NoteOff { key: old }, Instant::now(), true);
    }
    app.ui.mouse_key = Some(key);
    app.session
        .handle_live(LiveEvent::NoteOn { key, velocity }, Instant::now(), true);
}

pub(crate) fn pointer_down(app: &mut SurfApp, e: &MouseDownEvent) {
    app.ui.edit.focus = Focus::Roll;
    let g = Geom::of(app);
    let (x, y) = g.local(e.position);
    let grid = app.ui.edit.roll_snap.ticks(&app.session.project);
    match area(&g, x, y) {
        Area::Ruler => {
            let t = (g.tick_at(x) + g.region_start as f64).max(0.0) as u64;
            app.session.seek(app.session.project.tick_to_sec(t));
            app.ui.drag = Some(Drag::Scrub { tracks: false });
        }
        Area::Keys(key) => audition(app, key, 96),
        Area::Velocity => {
            let Some(region) = app.ui.edit.edit_region else {
                return;
            };
            let p = &app.session.project;
            let Some(id) = stem_at(p, region, &g, x, &app.ui.edit.notes) else {
                return;
            };
            if !app.ui.edit.notes.contains(&id) {
                app.ui.edit.notes = [id].into();
            }
            let (_, r) = p.find_region(region).expect("edit region exists");
            let base = r
                .notes
                .iter()
                .filter(|n| app.ui.edit.notes.contains(&n.id))
                .map(|n| (n.id, n.velocity))
                .collect();
            app.ui.drag = Some(Drag::Velocity { y0: y, base });
        }
        Area::Grid => {
            let Some(region) = app.ui.edit.edit_region else {
                app.session.status = "Select a region in the Tracks area first".to_string();
                return;
            };
            let p = &app.session.project;
            let hit = note_at(p, region, &g, x, y);
            let tool = app.ui.edit.roll_tool;
            let key = g.key_at(y).clamp(0, 127) as u8;
            // Double-click toggles a note, whatever the tool.
            if e.click_count == 2 {
                match hit {
                    Some((id, _)) => {
                        app.session.apply(
                            "Delete Note",
                            Edit::DeleteNotes {
                                region,
                                notes: vec![id],
                            },
                        );
                        app.ui.edit.notes.remove(&id);
                    }
                    None => {
                        let start = floor_snap(g.tick_at(x), grid);
                        editing::create_note(&mut app.session, &mut app.ui.edit, start, key, grid);
                    }
                }
                return;
            }
            match (tool, hit) {
                (Tool::Eraser, Some((id, _))) => {
                    app.session.apply(
                        "Delete Note",
                        Edit::DeleteNotes {
                            region,
                            notes: vec![id],
                        },
                    );
                    app.ui.edit.notes.remove(&id);
                }
                (Tool::Eraser, None) => {}
                (_, Some((id, edge))) => {
                    let add = e.modifiers.shift;
                    if add && app.ui.edit.notes.contains(&id) {
                        app.ui.edit.notes.remove(&id);
                        return;
                    }
                    if add {
                        app.ui.edit.notes.insert(id);
                    } else if !app.ui.edit.notes.contains(&id) {
                        app.ui.edit.notes = [id].into();
                    }
                    let (_, r) = p.find_region(region).expect("edit region exists");
                    let lead = r.note(id).cloned().expect("hit note exists");
                    app.ui.edit.new_velocity = lead.velocity;
                    let notes: Vec<Id> = app.ui.edit.notes.iter().copied().collect();
                    app.ui.drag = Some(if edge {
                        Drag::ResizeNotes { x0: x, notes }
                    } else {
                        audition(app, lead.key, lead.velocity);
                        Drag::MoveNotes {
                            x0: x,
                            y0: y,
                            lead_tick: lead.start,
                            lead_key: lead.key,
                            copy: e.modifiers.alt,
                            notes,
                        }
                    });
                }
                (Tool::Pencil, None) => {
                    let start = floor_snap(g.tick_at(x), grid);
                    if let Some(id) =
                        editing::create_note(&mut app.session, &mut app.ui.edit, start, key, grid)
                    {
                        audition(app, key, app.ui.edit.new_velocity);
                        // Keep dragging to stretch the new note.
                        app.ui.drag = Some(Drag::ResizeNotes {
                            x0: x,
                            notes: vec![id],
                        });
                    }
                }
                (_, None) => {
                    if !e.modifiers.shift {
                        app.ui.edit.notes.clear();
                    }
                    app.ui.marquee = Some((e.position, e.position));
                    app.ui.drag = Some(Drag::Marquee { tracks: false });
                }
            }
        }
    }
}

pub(crate) fn pointer_move(app: &mut SurfApp, e: &MouseMoveEvent) -> bool {
    let g = Geom::of(app);
    let (x, y) = g.local(e.position);
    if e.pressed_button != Some(MouseButton::Left) {
        if app.ui.drag.is_some() {
            pointer_up(app);
            return true;
        }
        // Pencil ghost follows the pointer.
        let grid = app.ui.edit.roll_snap.ticks(&app.session.project);
        let hover = (app.ui.edit.roll_tool == Tool::Pencil && matches!(area(&g, x, y), Area::Grid))
            .then(|| {
                (
                    floor_snap(g.tick_at(x), grid),
                    g.key_at(y).clamp(0, 127) as u8,
                )
            });
        let changed = hover != app.ui.pencil_hover;
        app.ui.pencil_hover = hover;
        return changed;
    }
    let Some(region) = app.ui.edit.edit_region else {
        return false;
    };
    let grid = app.ui.edit.roll_snap.ticks(&app.session.project);
    let Some(drag) = app.ui.drag.clone() else {
        return false;
    };
    match drag {
        Drag::Scrub { tracks: false } => {
            let t = (g.tick_at(x) + g.region_start as f64).max(0.0) as u64;
            let secs = app.session.project.tick_to_sec(t);
            app.session.seek(secs);
        }
        Drag::Marquee { tracks: false } => {
            let Some((a, _)) = app.ui.marquee else {
                return false;
            };
            app.ui.marquee = Some((a, e.position));
            let (ax, ay) = g.local(a);
            let (t0, t1) = (g.tick_at(ax.min(x)), g.tick_at(ax.max(x)));
            let (k_hi, k_lo) = (g.key_at(ay.min(y)), g.key_at(ay.max(y)));
            if let Some((_, r)) = app.session.project.find_region(region) {
                app.ui.edit.notes = r
                    .notes
                    .iter()
                    .filter(|n| {
                        (n.end() as f64) > t0
                            && (n.start as f64) < t1
                            && (n.key as i32) >= k_lo
                            && (n.key as i32) <= k_hi
                    })
                    .map(|n| n.id)
                    .collect();
            }
        }
        Drag::MoveNotes {
            x0,
            y0,
            lead_tick,
            lead_key,
            copy,
            notes,
        } => {
            if (x - x0).abs() < DRAG_SLOP && (y - y0).abs() < DRAG_SLOP {
                return false;
            }
            let target = snap(lead_tick as f64 + ((x - x0) / g.tick_w) as f64, grid).max(0);
            let ticks = target - lead_tick as i64;
            let keys = -((y - y0) / size::ROLL_ROW_H).round() as i32;
            let new_key = (lead_key as i32 + keys).clamp(0, 127) as u8;
            if app.ui.mouse_key != Some(new_key) {
                audition(app, new_key, app.ui.edit.new_velocity);
            }
            if copy {
                let (edit, ids) = copy_notes(&mut app.session.project, region, &notes, ticks, keys);
                app.session.preview("Copy Notes", edit);
                app.ui.edit.notes = ids.into_iter().collect();
            } else {
                app.session.preview(
                    "Move Notes",
                    Edit::MoveNotes {
                        region,
                        notes,
                        ticks,
                        keys,
                    },
                );
            }
            app.session.status = format!(
                "{} · {}",
                note_name(new_key),
                position_label(&app.session.project, g.region_start + target as u64)
            );
        }
        Drag::ResizeNotes { x0, notes } => {
            let delta = snap(((x - x0) / g.tick_w) as f64, grid);
            app.session.preview(
                "Resize Notes",
                Edit::ResizeNotes {
                    region,
                    notes,
                    ticks: delta,
                    min: grid,
                },
            );
        }
        Drag::Velocity { y0, base } => {
            let dv = ((y0 - y) * 127.0 / (size::ROLL_VELOCITY_H - 12.0)).round() as i32;
            let velocities: Vec<(Id, u8)> = base
                .iter()
                .map(|(id, v)| (*id, (*v as i32 + dv).clamp(1, 127) as u8))
                .collect();
            if let [(_, v)] = velocities.as_slice() {
                app.session.status = format!("Velocity {v}");
                app.ui.edit.new_velocity = *v;
            } else {
                app.session.status = format!("Velocity {dv:+}");
            }
            app.session
                .preview("Change Velocity", Edit::SetVelocity { region, velocities });
        }
        _ => return false,
    }
    true
}

pub(crate) fn pointer_up(app: &mut SurfApp) {
    if let Some(key) = app.ui.mouse_key.take() {
        app.session
            .handle_live(LiveEvent::NoteOff { key }, Instant::now(), true);
    }
    match app.ui.drag.take() {
        Some(Drag::MoveNotes { .. } | Drag::ResizeNotes { .. } | Drag::Velocity { .. }) => {
            app.session.commit();
            if let Some(label) = app.session.history.undo_label() {
                app.session.status = format!("{label} · ⌘Z to undo");
            }
        }
        Some(Drag::Marquee { tracks: false }) => {
            let n = app.ui.edit.notes.len();
            if n > 0 {
                app.session.status = format!("{n} note{} selected", if n == 1 { "" } else { "s" });
            }
        }
        _ => {}
    }
    app.ui.marquee = None;
}

/// ⌥-drag: copies of `notes` at the offset. Returns the edit and new ids.
fn copy_notes(p: &mut Project, region: Id, notes: &[Id], ticks: i64, keys: i32) -> (Edit, Vec<Id>) {
    let Some((_, r)) = p.find_region(region) else {
        return (Edit::Batch(vec![]), vec![]);
    };
    let sources: Vec<_> = r
        .notes
        .iter()
        .filter(|n| notes.contains(&n.id))
        .cloned()
        .collect();
    let copies: Vec<_> = sources
        .into_iter()
        .map(|mut n| {
            n.id = p.alloc();
            n.start = (n.start as i64 + ticks).max(0) as u64;
            n.key = (n.key as i32 + keys).clamp(0, 127) as u8;
            n
        })
        .collect();
    let ids = copies.iter().map(|n| n.id).collect();
    (
        Edit::AddNotes {
            region,
            notes: copies,
        },
        ids,
    )
}

/// "Bar Beat Div Tick" for an absolute tick, as the LCD shows it.
pub fn position_label(p: &Project, tick: u64) -> String {
    let tpb = p.ticks_per_beat();
    let per_bar = p.time_signature.0.max(1) as u64;
    let beats = tick / tpb;
    let sub = tick % tpb;
    format!(
        "{} {} {} {}",
        beats / per_bar + 1,
        beats % per_bar + 1,
        sub * 4 / tpb + 1,
        sub % (tpb / 4).max(1) + 1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_read_like_the_lcd() {
        let p = Project::from_song(&surf_core::song::Song::demo());
        assert_eq!(position_label(&p, 0), "1 1 1 1");
        assert_eq!(position_label(&p, 480 * 5 + 120), "2 2 2 1");
    }

    #[test]
    fn new_notes_land_on_the_grid_cell_under_the_pointer() {
        assert_eq!(floor_snap(119.0, 120), 0);
        assert_eq!(floor_snap(121.0, 120), 120);
        assert_eq!(floor_snap(-5.0, 120), 0);
    }
}
