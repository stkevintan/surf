//! Perform: falling notes onto an 88-key keyboard; live playing rises from
//! the keys. Flat, as in the prototype. Glow and particles are left for the
//! wgpu stage renderer (docs/architecture.md §2.8).

use std::time::Instant;

use gpui::{
    Bounds, Context, IntoElement, MouseButton, MouseDownEvent, MouseUpEvent, ParentElement, Pixels,
    Styled, Window, canvas, div, fill, point, prelude::*, px, size as gsize,
};

use surf_midi::LiveEvent;

use crate::shell::{SurfApp, pane_header};
use crate::theme::{color, radius};
use crate::views::{self, is_black, mix, tracks};

/// Seconds of song visible above the keyboard.
const LOOK_AHEAD: f64 = 3.0;
/// Seconds a released live note keeps rising before it is dropped.
const LIVE_TRAIL: f32 = 4.0;

#[derive(Clone, Copy, Default)]
struct KeyGeom {
    x: f32,
    w: f32,
    black: bool,
}

fn layout(width: f32) -> [KeyGeom; 128] {
    let mut keys = [KeyGeom::default(); 128];
    let whites = (21u8..=108).filter(|k| !is_black(*k)).count() as f32;
    let ww = width / whites;
    let bw = ww * 0.6;
    let mut x = 0.0;
    for key in 21u8..=108 {
        keys[key as usize] = if is_black(key) {
            KeyGeom {
                x: x - bw * 0.5,
                w: bw,
                black: true,
            }
        } else {
            let g = KeyGeom {
                x,
                w: ww,
                black: false,
            };
            x += ww;
            g
        };
    }
    keys
}

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let s = &app.session;
    let position = s.position();
    let beat_secs = views::beat_secs(s);
    let (per_bar, _) = s.time_signature();
    let active = s.active_keys();
    // Song notes with their track's color, muted and soloed-out tracks dropped.
    let p = &s.project;
    let colors: std::collections::HashMap<_, _> = p
        .tracks
        .iter()
        .filter(|t| p.audible(t))
        .map(|t| (t.id, views::track_color(t)))
        .collect();
    let song: Vec<(f64, f64, u8, gpui::Rgba)> = p
        .timed_notes()
        .into_iter()
        .filter(|(_, n)| n.end > position && n.start < position + LOOK_AHEAD)
        .filter_map(|(t, n)| colors.get(&t).map(|c| (n.start, n.end, n.key, *c)))
        .collect();
    let now = Instant::now();
    let live: Vec<(f32, Option<f32>, u8)> = s
        .live_notes
        .iter()
        .map(|n| {
            (
                now.duration_since(n.start).as_secs_f32(),
                n.end.map(|e| now.duration_since(e).as_secs_f32()),
                n.key,
            )
        })
        .filter(|(_, released, _)| released.is_none_or(|r| r < LIVE_TRAIL))
        .collect();
    let bounds_cell = app.ui.perform_bounds.clone();

    let paint = move |bounds: Bounds<Pixels>, _: (), window: &mut Window, cx: &mut gpui::App| {
        bounds_cell.set(bounds);
        let o = bounds.origin;
        let w = f32::from(bounds.size.width);
        let h = f32::from(bounds.size.height);
        let kb_h = (h * 0.18).clamp(56.0, 110.0);
        let hit = h - kb_h;
        let px_per_sec = hit / LOOK_AHEAD as f32;
        let keys = layout(w);

        window.paint_quad(fill(Bounds::new(o, gsize(px(w), px(hit))), color::VOID));
        // Beat lines fall with the music; bars brighter.
        let first = (position / beat_secs).ceil() as i64;
        let last = ((position + LOOK_AHEAD) / beat_secs).floor() as i64;
        for b in first..=last {
            let y = hit - ((b as f64 * beat_secs - position) as f32) * px_per_sec;
            let bar = b % per_bar.max(1) as i64 == 0;
            window.paint_quad(fill(
                Bounds::new(point(o.x, o.y + px(y)), gsize(px(w), px(1.0))),
                if bar {
                    color::GRID_BAR
                } else {
                    color::GRID_SUB
                },
            ));
        }
        // Song notes falling onto the keys.
        for (start, end, key, track) in &song {
            let track = *track;
            let g = keys[*key as usize];
            let bottom = (hit - (*start - position) as f32 * px_per_sec).min(hit);
            let top = hit - (*end - position) as f32 * px_per_sec;
            let on = *start <= position;
            let c = if on {
                mix(
                    track,
                    gpui::Rgba {
                        r: 1.0,
                        g: 1.0,
                        b: 1.0,
                        a: 1.0,
                    },
                    0.25,
                )
            } else {
                track
            };
            window.paint_quad(
                fill(
                    Bounds::new(
                        point(o.x + px(g.x + 1.0), o.y + px(top.max(0.0))),
                        gsize(px(g.w - 2.0), px((bottom - top.max(0.0)).max(3.0))),
                    ),
                    c,
                )
                .corner_radii(px(radius::SM - 1.0)),
            );
        }
        // Live notes rising from the keys.
        for (age, released, key) in &live {
            let g = keys[*key as usize];
            let top = hit - age * px_per_sec;
            let bottom = released.map_or(hit, |r| hit - r * px_per_sec);
            if bottom <= 0.0 {
                continue;
            }
            window.paint_quad(
                fill(
                    Bounds::new(
                        point(o.x + px(g.x + 1.0), o.y + px(top.max(0.0))),
                        gsize(px(g.w - 2.0), px((bottom - top.max(0.0)).max(3.0))),
                    ),
                    color::ACCENT_FG,
                )
                .corner_radii(px(radius::SM - 1.0)),
            );
        }
        // Hit line
        window.paint_quad(fill(
            Bounds::new(point(o.x, o.y + px(hit - 1.0)), gsize(px(w), px(1.5))),
            color::ACCENT_FG,
        ));
        // Keyboard: whites, then blacks on top.
        window.paint_quad(fill(
            Bounds::new(point(o.x, o.y + px(hit)), gsize(px(w), px(kb_h))),
            color::FIELD,
        ));
        for key in 21u8..=108 {
            let g = keys[key as usize];
            if g.black {
                continue;
            }
            let on = active.contains(&key);
            window.paint_quad(fill(
                Bounds::new(
                    point(o.x + px(g.x + 0.5), o.y + px(hit + 1.0)),
                    gsize(px(g.w - 1.0), px(kb_h - 2.0)),
                ),
                if on {
                    color::KEY_WHITE_ACTIVE
                } else {
                    color::KEY_WHITE
                },
            ));
            if key % 12 == 0 && g.w > 9.0 {
                tracks::text(
                    window,
                    cx,
                    &views::note_name(key),
                    o.x + px(g.x + 2.0),
                    o.y + px(h - 14.0),
                    9.0,
                    color::TEXT_DISABLED,
                    crate::theme::font::MONO,
                    false,
                );
            }
        }
        for key in 21u8..=108 {
            let g = keys[key as usize];
            if !g.black {
                continue;
            }
            let on = active.contains(&key);
            window.paint_quad(fill(
                Bounds::new(
                    point(o.x + px(g.x), o.y + px(hit + 1.0)),
                    gsize(px(g.w), px(kb_h * 0.62)),
                ),
                if on {
                    color::KEY_BLACK_ACTIVE
                } else {
                    color::KEY_BLACK
                },
            ));
        }
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .child(
            pane_header()
                .child(div().flex_1())
                .child(tracks::view_switch(app.ui.main, cx)),
        )
        .child(
            div()
                .id("perform")
                .flex_1()
                .min_h(px(0.0))
                .child(canvas(|_, _, _| {}, paint).size_full())
                // Click the on-screen keys to play.
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, e: &MouseDownEvent, _, cx| {
                        if let Some(key) = key_at(this, e.position) {
                            this.ui.mouse_key = Some(key);
                            this.session.handle_live(
                                LiveEvent::NoteOn { key, velocity: 96 },
                                Instant::now(),
                                true,
                            );
                            cx.notify();
                        }
                    }),
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _: &MouseUpEvent, _, cx| {
                        if let Some(key) = this.ui.mouse_key.take() {
                            this.session.handle_live(
                                LiveEvent::NoteOff { key },
                                Instant::now(),
                                true,
                            );
                            cx.notify();
                        }
                    }),
                ),
        )
}

