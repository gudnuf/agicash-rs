//! Phoenix-channel client state machine (serializer v2.0.0).
//! One private broadcast channel, wildcard events. Spec §5.4.
use crate::codec::{decode_binary, decode_text, PhoenixMessage};
use crate::error::RealtimeError;
use crate::transport::WsFrame;
use serde_json::{json, Value};

const VSN: &str = "2.0.0";
const CLIENT_VERSION: &str = "realtime-js/2.95.2";
pub const HEARTBEAT_MS: u64 = 25_000;
/// Socket-level backoff (spec §3.8 / `RealtimeClient.ts:55-56`), cap 10s.
pub const BACKOFF_MS: &[u64] = &[1000, 2000, 5000, 10000];

/// `wss://<base>/realtime/v1/websocket?apikey=<ANON>&log_level=info&vsn=2.0.0`
#[must_use]
pub fn build_connect_url(supabase_url: &str, anon_key: &str) -> String {
    let base = supabase_url.trim_end_matches('/');
    let ws = base
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    format!("{ws}/realtime/v1/websocket?apikey={anon_key}&log_level=info&vsn={VSN}")
}

#[must_use]
pub fn topic_for_user(user_id: &str) -> String {
    format!("realtime:wallet:{user_id}")
}

/// Spec §3.3 join payload.
#[must_use]
pub fn join_payload(user_jwt: &str) -> Value {
    json!({
        "config": {
            "broadcast": { "ack": false, "self": false },
            "presence": { "key": "", "enabled": false },
            "postgres_changes": [],
            "private": true
        },
        "access_token": user_jwt,
        "version": CLIENT_VERSION
    })
}

#[derive(Debug, Default)]
pub struct RefGen(usize);
impl RefGen {
    // Named `next` to mirror realtime-js `_makeRef` / the plan's call
    // sites; it is intentionally NOT `Iterator::next` (the ref allocator
    // is infinite and never yields `None`).
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> String {
        self.0 += 1;
        self.0.to_string()
    }
}

/// Classify a decoded message into a router action. Pure → unit-testable.
#[derive(Debug, PartialEq, Eq)]
pub enum RouterAction {
    HeartbeatAck,
    JoinReplyOk,
    JoinReplyError(String),
    Broadcast { event: String, payload_json: String },
    ChannelDown,
    Ignore,
}

#[must_use]
pub fn classify(m: &PhoenixMessage, current_join_ref: Option<&str>) -> RouterAction {
    if m.topic == "phoenix" && m.event == "phx_reply" {
        return RouterAction::HeartbeatAck;
    }
    match m.event.as_str() {
        "phx_reply" => match m.payload.get("status").and_then(Value::as_str) {
            Some("ok") => RouterAction::JoinReplyOk,
            _ => RouterAction::JoinReplyError(
                m.payload["response"]["reason"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string(),
            ),
        },
        "broadcast" => {
            let ev = m.payload["event"].as_str().unwrap_or_default().to_string();
            let pj = m.payload["payload"].to_string();
            RouterAction::Broadcast {
                event: ev,
                payload_json: pj,
            }
        }
        "phx_error" | "phx_close" => {
            // Ref-guard (spec §3.8): ignore stale-join close/error.
            if current_join_ref.is_some() && m.join_ref.as_deref() != current_join_ref {
                RouterAction::Ignore
            } else {
                RouterAction::ChannelDown
            }
        }
        // "system" (post-join "Subscribed to broadcast" diagnostic) and
        // every other event are non-actionable for our minimal client.
        _ => RouterAction::Ignore,
    }
}

/// Decode either frame kind into a `PhoenixMessage`.
pub fn decode_frame(f: &WsFrame) -> Result<PhoenixMessage, RealtimeError> {
    match f {
        WsFrame::Text(s) => decode_text(s),
        WsFrame::Binary(b) => decode_binary(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_connect_url_with_apikey_and_vsn_last() {
        let url = build_connect_url("https://127.0.0.1:54321", "ANON");
        assert_eq!(
            url,
            "wss://127.0.0.1:54321/realtime/v1/websocket?apikey=ANON&log_level=info&vsn=2.0.0"
        );
    }

    #[test]
    fn ref_allocator_is_monotonic_strings() {
        let mut r = RefGen::default();
        assert_eq!(r.next(), "1");
        assert_eq!(r.next(), "2");
        assert_eq!(r.next(), "3");
    }

    #[test]
    fn join_payload_matches_spec_3_3() {
        let p = join_payload("USERJWT");
        assert_eq!(p["config"]["private"], true);
        assert_eq!(p["config"]["broadcast"]["ack"], false);
        assert_eq!(p["config"]["broadcast"]["self"], false);
        assert_eq!(p["config"]["presence"]["enabled"], false);
        assert!(p["config"]["postgres_changes"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(p["access_token"], "USERJWT");
        assert_eq!(p["version"], "realtime-js/2.95.2");
    }

    #[test]
    fn topic_for_user_is_realtime_prefixed() {
        assert_eq!(topic_for_user("u1"), "realtime:wallet:u1");
    }

    #[test]
    fn classify_broadcast_extracts_event_and_payload() {
        let m = decode_text(
            r#"[null,null,"realtime:wallet:u1","broadcast",{"type":"broadcast","event":"ACCOUNT_UPDATED","payload":{"x":1}}]"#,
        )
        .unwrap();
        assert_eq!(
            classify(&m, Some("1")),
            RouterAction::Broadcast {
                event: "ACCOUNT_UPDATED".into(),
                payload_json: r#"{"x":1}"#.into()
            }
        );
    }

    #[test]
    fn classify_stale_close_is_ignored() {
        let m = decode_text(r#"["9",null,"realtime:wallet:u1","phx_close",{}]"#).unwrap();
        assert_eq!(classify(&m, Some("1")), RouterAction::Ignore);
    }

    #[test]
    fn classify_heartbeat_reply() {
        let m =
            decode_text(r#"[null,"2","phoenix","phx_reply",{"status":"ok","response":{}}]"#)
                .unwrap();
        assert_eq!(classify(&m, Some("1")), RouterAction::HeartbeatAck);
    }
}
