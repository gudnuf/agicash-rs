//! Stage-1 live wire-confirm against local Supabase Realtime v2.74.7.
//! Spec §5.8. Gated behind `real-supabase-tests`.
//!
//! THE RISK THIS RETIRES (plan Risk 1): does v2.74.7 deliver a
//! `realtime.send` private broadcast as a Text JSON-array frame or as a
//! binary `kind=4 userBroadcast` frame? We cannot know without observing
//! the live server, and every layer above the codec depends on the
//! answer. This test connects to the running local stack, joins the
//! private `realtime:wallet:<uid>` channel with a real user JWT, mutates
//! `wallet.accounts` for that user (fires the `ACCOUNT_UPDATED` trigger →
//! `realtime.send(..., is_private=true)`), captures the raw inbound
//! frame, PRINTS whether it was `WsFrame::Text` or `WsFrame::Binary`,
//! decodes it through the codec, and asserts the decoded event name.
//!
//! Required env (via dotenvy): SUPABASE_URL (or VITE_), SUPABASE_ANON_KEY
//! (or VITE_), SUPABASE_SERVICE_ROLE_KEY, SUPABASE_JWT_SECRET. The user
//! JWT is minted HS256 with the local project JWT secret (no JWT crate
//! dependency — `python3` from the dev shell does the HMAC), `sub` set to
//! a seeded `wallet.users` id so the realtime RLS join policy
//! (`realtime.topic() = 'wallet:'||auth.uid()`) admits it. The DB
//! mutation goes through the service-role REST endpoint (RLS-bypass) via
//! `curl`, mirroring `user_storage_integration.rs`'s service-role setup.
//!
//! Run: cargo test -p agicash-realtime --features real-supabase-tests \
//!        --test wire_confirm -- --nocapture
#![cfg(feature = "real-supabase-tests")]

use agicash_realtime::client::{build_connect_url, decode_frame, join_payload, topic_for_user};
use agicash_realtime::codec::encode_outbound;
use agicash_realtime::transport::RealtimeTransport;
use agicash_realtime::transport_native::NativeTransport;
use agicash_realtime::WsFrame;
use std::process::Command;
use std::time::Duration;

fn env_var(primary: &str, vite: &str) -> Option<String> {
    std::env::var(primary)
        .ok()
        .or_else(|| std::env::var(vite).ok())
}

fn env_ready() -> bool {
    let _ = dotenvy::dotenv();
    env_var("SUPABASE_URL", "VITE_SUPABASE_URL").is_some()
        && env_var("SUPABASE_ANON_KEY", "VITE_SUPABASE_ANON_KEY").is_some()
        && std::env::var("SUPABASE_SERVICE_ROLE_KEY").is_ok()
        && std::env::var("SUPABASE_JWT_SECRET").is_ok()
}

/// Mint an HS256 JWT `{sub, role:"authenticated", exp:+1h}` signed with
/// the local project secret. Uses `python3` (in the nix dev shell) so the
/// crate adds no JWT/HMAC dependency.
fn mint_user_jwt(secret: &str, sub: &str) -> String {
    let script = r#"
import sys, hmac, hashlib, base64, json, time
secret = sys.argv[1].encode(); sub = sys.argv[2]
b = lambda x: base64.urlsafe_b64encode(x).rstrip(b'=').decode()
h = b(json.dumps({"alg":"HS256","typ":"JWT"},separators=(',',':')).encode())
now = int(time.time())
p = b(json.dumps({"sub":sub,"role":"authenticated","aud":"authenticated","exp":now+3600,"iat":now},separators=(',',':')).encode())
s = b(hmac.new(secret, f"{h}.{p}".encode(), hashlib.sha256).digest())
print(f"{h}.{p}.{s}", end="")
"#;
    let out = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(secret)
        .arg(sub)
        .output()
        .expect("python3 to mint JWT");
    assert!(
        out.status.success(),
        "JWT mint failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 jwt")
}

