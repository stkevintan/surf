//! Channel strip, shared by the Inspector and the Mixer.
//!
//! Top to bottom: EQ thumbnail, setting, instrument, audio FX, sends, output,
//! pan, level readout, fader + meter, mute/solo, name. Only the fader and the
//! meter are live today (the one track plays through the master volume);
//! the rest holds its place for the multi-track mixer.

use gpui::{
    Context, FontWeight, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, ParentElement,
    Rgba, Styled, canvas, div, fill, point, prelude::*, px, size as gsize,
};

use crate::shell::SurfApp;
use crate::theme::{SlotColors, color, font, radius, size};
use crate::widgets::{Tone, ghost_slot, msr, slot};

/// Height of a strip with the fader area at its minimum (90 px).
pub const STRIP_MIN_H: f32 = 400.0;

pub struct StripModel {
    pub id: &'static str,
    pub name: String,
    pub track_color: Rgba,
    pub instrument: Option<(String, SlotColors)>,
    pub effects: Vec<&'static str>,
    pub output: &'static str,
    /// 0.0..=1.5, the engine's linear gain.
    pub volume: f32,
    pub peak: f32,
    pub mute: bool,
    pub solo: bool,
    pub selected: bool,
    pub master: bool,
    /// Where the fader was painted, shared with the pointer handlers.
    pub fader_bounds: std::rc::Rc<
        std::cell::RefCell<std::collections::HashMap<&'static str, gpui::Bounds<gpui::Pixels>>>,
    >,
}

/// Fader position (0..1, bottom to top) for a linear gain. +6 dB at the top,
/// -inf at the bottom, using the curve from the design spec.
pub fn gain_to_pos(gain: f32) -> f32 {
    if gain <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * gain.log10();
    ((db + 60.0) / 66.0).clamp(0.0, 1.0).powf(1.6)
}