/// Which key of the on-screen keyboard is under a window point.
fn key_at(app: &SurfApp, p: gpui::Point<Pixels>) -> Option<u8> {
    let b = app.ui.perform_bounds.get();
    let w = f32::from(b.size.width);
    let h = f32::from(b.size.height);
    let kb_h = (h * 0.18).clamp(56.0, 110.0);
    let x = f32::from(p.x - b.origin.x);
    let y = f32::from(p.y - b.origin.y);
    if y < h - kb_h || y > h || x < 0.0 || x > w {
        return None;
    }
    let keys = layout(w);
    let in_black_zone = y < h - kb_h + kb_h * 0.62;
    let hit = |k: u8| {
        let g = keys[k as usize];
        x >= g.x && x <= g.x + g.w
    };
    if in_black_zone && let Some(k) = (21u8..=108).filter(|k| is_black(*k)).find(|k| hit(*k)) {
        return Some(k);
    }
    (21u8..=108).filter(|k| !is_black(*k)).find(|k| hit(*k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_spans_88_keys_edge_to_edge() {
        let keys = layout(1040.0);
        assert!(keys[21].x.abs() < 1e-3, "A0 starts at the left edge");
        let c8 = keys[108];
        assert!(
            (c8.x + c8.w - 1040.0).abs() < 1e-2,
            "C8 ends at the right edge"
        );
        assert!(keys[22].black && keys[22].x > keys[21].x);
    }
}
