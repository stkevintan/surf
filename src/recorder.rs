//! Capture live MIDI into a list of [`RecordedNote`]s.
//!
//! The recorder is deliberately independent of the audio clock: MIDI events
//! carry an `Instant`, which keeps recording accurate even if the UI drops a
//! frame or the audio device changes.

use std::collections::HashMap;
use std::time::Instant;

use crate::song::RecordedNote;

#[derive(Default)]
pub struct Recorder {
    started: Option<Instant>,
    active: HashMap<u8, (f64, u8)>,
    pending_release: Vec<u8>,
    sustain: bool,
    notes: Vec<RecordedNote>,
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_recording(&self) -> bool {
        self.started.is_some()
    }

    pub fn start(&mut self, now: Instant) {
        self.started = Some(now);
        self.active.clear();
        self.pending_release.clear();
        self.notes.clear();
        self.sustain = false;
    }

    pub fn elapsed(&self, now: Instant) -> f64 {
        self.started
            .map(|start| now.saturating_duration_since(start).as_secs_f64())
            .unwrap_or(0.0)
    }

    pub fn note_on(&mut self, key: u8, velocity: u8, at: Instant) {
        let Some(start) = self.started else {
            return;
        };
        let offset = at.saturating_duration_since(start).as_secs_f64();
        // A retrigger without a note-off closes the previous note.
        if let Some((begin, vel)) = self.active.remove(&key) {
            self.notes.push(RecordedNote {
                key,
                velocity: vel,
                start: begin,
                end: offset,
            });
        }
        self.active.insert(key, (offset, velocity));
        self.pending_release.retain(|k| *k != key);
    }

    pub fn note_off(&mut self, key: u8, at: Instant) {
        let Some(start) = self.started else {
            return;
        };
        let offset = at.saturating_duration_since(start).as_secs_f64();
        if self.sustain {
            self.pending_release.push(key);
        } else {
            self.finish_note(key, offset);
        }
    }

    pub fn sustain(&mut self, down: bool, at: Instant) {
        let Some(start) = self.started else {
            return;
        };
        if !down {
            let offset = at.saturating_duration_since(start).as_secs_f64();
            let pending = std::mem::take(&mut self.pending_release);
            for key in pending {
                self.finish_note(key, offset);
            }
        }
        self.sustain = down;
    }

    fn finish_note(&mut self, key: u8, end: f64) {
        if let Some((begin, velocity)) = self.active.remove(&key) {
            self.notes.push(RecordedNote {
                key,
                velocity,
                start: begin,
                end: end.max(begin + 0.01),
            });
        }
    }

    /// Stop recording and return the captured take.
    pub fn finish(&mut self, now: Instant) -> Vec<RecordedNote> {
        let Some(start) = self.started.take() else {
            return Vec::new();
        };
        let end = now.saturating_duration_since(start).as_secs_f64();
        let active: Vec<u8> = self.active.keys().copied().collect();
        for key in active {
            self.finish_note(key, end);
        }
        self.pending_release.clear();
        self.sustain = false;
        let mut notes = std::mem::take(&mut self.notes);
        notes.sort_by(|a, b| a.start.total_cmp(&b.start));
        notes
    }

    pub fn cancel(&mut self) {
        self.started = None;
        self.active.clear();
        self.pending_release.clear();
        self.notes.clear();
        self.sustain = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn captures_a_note() {
        let t0 = Instant::now();
        let mut rec = Recorder::new();
        rec.start(t0);
        rec.note_on(60, 100, t0 + Duration::from_millis(100));
        rec.note_off(60, t0 + Duration::from_millis(600));
        let notes = rec.finish(t0 + Duration::from_millis(700));
        assert_eq!(notes.len(), 1);
        assert!((notes[0].start - 0.1).abs() < 1e-3);
        assert!((notes[0].end - 0.6).abs() < 1e-3);
    }

    #[test]
    fn sustain_defers_note_off() {
        let t0 = Instant::now();
        let mut rec = Recorder::new();
        rec.start(t0);
        rec.sustain(true, t0);
        rec.note_on(64, 90, t0 + Duration::from_millis(10));
        rec.note_off(64, t0 + Duration::from_millis(100));
        rec.sustain(false, t0 + Duration::from_millis(500));
        let notes = rec.finish(t0 + Duration::from_millis(600));
        assert_eq!(notes.len(), 1);
        assert!((notes[0].end - 0.5).abs() < 1e-3);
    }
}
