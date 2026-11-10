//! Small building blocks shared by every view, styled from [`crate::theme`].
//!
//! They return plain `Div`s so call sites can add `.id(..)`, `.on_click(..)`
//! and sizing the same way as any GPUI element.

use gpui::{
    AppContext, Div, FontWeight, InteractiveElement, IntoElement, ParentElement, Rgba, SharedString,
    Styled, div, prelude::FluentBuilder, px, svg,
};

use crate::icons;
use crate::theme::{SlotColors, color, font, radius, size};

/// How a control shows that it is on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Off,
    /// Accent tint: a toggled tool, a selected tab.
    On,
    /// Solid fills for transport state.
    Play,
    Record,
    Mute,
    Solo,
}

impl Tone {
    pub fn from_bool(on: bool) -> Self {
        if on { Tone::On } else { Tone::Off }
    }

    /// (background, border, foreground)
    fn colors(self) -> (Rgba, Rgba, Rgba) {
        match self {
            Tone::Off => (color::RAISED, color::LINE_STRONG, color::TEXT_SECONDARY),
            Tone::On => (color::ACCENT_BG, color::ACCENT_LINE, color::ACCENT_FG),
            Tone::Play => (color::PLAY, color::PLAY, color::VOID),
            Tone::Record => (color::RECORD, color::RECORD, color::VOID),
            Tone::Mute => (color::MUTE, color::MUTE, color::VOID),
            Tone::Solo => (color::SOLO, color::SOLO, color::VOID),
        }
    }
}

/// Square icon button for the control bar.
pub fn icon_button(icon: &str, tone: Tone) -> Div {
    let (bg, border, fg) = tone.colors();
    div()
        .flex_none()
        .size(px(size::ICON_BUTTON))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::MD))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_color(fg)
        .cursor_pointer()
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(size::ICON))
                .text_color(fg),
        )
}

/// Small icon button for pane headers, 24 px like the segmented controls.
pub fn header_icon(icon: &str) -> Div {
    div()
        .flex_none()
        .size(px(24.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::MD - 1.0))
        .text_color(color::TEXT_SECONDARY)
        .hover(|d| d.bg(color::HOVER).text_color(color::TEXT_PRIMARY))
        .cursor_pointer()
        .child(
            svg()
                .path(icons::path(icon))
                .size(px(size::ICON))
                .text_color(color::TEXT_SECONDARY),
        )
}

/// Hover text: a name and, if it has one, its shortcut.
pub struct Tooltip {
    text: SharedString,
    keys: Option<SharedString>,
}

impl Tooltip {
    pub fn view(
        text: impl Into<SharedString>,
        keys: Option<&'static str>,
        cx: &mut gpui::App,
    ) -> gpui::AnyView {
        let text = text.into();
        cx.new(|_| Tooltip {
            text,
            keys: keys.map(SharedString::from),
        })
        .into()
    }
}

impl gpui::Render for Tooltip {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(radius::MD))
            .bg(color::RAISED)
            .border_1()
            .border_color(color::LINE_STRONG)
            .text_size(px(font::SMALL))
            .text_color(color::TEXT_PRIMARY)
            .child(self.text.clone())
            .when_some(self.keys.clone(), |d, keys| {
                d.child(
                    div()
                        .font_family(font::MONO)
                        .text_color(color::TEXT_TERTIARY)
                        .child(keys),
                )
            })
    }
}

/// Text button for the toolbar and pane headers. `planned` dims it.
pub fn text_button(label: impl Into<SharedString>, planned: bool) -> Div {
    div()
        .flex_none()
        .h(px(24.0))
        .px(px(9.0))
        .flex()
        .items_center()
        .rounded(px(radius::MD - 1.0))
        .text_size(px(font::BODY))
        .text_color(color::TEXT_SECONDARY)
        .when_planned(planned)
        .cursor_pointer()
        .child(label.into())
}

/// Outlined button, as in the inspector and library footers.
pub fn button(label: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .h(px(24.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::MD))
        .bg(color::CHROME)
        .border_1()
        .border_color(color::LINE_STRONG)
        .text_size(px(font::BODY))
        .text_color(color::TEXT_SECONDARY)
        .cursor_pointer()
        .child(label.into())
}

