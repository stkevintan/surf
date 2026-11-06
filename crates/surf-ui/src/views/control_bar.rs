//! Control bar: area toggles, transport, LCD, count-in / metronome, MIDI
//! activity, CPU, and the right-hand window toggles.

use gpui::{Context, FontWeight, IntoElement, ParentElement, Styled, div, prelude::*, px};

use crate::shell::{EditorTab, SurfApp, on_click};
use crate::theme::{color, font, radius, size};
use crate::widgets::{StyleExt, Tone, icon_button, label, separator};

pub fn render(app: &SurfApp, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let s = &app.session;
    let ui = &app.ui;
    let playing = s.engine.playing;
    let recording = s.is_recording();

    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(10.0))
        .h(px(size::CONTROL_BAR_H))
        .px(px(10.0))
        .bg(color::CHROME)
        // Area toggles
        .child(
            group()
                .child(toggle("btn-library", "library", ui.show_library, cx, |a| {
                    a.ui.show_library = !a.ui.show_library
                }))
                .child(toggle(
                    "btn-inspector",
                    "inspector",
                    ui.show_inspector,
                    cx,
                    |a| a.ui.show_inspector = !a.ui.show_inspector,
                ))
                .child(toggle("btn-toolbar", "toolbar", ui.show_toolbar, cx, |a| {
                    a.ui.show_toolbar = !a.ui.show_toolbar
                }))
                .child(planned("btn-smart", "smart", "Smart Controls", cx))
                .child(toggle(
                    "btn-mixer",
                    "mixer",
                    ui.show_editor && ui.tab == EditorTab::Mixer,
                    cx,
                    |a| a.open_tab(EditorTab::Mixer, true),
                ))
                .child(toggle(
                    "btn-editors",
                    "editors",
                    ui.show_editor && ui.tab != EditorTab::Mixer,
                    cx,
                    |a| a.open_tab(EditorTab::PianoRoll, true),
                )),
        )
        .child(separator())
        // Transport
        .child(
            group()
                .child(action("t-rew", "rewind", Tone::Off, cx, |a| {
                    a.seek_bars(-1.0)
                }))
                .child(action("t-fwd", "forward", Tone::Off, cx, |a| {
                    a.seek_bars(1.0)
                }))
                .child(action("t-stop", "stop", Tone::Off, cx, |a| {
                    // Logic: Stop while stopped returns to the start.
                    if !a.session.engine.playing {
                        a.session.seek(0.0);
                    }
                    a.session.stop_playback();
                }))
                .child(action(
                    "t-play",
                    if playing { "pause" } else { "play" },
                    if playing { Tone::Play } else { Tone::Off },
                    cx,
                    |a| a.session.toggle_play(),
                ))
                .child(action(
                    "t-rec",
                    "record",
                    if recording { Tone::Record } else { Tone::Off },
                    cx,
                    |a| a.session.toggle_record(),
                ))
                .child(action(
                    "t-cycle",
                    "cycle",
                    Tone::from_bool(s.loop_playback),
                    cx,
                    |a| a.session.toggle_loop(),
                )),
        )
        .child(lcd(app))
        .child(
            group()
                .child(on_click(
                    div()
                        .id("t-count")
                        .flex_none()
                        .h(px(size::ICON_BUTTON))
                        .px(px(6.0))
                        .flex()
                        .items_center()
                        .rounded(px(radius::MD))
                        .bg(if ui.count_in {
                            color::ACCENT_BG
                        } else {
                            color::RAISED
                        })
                        .border_1()
                        .border_color(if ui.count_in {
                            color::ACCENT_LINE
                        } else {
                            color::LINE_STRONG
                        })
                        .font_family(font::MONO)
                        .font_weight(FontWeight::MEDIUM)
                        .text_size(px(10.0))
                        .text_color(if ui.count_in {
                            color::ACCENT_FG
                        } else {
                            color::TEXT_SECONDARY
                        })
                        .cursor_pointer()
                        .child("1234"),
                    cx,
                    |a| {
                        a.ui.count_in = !a.ui.count_in;
                        a.planned("Count-in");
                    },
                ))
                .child(toggle("t-metro", "metronome", ui.metronome, cx, |a| {
                    a.toggle_metronome()
                })),
        )
        .child(midi_activity(app))
        .child(cpu(app))
        .child(div().flex_1())
        .child(
            group()
                .child(toggle("btn-typing", "typing", ui.musical_typing, cx, |a| {
                    a.toggle_typing()
                }))
                .child(planned("btn-list", "list", "List Editors", cx))
                .child(planned("btn-notepad", "notepad", "Notepad", cx))
                .child(planned("btn-loops", "loops", "Loop Browser", cx)),
        )
}

fn group() -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(3.0))
}

