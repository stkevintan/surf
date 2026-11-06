//! Toolbar: common edit commands, planned features, export.

use gpui::{Context, IntoElement, ParentElement, Styled, div, prelude::*, px};

use crate::shell::{self, SurfApp, on_click, on_click_cx, on_click_window};
use crate::theme::{color, size};
use crate::widgets::{label, separator, text_button};

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let rendering = app.session.rendering;
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(10.0))
        .h(px(size::TOOLBAR_H))
        .px(px(10.0))
        .bg(color::PANEL)
        .border_t_1()
        .border_color(color::LINE)
        .child(
            row()
                .child(on_click_window(
                    text_button("Open…", false).id("tb-open"),
                    cx,
                    |a, window, cx| shell::open_file(a, window, cx),
                ))
                .child(on_click_cx(
                    text_button(
                        if app.session.is_dirty() { "Save •" } else { "Save" },
                        false,
                    )
                    .id("tb-save"),
                    cx,
                    |a, cx| shell::save_project(a, cx, false),
                ))
                .child(on_click(
                    text_button("Demo", false).id("tb-demo"),
                    cx,
                    |a| {
                        a.session.load_demo();
                        a.center_roll_on_song();
                    },
                )),
        )
        .child(separator())
        .child(
            row()
                .child(on_click(
                    text_button("Quantize", false).id("tb-quantize"),
                    cx,
                    |a| crate::editing::quantize(&mut a.session, &a.ui.edit),
                ))
                .child(on_click(
                    text_button("Split at Playhead", false).id("tb-split"),
                    cx,
                    |a| crate::editing::split_at_playhead(&mut a.session, &mut a.ui.edit),
                ))
                .child(on_click(
                    text_button("Join", false).id("tb-join"),
                    cx,
                    |a| crate::editing::join(&mut a.session, &mut a.ui.edit),
                ))
                .child(on_click(
                    text_button("New Track", false).id("tb-newtrack"),
                    cx,
                    |a| crate::editing::new_track(&mut a.session, &mut a.ui.edit),
                )),
        )
        .child(separator())
        .child(
            row()
                .child(planned("tb-automation", "Automation", cx))
                .child(planned("tb-bounce", "Bounce in Place", cx))
                .child(planned("tb-smart-tempo", "Smart Tempo", cx))
                .child(planned("tb-markers", "Markers", cx)),
        )
        .child(div().flex_1())
        .child(
            row()
                .child(label("Export").mr(px(6.0)))
                .child(on_click_cx(
                    text_button("MIDI", false).id("tb-export-midi"),
                    cx,
                    shell::export_midi,
                ))
                .child(on_click_cx(
                    text_button(
                        if rendering {
                            "Bouncing…"
                        } else {
                            "Bounce WAV"
                        },
                        false,
                    )
                    .id("tb-export-wav"),
                    cx,
                    shell::export_wav,
                ))
                .child(planned("tb-video", "Video", cx)),
        )
}

fn row() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(3.0))
}

fn planned(id: &'static str, what: &'static str, cx: &mut Context<SurfApp>) -> impl IntoElement {
    on_click(text_button(what, true).id(id), cx, move |a| a.planned(what))
}