/// Uppercase condensed label (section headers, LCD captions).
pub fn label(text: impl Into<SharedString>) -> Div {
    div()
        .font_family(font::LABEL)
        .font_weight(FontWeight::SEMIBOLD)
        .text_size(px(font::LABEL_SIZE))
        .text_color(color::TEXT_TERTIARY)
        .child(text.into().to_uppercase())
}

/// A pill in a segmented control. Group them in a [`segmented`] row.
pub fn segment(text: impl Into<SharedString>, on: bool) -> Div {
    div()
        .h(px(22.0))
        .px(px(10.0))
        .flex()
        .items_center()
        .text_size(px(font::BODY))
        .text_color(if on {
            color::ACCENT_FG
        } else {
            color::TEXT_SECONDARY
        })
        .when_bg(on.then_some(color::ACCENT_BG))
        .cursor_pointer()
        .child(text.into())
}

pub fn segmented() -> Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .rounded(px(radius::MD))
        .border_1()
        .border_color(color::LINE_STRONG)
        .overflow_hidden()
}

/// Vertical hairline between control groups.
pub fn separator() -> Div {
    div().flex_none().w(px(1.0)).h(px(22.0)).bg(color::LINE)
}

/// A read-only value field (inspector parameters).
pub fn field(value: impl Into<SharedString>) -> Div {
    div()
        .h(px(20.0))
        .px(px(7.0))
        .flex()
        .items_center()
        .min_w(px(0.0))
        .rounded(px(radius::SM))
        .bg(color::FIELD)
        .border_1()
        .border_color(color::LINE)
        .text_size(px(font::SMALL))
        .text_color(color::TEXT_PRIMARY)
        .overflow_hidden()
        .whitespace_nowrap()
        .child(value.into())
}

/// A channel-strip slot (instrument, effect, send, output).
pub fn slot(text: impl Into<SharedString>, colors: Option<SlotColors>) -> Div {
    let (fg, bg, line) = match colors {
        Some(c) => (c.fg, c.bg, c.line),
        None => (color::TEXT_TERTIARY, color::FIELD, color::LINE),
    };
    div()
        .flex_none()
        .h(px(size::STRIP_SLOT_H))
        .px(px(4.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::SM))
        .bg(bg)
        .border_1()
        .border_color(line)
        .text_size(px(10.0))
        .text_color(fg)
        .overflow_hidden()
        .whitespace_nowrap()
        .child(text.into())
}

/// An empty, dashed slot that offers to add something.
pub fn ghost_slot(text: impl Into<SharedString>) -> Div {
    div()
        .flex_none()
        .h(px(size::STRIP_SLOT_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::SM))
        .border_1()
        .border_dashed()
        .border_color(color::LINE_STRONG)
        .text_size(px(10.0))
        .text_color(color::TEXT_DISABLED)
        .cursor_pointer()
        .child(text.into())
}

/// Mute / Solo / Record-arm toggle.
pub fn msr(letter: &'static str, on_tone: Tone, on: bool) -> Div {
    let (bg, border, fg) = if on {
        on_tone.colors()
    } else {
        (color::LANE, color::LINE_STRONG, color::TEXT_TERTIARY)
    };
    div()
        .flex_none()
        .w(px(size::MSR_W))
        .h(px(size::MSR_H))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius::SM))
        .bg(bg)
        .border_1()
        .border_color(border)
        .font_family(font::LABEL)
        .font_weight(FontWeight::BOLD)
        .text_size(px(11.0))
        .text_color(fg)
        .cursor_pointer()
        .child(letter)
}

/// "planned" badge for tabs and toolbar items that are not built yet.
pub fn planned_badge() -> impl IntoElement {
    div()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(3.0))
        .border_1()
        .border_color(color::LINE_STRONG)
        .text_size(px(8.5))
        .text_color(color::TEXT_TERTIARY)
        .child("planned")
}

/// Small style helpers so builders stay readable.
pub trait StyleExt: Styled + Sized {
    /// Planned features are visible but dimmed.
    fn when_planned(self, planned: bool) -> Self {
        if planned { self.opacity(0.4) } else { self }
    }

    fn when_bg(self, bg: Option<Rgba>) -> Self {
        match bg {
            Some(bg) => self.bg(bg),
            None => self,
        }
    }
}

impl<T: Styled + Sized> StyleExt for T {}