fn toggle(
    id: &'static str,
    icon: &'static str,
    on: bool,
    cx: &mut Context<SurfApp>,
    f: impl Fn(&mut SurfApp) + 'static,
) -> impl IntoElement {
    action(id, icon, Tone::from_bool(on), cx, f)
}

fn action(
    id: &'static str,
    icon: &'static str,
    tone: Tone,
    cx: &mut Context<SurfApp>,
    f: impl Fn(&mut SurfApp) + 'static,
) -> impl IntoElement {
    on_click(icon_button(icon, tone).id(id), cx, f)
}

fn planned(
    id: &'static str,
    icon: &'static str,
    what: &'static str,
    cx: &mut Context<SurfApp>,
) -> impl IntoElement {
    on_click(
        icon_button(icon, Tone::Off).id(id).when_planned(true),
        cx,
        move |a| a.planned(what),
    )
}

/// The LCD: bar/beat/division/tick, time, tempo, signature and key.
fn lcd(app: &SurfApp) -> impl IntoElement {
    let s = &app.session;
    let pos = s.position();
    let bpm = s.bpm().max(1.0);
    let (num, den) = s.time_signature();
    let beats = pos * bpm / 60.0;
    let per_bar = num.max(1) as f64;
    let bar = (beats / per_bar).floor() as u64 + 1;
    let beat = (beats % per_bar).floor() as u64 + 1;
    let div16 = ((beats.fract()) * 4.0).floor() as u64 + 1;
    let tick = ((beats * 4.0).fract() * 240.0).floor() as u64;
    let h = (pos / 3600.0).floor() as u64;
    let m = ((pos / 60.0).floor() as u64) % 60;
    let secs = pos % 60.0;

    div()
        .flex()
        .flex_row()
        .flex_none()
        .h(px(size::LCD_H))
        .rounded(px(radius::LG))
        .overflow_hidden()
        .bg(color::FIELD)
        .border_1()
        .border_color(color::LINE)
        .child(cell(
            format!("{bar} {beat} {div16} {:>3}", tick + 1),
            true,
            "Bar · Beat · Div · Tick",
        ))
        .child(cell(format!("{h:02}:{m:02}:{secs:06.3}"), false, "Time"))
        .child(cell(format!("{bpm:.0}"), true, "Tempo"))
        .child(cell(format!("{num}/{den}"), false, "Signature"))
}

fn cell(value: String, big: bool, caption: &'static str) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .justify_center()
        .gap(px(4.0))
        .px(px(12.0))
        .border_r_1()
        .border_color(color::LINE)
        .child(
            div()
                .font_family(font::MONO)
                .text_size(px(if big { font::LCD } else { font::LCD_SMALL }))
                .text_color(if big {
                    color::ACCENT_FG
                } else {
                    color::TEXT_PRIMARY
                })
                .line_height(px(15.0))
                .child(value),
        )
        .child(label(caption).text_size(px(9.5)))
}

/// Five bars that light up with incoming notes.
fn midi_activity(app: &SurfApp) -> impl IntoElement {
    let s = &app.session;
    let level = s
        .live_notes
        .iter()
        .filter(|n| n.start.elapsed().as_secs_f32() < 0.15)
        .map(|n| n.velocity)
        .max()
        .unwrap_or(0) as f32
        / 127.0;
    let connected = s.midi.selected().is_some();
    let bars = (0..5).map(move |i| {
        let h = if level > 0.0 {
            3.0 + level * 11.0 * (1.0 - i as f32 * 0.15)
        } else {
            3.0
        };
        div()
            .w(px(3.0))
            .h(px(h))
            .rounded(px(1.0))
            .bg(if level > 0.0 {
                color::PLAY
            } else {
                color::LINE_STRONG
            })
    });
    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .gap(px(6.0))
        .child(label("MIDI").when(!connected, |d| d.text_color(color::TEXT_DISABLED)))
        .child(
            div()
                .flex()
                .flex_row()
                .items_end()
                .gap(px(2.0))
                .h(px(14.0))
                .children(bars),
        )
}

/// CPU meter. The engine does not measure load yet, so this shows the
/// output peak as a stand-in and is labelled accordingly in the tooltip.
fn cpu(app: &SurfApp) -> impl IntoElement {
    let peak = app.session.engine.peak.clamp(0.0, 1.0);
    div()
        .flex()
        .flex_col()
        .flex_none()
        .gap(px(4.0))
        .w(px(52.0))
        .child(label("Out"))
        .child(
            div()
                .h(px(4.0))
                .rounded(px(2.0))
                .bg(color::RAISED)
                .overflow_hidden()
                .child(div().h_full().w(px(52.0 * peak)).bg(if peak > 0.9 {
                    color::RECORD
                } else {
                    color::PLAY
                })),
        )
}
