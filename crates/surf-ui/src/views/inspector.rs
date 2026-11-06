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
    let p = &s.project;
    let region = ui.edit.edit_region(p).map(|(_, r)| r);
    let region_name = region.map_or("–".to_string(), |r| r.name.clone());
    let region_len = region.map_or("–".to_string(), |r| bars_beats(p, r.len));
    let track = ui
        .edit
        .track
        .and_then(|t| p.track(t))
        .or_else(|| p.tracks.first());
    let track_name = track.map_or("–".to_string(), |t| t.name.clone());
    let channel = track.map_or(0, |t| t.channel);
    let track_id = track.map(|t| t.id);

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
                .child(param("Length", field(region_len).font_family(font::MONO))),
        )
    });

    let track_section = section(
        "ins-track",
        format!("Track: {track_name}"),
        ui.track_open,
        cx,
        |a| a.ui.track_open = !a.ui.track_open,
    )
    .when(ui.track_open, |d| {
        d.child(
            params()
                .child(param(
                    "Instrument",
                    field(track.map_or("–".to_string(), |t| views::instrument_name(s, t))),
                ))
                .child(param(
                    "MIDI Channel",
                    channel_stepper(cx, track_id, channel),
                ))
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
                .when_some(
                    track.map(|t| (t, p.tracks.iter().position(|x| x.id == t.id).unwrap_or(0))),
                    |d, (t, i)| d.child(strip::render(views::track_strip(app, t, i), None, cx)),
                )
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
    let s = &app.session;
    let loaded = app
        .ui
        .edit
        .track
        .and_then(|t| s.project.track(t))
        .or_else(|| s.project.tracks.first())
        .and_then(|t| t.instrument.as_ref())
        .is_some_and(|path| s.rack.host(path).is_some());
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

/// A length as "bars beats", Logic style ("4 0" is four bars).
fn bars_beats(p: &surf_core::project::Project, ticks: u64) -> String {
    let beat = p.ticks_per_beat();
    let per_bar = p.time_signature.0.max(1) as u64;
    let beats = ticks / beat;
    format!("{} {} 0 0", beats / per_bar, beats % per_bar)
}

/// − channel + : the MIDI channel the track plays on (1–16).
fn channel_stepper(
    cx: &mut Context<SurfApp>,
    track: Option<surf_core::project::Id>,
    channel: u8,
) -> gpui::Div {
    let step = |cx: &mut Context<SurfApp>, text: &'static str, delta: i8| {
        on_click(
            crate::widgets::button(text)
                .id(gpui::SharedString::from(format!("ch{text}")))
                .px(px(6.0))
                .h(px(20.0)),
            cx,
            move |a| {
                let Some(t) = track else { return };
                let Some(cur) = a.session.project.track(t).map(|t| t.channel) else {
                    return;
                };
                let next = (cur as i8 + delta).clamp(0, 15) as u8;
                if next != cur {
                    crate::editing::set_track(
                        &mut a.session,
                        t,
                        surf_core::edit::TrackField::Channel(next),
                    );
                }
            },
        )
    };
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .child(step(cx, "−", -1))
        .child(
            field(format!("{}", channel + 1))
                .font_family(font::MONO)
                .flex_1(),
        )
        .child(step(cx, "+", 1))
}
