//! The pieces of the window. Each module renders one area from the app state.

pub mod control_bar;
pub mod editor;
pub mod inspector;
pub mod library;
pub mod perform;
pub mod piano_roll;
pub mod status_bar;
pub mod strip;
pub mod toolbar;
pub mod tracks;

use gpui::Rgba;

use crate::session::Session;
use crate::shell::SurfApp;
use crate::theme::color;

/// The song shown as a track. There is one until the multi-track model lands.
pub struct TrackModel {
    pub name: String,
    pub instrument: String,
    pub plugin: bool,
    pub color: Rgba,
}

pub fn track_model(s: &Session) -> TrackModel {
    match &s.vst {
        Some(vst) => TrackModel {
            name: vst.name.clone(),
            instrument: vst.name.clone(),
            plugin: true,
            color: color::TRACKS[0],
        },
        None => TrackModel {
            name: "Grand Piano".to_string(),
            instrument: "Surf Piano".to_string(),
            plugin: false,
            color: color::TRACKS[0],
        },
    }
}

/// Song length in whole bars, at least 1.
pub fn song_bars(s: &Session) -> u64 {
    let (num, _) = s.time_signature();
    let beats = s.timeline.duration * s.bpm().max(1.0) / 60.0;
    ((beats / num.max(1) as f64).ceil() as u64).max(1)
}

/// Seconds per beat at the song's tempo.
pub fn beat_secs(s: &Session) -> f64 {
    60.0 / s.bpm().max(1.0)
}

pub fn track_strip(app: &SurfApp, selected: bool) -> strip::StripModel {
    let s = &app.session;
    let t = track_model(s);
    strip::StripModel {
        id: "track-1",
        name: t.name,
        track_color: t.color,
        instrument: Some((
            t.instrument,
            if t.plugin {
                color::SLOT_INST_PLUGIN
            } else {
                color::SLOT_INST_BUILTIN
            },
        )),
        effects: Vec::new(),
        output: "Stereo Out",
        volume: s.volume,
        peak: s.engine.peak,
        mute: false,
        solo: false,
        selected,
        master: false,
        fader_bounds: app.ui.fader_bounds.clone(),
    }
}

pub fn master_strip(app: &SurfApp) -> strip::StripModel {
    let s = &app.session;
    strip::StripModel {
        id: "master",
        name: "Stereo Out".to_string(),
        track_color: color::TEXT_TERTIARY,
        instrument: None,
        effects: Vec::new(),
        output: "Out 1-2",
        volume: s.volume,
        peak: s.engine.peak,
        mute: false,
        solo: false,
        selected: false,
        master: true,
        fader_bounds: app.ui.fader_bounds.clone(),
    }
}

/// Note name in Logic's convention, where middle C (MIDI 60) is C3.
pub fn note_name(key: u8) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!("{}{}", NAMES[(key % 12) as usize], key as i32 / 12 - 2)
}

pub fn is_black(key: u8) -> bool {
    matches!(key % 12, 1 | 3 | 6 | 8 | 10)
}

/// Piano-roll note color for a velocity, interpolated across the stops.
pub fn velocity_color(velocity: u8) -> Rgba {
    let stops = &color::VELOCITY;
    let t = (velocity.clamp(1, 127) as f32 - 1.0) / 126.0 * (stops.len() - 1) as f32;
    let i = (t.floor() as usize).min(stops.len() - 2);
    mix(stops[i], stops[i + 1], t - i as f32)
}

pub fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: a.a + (b.a - a.a) * t,
    }
}

pub fn with_alpha(mut c: Rgba, a: f32) -> Rgba {
    c.a = a;
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logic_note_names() {
        assert_eq!(note_name(60), "C3");
        assert_eq!(note_name(21), "A-1");
        assert_eq!(note_name(108), "C7");
    }

    #[test]
    fn velocity_colors_hit_the_end_stops() {
        assert_eq!(velocity_color(1), color::VELOCITY[0]);
        assert_eq!(
            velocity_color(127),
            color::VELOCITY[color::VELOCITY.len() - 1]
        );
    }
}
