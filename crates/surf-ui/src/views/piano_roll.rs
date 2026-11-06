//! Piano Roll: keyboard column, note grid colored by velocity, velocity lane.
//!
//! Read-only for now: drag-editing, quantize and the local inspector's
//! controls need the multi-track model with commands and undo. Scrolling,
//! zooming, seeking and auditioning keys work.

use gpui::{
    Bounds, Context, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement, Pixels,
    ScrollWheelEvent, Styled, Window, canvas, div, fill, point, prelude::*, px, size as gsize,
};

use surf_midi::LiveEvent;

use crate::shell::{SurfApp, on_click};
use crate::theme::{color, font, radius, size};
use crate::views::{self, is_black, mix, note_name, tracks, velocity_color, with_alpha};
use crate::widgets::{StyleExt, button, label, segment, segmented};

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .flex_1()
        .min_h(px(0.0))
        .child(local_inspector(app, cx))
        .child(grid(app, cx))
}

/// Bar above the roll: tool and grid pickers.
pub fn bar(_app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(label("Tool"))
        .child(
            segmented().child(segment("Pointer", true)).child(on_click(
                segment("Pencil", false)
                    .id("roll-pencil")
                    .when_planned(true),
                cx,
                |a| a.planned("Pencil tool"),
            )),
        )
        .child(label("Grid"))
        .child(
            segmented()
                .child(segment("1/4", false))
                .child(segment("1/8", false))
                .child(segment("1/16", true)),
        )
}

fn local_inspector(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let t = views::track_model(&app.session);
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
                .child(div().size(px(10.0)).rounded(px(3.0)).bg(t.color))
                .child(
                    div()
                        .text_size(px(12.0))
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(app.session.song_name.clone()),
                ),
        )
        .child(row("Time Quantize", "1/16 Note"))
        .child(on_click(
            button("Q").id("roll-quantize").when_planned(true),
            cx,
            |a| a.planned("Quantize"),
        ))
        .child(row("Strength", "100").when_planned(true))
        .child(row("Scale Quantize", "Off").when_planned(true))
        .child(row("Velocity", "–").when_planned(true))
        .child(row("Articulation", "None").when_planned(true))
}

fn row(name: &'static str, value: &'static str) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .child(
            div()
                .text_size(px(10.5))
                .text_color(color::TEXT_TERTIARY)
                .child(name),
        )
        .child(crate::widgets::field(value))
}