pub fn pos_to_gain(pos: f32) -> f32 {
    let db = pos.clamp(0.0, 1.0).powf(1.0 / 1.6) * 66.0 - 60.0;
    if db < -59.5 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

fn fmt_db(gain: f32) -> String {
    if gain <= 0.0 {
        "-∞".to_string()
    } else {
        format!("{:+.1}", 20.0 * gain.log10())
    }
}

pub fn render(
    model: StripModel,
    width: Option<f32>,
    cx: &mut Context<SurfApp>,
) -> impl IntoElement {
    let is_master = model.master;
    let id = model.id;
    let pos = gain_to_pos(model.volume);
    let peak = model.peak.clamp(0.0, 1.0);
    let bounds_out = model.fader_bounds.clone();

    let fader = canvas(
        move |_, _, _| {},
        move |bounds, _, window, _| {
            bounds_out.borrow_mut().insert(id, bounds);
            let w = f32::from(bounds.size.width);
            let h = f32::from(bounds.size.height);
            let o = bounds.origin;
            // Track
            let track_x = w / 2.0 - 6.0;
            window.paint_quad(
                fill(
                    gpui::Bounds::new(point(o.x + px(track_x), o.y), gsize(px(4.0), px(h))),
                    color::FIELD,
                )
                .corner_radii(px(2.0))
                .border_widths(px(1.0))
                .border_color(color::LINE),
            );
            // Meter: two channels to the right of the track
            for ch in 0..2 {
                let mx = o.x + px(track_x + 10.0 + ch as f32 * 4.0);
                window.paint_quad(fill(
                    gpui::Bounds::new(point(mx, o.y), gsize(px(3.0), px(h))),
                    color::FIELD,
                ));
                let level = peak * if ch == 0 { 1.0 } else { 0.96 };
                let lh = h * level;
                // Green up to 72%, yellow to 88%, red above (flat bands).
                for (from, to, c) in [
                    (0.0, 0.72, color::PLAY),
                    (0.72, 0.88, color::WARN),
                    (0.88, 1.0, color::RECORD),
                ] {
                    let a = (h * from).min(lh);
                    let b = (h * to).min(lh);
                    if b > a {
                        window.paint_quad(fill(
                            gpui::Bounds::new(
                                point(mx, o.y + px(h - b)),
                                gsize(px(3.0), px(b - a)),
                            ),
                            c,
                        ));
                    }
                }
            }
            // Cap
            let cap_h = 30.0;
            let cy = (h - cap_h) * (1.0 - pos);
            let cap = gpui::Bounds::new(
                point(o.x + px(track_x - 10.0), o.y + px(cy)),
                gsize(px(24.0), px(cap_h)),
            );
            window.paint_quad(
                fill(cap, color::RAISED)
                    .corner_radii(px(radius::SM))
                    .border_widths(px(1.0))
                    .border_color(color::LINE_STRONG),
            );
            window.paint_quad(fill(
                gpui::Bounds::new(
                    point(cap.origin.x + px(4.0), cap.origin.y + px(cap_h / 2.0 - 1.0)),
                    gsize(px(16.0), px(2.0)),
                ),
                color::ACCENT_FG,
            ));
        },
    )
    .size_full();

    let fader_area = div()
        .id(gpui::ElementId::Name(format!("{id}-fader").into()))
        .flex_1()
        .min_h(px(90.0))
        .cursor_row_resize()
        .child(fader)
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, e: &MouseDownEvent, window, cx| {
                set_from_pointer(this, e.position.y, window, id);
                this.ui.fader_drag = Some(id);
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(move |this, e: &MouseMoveEvent, window, cx| {
            if this.ui.fader_drag == Some(id) && e.pressed_button == Some(MouseButton::Left) {
                set_from_pointer(this, e.position.y, window, id);
                cx.notify();
            } else if e.pressed_button.is_none() {
                this.ui.fader_drag = None;
            }
        }));

    // Fixed rows never shrink; only the fader area flexes. In a short
    // container (the editor at its default height) the strip keeps its
    // minimum height and the container scrolls.
    div()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .pt(px(6.0))
        .px(px(5.0))
        .min_w(px(0.0))
        .min_h(px(STRIP_MIN_H))
        .when_some(width, |d, w| d.w(px(w)).flex_none())
        .when(width.is_none(), |d| d.flex_1())
        .bg(if model.selected {
            color::STRIP_SEL
        } else {
            color::PANEL
        })
        .child(eq_thumb(model.track_color))
        .child(caption("Setting"))
        .child(slot(if is_master { "Stereo Out" } else { "Default" }, None))
        .child(caption(if is_master { "" } else { "Instrument" }))
        .child(match &model.instrument {
            Some((name, colors)) => slot(name.clone(), Some(*colors)).into_any_element(),
            None => div()
                .flex_none()
                .h(px(size::STRIP_SLOT_H))
                .into_any_element(),
        })
        .child(caption("Audio FX"))
        .children(model.effects.iter().map(|fx| {
            slot(
                *fx,
                Some(crate::theme::SlotColors {
                    fg: color::ACCENT_FG,
                    bg: color::ACCENT_BG,
                    line: color::ACCENT_LINE,
                }),
            )
        }))
        .child(ghost_slot("+"))
        .child(caption("Sends"))
        .child(ghost_slot(if is_master { "–" } else { "+" }))
        .child(caption("Output"))
        .child(slot(model.output, None).text_color(color::TEXT_SECONDARY))
        .child(knob())
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .justify_between()
                .px(px(2.0))
                .font_family(font::MONO)
                .text_size(px(9.5))
                .child(
                    div()
                        .text_color(color::TEXT_SECONDARY)
                        .child(fmt_db(model.volume)),
                )
                .child(
                    div()
                        .text_color(color::TEXT_TERTIARY)
                        .child(fmt_db(model.peak)),
                ),
        )
        .child(fader_area)
        .child(
            div()
                .flex_none()
                .flex()
                .flex_row()
                .justify_center()
                .gap(px(3.0))
                .child(msr("M", Tone::Mute, model.mute))
                .when(!is_master, |d| d.child(msr("S", Tone::Solo, model.solo))),
        )
        .child(
            div()
                .flex_none()
                .h(px(size::STRIP_NAME_H))
                .mt(px(3.0))
                .mx(px(-5.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(color::CHROME)
                .border_t_2()
                .border_color(model.track_color)
                .text_size(px(font::SMALL))
                .font_weight(FontWeight::MEDIUM)
                .overflow_hidden()
                .whitespace_nowrap()
                .child(model.name),
        )
}

fn set_from_pointer(app: &mut SurfApp, y: gpui::Pixels, window: &gpui::Window, id: &'static str) {
    // Map the pointer through the bounds the fader was painted at last frame.
    let bounds = app.ui.fader_bounds.borrow().get(id).copied();
    if let Some(bounds) = bounds {
        let top = f32::from(bounds.origin.y);
        let h = f32::from(bounds.size.height).max(1.0);
        let pos = 1.0 - ((f32::from(y) - top - 15.0) / (h - 30.0).max(1.0));
        app.session.set_volume(pos_to_gain(pos));
    }
    let _ = window;
}

fn eq_thumb(track: Rgba) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            window.paint_quad(
                fill(bounds, color::FIELD)
                    .corner_radii(px(radius::SM))
                    .border_widths(px(1.0))
                    .border_color(color::LINE),
            );
            // A flat EQ curve with a gentle lift, like the prototype's thumbnail.
            let w = f32::from(bounds.size.width);
            let h = f32::from(bounds.size.height);
            let mut b = gpui::PathBuilder::stroke(px(1.3));
            for i in 0..=24 {
                let t = i as f32 / 24.0;
                let y = h * (0.6 - 0.12 * (t * 6.0).sin() * (1.0 - t));
                let p = point(bounds.origin.x + px(t * w), bounds.origin.y + px(y));
                if i == 0 { b.move_to(p) } else { b.line_to(p) }
            }
            if let Ok(path) = b.build() {
                window.paint_path(path, track);
            }
        },
    )
    .flex_none()
    .h(px(26.0))
    .w_full()
}

fn knob() -> impl IntoElement {
    div()
        .flex_none()
        .size(px(size::STRIP_KNOB))
        .mx_auto()
        .mt(px(3.0))
        .rounded_full()
        .bg(color::RAISED)
        .border_1()
        .border_color(color::LINE_STRONG)
        .flex()
        .justify_center()
        .child(
            div()
                .mt(px(2.0))
                .w(px(2.0))
                .h(px(8.0))
                .rounded(px(1.0))
                .bg(color::ACCENT_FG),
        )
}

fn caption(text: &'static str) -> impl IntoElement {
    div()
        .flex_none()
        .mt(px(3.0))
        .h(px(9.0))
        .text_center()
        .font_family(font::LABEL)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(font::CAPTION))
        .text_color(color::TEXT_TERTIARY)
        .child(text.to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fader_curve_round_trips_and_spans_the_range() {
        assert_eq!(gain_to_pos(0.0), 0.0);
        assert!(
            (gain_to_pos(10f32.powf(6.0 / 20.0)) - 1.0).abs() < 1e-4,
            "+6 dB at the top"
        );
        for g in [0.05, 0.25, 0.5, 0.9, 1.0, 1.5] {
            assert!((pos_to_gain(gain_to_pos(g)) - g).abs() < 1e-3, "gain {g}");
        }
        assert_eq!(pos_to_gain(0.0), 0.0);
    }
}
