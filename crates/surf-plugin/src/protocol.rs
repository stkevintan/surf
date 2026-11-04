//! Control protocol between the UI process and a plugin host.
//!
//! One JSON object per line: requests on the host's stdin (tagged `"cmd"`),
//! events on its stdout (tagged `"event"`). The host sends `Ready` once,
//! unprompted. `Exited` is never sent by the host: the client synthesizes it
//! when stdout closes.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderNote {
    pub key: u8,
    pub velocity: u8,
    pub channel: u8,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd")]
pub enum HelperRequest {
    OpenEditor,
    CloseEditor,
    SaveState {
        path: String,
    },
    LoadState {
        path: String,
    },
    Render {
        output: String,
        sample_rate: u32,
        notes: Vec<RenderNote>,
        duration: f64,
        tail_secs: f64,
    },
    Ping,
    Shutdown,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "event")]
pub enum HelperEvent {
    Ready {
        name: String,
        sample_rate: u32,
    },
    Error {
        message: String,
    },
    StateSaved {
        bytes: usize,
        path: String,
    },
    StateLoaded,
    EditorOpened,
    EditorClosed,
    RenderDone {
        path: String,
        duration: f64,
        peak: f32,
    },
    Pong,
    Exited,
}

#[cfg(test)]
mod tests {
    use super::*;

    // The UI and the host are separate binaries; pin the wire format so they
    // cannot drift apart unnoticed.
    #[test]
    fn wire_format_is_stable() {
        let req = serde_json::to_string(&HelperRequest::SaveState {
            path: "a.state".into(),
        })
        .unwrap();
        assert_eq!(req, r#"{"cmd":"SaveState","path":"a.state"}"#);
        let ev: HelperEvent =
            serde_json::from_str(r#"{"event":"Ready","name":"Kontakt 8","sample_rate":48000}"#)
                .unwrap();
        assert!(
            matches!(ev, HelperEvent::Ready { ref name, sample_rate: 48000 } if name == "Kontakt 8")
        );
        let closed = serde_json::to_string(&HelperEvent::EditorClosed).unwrap();
        assert_eq!(closed, r#"{"event":"EditorClosed"}"#);
    }
}
