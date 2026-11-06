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
        EditorTab::EventList => event_list(app, cx).into_any_element(),
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
/// The edited region's notes as a table, in Logic's bar/beat/div/tick.
/// Click selects (shift adds), the same selection as the Piano Roll.
fn event_list(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let p = &app.session.project;
    let e = &app.ui.edit;
    let region = e.edit_region(p);
    let title = region.map_or("No region".to_string(), |(t, r)| {
        format!("{} · {}", t.name, r.name)
    });
    let channel = region.map_or(0, |(t, _)| t.channel);
    let headers = ["Position", "Status", "Ch", "Num", "Val", "Length"];
    let rows: Vec<_> = region
        .map(|(_, r)| {
            r.notes
                .iter()
                .map(|n| {
                    let id = n.id;
                    let selected = e.notes.contains(&id);
                    let cells = [
                        piano_roll::position_label(p, r.start + n.start),
                        "Note".to_string(),
                        (channel + 1).to_string(),
                        views::note_name(n.key),
                        n.velocity.to_string(),
                        length_label(p, n.len),
                    ];
                    div()
                        .id(SharedString::from(format!("ev-{}", id.0)))
                        .flex()
                        .flex_row()
                        .border_b_1()
                        .border_color(color::LINE_ROW)
                        .when(selected, |d| d.bg(color::ACCENT_BG))
                        .hover(|d| d.bg(color::HOVER))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, _, cx| {
                            let sel = &mut this.ui.edit.notes;
                            if ev.modifiers().shift {
                                if !sel.remove(&id) {
                                    sel.insert(id);
                                }
                            } else {
                                *sel = [id].into();
                            }
                            this.ui.edit.focus = crate::editing::Focus::Roll;
                            cx.notify();
                        }))
                        .children(cells.into_iter().map(move |c| {
                            div()
                                .flex_1()
                                .px(px(10.0))
                                .py(px(4.0))
                                .font_family(font::MONO)
                                .text_size(px(font::SMALL))
                                .text_color(if selected {
                                    color::ACCENT_FG
                                } else {
                                    color::TEXT_SECONDARY
                                })
                                .child(c)
                        }))
                })
                .collect()
        })
        .unwrap_or_default();
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h(px(0.0))
        .child(
            bar()
                .child(div().text_color(color::TEXT_SECONDARY).child("Notes in"))
                .child(div().text_color(color::TEXT_PRIMARY).child(title))
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

/// A length as "bars beats divisions ticks".
fn length_label(p: &surf_core::project::Project, ticks: u64) -> String {
    let tpb = p.ticks_per_beat();
    let per_bar = p.time_signature.0.max(1) as u64;
    let div = (tpb / 4).max(1);
    let beats = ticks / tpb;
    let sub = ticks % tpb;
    format!(
        "{} {} {} {:>3}",
        beats / per_bar,
        beats % per_bar,
        sub / div,
        sub % div
    )
}

fn mixer(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let strips: Vec<_> = app
        .session
        .project
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            strip::render(views::track_strip(app, t, i), Some(size::STRIP_W), cx).into_any_element()
        })
        .collect();
    div()
        .id("mixer")
        .flex()
        .flex_row()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(1.0))
        .bg(color::LINE)
        .overflow_scroll()
        .children(strips)
        .child(div().flex_1().h_full().bg(color::PANEL))
        .child(strip::render(
            views::master_strip(app),
            Some(size::STRIP_W),
            cx,
        ))
}
