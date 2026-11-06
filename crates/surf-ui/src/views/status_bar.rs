//! Status bar: audio device, plugin host, the latest message.

use gpui::{Context, IntoElement, ParentElement, Styled, div, px};

use crate::shell::SurfApp;
use crate::theme::{color, font, size};

pub fn render(app: &SurfApp, _cx: &mut Context<SurfApp>) -> impl IntoElement {
    let s = &app.session;
    let engine = &s.engine;
    let device = format!(
        "{} · {} kHz",
        engine.output,
        engine.sample_rate as f32 / 1000.0
    );
    let host = match s.rack.hosts.len() {
        0 => "Built-in piano".to_string(),
        1 => format!("Plugin host: {}", s.rack.hosts[0].client.name),
        n => format!("{n} plugin hosts"),
    };
    let midi = match s
        .midi
        .selected()
        .and_then(|i| s.midi.ports().get(i).cloned())
    {
        Some(port) => format!("MIDI: {port}"),
        None => "No MIDI input".to_string(),
    };

    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(16.0))
        .h(px(size::STATUS_BAR_H))
        .px(px(10.0))
        .bg(color::CHROME)
        .border_t_1()
        .border_color(color::LINE)
        .text_size(px(font::SMALL))
        .text_color(color::TEXT_TERTIARY)
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .flex_none()
                .child(
                    div()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(if engine.sample_accurate {
                            color::PLAY
                        } else {
                            color::WARN
                        }),
                )
                .child(device),
        )
        .child(div().flex_none().child(host))
        .child(div().flex_none().child(midi))
        .child(
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(color::TEXT_SECONDARY)
                .child(s.status.clone()),
        )
}