/// REST GET via curl (service role, RLS-bypass) — returns response body.
fn curl(args: &[&str]) -> String {
    let out = Command::new("curl")
        .arg("-sk")
        .arg("--max-time")
        .arg("10")
        .args(args)
        .output()
        .expect("curl");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[tokio::test]
async fn capture_join_reply_and_broadcast_encoding() {
    if !env_ready() {
        eprintln!("SKIP: local stack env not present (SUPABASE_URL/ANON_KEY/SERVICE_ROLE_KEY/JWT_SECRET)");
        return;
    }
    let base = env_var("SUPABASE_URL", "VITE_SUPABASE_URL").unwrap();
    let anon = env_var("SUPABASE_ANON_KEY", "VITE_SUPABASE_ANON_KEY").unwrap();
    let service_role = std::env::var("SUPABASE_SERVICE_ROLE_KEY").unwrap();
    let jwt_secret = std::env::var("SUPABASE_JWT_SECRET").unwrap();

    // 1. Find (or refuse without) a seeded wallet.accounts row. We mutate
    //    an existing account so the ACCOUNT_UPDATED trigger fires for a
    //    user whose id we control as the JWT `sub`.
    let accounts_url =
        format!("{base}/rest/v1/accounts?select=id,user_id,name&limit=1");
    let body = curl(&[
        &accounts_url,
        "-H",
        &format!("apikey: {service_role}"),
        "-H",
        &format!("Authorization: Bearer {service_role}"),
        "-H",
        "Accept-Profile: wallet",
    ]);
    let rows: serde_json::Value =
        serde_json::from_str(&body).unwrap_or(serde_json::Value::Null);
    let row = rows
        .as_array()
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "no seeded wallet.accounts row to mutate (run the storage \
                 integration fixture / seed the stack first). REST said: {body}"
            )
        });
    let account_id = row["id"].as_str().expect("account id").to_string();
    let user_id = row["user_id"].as_str().expect("user_id").to_string();
    eprintln!("[wire-confirm] target user_id={user_id} account_id={account_id}");

    // 2. Mint the user JWT (sub == user_id → RLS join policy admits).
    let user_jwt = mint_user_jwt(&jwt_secret, &user_id);

    // 3. Connect + send phx_join (join_ref == ref of this push).
    let url = build_connect_url(&base, &anon);
    eprintln!("[wire-confirm] connecting: {url}");
    let mut t = NativeTransport::new();
    t.connect(&url).await.expect("ws connect");
    let topic = topic_for_user(&user_id);
    let join = encode_outbound(Some("1"), "1", &topic, "phx_join", join_payload(&user_jwt));
    t.send_text(join).await.expect("send phx_join");

    // 4. Read frames until the join is acked, then trigger the DB change
    //    and capture the broadcast frame. Tag every frame Text vs Binary.
    let mut joined = false;
    let mut mutation_sent = false;
    let mut broadcast_kind: Option<&'static str> = None;
    let mut broadcast_event: Option<String> = None;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        let frame = match tokio::time::timeout(Duration::from_secs(10), t.recv()).await {
            Ok(Some(Ok(f))) => f,
            Ok(Some(Err(e))) => panic!("transport error before broadcast: {e}"),
            Ok(None) => panic!("socket ended before broadcast"),
            Err(_) => {
                eprintln!("[wire-confirm] 10s idle; joined={joined} mutation_sent={mutation_sent}");
                continue;
            }
        };

        let (kind, raw_preview): (&'static str, String) = match &frame {
            WsFrame::Text(s) => ("Text", s.chars().take(240).collect()),
            WsFrame::Binary(b) => (
                "Binary",
                format!(
                    "kind_byte={} len={} bytes[..16]={:02x?}",
                    b.first().copied().unwrap_or(0),
                    b.len(),
                    &b[..b.len().min(16)]
                ),
            ),
        };
        eprintln!("[wire-confirm] <<< {kind} frame: {raw_preview}");

        let msg = match decode_frame(&frame) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[wire-confirm] decode_frame failed ({e}) — raw above");
                panic!("codec could not decode a live {kind} frame: {e}");
            }
        };
        eprintln!(
            "[wire-confirm]     decoded: topic={} event={} payload_keys={:?}",
            msg.topic,
            msg.event,
            msg.payload
                .as_object()
                .map(|o| o.keys().cloned().collect::<Vec<_>>())
        );

        if msg.event == "phx_reply" && msg.r#ref.as_deref() == Some("1") {
            let status = msg.payload["status"].as_str().unwrap_or("");
            assert_eq!(
                status, "ok",
                "phx_join was rejected (RLS/auth): {}",
                msg.payload
            );
            joined = true;
            eprintln!("[wire-confirm] JOIN OK on {topic}");
        }

        if joined && !mutation_sent {
            // Fire ACCOUNT_UPDATED: PATCH the account name (service role).
            let patch_url = format!("{base}/rest/v1/accounts?id=eq.{account_id}");
            let new_name = format!("wire-confirm-{}", uuid::Uuid::new_v4().simple());
            let resp = curl(&[
                "-X",
                "PATCH",
                &patch_url,
                "-H",
                &format!("apikey: {service_role}"),
                "-H",
                &format!("Authorization: Bearer {service_role}"),
                "-H",
                "Content-Profile: wallet",
                "-H",
                "Content-Type: application/json",
                "-H",
                "Prefer: return=minimal",
                "-d",
                &format!(r#"{{"name":"{new_name}"}}"#),
            ]);
            eprintln!("[wire-confirm] PATCH accounts → ACCOUNT_UPDATED (resp={resp:?})");
            mutation_sent = true;
        }

        if msg.event == "broadcast" {
            broadcast_kind = Some(kind);
            broadcast_event = msg.payload["event"].as_str().map(str::to_string);
            eprintln!(
                "[wire-confirm] *** BROADCAST CAPTURED *** wire-kind={kind} \
                 app-event={:?}",
                broadcast_event
            );
            break;
        }
    }

    assert!(joined, "never received phx_reply{{status:ok}} for the join");
    let kind = broadcast_kind.expect("no broadcast frame captured within 30s");
    let event = broadcast_event.unwrap_or_default();
    assert!(
        event.starts_with("ACCOUNT_") || event.starts_with("TRANSACTION_"),
        "unexpected broadcast event name: {event}"
    );

    // THE RECORDED FINDING. This line is the answer that retires Risk 1.
    eprintln!(
        "================================================================\n\
         WIRE-CONFIRM RESULT (Supabase Realtime v2.74.7, realtime.send,\n\
         private channel): broadcast delivered as WsFrame::{kind}\n\
         (app event = {event}). Recorded in codec.rs WIRE_CONFIRM_FINDING.\n\
         ================================================================"
    );

    let _ = t.close(1000, "wire-confirm done").await;

    // Hard assertion so the recorded constant in codec.rs cannot silently
    // drift from reality: it MUST equal the observed wire kind.
    assert_eq!(
        kind,
        agicash_realtime::codec::WIRE_CONFIRM_FINDING,
        "codec.rs WIRE_CONFIRM_FINDING ({}) disagrees with the live \
         observed frame kind ({kind}) — update the constant + comment",
        agicash_realtime::codec::WIRE_CONFIRM_FINDING
    );
}
