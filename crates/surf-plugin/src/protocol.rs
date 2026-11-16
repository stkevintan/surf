//! Control protocol between the UI process and a plugin host.
//!
//! One JSON object per line: requests on the host's stdin (tagged `"cmd"`),
//! events on its stdout (tagged `"event"`). The host sends `Ready` once,
//! unprompted. `Exited` is never sent by the host: the client synthesizes it
//! when stdout closes.
//!
//! Each request carries an `id`, and the host's reply to it carries the same
//! one, so a reply that arrives after its request timed out is never taken as
//! the answer to the next request. Events the host sends on its own (`Ready`,
//! an editor closed by the user) have no id.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenderNote {
    pub key: u8,
    pub velocity: u8,
    pub channel: u8,
    pub start: f64,
    pub end: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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
        /// (channel, controller, value) sent before the first note: the
        /// channel volumes of tracks that share the plugin.
        #[serde(default)]
        controls: Vec<(u8, u8, u8)>,
    },
    Ping,
    Shutdown,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
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

/// A request as sent: the request and the id its reply will carry.
#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    /// Chosen by the client, unique per helper.
    #[serde(default)]
    pub id: u64,
    #[serde(flatten)]
    pub request: HelperRequest,
}

/// An event as sent: the event and, for a reply, the id of its request.
#[derive(Debug, Serialize, Deserialize)]
pub struct Reply {
    /// The request this answers; none for events the host sends on its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(flatten)]
    pub event: HelperEvent,
}

/// Whether an event with id `got` is what a wait for `want` should look at: the
/// reply to that request, or anything when waiting for no request in particular.
pub fn answers(want: Option<u64>, got: Option<u64>) -> bool {
    want.is_none() || got == want
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

    /// Review M9: ids go out with requests and come back with their replies;
    /// events the host sends on its own carry none.
    #[test]
    fn replies_carry_their_request_id() {
        let req = serde_json::to_string(&Request {
            id: 7,
            request: HelperRequest::SaveState {
                path: "a.state".into(),
            },
        })
        .unwrap();
        assert_eq!(req, r#"{"id":7,"cmd":"SaveState","path":"a.state"}"#);
        let back: Request = serde_json::from_str(&req).unwrap();
        assert!(matches!(back.request, HelperRequest::SaveState { .. }) && back.id == 7);

        let reply = serde_json::to_string(&Reply {
            id: Some(7),
            event: HelperEvent::StateSaved {
                bytes: 3,
                path: "a.state".into(),
            },
        })
        .unwrap();
        let back: Reply = serde_json::from_str(&reply).unwrap();
        assert_eq!(back.id, Some(7));
        assert!(matches!(
            back.event,
            HelperEvent::StateSaved { bytes: 3, .. }
        ));

        let own = serde_json::to_string(&Reply {
            id: None,
            event: HelperEvent::EditorClosed,
        })
        .unwrap();
        assert_eq!(own, r#"{"event":"EditorClosed"}"#);
        let back: Reply = serde_json::from_str(r#"{"event":"Pong"}"#).unwrap();
        assert_eq!(back.id, None);
    }

    /// A late reply to request 1 is not the answer to request 2.
    #[test]
    fn a_late_reply_is_not_the_next_answer() {
        assert!(!answers(Some(2), Some(1)));
        assert!(!answers(Some(2), None), "nor an unsolicited event");
        assert!(answers(Some(2), Some(2)));
        assert!(answers(None, None), "Ready, before any request");
    }
}
