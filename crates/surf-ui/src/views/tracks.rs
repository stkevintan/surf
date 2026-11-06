//! Tracks area: header column + timeline canvas.
//!
//! One track, one region covering the song, until the multi-track model
//! lands. The ruler seeks (lower half) and shows the cycle (upper half);
//! Cmd/Ctrl + wheel zooms.

use gpui::{
    Bounds, Context, FontWeight, IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels,
    ScrollWheelEvent, SharedString, Styled, TextRun, Window, canvas, div, fill, font as gfont,
    point, prelude::*, px, size as gsize, svg,
};

use crate::icons;
use crate::shell::{MainView, SurfApp, on_click, pane_header};
use crate::theme::{color, font, radius, size};
use crate::views::{self, mix, with_alpha};
use crate::widgets::{Tone, label, msr, segment, segmented};

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let ui = &app.ui;
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .child(
            pane_header()
                .child(planned_menu("Edit", cx))
                .child(planned_menu("Functions", cx))
                .child(planned_menu("View", cx))
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

fn planned_menu(name: &'static str, cx: &mut Context<SurfApp>) -> impl IntoElement {
    on_click(
        div()
            .id(SharedString::from(format!("menu-{name}")))
            .h(px(22.0))
            .px(px(8.0))
            .flex()
            .items_center()
            .gap(px(3.0))
            .rounded(px(radius::MD - 1.0))
            .text_size(px(font::BODY))
            .text_color(color::TEXT_SECONDARY)
            .hover(|d| d.bg(color::HOVER))
            .cursor_pointer()
            .child(name)
            .child(
                svg()
                    .path(icons::path("chevron"))
                    .size(px(8.0))
                    .text_color(color::TEXT_TERTIARY),
            ),
        cx,
        move |a| a.planned(&format!("{name} menu")),
    )
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
    let t = views::track_model(&app.session);
    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(size::TRACK_HEADER_W))
        .bg(color::HEAD)
        .border_r_1()
        .border_color(color::LINE)
        .child(
            div()
                .flex()
                .flex_row()
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
                        .opacity(0.4)
                        .cursor_pointer()
                        .child(
                            svg()
                                .path(icons::path("plus"))
                                .size(px(12.0))
                                .text_color(color::TEXT_SECONDARY),
                        ),
                    cx,
                    |a| a.planned("New Track"),
                ))
                .child(label("Tracks"))
                .child(div().flex_1())
                .child(label("Global ▸").opacity(0.4)),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .h(px(size::LANE_H))
                .pr(px(8.0))
                .bg(color::HEAD_SEL)
                .border_b_1()
                .border_color(color::LINE)
                .border_l_3()
                .child(
                    div()
                        .w(px(17.0))
                        .text_right()
                        .font_family(font::MONO)
                        .text_size(px(10.0))
                        .text_color(color::TEXT_TERTIARY)
                        .child("1"),
                )
                .child(
                    div()
                        .size(px(24.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(radius::SM + 1.0))
                        .border_1()
                        .border_color(t.color)
                        .child(
                            svg()
                                .path(icons::path("piano"))
                                .size(px(14.0))
                                .text_color(t.color),
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
                                .child(t.name),
                        )
                        .child(
                            div()
                                .text_size(px(9.5))
                                .text_color(color::TEXT_TERTIARY)
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .child(t.instrument),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .gap(px(2.0))
                        .child(msr("M", Tone::Mute, false))
                        .child(msr("S", Tone::Solo, false))
                        .child(msr("R", Tone::Record, app.session.is_recording())),
                )
                .border_color(t.color),
        )
}

