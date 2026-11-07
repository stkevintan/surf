//! The pieces of the window. Each module renders one area from the app state.

pub mod control_bar;
pub mod editor;
pub mod inspector;
pub mod library;
pub mod menu_bar;
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

/// Palette color for a track.
pub fn track_color(t: &surf_core::project::Track) -> Rgba {
    color::TRACKS[t.color % color::TRACKS.len()]
}

/// What a track plays: its plugin's name, or the built-in piano. A plugin
/// that would not load says so, since its track falls back to the piano.
pub fn instrument_name(s: &Session, track: &surf_core::project::Track) -> String {
    match &track.instrument {
        None => "Surf Piano".to_string(),
        Some(path) => {
            let name = s
                .rack
                .host(path)
                .map(|h| h.client.name.clone())
                .unwrap_or_else(|| {
                    path.file_stem()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default()
                });
            if s.rack.failed.contains(path) {
                format!("{name} (offline)")
            } else {
                name
            }
        }
    }
}

/// Meter level of a track: its slot's peak, which a shared plugin's tracks
/// have in common.
pub fn track_peak(s: &Session, track: &surf_core::project::Track) -> f32 {
    if !s.project.audible(track) {
        return 0.0;
    }
    let plan = crate::rack::plan(&s.project, &s.rack.slots, &s.rack.failed);
    s.project
        .tracks
        .iter()
        .position(|t| t.id == track.id)
        .and_then(|i| plan.routes.get(i))
        .map_or(0.0, |r| s.engine.slot_peaks[r.slot as usize])
}

/// Song length in whole bars, at least 1.
pub fn song_bars(s: &Session) -> u64 {
    let bar = s.project.ticks_per_bar().max(1);
    s.project.end().div_ceil(bar).max(1)
}

/// Seconds per beat at the song's tempo.
pub fn beat_secs(s: &Session) -> f64 {
    60.0 / s.bpm().max(1.0)
}

/// Channel strip for a track. `index` is its position, for a stable id.
pub fn track_strip(
    app: &SurfApp,
    track: &surf_core::project::Track,
    index: usize,
) -> strip::StripModel {
    let s = &app.session;
    strip::StripModel {
        id: strip_id(index),
        track: Some(track.id),
        name: track.name.clone(),
        track_color: track_color(track),
        instrument: Some((
            instrument_name(s, track),
            if track.instrument.is_some() {
                color::SLOT_INST_PLUGIN
            } else {
                color::SLOT_INST_BUILTIN
            },
        )),
        effects: Vec::new(),
        output: "Stereo Out",
        volume: track.volume,
        peak: track_peak(s, track),
        mute: track.mute,
        solo: track.solo,
        selected: app.ui.edit.track == Some(track.id),
        master: false,
        fader_bounds: app.ui.fader_bounds.clone(),
    }
}

/// Fader ids must be `'static`; tracks beyond this share the last id.
fn strip_id(index: usize) -> &'static str {
    const IDS: [&str; 16] = [
        "track-1", "track-2", "track-3", "track-4", "track-5", "track-6", "track-7", "track-8",
        "track-9", "track-10", "track-11", "track-12", "track-13", "track-14", "track-15",
        "track-16",
    ];
    IDS[index.min(IDS.len() - 1)]
}

pub fn master_strip(app: &SurfApp) -> strip::StripModel {
    let s = &app.session;
    strip::StripModel {
        id: "master",
        track: None,
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
