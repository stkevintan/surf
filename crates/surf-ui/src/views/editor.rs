//! Bottom editor: Mixer · Piano Roll · Event List, with planned tabs held in
//! place. Resized by the splitter above it; maximize hides the tracks area.

use gpui::{
    Context, FontWeight, IntoElement, ParentElement, SharedString, Styled, div, prelude::*, px, svg,
};

use crate::icons;
use crate::shell::{EditorTab, SurfApp, on_click};
use crate::theme::{color, font, size};
use crate::views::{self, piano_roll, strip};
use crate::widgets::planned_badge;

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let ui = &app.ui;
    let body = match ui.tab {
        EditorTab::PianoRoll => div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.0))
            .child(bar().child(piano_roll::bar(app, cx)))
            .child(piano_roll::render(app, cx))
            .into_any_element(),
        EditorTab::EventList => event_list(app).into_any_element(),
        EditorTab::Mixer => mixer(app, cx).into_any_element(),
    };

    div()
        .flex()
        .flex_col()
        .when(ui.editor_max, |d| d.flex_1())
        .when(!ui.editor_max, |d| {
            // The mixer needs room for a whole channel strip, as in Logic.
            let h = if ui.tab == EditorTab::Mixer {
                ui.editor_h
                    .max(strip::STRIP_MIN_H + size::EDITOR_TABS_H + 2.0)
            } else {
                ui.editor_h
            };
            d.flex_none().h(px(h))
        })
        .min_h(px(0.0))
        .child(
            div()
                .flex()
                .flex_row()
                .flex_none()
                .h(px(size::EDITOR_TABS_H))
                .pl(px(4.0))
                .bg(color::CHROME)
                .border_b_1()
                .border_color(color::LINE)
                .child(tab(
                    "Mixer",
                    ui.tab == EditorTab::Mixer,
                    Some(EditorTab::Mixer),
                    cx,
                ))
                .child(tab(
                    "Piano Roll",
                    ui.tab == EditorTab::PianoRoll,
                    Some(EditorTab::PianoRoll),
                    cx,
                ))
                .child(tab(
                    "Event List",
                    ui.tab == EditorTab::EventList,
                    Some(EditorTab::EventList),
                    cx,
                ))
                .child(tab("Step Sequencer", false, None, cx))
                .child(tab("Score", false, None, cx))
                .child(div().flex_1())
                .child(corner("ed-max", "maximize", cx, |a| {
                    a.ui.editor_max = !a.ui.editor_max
                }))
                .child(corner("ed-close", "close", cx, |a| {
                    a.ui.show_editor = false;
                    a.ui.editor_max = false;
                })),
        )
        .child(body)
}

fn tab(
    name: &'static str,
    selected: bool,
    target: Option<EditorTab>,
    cx: &mut Context<SurfApp>,
) -> impl IntoElement {
    let el = div()
        .id(SharedString::from(format!("tab-{name}")))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(6.0))
        .px(px(12.0))
        .font_family(font::LABEL)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(12.0))
        .text_color(match (selected, target.is_some()) {
            (true, _) => color::TEXT_PRIMARY,
            (false, true) => color::TEXT_TERTIARY,
            (false, false) => color::TEXT_DISABLED,
        })
        .when(selected, |d| d.border_b_2().border_color(color::ACCENT_FG))
        .cursor_pointer()
        .child(name.to_uppercase())
        .when(target.is_none(), |d| d.child(planned_badge()));
    on_click(el, cx, move |a| match target {
        Some(t) => a.open_tab(t, false),
        None => a.planned(name),
    })
}

fn corner(
    id: &'static str,
    icon: &'static str,
    cx: &mut Context<SurfApp>,
    f: impl Fn(&mut SurfApp) + 'static,
) -> impl IntoElement {
    on_click(
        div()
            .id(id)
            .w(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .text_color(color::TEXT_SECONDARY)
            .hover(|d| d.text_color(color::TEXT_PRIMARY))
            .cursor_pointer()
            .child(
                svg()
                    .path(icons::path(icon))
                    .size(px(13.0))
                    .text_color(color::TEXT_SECONDARY),
            ),
        cx,
        f,
    )
}

fn bar() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(10.0))
        .h(px(size::EDITOR_BAR_H))
        .px(px(8.0))
        .bg(color::PANEL)
        .border_b_1()
        .border_color(color::LINE)
}

/// The song's notes as a table, in Logic's bar/beat/div/tick positions.
fn event_list(app: &SurfApp) -> impl IntoElement {
    let s = &app.session;
    let beat_secs = views::beat_secs(s);
    let (per_bar, _) = s.time_signature();
    let per_bar = per_bar.max(1) as f64;
    let position = |secs: f64| {
        let beats = secs / beat_secs;
        let bar = (beats / per_bar).floor() as u64 + 1;
        let beat = (beats % per_bar).floor() as u64 + 1;
        let div = (beats.fract() * 4.0).floor() as u64 + 1;
        let tick = ((beats * 4.0).fract() * 240.0).round() as u64 % 240;
        format!("{bar} {beat} {div} {tick:>3}")
    };
    let length = |secs: f64| {
        let beats = secs / beat_secs;
        let bars = (beats / per_bar).floor() as u64;
        let b = (beats % per_bar).floor() as u64;
        let d = (beats.fract() * 4.0).floor() as u64;
        let t = ((beats * 4.0).fract() * 240.0).round() as u64 % 240;
        format!("{bars} {b} {d} {t:>3}")
    };
    let headers = ["Position", "Status", "Ch", "Num", "Val", "Length"];
    let rows = s.timeline.notes.iter().map(move |n| {
        let cells = [
            position(n.start),
            "Note".to_string(),
            (n.channel + 1).to_string(),
            views::note_name(n.key),
            n.velocity.to_string(),
            length(n.end - n.start),
        ];
        div()
            .flex()
            .flex_row()
            .border_b_1()
            .border_color(color::LINE_ROW)
            .hover(|d| d.bg(color::HOVER))
            .children(cells.into_iter().map(|c| {
                div()
                    .flex_1()
                    .px(px(10.0))
                    .py(px(4.0))
                    .font_family(font::MONO)
                    .text_size(px(font::SMALL))
                    .text_color(color::TEXT_SECONDARY)
                    .child(c)
            }))
    });
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .child(
            bar()
                .child(div().text_color(color::TEXT_SECONDARY).child("Notes in"))
                .child(
                    div()
                        .text_color(color::TEXT_PRIMARY)
                        .child(s.song_name.clone()),
                )
                .child(div().flex_1())
                .child(
                    crate::widgets::label("Filter: Notes · CC · Program · Pitch Bend (planned)")
                        .opacity(0.4),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .bg(color::CHROME)
                .border_b_1()
                .border_color(color::LINE)
                .children(
                    headers
                        .iter()
                        .map(|h| crate::widgets::label(*h).flex_1().px(px(10.0)).py(px(6.0))),
                ),
        )
        .child(
            div()
                .id("event-list")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .children(rows),
        )
}

fn mixer(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    div()
        .id("mixer")
        .flex()
        .flex_row()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(1.0))
        .bg(color::LINE)
        .overflow_scroll()
        .child(strip::render(
            views::track_strip(app, true),
            Some(size::STRIP_W),
            cx,
        ))
        .child(div().flex_1().h_full().bg(color::PANEL))
        .child(strip::render(
            views::master_strip(app),
            Some(size::STRIP_W),
            cx,
        ))
}
