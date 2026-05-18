//! Stage-2 live subscription integration against local Supabase Realtime
//! v2.74.7. Spec §5.8 steps 2 (broadcast received through the *service*),
//! 4 (delivery continues), 5 (user-JWT rotation survives a re-join),
//! 6 (forced socket drop → reconnect → a SECOND `Connected`), plus the
//! clean-shutdown contract (`stop()` → `StatusChanged(Closed)`).
//! Gated behind `real-supabase-tests` so the default `cargo test
//! --workspace` stays hermetic (mirrors `wire_confirm.rs`).
//!
//! Unlike the Stage-1 `wire_confirm` test (raw transport), this drives
//! the full [`WalletRealtimeService`] supervisor: subscribe a receiver,
//! `run()` on a task, and assert the *domain* events the platform shells
//! will consume.
//!
//! Required env (via dotenvy, identical to `wire_confirm.rs`):
//! `SUPABASE_URL` (or `VITE_*`), `SUPABASE_ANON_KEY` (or `VITE_*`),
//! `SUPABASE_SERVICE_ROLE_KEY`, `SUPABASE_JWT_SECRET`. The user JWT is
//! minted HS256 with the local project secret (no JWT crate dep —
//! `python3` from the dev shell does the HMAC), `sub` set to a seeded
//! `wallet.users` id so the realtime RLS join policy admits it. DB
//! mutations go through the service-role REST endpoint (RLS-bypass) via
//! `curl`, mirroring `user_storage_integration.rs`.
//!
//! Run: `cargo test -p agicash-realtime --features real-supabase-tests
//! --test subscription_integration -- --nocapture`
#![cfg(feature = "real-supabase-tests")]

use agicash_realtime::client::{build_connect_url, JwtSource};
use agicash_realtime::service::{TransportFactory, WalletRealtimeService};
use agicash_realtime::transport::{RealtimeTransport, WsFrame};
use agicash_realtime::transport_native::NativeTransport;
use agicash_realtime::{RealtimeStatus, WalletRealtimeEvent};
use futures_util::StreamExt;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
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

