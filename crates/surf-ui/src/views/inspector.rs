//! Inspector: Region and Track parameters, then two channel strips (the
//! selected track and Stereo Out), as in Logic.

use gpui::{Context, FontWeight, IntoElement, ParentElement, Styled, div, prelude::*, px, svg};

use crate::icons;
use crate::shell::{SurfApp, on_click};
use crate::theme::{color, font, size};
use crate::views::{self, strip};
use crate::widgets::field;

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let s = &app.session;
    let ui = &app.ui;
    let region_name = s.song_name.clone();
    let len_bars = views::song_bars(s);
    let track = views::track_model(s);

    let region = section(
        "ins-region",
        format!("Region: {region_name}"),
        ui.region_open,
        cx,
        |a| a.ui.region_open = !a.ui.region_open,
    )
    .when(ui.region_open, |d| {
        d.child(
            params()
                .child(param("Mute", field("")))
                .child(param(
                    "Loop",
                    field(if s.loop_playback { "On" } else { "Off" }),
                ))
                .child(param("Quantize", field("Off")))
                .child(param("Transpose", field("0").font_family(font::MONO)))
                .child(param("Velocity", field("0").font_family(font::MONO)))
                .child(param(
                    "Length",
                    field(format!("{len_bars} 0 0 0")).font_family(font::MONO),
                )),
        )
    });

    let track_section = section(
        "ins-track",
        format!("Track: {}", track.name),
        ui.track_open,
        cx,
        |a| a.ui.track_open = !a.ui.track_open,
    )
    .when(ui.track_open, |d| {
        d.child(
            params()
                .child(param("Instrument", field(track.instrument.clone())))
                .child(param("MIDI Channel", field("All").font_family(font::MONO)))
                .child(param("Freeze", field("Off")))
                .child(param("Transpose", field("0").font_family(font::MONO)))
                .child(param("Key Limit", field("C-2  G8").font_family(font::MONO))),
        )
    });

    div()
        .id("inspector")
        .flex()
        .flex_col()
        .flex_none()
        .w(px(size::INSPECTOR_W))
        .bg(color::PANEL)
        .border_r_1()
        .border_color(color::LINE)
        .overflow_y_scroll()
        .child(region)
        .child(instrument_actions(app, cx))
        .child(track_section)
        .child(
            div()
                .flex()
                .flex_row()
                .flex_1()
                .min_h(px(320.0))
                .gap(px(1.0))
                .bg(color::LINE)
                .child(strip::render(views::track_strip(app, true), None, cx))
                .child(strip::render(views::master_strip(app), None, cx)),
        )
}

fn section(
    id: &'static str,
    title: String,
    open: bool,
    cx: &mut Context<SurfApp>,
    toggle: impl Fn(&mut SurfApp) + 'static,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .border_b_1()
        .border_color(color::LINE)
        .child(on_click(
            div()
                .id(id)
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .h(px(size::INSPECTOR_SECTION_H))
                .px(px(8.0))
                .bg(color::CHROME)
                .hover(|d| d.bg(color::HOVER))
                .cursor_pointer()
                .child(
                    svg()
                        .path(icons::path("chevron"))
                        .size(px(10.0))
                        .text_color(color::TEXT_TERTIARY)
                        .when(!open, |s| {
                            s.with_transformation(gpui::Transformation::rotate(gpui::radians(
                                -std::f32::consts::FRAC_PI_2,
                            )))
                        }),
                )
                .child(
                    div()
                        .font_family(font::LABEL)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(px(12.0))
                        .text_color(color::TEXT_PRIMARY)
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .child(title),
                ),
            cx,
            toggle,
        ))
}

fn params() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(5.0))
        .px(px(10.0))
        .pt(px(6.0))
        .pb(px(10.0))
}

fn param(name: &'static str, value: gpui::Div) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .child(
            div()
                .flex_none()
                .w(px(size::INSPECTOR_LABEL_W))
                .text_right()
                .text_size(px(font::SMALL))
                .text_color(color::TEXT_TERTIARY)
                .child(name),
        )
        .child(value.flex_1())
}

/// Plugin controls between the Region and Track sections: open the plugin's
/// own editor and save its state. Only shown when a plugin is loaded.
fn instrument_actions(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let loaded = app.session.vst.is_some();
    div().when(loaded, |d| {
        d.flex()
            .flex_row()
            .gap(px(6.0))
            .p(px(8.0))
            .border_b_1()
            .border_color(color::LINE)
            .child(on_click(
                crate::widgets::button("Open Editor")
                    .id("ins-editor")
                    .flex_1(),
                cx,
                |a| a.session.open_vst_editor(),
            ))
            .child(on_click(
                crate::widgets::button("Save State")
                    .id("ins-save-state")
                    .flex_1(),
                cx,
                |a| a.session.save_vst_state(),
            ))
    })
}