fn timeline(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let s = &app.session;
    let beat_w = app.ui.tracks_beat_w;
    let beat_secs = views::beat_secs(s);
    let (per_bar, _) = s.time_signature();
    let per_bar = per_bar.max(1) as f64;
    let song_beats = s.timeline.duration / beat_secs;
    let region_beats = (views::song_bars(s) as f64) * per_bar;
    let playhead_beats = s.position() / beat_secs;
    let looping = s.loop_playback;
    let recording = s.is_recording();
    let name: SharedString = s.song_name.clone().into();
    let track = views::track_model(s).color;
    // (start beat, length beats, key) for the note thumbnail
    let notes: Vec<(f64, f64, u8)> = s
        .timeline
        .notes
        .iter()
        .map(|n| (n.start / beat_secs, (n.end - n.start) / beat_secs, n.key))
        .collect();
    let (lo, hi) = notes
        .iter()
        .fold((127u8, 0u8), |(lo, hi), n| (lo.min(n.2), hi.max(n.2)));

    let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut Window, cx: &mut gpui::App| {
        let o = bounds.origin;
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        let ruler = size::RULER_TRACKS_H;
        let x_of = |beats: f64| o.x + px(beats as f32 * beat_w);

        window.paint_quad(fill(bounds, color::LANE));
        // Selected lane
        window.paint_quad(fill(
            Bounds::new(point(o.x, o.y + px(ruler)), gsize(px(w), px(size::LANE_H))),
            color::LANE_SEL,
        ));
        window.paint_quad(fill(
            Bounds::new(
                point(o.x, o.y + px(ruler + size::LANE_H - 1.0)),
                gsize(px(w), px(1.0)),
            ),
            color::LINE_ROW,
        ));
        // Grid: bars brighter than beats; skip beats when zoomed far out.
        let total_beats = (w / beat_w).ceil() as i64 + 1;
        let step = if beat_w < 8.0 { per_bar as i64 } else { 1 };
        let mut b = 0;
        while b <= total_beats {
            let bar = b as f64 % per_bar == 0.0;
            window.paint_quad(fill(
                Bounds::new(
                    point(x_of(b as f64), o.y + px(ruler)),
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
        // Past the end of the song
        let end_x = x_of(region_beats);
        if end_x < o.x + px(w) {
            window.paint_quad(fill(
                Bounds::new(
                    point(end_x, o.y + px(ruler)),
                    gsize(o.x + px(w) - end_x, px(h - ruler)),
                ),
                color::GRID_OUTSIDE,
            ));
        }
        // The region
        if !notes.is_empty() || region_beats > 0.0 {
            let ry = o.y + px(ruler + 2.0);
            let rh = size::LANE_H - 5.0;
            let rw = (region_beats as f32 * beat_w - 1.0).max(4.0);
            let rb = Bounds::new(point(o.x + px(0.5), ry), gsize(px(rw), px(rh)));
            window.paint_quad(
                fill(rb, with_alpha(track, 0.3))
                    .corner_radii(px(radius::SM))
                    .border_widths(px(1.5))
                    .border_color(track),
            );
            text(
                window,
                cx,
                &name.to_uppercase(),
                o.x + px(6.0),
                ry + px(2.0),
                10.5,
                track,
                font::LABEL,
                true,
            );
            let span = (hi.saturating_sub(lo) as f32 + 2.0).max(1.0);
            let nh = ((rh - 20.0) / span).clamp(1.5, 3.0);
            for (start, len, key) in &notes {
                let nx = x_of(*start);
                let ny = ry + px(16.0 + (hi as f32 + 1.0 - *key as f32) / span * (rh - 20.0));
                window.paint_quad(fill(
                    Bounds::new(
                        point(nx, ny),
                        gsize(px((*len as f32 * beat_w - 1.0).max(1.5)), px(nh)),
                    ),
                    track,
                ));
            }
        }
        if recording {
            let ry = o.y + px(ruler + 2.0);
            let rw = (playhead_beats as f32 * beat_w).max(2.0);
            window.paint_quad(
                fill(
                    Bounds::new(point(o.x, ry), gsize(px(rw), px(size::LANE_H - 5.0))),
                    with_alpha(color::RECORD, 0.4),
                )
                .corner_radii(px(radius::SM))
                .border_widths(px(1.0))
                .border_color(color::RECORD),
            );
        }
        // Ruler
        ruler_paint(
            window,
            cx,
            o,
            w,
            ruler,
            beat_w,
            per_bar,
            if looping { Some(region_beats) } else { None },
        );
        // Playhead
        let px_ = x_of(playhead_beats);
        window.paint_quad(fill(
            Bounds::new(point(px_, o.y), gsize(px(1.0), px(h))),
            color::ACCENT_FG,
        ));
        let _ = song_beats;
    };

    let bpb = per_bar;
    div()
        .id("tracks-timeline")
        .relative()
        .flex_1()
        .min_w(px(0.0))
        .h_full()
        .overflow_hidden()
        .child(canvas(|_, _, _| {}, paint).size_full())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                // Click on the ruler seeks; elsewhere selects the region (only one for now).
                let bounds = this.ui.tracks_bounds.get();
                let local_x = f32::from(e.position.x - bounds.origin.x);
                let local_y = f32::from(e.position.y - bounds.origin.y);
                if local_y < size::RULER_TRACKS_H {
                    let beats = (local_x / this.ui.tracks_beat_w).max(0.0) as f64;
                    this.session.seek(beats * views::beat_secs(&this.session));
                } else if e.click_count == 2 {
                    this.open_tab(crate::shell::EditorTab::PianoRoll, false);
                }
                let _ = (window, bpb);
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
        .child(bounds_probe(app))
}

/// Records the timeline's painted bounds so pointer handlers can map
/// window coordinates into it.
fn bounds_probe(app: &SurfApp) -> impl IntoElement {
    let cell = app.ui.tracks_bounds.clone();
    canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {})
        .absolute()
        .size_full()
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
