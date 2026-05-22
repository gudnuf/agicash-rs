//! Phoenix-channel client state machine (serializer v2.0.0).
//! One private broadcast channel, wildcard events. Spec §5.4.
use crate::codec::{decode_binary, decode_text, encode_outbound, PhoenixMessage};
use crate::error::RealtimeError;
use crate::event::{RealtimeStatus, WalletEvent, WalletRealtimeEvent};
use crate::transport::{RealtimeTransport, TransportBounds, WsFrame};
use serde_json::{json, Value};
use std::sync::Arc;

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

/// Async token source — re-uses the codebase `TokenProvider` contract
/// (spec §5.6). Returns a fresh user JWT.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait JwtSource: TransportBounds {
    async fn user_jwt(&self) -> Result<String, RealtimeError>;
}

/// Sink the client pushes `WalletRealtimeEvent`s into (an
/// `async-broadcast` sender supplied by the service).
pub type EventSink = async_broadcast::Sender<WalletRealtimeEvent>;

pub struct PhoenixClient<T: RealtimeTransport> {
    transport: T,
    url: String,
    user_id: String,
    refs: RefGen,
    join_ref: Option<String>,
    jwt: Arc<dyn JwtSource>,
    sink: EventSink,
}

impl<T: RealtimeTransport> std::fmt::Debug for PhoenixClient<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PhoenixClient")
            .field("url", &self.url)
            .field("user_id", &self.user_id)
            .field("join_ref", &self.join_ref)
            .finish_non_exhaustive()
    }
}

impl<T: RealtimeTransport> PhoenixClient<T> {
    pub fn new(
        transport: T,
        url: String,
        user_id: String,
        jwt: Arc<dyn JwtSource>,
        sink: EventSink,
    ) -> Self {
        Self {
            transport,
            url,
            user_id,
            refs: RefGen::default(),
            join_ref: None,
            jwt,
            sink,
        }
    }

    /// Connect the socket and send the `phx_join` push (spec §3.3). The
    /// join ack is observed later by [`Self::serve_step`] (it arrives as
    /// an inbound `phx_reply`). Split out from the serve loop so the
    /// service can race the inbound router against its 25s heartbeat
    /// timer (`run_once`'s monolithic loop holds `&mut self` for its
    /// whole lifetime, which a racing `send_heartbeat` cannot share).
    pub async fn connect_and_join(&mut self) -> Result<(), RealtimeError> {
        let _ = self.sink.try_broadcast(WalletRealtimeEvent::StatusChanged(
            RealtimeStatus::Connecting,
        ));
        self.transport.connect(&self.url).await?;

        // Join (spec §3.3): join_ref == ref of this push.
        let jr = self.refs.next();
        self.join_ref = Some(jr.clone());
        let token = self.jwt.user_jwt().await?;
        let topic = topic_for_user(&self.user_id);
        let frame = encode_outbound(Some(&jr), &jr, &topic, "phx_join", join_payload(&token));
        self.transport.send_text(frame).await?;
        Ok(())
    }

    /// Receive and route exactly one inbound frame. Returns `Ok(true)`
    /// to keep serving, `Ok(false)` on a clean socket end, `Err` on a
    /// channel-down / transport error (caller applies backoff + retries).
    /// The 25s heartbeat is driven by the service racing a timer that
    /// calls [`Self::send_heartbeat`] — this fn stays transport-pure.
    pub async fn serve_step(&mut self) -> Result<bool, RealtimeError> {
        let Some(item) = self.transport.recv().await else {
            return Err(RealtimeError::Transport(crate::TransportError::Closed(
                "recv ended".into(),
            )));
        };
        let frame = item?;
        let msg = decode_frame(&frame)?;
        match classify(&msg, self.join_ref.as_deref()) {
            RouterAction::JoinReplyOk => {
                let _ = self.sink.try_broadcast(WalletRealtimeEvent::StatusChanged(
                    RealtimeStatus::Subscribed,
                ));
                // No replay → tell caller to catch up (spec §5.5).
                let _ = self.sink.try_broadcast(WalletRealtimeEvent::Connected);
            }
            RouterAction::JoinReplyError(r) => {
                let _ = self
                    .sink
                    .try_broadcast(WalletRealtimeEvent::Error(format!("join rejected: {r}")));
                return Err(RealtimeError::JoinRejected(r));
            }
            RouterAction::Broadcast {
                event,
                payload_json,
            } => {
                // Emit `Event` (string-shaped) for the refetch-style
                // consumers (FFI bridge, Leptos pump, driver), then
                // `Change` (typed) for the cache layer. Order matters:
                // `Event` first so today's "something changed → refetch"
                // discipline kicks off ahead of the cache delta apply,
                // matching the React app's `useTrackWalletChanges` →
                // typed-handler ordering. Both fires are best-effort
                // (`try_broadcast`) — an overflowing channel drops the
                // oldest, never blocks the supervisor.
                let typed = crate::payload::parse_change(&event, &payload_json);
                let _ = self
                    .sink
                    .try_broadcast(WalletRealtimeEvent::Event(WalletEvent {
                        event,
                        payload_json,
                    }));
                let _ = self
                    .sink
                    .try_broadcast(WalletRealtimeEvent::Change(Box::new(typed)));
            }
            RouterAction::ChannelDown => {
                let _ = self.sink.try_broadcast(WalletRealtimeEvent::StatusChanged(
                    RealtimeStatus::Reconnecting,
                ));
                return Err(RealtimeError::Transport(crate::TransportError::Closed(
                    "channel down".into(),
                )));
            }
            RouterAction::HeartbeatAck | RouterAction::Ignore => {}
        }
        Ok(true)
    }

