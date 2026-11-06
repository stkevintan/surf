//! Toolbar: planned features and export. Edit commands live in the menu
//! bar and the Tracks header; New Track is in the track list.

use gpui::{Context, IntoElement, ParentElement, Styled, div, prelude::*, px};

use crate::shell::{self, SurfApp, on_click, on_click_cx};
use crate::theme::{color, size};
use crate::widgets::{label, text_button};

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