fn grid(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let s = &app.session;
    let beat_w = app.ui.roll_beat_w;
    let top_key = app.ui.roll_top_key;
    let scroll_x = app.ui.roll_scroll_x;
    let beat_secs = views::beat_secs(s);
    let (per_bar, _) = s.time_signature();
    let per_bar = per_bar.max(1) as f64;
    let region_beats = views::song_bars(s) as f64 * per_bar;
    let playhead = s.position() / beat_secs;
    let active = s.active_keys();
    let notes: Vec<(f64, f64, u8, u8)> = s
        .timeline
        .notes
        .iter()
        .map(|n| {
            (
                n.start / beat_secs,
                (n.end - n.start) / beat_secs,
                n.key,
                n.velocity,
            )
        })
        .collect();
    let track = views::track_model(s).color;
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
        let x_of = |beats: f64| gx + px(beats as f32 * beat_w - scroll_x);
        let y_of = |key: u8| gy + px((top_key as f32 - key as f32) * row);
        let rows = (grid_h / row).ceil() as i32 + 1;

        window.paint_quad(fill(bounds, color::LANE));
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds: Bounds::new(point(gx, gy), gsize(px(w - keys_w), px(grid_h))),
            }),
            |window| {
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
                // Region end shading
                let end = x_of(region_beats);
                window.paint_quad(fill(
                    Bounds::new(point(end, gy), gsize(px(w), px(grid_h))),
                    color::GRID_OUTSIDE,
                ));
                // 1/16 grid
                let sixteenth = 0.25;
                let first = (scroll_x / beat_w / sixteenth as f32).floor() as i64;
                let last = ((scroll_x + w) / beat_w / sixteenth as f32).ceil() as i64;
                for i in first..=last {
                    let b = i as f64 * sixteenth;
                    let c = if b % per_bar == 0.0 {
                        color::GRID_BAR
                    } else if b.fract() == 0.0 {
                        color::GRID_BEAT
                    } else {
                        color::GRID_SUB
                    };
                    window.paint_quad(fill(
                        Bounds::new(point(x_of(b), gy), gsize(px(1.0), px(grid_h))),
                        c,
                    ));
                }
                // Notes
                for (start, len, key, vel) in &notes {
                    let x = x_of(*start);
                    let y = y_of(*key);
                    let nw = (*len as f32 * beat_w - 1.0).max(3.0);
                    if y < gy - px(row) || y > gy + px(grid_h) {
                        continue;
                    }
                    let c = velocity_color(*vel);
                    let sounding = playhead >= *start && playhead < start + len;
                    let body = if sounding {
                        mix(
                            c,
                            gpui::Rgba {
                                r: 1.0,
                                g: 1.0,
                                b: 1.0,
                                a: 1.0,
                            },
                            0.35,
                        )
                    } else {
                        c
                    };
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                point(x + px(0.5), y + px(1.0)),
                                gsize(px(nw), px(row - 2.0)),
                            ),
                            body,
                        )
                        .corner_radii(px(radius::XS))
                        .border_widths(px(1.0))
                        .border_color(mix(
                            c,
                            gpui::Rgba {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 1.0,
                            },
                            0.5,
                        )),
                    );
                    // Velocity line inside the note, Logic style.
                    window.paint_quad(fill(
                        Bounds::new(
                            point(x + px(2.0), y + px(row / 2.0 - 0.5)),
                            gsize(px(((nw - 4.0) * *vel as f32 / 127.0).max(1.0)), px(1.0)),
                        ),
                        with_alpha(
                            gpui::Rgba {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 1.0,
                            },
                            0.35,
                        ),
                    ));
                    if nw > 30.0 && row >= 11.0 {
                        tracks::text(
                            window,
                            cx,
                            &note_name(*key),
                            x + px(4.0),
                            y + px(0.5),
                            8.5,
                            gpui::Rgba {
                                r: 0.0,
                                g: 0.0,
                                b: 0.0,
                                a: 0.7,
                            },
                            font::MONO,
                            false,
                        );
                    }
                }
            },
        );
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
                for (start, _, _, vel) in &notes {
                    let x = x_of(*start);
                    let stem = (vel_h - 12.0) * *vel as f32 / 127.0;
                    let c = velocity_color(*vel);
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
        // Ruler (project time) with the region extent under it
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
                window.paint_quad(fill(
                    Bounds::new(
                        point(x_of(0.0), o.y + px(ruler - 3.0)),
                        gsize(px(region_beats as f32 * beat_w), px(2.0)),
                    ),
                    track,
                ));
            },
        );
        window.paint_quad(fill(
            Bounds::new(point(o.x, o.y), gsize(px(keys_w), px(ruler))),
            color::CHROME,
        ));
        window.paint_quad(fill(
            Bounds::new(point(o.x + px(6.0), o.y + px(8.0)), gsize(px(8.0), px(8.0))),
            track,
        ));
        // Playhead
        let phx = x_of(playhead);
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
        .child(canvas(|_, _, _| {}, paint).size_full())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, e: &MouseDownEvent, _, cx| {
                let b = this.ui.roll_bounds.get();
                let lx = f32::from(e.position.x - b.origin.x);
                let ly = f32::from(e.position.y - b.origin.y);
                let grid_h =
                    f32::from(b.size.height) - size::RULER_EDITOR_H - size::ROLL_VELOCITY_H;
                if ly < size::RULER_EDITOR_H && lx > size::ROLL_KEYS_W {
                    let beats = ((lx - size::ROLL_KEYS_W + this.ui.roll_scroll_x)
                        / this.ui.roll_beat_w)
                        .max(0.0);
                    this.session
                        .seek(beats as f64 * views::beat_secs(&this.session));
                } else if lx < size::ROLL_KEYS_W
                    && ly > size::RULER_EDITOR_H
                    && ly < size::RULER_EDITOR_H + grid_h
                {
                    // Audition the clicked key.
                    let key = this.ui.roll_top_key as i32
                        - ((ly - size::RULER_EDITOR_H) / size::ROLL_ROW_H).floor() as i32;
                    if (0..=127).contains(&key) {
                        let key = key as u8;
                        this.ui.mouse_key = Some(key);
                        this.session.handle_live(
                            LiveEvent::NoteOn { key, velocity: 96 },
                            std::time::Instant::now(),
                            true,
                        );
                    }
                }
                cx.notify();
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _: &MouseUpEvent, _, cx| {
                if let Some(key) = this.ui.mouse_key.take() {
                    this.session.handle_live(
                        LiveEvent::NoteOff { key },
                        std::time::Instant::now(),
                        true,
                    );
                    cx.notify();
                }
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
                let top = (this.ui.roll_top_key as i32 + rows).clamp(30, 127);
                this.ui.roll_top_key = top as u8;
            }
            cx.notify();
        }))
}