    /// One connect→join→serve cycle. Returns `Ok(())` on clean stop,
    /// `Err` if the socket dropped (caller applies backoff + retries).
    /// Convenience wrapper over [`Self::connect_and_join`] +
    /// [`Self::serve_step`] for callers that do not interleave a
    /// heartbeat timer (the service does — see `service::run`).
    pub async fn run_once(&mut self) -> Result<(), RealtimeError> {
        self.connect_and_join().await?;
        while self.serve_step().await? {}
        Err(RealtimeError::Transport(crate::TransportError::Closed(
            "recv ended".into(),
        )))
    }

    /// Send one heartbeat + run the token-refresh check (spec §3.7/§3.9).
    /// Called by the service's 25s timer. Fire-and-forget for the token
    /// push (no reply awaited).
    pub async fn send_heartbeat(&mut self) -> Result<(), RealtimeError> {
        let jr = self.join_ref.clone();
        let r = self.refs.next();
        let hb = encode_outbound(jr.as_deref(), &r, "phoenix", "heartbeat", json!({}));
        self.transport.send_text(hb).await?;
        // Token refresh piggyback: re-fetch; push access_token if joined.
        if let Some(jref) = self.join_ref.clone() {
            let token = self.jwt.user_jwt().await?;
            let rr = self.refs.next();
            let at = encode_outbound(
                Some(&jref),
                &rr,
                &topic_for_user(&self.user_id),
                "access_token",
                json!({ "access_token": token }),
            );
            self.transport.send_text(at).await?;
        }
        Ok(())
    }

    pub async fn leave_and_close(&mut self) -> Result<(), RealtimeError> {
        if let Some(jref) = self.join_ref.clone() {
            let r = self.refs.next();
            let leave = encode_outbound(
                Some(&jref),
                &r,
                &topic_for_user(&self.user_id),
                "phx_leave",
                json!({}),
            );
            let _ = self.transport.send_text(leave).await;
        }
        self.transport.close(1000, "client leave").await?;
        Ok(())
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
        let m = decode_text(r#"[null,"2","phoenix","phx_reply",{"status":"ok","response":{}}]"#)
            .unwrap();
        assert_eq!(classify(&m, Some("1")), RouterAction::HeartbeatAck);
    }

    struct ScriptTransport {
        outbound: std::sync::Mutex<Vec<String>>,
        inbound: std::sync::Mutex<std::collections::VecDeque<WsFrame>>,
    }
    #[async_trait::async_trait]
    impl RealtimeTransport for ScriptTransport {
        async fn connect(&mut self, _u: &str) -> Result<(), crate::TransportError> {
            Ok(())
        }
        async fn send_text(&mut self, f: String) -> Result<(), crate::TransportError> {
            self.outbound.lock().unwrap().push(f);
            Ok(())
        }
        async fn recv(&mut self) -> Option<Result<WsFrame, crate::TransportError>> {
            self.inbound.lock().unwrap().pop_front().map(Ok)
        }
        async fn close(&mut self, _c: u16, _r: &str) -> Result<(), crate::TransportError> {
            Ok(())
        }
    }
    struct StubJwt;
    #[async_trait::async_trait]
    impl JwtSource for StubJwt {
        async fn user_jwt(&self) -> Result<String, RealtimeError> {
            Ok("JWT".into())
        }
    }

    #[tokio::test]
    async fn run_once_sends_join_then_emits_connected_and_event() {
        let inbound = std::collections::VecDeque::from(vec![
            WsFrame::Text(
                r#"[null,"1","realtime:wallet:u1","phx_reply",{"status":"ok","response":{"postgres_changes":[]}}]"#.into(),
            ),
            WsFrame::Text(
                r#"[null,null,"realtime:wallet:u1","broadcast",{"type":"broadcast","event":"ACCOUNT_UPDATED","payload":{"a":1}}]"#.into(),
            ),
        ]);
        let t = ScriptTransport {
            outbound: std::sync::Mutex::default(),
            inbound: std::sync::Mutex::new(inbound),
        };
        let (tx, mut rx) = async_broadcast::broadcast(16);
        let mut c = PhoenixClient::new(t, "ws://x".into(), "u1".into(), Arc::new(StubJwt), tx);
        let _ = c.run_once().await; // ends when inbound drains (Err)
        let got: Vec<_> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
        assert!(got
            .iter()
            .any(|e| matches!(e, WalletRealtimeEvent::Connected)));
        assert!(got.iter().any(|e| matches!(
            e, WalletRealtimeEvent::Event(ev) if ev.event == "ACCOUNT_UPDATED"
        )));
        // join frame went out, topic + private + jwt present
        let out = c.transport.outbound.lock().unwrap().clone();
        assert!(out[0].contains(r#""realtime:wallet:u1","phx_join""#));
        assert!(out[0].contains(r#""access_token":"JWT""#));
        assert!(out[0].contains(r#""private":true"#));
    }
}
