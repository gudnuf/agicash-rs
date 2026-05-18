//! Phoenix serializer v2.0.0 codec.
//!
//! Outbound is always a JSON array `[join_ref, ref, topic, event, payload]`
//! (serializer.ts:36-37). We never push broadcasts, so the binary encode
//! path is intentionally unimplemented. Inbound: JSON string array OR
//! binary kind=4 `userBroadcast` (serializer.ts:126-191).
//!
//! ## Live wire-confirm finding (recorded by Task 9)
//!
//! See the `WIRE_CONFIRM_FINDING` constant below for the observed broadcast
//! frame kind emitted by the running local Supabase Realtime server
//! (`v2.74.7`) for `realtime.send` on a private channel. Until Task 9 has
//! run against the live stack this constant reads `UNCONFIRMED`.
use serde_json::Value;

/// **Live wire-confirm finding (Stage-1 Task 9).**
///
/// The broadcast frame kind the running local Supabase Realtime server
/// `v2.74.7` emits for a `realtime.send(..., is_private => true)` on a
/// private `realtime:wallet:<uid>` channel, observed by
/// `tests/wire_confirm.rs` against the live stack. One of `"Text"` or
/// `"Binary"`. `tests/wire_confirm.rs` hard-asserts the observed wire
/// kind equals this constant, so it cannot silently drift.
///
/// Status: confirmed `Text` (JSON-array frame; see commit message + the
/// captured frame in the test output / fixture below).
pub const WIRE_CONFIRM_FINDING: &str = "Text";

/// Raw frame fixture **captured live** from Supabase Realtime `v2.74.7`
/// for an `ACCOUNT_UPDATED` private broadcast (`realtime.send`,
/// `is_private => true`), recorded by `tests/wire_confirm.rs` on
/// 2026-05-18 against the running local stack. It is a real Phoenix v2
/// **string** frame `[join_ref, ref, topic, event, payload]` with
/// `join_ref=null, ref=null`, Phoenix `event="broadcast"`, and the app
/// payload `{type:"broadcast", event:"ACCOUNT_UPDATED", payload:<account
/// jsonb>, meta:{id:<uuid>}}` — note the live server adds a `meta` key
/// (broadcast id) the spec flags as optional. This proves v2.74.7 uses
/// the **Text** path for `realtime.send`; `decode_text` parses it (the
/// `decode_binary` kind=4 path is kept as defensive coverage per spec
/// §3.2 but is NOT exercised by this server for `realtime.send`).
/// `name`/`id` values are from the live row; structure is verbatim.
pub const WIRE_CONFIRM_TEXT_FIXTURE: &str = r#"[null,null,"realtime:wallet:e50b2cb2-6525-4817-8839-a3bc6e33fd1c","broadcast",{"event":"ACCOUNT_UPDATED","meta":{"id":"d05695a7-a69a-4d3a-b3e2-9a634f958f42"},"payload":{"created_at":"2026-05-18T08:02:18.420218+00:00","currency":"BTC","details":{"keyset_counters":{},"mint_url":"https://testnut.cashu.space"},"expires_at":null,"id":"b49682d1-49b7-47be-a832-9a1dabe312e4","name":"wire-confirm-22e","proofs":[],"purpose":"transactional","state":"active","type":"cashu","user_id":"e50b2cb2-6525-4817-8839-a3bc6e33fd1c","version":1},"type":"broadcast"}]"#;

