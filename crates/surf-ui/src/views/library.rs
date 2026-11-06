//! Library: the plugin browser. Picking a row loads it as the instrument.

use gpui::{Context, FontWeight, IntoElement, ParentElement, Styled, div, prelude::*, px, svg};

use crate::icons;
use crate::shell::{SurfApp, on_click, pane_header};
use crate::theme::{color, font, radius, size};
use crate::widgets::button;

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let s = &app.session;
    let loaded = s.vst.as_ref().map(|v| v.path.clone());
    let using_builtin = loaded.is_none();

    let mut vst3 = Vec::new();
    let mut vst2 = Vec::new();
    for (index, entry) in s.plugin_scan.iter().enumerate() {
        let selected = loaded.as_ref() == Some(&entry.path);
        let row = item(
            ("lib-plugin", index),
            entry.name.clone(),
            if entry.vst2 { "VST2" } else { "VST3" },
            selected,
        );
        let row = on_click(row, cx, move |a| a.session.load_scanned_plugin(index));
        if entry.vst2 {
            vst2.push(row)
        } else {
            vst3.push(row)
        }
    }

    div()
        .flex()
        .flex_col()
        .flex_none()
        .w(px(size::LIBRARY_W))
        .bg(color::PANEL)
        .border_r_1()
        .border_color(color::LINE)
        .child(
            pane_header()
                .child(title("Library"))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(font::SMALL))
                        .text_color(color::TEXT_TERTIARY)
                        .child("Instrument"),
                ),
        )
        .child(
            div()
                .id("library-list")
                .flex()
                .flex_col()
                .flex_1()
                .min_h(px(0.0))
                .overflow_y_scroll()
                .py(px(4.0))
                .child(category("Surf"))
                .child(on_click(
                    item(
                        "lib-builtin",
                        "Surf Piano".into(),
                        "Built-in",
                        using_builtin,
                    ),
                    cx,
                    |a| a.session.unload_vst(),
                ))
                .child(category("VST3 Instruments"))
                .children(vst3)
                .when(!vst2.is_empty(), |this| {
                    this.child(category("VST2 Instruments")).children(vst2)
                })
                .child(category("CLAP"))
                .child(
                    div()
                        .px(px(12.0))
                        .py(px(4.0))
                        .text_size(px(font::SMALL))
                        .text_color(color::TEXT_DISABLED)
                        .child("Planned"),
                ),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .gap(px(6.0))
                .p(px(8.0))
                .border_t_1()
                .border_color(color::LINE)
                .child(on_click(
                    button("Rescan").id("lib-rescan").flex_1(),
                    cx,
                    |a| a.session.scan_plugins(),
                ))
                .child(on_click(
                    button("Folder…").id("lib-folder").flex_1(),
                    cx,
                    |a| a.session.load_vst_dialog(),
                )),
        )
}

fn title(text: &'static str) -> impl IntoElement {
    div()
        .font_family(font::LABEL)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(font::TITLE))
        .text_color(color::TEXT_PRIMARY)
        .child(text.to_uppercase())
}

fn category(text: &'static str) -> impl IntoElement {
    div()
        .px(px(10.0))
        .pt(px(10.0))
        .pb(px(4.0))
        .font_family(font::LABEL)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(font::LABEL_SIZE))
        .text_color(color::TEXT_TERTIARY)
        .child(text.to_uppercase())
}

fn item(
    id: impl Into<gpui::ElementId>,
    name: String,
    format: &'static str,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .mx(px(4.0))
        .px(px(8.0))
        .py(px(5.0))
        .rounded(px(radius::SM + 1.0))
        .text_color(if selected {
            color::ACCENT_FG
        } else {
            color::TEXT_SECONDARY
        })
        .when(selected, |d| {
            d.bg(color::ACCENT_BG)
                .border_1()
                .border_color(color::ACCENT_LINE)
        })
        .hover(|d| d.bg(color::HOVER))
        .cursor_pointer()
        .child(
            svg()
                .path(icons::path("keys"))
                .size(px(13.0))
                .text_color(if selected {
                    color::ACCENT_FG
                } else {
                    color::TEXT_TERTIARY
                }),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .child(name),
        )
        .child(
            div()
                .text_size(px(9.5))
                .text_color(color::TEXT_TERTIARY)
                .child(format),
        )
}
