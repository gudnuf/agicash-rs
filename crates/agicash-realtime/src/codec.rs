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
//! `v2.74.7` for `realtime.send` on a private channel. Until Task 9 has run
//! against the live stack this constant reads "UNCONFIRMED".
use serde_json::Value;

/// Encode an outbound control/heartbeat/join/leave/access_token frame.
/// `join_ref`/`r#ref` are `Option<&str>` → JSON `null` when `None`.
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
}