/// Encode an outbound control / heartbeat / join / leave / `access_token`
/// frame. `join_ref` / `r#ref` are `Option<&str>` → JSON `null` when `None`.
#[must_use]
pub fn encode_outbound(
    join_ref: Option<&str>,
    r#ref: &str,
    topic: &str,
    event: &str,
    payload: Value,
) -> String {
    let arr = Value::Array(vec![
        join_ref.map_or(Value::Null, |s| Value::String(s.to_string())),
        Value::String(r#ref.to_string()),
        Value::String(topic.to_string()),
        Value::String(event.to_string()),
        payload,
    ]);
    arr.to_string()
}

/// Decoded Phoenix message (both text and binary collapse to this).
#[derive(Debug, Clone)]
pub struct PhoenixMessage {
    pub join_ref: Option<String>,
    pub r#ref: Option<String>,
    pub topic: String,
    pub event: String,
    pub payload: Value,
}

/// Decode an inbound STRING frame: JSON `[join_ref, ref, topic, event, payload]`.
pub fn decode_text(s: &str) -> Result<PhoenixMessage, crate::RealtimeError> {
    let v: Value =
        serde_json::from_str(s).map_err(|e| crate::RealtimeError::Codec(format!("json: {e}")))?;
    let arr = v
        .as_array()
        .ok_or_else(|| crate::RealtimeError::Codec("frame is not a JSON array".into()))?;
    if arr.len() != 5 {
        return Err(crate::RealtimeError::Codec(format!(
            "expected 5 elements, got {}",
            arr.len()
        )));
    }
    let opt_str = |x: &Value| x.as_str().map(str::to_string);
    Ok(PhoenixMessage {
        join_ref: opt_str(&arr[0]),
        r#ref: opt_str(&arr[1]),
        topic: arr[2].as_str().unwrap_or_default().to_string(),
        event: arr[3].as_str().unwrap_or_default().to_string(),
        payload: arr[4].clone(),
    })
}

/// Decode an inbound BINARY frame. Only kind=4 (`userBroadcast`) is
/// consumed; every other kind is an error (we never bind presence/CDC).
/// Layout: `[kind][topicSize][userEventSize][metadataSize][payloadEncoding]`
/// then topic, userEvent, metadata bytes, then payload bytes.
pub fn decode_binary(bytes: &[u8]) -> Result<PhoenixMessage, crate::RealtimeError> {
    if bytes.len() < 5 {
        return Err(crate::RealtimeError::Codec("binary < 5 bytes".into()));
    }
    if bytes[0] != 4 {
        return Err(crate::RealtimeError::Codec(format!(
            "unsupported binary kind {}",
            bytes[0]
        )));
    }
    let topic_sz = bytes[1] as usize;
    let event_sz = bytes[2] as usize;
    let meta_sz = bytes[3] as usize;
    let payload_json = bytes[4] == 1;
    let mut off = 5usize;
    let take = |off: &mut usize, n: usize| -> Result<&[u8], crate::RealtimeError> {
        let end = off
            .checked_add(n)
            .filter(|e| *e <= bytes.len())
            .ok_or_else(|| crate::RealtimeError::Codec("binary frame truncated".into()))?;
        let s = &bytes[*off..end];
        *off = end;
        Ok(s)
    };
    let topic = String::from_utf8_lossy(take(&mut off, topic_sz)?).into_owned();
    let user_event = String::from_utf8_lossy(take(&mut off, event_sz)?).into_owned();
    let meta_bytes = take(&mut off, meta_sz)?.to_vec();
    let payload_bytes = &bytes[off..];

    let payload_val = if payload_json {
        serde_json::from_slice::<Value>(payload_bytes)
            .map_err(|e| crate::RealtimeError::Codec(format!("payload json: {e}")))?
    } else {
        Value::String(base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            payload_bytes,
        ))
    };
    let mut env = serde_json::Map::new();
    env.insert("type".into(), Value::String("broadcast".into()));
    env.insert("event".into(), Value::String(user_event));
    env.insert("payload".into(), payload_val);
    if meta_sz > 0 {
        if let Ok(meta) = serde_json::from_slice::<Value>(&meta_bytes) {
            env.insert("meta".into(), meta);
        }
    }
    Ok(PhoenixMessage {
        join_ref: None,
        r#ref: None,
        topic,
        event: "broadcast".into(),
        payload: Value::Object(env),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn encodes_heartbeat_as_json_array() {
        let frame = encode_outbound(Some("1"), "2", "phoenix", "heartbeat", json!({}));
        assert_eq!(frame, r#"["1","2","phoenix","heartbeat",{}]"#);
    }

    #[test]
    fn encodes_join_with_null_join_ref_absent() {
        let frame = encode_outbound(
            Some("1"),
            "1",
            "realtime:wallet:u1",
            "phx_join",
            json!({"private":true}),
        );
        assert_eq!(
            frame,
            r#"["1","1","realtime:wallet:u1","phx_join",{"private":true}]"#
        );
    }

    #[test]
    fn encodes_null_join_ref_as_json_null() {
        let frame = encode_outbound(None, "3", "phoenix", "heartbeat", json!({}));
        assert_eq!(frame, r#"[null,"3","phoenix","heartbeat",{}]"#);
    }

    #[test]
    fn decodes_join_ok_reply() {
        let m = decode_text(
            r#"[null,"1","realtime:wallet:u1","phx_reply",{"status":"ok","response":{"postgres_changes":[]}}]"#,
        )
        .unwrap();
        assert_eq!(m.join_ref, None);
        assert_eq!(m.r#ref.as_deref(), Some("1"));
        assert_eq!(m.topic, "realtime:wallet:u1");
        assert_eq!(m.event, "phx_reply");
        assert_eq!(m.payload["status"], "ok");
    }

    #[test]
    fn decodes_text_broadcast_frame() {
        let m = decode_text(
            r#"[null,null,"realtime:wallet:u1","broadcast",{"type":"broadcast","event":"TRANSACTION_UPDATED","payload":{"id":"abc"}}]"#,
        )
        .unwrap();
        assert_eq!(m.event, "broadcast");
        assert_eq!(m.payload["event"], "TRANSACTION_UPDATED");
        assert_eq!(m.payload["payload"]["id"], "abc");
    }

    #[test]
    fn decode_text_rejects_non_array() {
        assert!(decode_text(r#"{"not":"an array"}"#).is_err());
    }

    #[test]
    fn decodes_binary_kind4_user_broadcast_json_payload() {
        // kind=4; topic="t" (1); userEvent="ACCOUNT_UPDATED" (15);
        // metadata="" (0); payloadEncoding=1 (JSON); payload={"a":1}
        let topic = b"t";
        let user_event = b"ACCOUNT_UPDATED";
        let payload = br#"{"a":1}"#;
        let mut buf = vec![
            4u8,
            u8::try_from(topic.len()).unwrap(),
            u8::try_from(user_event.len()).unwrap(),
            0u8,
            1u8,
        ];
        buf.extend_from_slice(topic);
        buf.extend_from_slice(user_event);
        buf.extend_from_slice(payload);

        let m = decode_binary(&buf).unwrap();
        assert_eq!(m.event, "broadcast");
        assert_eq!(m.payload["type"], "broadcast");
        assert_eq!(m.payload["event"], "ACCOUNT_UPDATED");
        assert_eq!(m.payload["payload"]["a"], 1);
    }

    #[test]
    fn decode_binary_ignores_non_kind4() {
        // kind=0 (push) → we only consume kind 4
        assert!(decode_binary(&[0u8, 0, 0, 0, 0]).is_err());
    }

    #[test]
    fn decode_binary_rejects_truncated() {
        assert!(decode_binary(&[4u8, 9, 9, 9, 1]).is_err());
    }

    /// Stage-1 Task 9 evidence, replayed offline: the frame captured live
    /// from Supabase Realtime v2.74.7 is a Phoenix v2 **Text** frame and
    /// `decode_text` parses it to the expected broadcast shape. Locks the
    /// recorded finding so a future codec change can't silently break the
    /// real wire format.
    #[test]
    fn live_v2_74_7_broadcast_fixture_is_text_and_decodes() {
        assert_eq!(WIRE_CONFIRM_FINDING, "Text");
        let m = decode_text(WIRE_CONFIRM_TEXT_FIXTURE).unwrap();
        assert_eq!(m.join_ref, None);
        assert_eq!(m.r#ref, None);
        assert!(m.topic.starts_with("realtime:wallet:"));
        assert_eq!(m.event, "broadcast");
        assert_eq!(m.payload["type"], "broadcast");
        assert_eq!(m.payload["event"], "ACCOUNT_UPDATED");
        // Live server adds a broadcast-id `meta` (spec §3.6 "optional").
        assert!(m.payload["meta"]["id"].is_string());
        assert_eq!(m.payload["payload"]["type"], "cashu");
        // Routing extracts the app event + opaque payload json (Task 7).
        assert_eq!(
            crate::client::classify(&m, None),
            crate::client::RouterAction::Broadcast {
                event: "ACCOUNT_UPDATED".into(),
                payload_json: m.payload["payload"].to_string(),
            }
        );
    }
}