/// Mint an HS256 JWT `{sub, role:"authenticated", exp}` signed with the
/// local project secret. `exp_secs` from now lets the test mint a
/// near-expiry then a fresh token (spec §5.8 step 5). Uses `python3`
/// (in the nix dev shell) so the crate adds no JWT/HMAC dependency.
fn mint_user_jwt(secret: &str, sub: &str, exp_secs: i64) -> String {
    let script = r#"
import sys, hmac, hashlib, base64, json, time
secret = sys.argv[1].encode(); sub = sys.argv[2]; exp = int(sys.argv[3])
b = lambda x: base64.urlsafe_b64encode(x).rstrip(b'=').decode()
h = b(json.dumps({"alg":"HS256","typ":"JWT"},separators=(',',':')).encode())
now = int(time.time())
p = b(json.dumps({"sub":sub,"role":"authenticated","aud":"authenticated","exp":now+exp,"iat":now},separators=(',',':')).encode())
s = b(hmac.new(secret, f"{h}.{p}".encode(), hashlib.sha256).digest())
print(f"{h}.{p}.{s}", end="")
"#;
    let out = Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(secret)
        .arg(sub)
        .arg(exp_secs.to_string())
        .output()
        .expect("python3 to mint JWT");
    assert!(
        out.status.success(),
        "JWT mint failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("utf8 jwt")
}

/// `curl` REST helper (service role, RLS-bypass) — returns the body.
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

/// JWT source that mints a FRESH token on every call (spec §5.8 step 5:
/// the user-JWT rotates; the channel must keep delivering across the
/// re-join that fetches a new token). The first `near_exp` mints emulate
/// a token close to expiry; thereafter long-lived ones. Every call is
/// counted so the test can assert the service really re-fetched.
struct RotatingJwt {
    secret: String,
    sub: String,
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl JwtSource for RotatingJwt {
    async fn user_jwt(&self) -> Result<String, agicash_realtime::RealtimeError> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        // First two fetches: short-lived (still valid for the join, but
        // emulates "near expiry → rotate"); later: long-lived fresh.
        let exp = if n < 2 { 120 } else { 3600 };
        Ok(mint_user_jwt(&self.secret, &self.sub, exp))
    }
}

/// Wraps `NativeTransport` and, once `drop_flag` is set, makes the very
/// next `recv` return a `Closed` error — a deterministic forced socket
/// drop so the supervisor exercises its reconnect/backoff path (spec
/// §5.8 step 6) without depending on killing the realtime container.
struct ForceDropTransport {
    inner: NativeTransport,
    drop_flag: Arc<AtomicBool>,
    armed: bool,
}

#[async_trait::async_trait]
impl RealtimeTransport for ForceDropTransport {
    async fn connect(&mut self, u: &str) -> Result<(), agicash_realtime::TransportError> {
        self.inner.connect(u).await
    }
    async fn send_text(
        &mut self,
        f: String,
    ) -> Result<(), agicash_realtime::TransportError> {
        self.inner.send_text(f).await
    }
    async fn recv(
        &mut self,
    ) -> Option<Result<WsFrame, agicash_realtime::TransportError>> {
        // Only this (the first) connection honours the drop. After the
        // reconnect, `armed` is false so the new socket runs normally.
        // The drop must interrupt a *parked* recv (an idle live socket
        // has no inbound frames), so race the real recv against a poll
        // of the drop flag rather than checking it only on entry.
        use futures_util::future::{select, Either};
        if !self.armed {
            return self.inner.recv().await;
        }
        let flag = Arc::clone(&self.drop_flag);
        let real = std::pin::pin!(self.inner.recv());
        let dropped = std::pin::pin!(async move {
            loop {
                if flag.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        });
        match select(real, dropped).await {
            Either::Left((frame, _)) => frame,
            Either::Right(((), _)) => Some(Err(
                agicash_realtime::TransportError::Closed(
                    "forced drop (integration test)".into(),
                ),
            )),
        }
    }
    async fn close(
        &mut self,
        c: u16,
        r: &str,
    ) -> Result<(), agicash_realtime::TransportError> {
        self.inner.close(c, r).await
    }
}

struct DropFactory {
    drop_flag: Arc<AtomicBool>,
    made: AtomicUsize,
}

#[async_trait::async_trait]
impl TransportFactory for DropFactory {
    async fn make(&self) -> Box<dyn RealtimeTransport> {
        // The first transport is armed for the forced drop; every
        // reconnect after that is a clean NativeTransport.
        let armed = self.made.fetch_add(1, Ordering::SeqCst) == 0;
        Box::new(ForceDropTransport {
            inner: NativeTransport::new(),
            drop_flag: Arc::clone(&self.drop_flag),
            armed,
        })
    }
}

/// PATCH the seeded account name (service role) → fires the
/// `ACCOUNT_UPDATED` trigger → `realtime.send(..., is_private=true)` on
/// `wallet:<user_id>`. Returns the new name so the caller can correlate.
fn fire_account_updated(base: &str, service_role: &str, account_id: &str) -> String {
    let patch_url = format!("{base}/rest/v1/accounts?id=eq.{account_id}");
    let new_name = format!("sub-int-{}", uuid::Uuid::new_v4().simple());
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
    eprintln!("[sub-int] PATCH accounts ({new_name}) → ACCOUNT_UPDATED (resp={resp:?})");
    new_name
}

/// Pull the next event with a timeout, panicking with context on stall.
async fn next_event(
    rx: &mut async_broadcast::Receiver<WalletRealtimeEvent>,
    secs: u64,
    what: &str,
) -> WalletRealtimeEvent {
    match tokio::time::timeout(Duration::from_secs(secs), rx.next()).await {
        Ok(Some(e)) => e,
        Ok(None) => panic!("event stream ended while waiting for {what}"),
        // `timeout` only ever yields `Elapsed`; bind it so clippy's
        // wild-err-arm lint is satisfied without an allow.
        Err(elapsed) => {
            panic!("timed out ({elapsed}) after {secs}s waiting for {what}")
        }
    }
}

// One linear live scenario (connect → broadcast → drop → reconnect →
// rotate → stop); splitting it would scatter the live-evidence narrative
// and re-pay the seed/connect cost per fragment.
#[allow(clippy::too_many_lines)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn db_change_yields_wallet_event_and_reconnect_yields_second_connected() {
    if !env_ready() {
        eprintln!(
            "SKIP: local stack env not present \
             (SUPABASE_URL/ANON_KEY/SERVICE_ROLE_KEY/JWT_SECRET)"
        );
        return;
    }
    let base = env_var("SUPABASE_URL", "VITE_SUPABASE_URL").unwrap();
    let anon = env_var("SUPABASE_ANON_KEY", "VITE_SUPABASE_ANON_KEY").unwrap();
    let service_role = std::env::var("SUPABASE_SERVICE_ROLE_KEY").unwrap();
    let jwt_secret = std::env::var("SUPABASE_JWT_SECRET").unwrap();

    // 1. Find a seeded wallet.accounts row (same fixture as wire_confirm
    //    / user_storage_integration): we mutate an existing account so
    //    the ACCOUNT_UPDATED trigger fires for a user whose id we control
    //    as the JWT `sub` (RLS join policy: topic == 'wallet:'||sub).
    let accounts_url =
        format!("{base}/rest/v1/accounts?select=id,user_id&limit=1");
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
                "no seeded wallet.accounts row (seed the stack first). \
                 REST said: {body}"
            )
        });
    let account_id = row["id"].as_str().expect("account id").to_string();
    let user_id = row["user_id"].as_str().expect("user_id").to_string();
    eprintln!("[sub-int] target user_id={user_id} account_id={account_id}");

    // 2. Build the service: native transport via a factory that arms a
    //    forced drop on the first socket; a JwtSource that rotates the
    //    user JWT on every fetch (join + every reconnect re-fetch).
    let drop_flag = Arc::new(AtomicBool::new(false));
    let factory = Arc::new(DropFactory {
        drop_flag: Arc::clone(&drop_flag),
        made: AtomicUsize::new(0),
    });
    let jwt = Arc::new(RotatingJwt {
        secret: jwt_secret.clone(),
        sub: user_id.clone(),
        calls: AtomicUsize::new(0),
    });
    // Sanity: the URL the service will dial is the live realtime ws.
    assert!(build_connect_url(&base, &anon).contains("/realtime/v1/websocket"));

    let svc = Arc::new(WalletRealtimeService::new(
        &base,
        &anon,
        user_id.clone(),
        Arc::clone(&jwt) as Arc<dyn JwtSource>,
        factory,
    ));
    let mut rx = svc.subscribe();
    let runner = {
        let svc = Arc::clone(&svc);
        tokio::spawn(async move { svc.run().await })
    };

    // 3. First domain event must be Connected (join ok, no replay → the
    //    caller catch-up signal). StatusChanged frames may precede it.
    let mut first_connected = false;
    for _ in 0..10 {
        match next_event(&mut rx, 30, "first Connected").await {
            WalletRealtimeEvent::Connected => {
                first_connected = true;
                break;
            }
            WalletRealtimeEvent::StatusChanged(s) => {
                eprintln!("[sub-int] status (pre-connect): {s:?}");
            }
            WalletRealtimeEvent::Error(e) => panic!("error before join: {e}"),
            WalletRealtimeEvent::Event(e) => {
                panic!("unexpected event before Connected: {}", e.event)
            }
        }
    }
    assert!(first_connected, "service emitted Connected after join");
    eprintln!("[sub-int] ✓ first Connected (join ok, catch-up signal)");

    // 4. Out-of-band DB change → assert a WalletRealtimeEvent::Event with
    //    an ACCOUNT_*/TRANSACTION_* name + non-empty payload arrives.
    fire_account_updated(&base, &service_role, &account_id);
    let mut saw_event = false;
    for _ in 0..12 {
        match next_event(&mut rx, 15, "broadcast Event").await {
            WalletRealtimeEvent::Event(e) => {
                assert!(
                    e.event.starts_with("ACCOUNT_")
                        || e.event.starts_with("TRANSACTION_"),
                    "unexpected event name: {}",
                    e.event
                );
                assert!(
                    !e.payload_json.is_empty() && e.payload_json != "null",
                    "event payload should be non-empty: {:?}",
                    e.payload_json
                );
                eprintln!(
                    "[sub-int] ✓ broadcast through service: {} payload={}",
                    e.event,
                    &e.payload_json[..e.payload_json.len().min(80)]
                );
                saw_event = true;
                break;
            }
            WalletRealtimeEvent::StatusChanged(s) => {
                eprintln!("[sub-int] status: {s:?}");
            }
            WalletRealtimeEvent::Connected => {}
            WalletRealtimeEvent::Error(e) => panic!("error before event: {e}"),
        }
    }
    assert!(saw_event, "DB change delivered as a domain Event");

    // 5+6. Force the socket drop. The supervisor must emit
    //      StatusChanged(Reconnecting), back off, re-mint a FRESH JWT
    //      (RotatingJwt — spec §5.8 step 5 token rotation), re-join, and
    //      emit a SECOND Connected (step 6).
    let jwt_calls_before = jwt.calls.load(Ordering::SeqCst);
    drop_flag.store(true, Ordering::SeqCst);
    eprintln!("[sub-int] forced socket drop armed");

    let mut second_connected = false;
    let mut saw_reconnecting = false;
    for _ in 0..20 {
        match next_event(&mut rx, 20, "second Connected after drop").await {
            WalletRealtimeEvent::StatusChanged(RealtimeStatus::Reconnecting) => {
                saw_reconnecting = true;
                eprintln!("[sub-int] status: Reconnecting (drop observed)");
            }
            WalletRealtimeEvent::StatusChanged(s) => {
                eprintln!("[sub-int] status: {s:?}");
            }
            WalletRealtimeEvent::Connected => {
                second_connected = true;
                break;
            }
            WalletRealtimeEvent::Event(_) | WalletRealtimeEvent::Error(_) => {}
        }
    }
    assert!(
        saw_reconnecting,
        "service signalled Reconnecting on the forced drop"
    );
    assert!(
        second_connected,
        "reconnect produced a SECOND Connected (catch-up)"
    );
    let jwt_calls_after = jwt.calls.load(Ordering::SeqCst);
    assert!(
        jwt_calls_after > jwt_calls_before,
        "reconnect re-fetched a fresh user JWT (rotation): {jwt_calls_before} → {jwt_calls_after}"
    );
    eprintln!(
        "[sub-int] ✓ reconnect → 2nd Connected; JWT re-fetched \
         ({jwt_calls_before}→{jwt_calls_after}, rotation survived)"
    );

    // Delivery continues after the rotation/reconnect: a 2nd DB change
    // must still arrive through the (new) socket joined with a NEW token.
    fire_account_updated(&base, &service_role, &account_id);
    let mut saw_post_rotation_event = false;
    for _ in 0..12 {
        match next_event(&mut rx, 15, "post-reconnect Event").await {
            WalletRealtimeEvent::Event(e) => {
                eprintln!(
                    "[sub-int] ✓ delivery continues post-rotation: {}",
                    e.event
                );
                saw_post_rotation_event = true;
                break;
            }
            other => eprintln!("[sub-int] (post-rotation) {other:?}"),
        }
    }
    assert!(
        saw_post_rotation_event,
        "delivery continued after token rotation + reconnect"
    );

    // 7. Clean shutdown: stop() → phx_leave + close + StatusChanged(Closed).
    svc.stop();
    let mut saw_closed = false;
    for _ in 0..10 {
        match next_event(&mut rx, 10, "StatusChanged(Closed)").await {
            WalletRealtimeEvent::StatusChanged(RealtimeStatus::Closed) => {
                saw_closed = true;
                break;
            }
            other => eprintln!("[sub-int] (draining to Closed) {other:?}"),
        }
    }
    assert!(saw_closed, "stop() emitted StatusChanged(Closed)");
    let _ = tokio::time::timeout(Duration::from_secs(5), runner).await;
    eprintln!(
        "================================================================\n\
         SUB-INTEGRATION OK (Supabase Realtime v2.74.7): service joined,\n\
         delivered a real DB broadcast, survived a forced drop →\n\
         reconnect (2nd Connected) with a rotated user JWT, kept\n\
         delivering, then stopped cleanly (Closed).\n\
         ================================================================"
    );
}
