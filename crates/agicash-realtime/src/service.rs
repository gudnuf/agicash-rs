//! Subscription service: owns the connect→join→serve→reconnect lifecycle
//! and the 25s heartbeat/token-refresh timer. Wraps [`PhoenixClient`].
//! Spec §2.5 + §5.4(5,7). Maps every broadcast to a domain
//! [`WalletRealtimeEvent`] on an `async-broadcast` channel the platform
//! shells subscribe to. Emits [`WalletRealtimeEvent::Connected`] on every
//! (re)join (there is **no replay** — the caller refetches state on that
//! signal; spec §5.5).
//!
//! Timer split (no extra deps): native uses `tokio::time`; wasm uses
//! `wasm-bindgen-futures` over a global-`setTimeout`-backed `js_sys::Promise`
//! (the codebase's approved wasm async primitive — we deliberately do NOT
//! pull in `gloo-timers`).
use crate::client::{build_connect_url, JwtSource, PhoenixClient, BACKOFF_MS, HEARTBEAT_MS};
use crate::event::{RealtimeStatus, WalletRealtimeEvent};
use crate::transport::RealtimeTransport;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Builds a fresh transport per (re)connect (mirrors the app's
/// "rebuild channel on resubscribe", spec §2.5). Boxed so native/wasm
/// share the service code.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait TransportFactory: crate::transport::TransportBounds {
    async fn make(&self) -> Box<dyn RealtimeTransport>;
}

/// Newtype so `PhoenixClient<T: RealtimeTransport>` accepts a boxed
/// trait object (the factory hands out `Box<dyn RealtimeTransport>`).
pub struct BoxedTransport(pub Box<dyn RealtimeTransport>);

impl std::fmt::Debug for BoxedTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The inner trait object is opaque; identity is all we can show.
        f.write_str("BoxedTransport(dyn RealtimeTransport)")
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl RealtimeTransport for BoxedTransport {
    async fn connect(&mut self, u: &str) -> Result<(), crate::TransportError> {
        self.0.connect(u).await
    }
    async fn send_text(&mut self, f: String) -> Result<(), crate::TransportError> {
        self.0.send_text(f).await
    }
    async fn recv(&mut self) -> Option<Result<crate::WsFrame, crate::TransportError>> {
        self.0.recv().await
    }
    async fn close(&mut self, c: u16, r: &str) -> Result<(), crate::TransportError> {
        self.0.close(c, r).await
    }
}

/// The wallet realtime subscription service. Construct with [`Self::new`],
/// hand consumers a [`Self::subscribe`] receiver, then drive [`Self::run`]
/// (on a tokio task natively / `spawn_local` on wasm). Call [`Self::stop`]
/// to leave the channel and close the socket.
pub struct WalletRealtimeService {
    url: String,
    user_id: String,
    jwt: Arc<dyn JwtSource>,
    factory: Arc<dyn TransportFactory>,
    tx: async_broadcast::Sender<WalletRealtimeEvent>,
    rx: async_broadcast::Receiver<WalletRealtimeEvent>,
    stop: Arc<AtomicBool>,
    /// Wake the supervisor's `serve_step` race the instant `stop()` is
    /// called (the flag alone is only seen at loop tops, and `serve_step`
    /// can park indefinitely on an idle socket). `async-broadcast` is
    /// already a dep and works on both native + wasm — no new dependency.
    stop_tx: async_broadcast::Sender<()>,
    stop_rx: async_broadcast::Receiver<()>,
}

impl std::fmt::Debug for WalletRealtimeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletRealtimeService")
            .field("url", &self.url)
            .field("user_id", &self.user_id)
            .field("stopped", &self.stop.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl WalletRealtimeService {
    /// `supabase_url` + `anon_key` build the constant socket URL; the
    /// rotating user JWT comes from `jwt` (see [`TokenProviderJwtSource`]).
    #[must_use]
    pub fn new(
        supabase_url: &str,
        anon_key: &str,
        user_id: String,
        jwt: Arc<dyn JwtSource>,
        factory: Arc<dyn TransportFactory>,
    ) -> Self {
        let (mut tx, rx) = async_broadcast::broadcast(256);
        // Don't wedge the supervisor when no consumer is draining yet:
        // overflow drops the oldest item instead of blocking `broadcast`.
        tx.set_overflow(true);
        let (mut stop_tx, stop_rx) = async_broadcast::broadcast(1);
        stop_tx.set_overflow(true);
        Self {
            url: build_connect_url(supabase_url, anon_key),
            user_id,
            jwt,
            factory,
            tx,
            rx,
            stop: Arc::new(AtomicBool::new(false)),
            stop_tx,
            stop_rx,
        }
    }

    /// Clone a receiver for a consumer (FFI bridge / Leptos). Every
    /// consumer sees every event from the moment it clones.
    #[must_use]
    pub fn subscribe(&self) -> async_broadcast::Receiver<WalletRealtimeEvent> {
        self.rx.clone()
    }

    /// Request shutdown. The supervisor finishes its current cycle,
    /// `phx_leave`s + closes the socket, emits
    /// `StatusChanged(Closed)`, and returns from [`Self::run`].
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // Wake an idle/parked supervisor immediately.
        let _ = self.stop_tx.try_broadcast(());
    }

    /// Shared stop flag, so a spawned `run` future and external callers
    /// observe the same shutdown signal.
    #[must_use]
    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Run the connect→join→serve→backoff→reconnect supervisor until
    /// [`Self::stop`]. On native run this on a tokio task; on wasm via
    /// `spawn_local`. The heartbeat timer races the serve loop (25s,
    /// spec §3.7) and each tick also runs the token-refresh push
    /// (§3.9). On every (re)join the client emits `Connected` so the
    /// caller can catch up (no replay, §5.5).
    pub async fn run(&self) {
        let mut attempt = 0usize;
        while !self.stop.load(Ordering::Relaxed) {
            let transport = self.factory.make().await;
            let mut client = PhoenixClient::new(
                BoxedTransport(transport),
                self.url.clone(),
                self.user_id.clone(),
                Arc::clone(&self.jwt),
                self.tx.clone(),
            );

            let result = self.serve_with_heartbeat(&mut client).await;
            let _ = client.leave_and_close().await;

            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            if result.is_ok() {
                attempt = 0;
            } else {
                // The serve cycle failed (socket/channel down) and we are
                // NOT stopping → we will back off and reconnect. Surface
                // `Reconnecting` here so a *transport-level* drop (a bare
                // `recv` error, which `serve_step` propagates without a
                // protocol `phx_close`) still tells the UI it lost the
                // channel — not just the protocol-level `ChannelDown`.
                let _ = self.tx.try_broadcast(WalletRealtimeEvent::StatusChanged(
                    RealtimeStatus::Reconnecting,
                ));
                let idx = attempt.min(BACKOFF_MS.len() - 1);
                self.backoff_sleep(BACKOFF_MS[idx]).await;
                attempt += 1;
            }
        }
        let _ = self
            .tx
            .try_broadcast(WalletRealtimeEvent::StatusChanged(RealtimeStatus::Closed));
    }

    /// Connect+join, then serve inbound frames while a 25s timer
    /// interleaves `send_heartbeat` (heartbeat + token-refresh push,
    /// spec §3.7/§3.9). Returns `Ok(())` if the loop ended because
    /// `stop()` was requested, `Err` on socket/channel failure (caller
    /// applies backoff). Cfg-split timer impl below: `tokio::time`
    /// natively, `wasm-bindgen-futures` on wasm — NO new dependency.
    #[cfg(not(target_arch = "wasm32"))]
    async fn serve_with_heartbeat(
        &self,
        client: &mut PhoenixClient<BoxedTransport>,
    ) -> Result<(), crate::RealtimeError> {
        use futures_util::StreamExt;
        client.connect_and_join().await?;
        let mut hb = tokio::time::interval(std::time::Duration::from_millis(HEARTBEAT_MS));
        hb.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        hb.tick().await; // consume the immediate first tick
        let mut stop_rx = self.stop_rx.clone();
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            tokio::select! {
                biased;
                _ = stop_rx.next() => {
                    return Ok(());
                }
                step = client.serve_step() => {
                    if !step? {
                        return Err(crate::RealtimeError::Transport(
                            crate::TransportError::Closed("recv ended".into()),
                        ));
                    }
                }
                _ = hb.tick() => {
                    client.send_heartbeat().await?;
                }
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    async fn serve_with_heartbeat(
        &self,
        client: &mut PhoenixClient<BoxedTransport>,
    ) -> Result<(), crate::RealtimeError> {
        use futures_util::future::{select, Either};
        use futures_util::StreamExt;
        client.connect_and_join().await?;
        let mut stop_rx = self.stop_rx.clone();

        // What the serve-vs-timer race resolved to. Computing this in an
        // inner scope lets the `client.serve_step()` borrow (held by the
        // pinned race future) end *before* the heartbeat branch needs a
        // second `&mut client` — mirrors `tokio::select!`'s drop of the
        // unselected future, which `futures_util::select` does not do
        // while the result is still being matched.
        enum Tick {
            Served(bool),
            Heartbeat,
            Stop,
        }

        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            // Race serve_step vs the heartbeat timer; the stop signal is
            // folded into the timer side so a parked socket still wakes.
            let tick = {
                let step = std::pin::pin!(client.serve_step());
                // `Box::pin` (alloc only, no new dep) sidesteps the
                // `pin!`-of-temporary lifetime trap inside the inner
                // future: the heap-pinned timer outlives the `.await`.
                let timer = std::pin::pin!(async {
                    let sleep = Box::pin(wasm_sleep_ms(HEARTBEAT_MS));
                    let stop = Box::pin(stop_rx.next());
                    select(sleep, stop).await
                });
                match select(step, timer).await {
                    Either::Left((res, _)) => Tick::Served(res?),
                    Either::Right((Either::Left(((), _)), _)) => Tick::Heartbeat,
                    Either::Right((Either::Right((_, _)), _)) => Tick::Stop,
                }
            };
            match tick {
                Tick::Served(true) => {}
                Tick::Served(false) => {
                    return Err(crate::RealtimeError::Transport(
                        crate::TransportError::Closed("recv ended".into()),
                    ));
                }
                Tick::Heartbeat => client.send_heartbeat().await?,
                Tick::Stop => return Ok(()),
            }
        }
    }

    /// Backoff delay between reconnect attempts that returns early if
    /// `stop()` is called mid-wait (so shutdown isn't blocked for up to
    /// 10s). Native: `tokio::time::sleep`; wasm: `wasm-bindgen-futures`
    /// global-`setTimeout` future — NO new dependency. Both race the
    /// `stop` broadcast.
    async fn backoff_sleep(&self, ms: u64) {
        use futures_util::future::select;
        use futures_util::StreamExt;
        let mut stop_rx = self.stop_rx.clone();
        #[cfg(not(target_arch = "wasm32"))]
        let delay = std::pin::pin!(tokio::time::sleep(std::time::Duration::from_millis(ms)));
        #[cfg(target_arch = "wasm32")]
        let delay = std::pin::pin!(wasm_sleep_ms(ms));
        let _ = select(delay, std::pin::pin!(stop_rx.next())).await;
    }
}

/// Wasm sleep with NO extra dependency: wraps the JS global `setTimeout`
/// in a `js_sys::Promise` and awaits it via `wasm-bindgen-futures`.
/// `setTimeout` is resolved off `js_sys::global()` by reflection so we
/// need neither the `web-sys` `Window` feature nor `gloo-timers`.
#[cfg(target_arch = "wasm32")]
async fn wasm_sleep_ms(ms: u64) {
    use wasm_bindgen::{closure::Closure, JsCast, JsValue};
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let set_timeout = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("setTimeout"))
            .ok()
            .and_then(|v| v.dyn_into::<js_sys::Function>().ok());
        if let Some(set_timeout) = set_timeout {
            // Keep the resolver alive until the timer fires.
            let cb = Closure::once_into_js(move || {
                let _ = resolve.call0(&JsValue::NULL);
            });
            let _ = set_timeout.call2(
                &JsValue::NULL,
                &cb,
                #[allow(clippy::cast_precision_loss)]
                &JsValue::from_f64(ms as f64),
            );
        } else {
            // No timer host (should not happen in a browser/worker):
            // resolve immediately so the supervisor still makes progress.
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// Adapts the codebase `agicash_traits::TokenProvider` (the `OpenSecret`
/// third-party JWT — exactly the realtime join `access_token`, spec
/// §2.2/§5.6) into the realtime [`JwtSource`]. The native bound carries
/// `Send + Sync`; the wasm path drops it — mirrors `SupabaseStorage`'s
/// `tokens` field split (`agicash-storage-supabase/src/client.rs`).
#[cfg(not(target_arch = "wasm32"))]
pub struct TokenProviderJwtSource(pub Arc<dyn agicash_traits::TokenProvider + Send + Sync>);
#[cfg(target_arch = "wasm32")]
pub struct TokenProviderJwtSource(pub Arc<dyn agicash_traits::TokenProvider>);

impl std::fmt::Debug for TokenProviderJwtSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The wrapped provider is an opaque trait object (the OpenSecret
        // token source); we never expose the JWT itself.
        f.write_str("TokenProviderJwtSource(dyn TokenProvider)")
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl JwtSource for TokenProviderJwtSource {
    async fn user_jwt(&self) -> Result<String, crate::RealtimeError> {
        self.0
            .get_jwt()
            .await
            .map_err(|e| crate::RealtimeError::Token(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{RealtimeTransport, WsFrame};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// Scripted fake transport. Inbound frames are dealt one per `recv`;
    /// once drained `recv` parks forever (mirrors a live idle socket) so
    /// the heartbeat timer / `stop()` drive the loop, not EOF.
    struct ScriptTransport {
        outbound: Arc<Mutex<Vec<String>>>,
        inbound: Mutex<VecDeque<WsFrame>>,
        drained_blocks: bool,
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
            if let Some(f) = self.inbound.lock().unwrap().pop_front() {
                return Some(Ok(f));
            }
            if self.drained_blocks {
                // Park: simulate a connected-but-idle socket.
                std::future::pending::<()>().await;
            }
            None
        }
        async fn close(&mut self, _c: u16, _r: &str) -> Result<(), crate::TransportError> {
            Ok(())
        }
    }

    struct FakeFactory {
        outbound: Arc<Mutex<Vec<String>>>,
        script: Mutex<Vec<VecDeque<WsFrame>>>,
        drained_blocks: bool,
    }

    #[async_trait::async_trait]
    impl TransportFactory for FakeFactory {
        async fn make(&self) -> Box<dyn RealtimeTransport> {
            let inbound = self.script.lock().unwrap().pop().unwrap_or_default();
            Box::new(ScriptTransport {
                outbound: Arc::clone(&self.outbound),
                inbound: Mutex::new(inbound),
                drained_blocks: self.drained_blocks,
            })
        }
    }

    struct StubJwt;
    #[async_trait::async_trait]
    impl JwtSource for StubJwt {
        async fn user_jwt(&self) -> Result<String, crate::RealtimeError> {
            Ok("JWT".into())
        }
    }

    fn join_ok() -> WsFrame {
        WsFrame::Text(
            r#"[null,"1","realtime:wallet:u1","phx_reply",{"status":"ok","response":{"postgres_changes":[]}}]"#
                .into(),
        )
    }
    fn broadcast(event: &str) -> WsFrame {
        WsFrame::Text(format!(
            r#"[null,null,"realtime:wallet:u1","broadcast",{{"type":"broadcast","event":"{event}","payload":{{"a":1}}}}]"#
        ))
    }

    #[tokio::test]
    async fn start_emits_connected_then_event_then_stop_leaves() {
        let outbound = Arc::new(Mutex::new(Vec::new()));
        // One connect cycle: join ok → a broadcast → then park idle.
        let factory = Arc::new(FakeFactory {
            outbound: Arc::clone(&outbound),
            script: Mutex::new(vec![VecDeque::from(vec![
                join_ok(),
                broadcast("TRANSACTION_CREATED"),
            ])]),
            drained_blocks: true,
        });
        let svc = Arc::new(WalletRealtimeService::new(
            "http://127.0.0.1:54321",
            "ANON",
            "u1".into(),
            Arc::new(StubJwt),
            factory,
        ));
        let mut rx = svc.subscribe();

        let runner = {
            let svc = Arc::clone(&svc);
            tokio::spawn(async move { svc.run().await })
        };

        // Drive: collect events until we see Connected + the Event.
        let mut saw_connected = false;
        let mut saw_event = false;
        let collect = async {
            while !(saw_connected && saw_event) {
                match futures_util::StreamExt::next(&mut rx).await {
                    Some(WalletRealtimeEvent::Connected) => saw_connected = true,
                    Some(WalletRealtimeEvent::Event(e)) => {
                        assert_eq!(e.event, "TRANSACTION_CREATED");
                        saw_event = true;
                    }
                    Some(_) => {}
                    None => break,
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), collect)
            .await
            .expect("Connected + Event within 5s");
        assert!(saw_connected && saw_event);

        svc.stop();
        // Drain to Closed.
        let drain = async {
            loop {
                match futures_util::StreamExt::next(&mut rx).await {
                    Some(WalletRealtimeEvent::StatusChanged(RealtimeStatus::Closed)) | None => {
                        break
                    }
                    Some(_) => {}
                }
            }
        };
        tokio::time::timeout(std::time::Duration::from_secs(5), drain)
            .await
            .expect("StatusChanged(Closed) after stop()");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), runner).await;

        let out = outbound.lock().unwrap().clone();
        assert!(
            out.iter()
                .any(|f| f.contains(r#""realtime:wallet:u1","phx_join""#)),
            "join was sent: {out:?}"
        );
        assert!(
            out.iter().any(|f| f.contains(r#""phx_leave""#)),
            "phx_leave was sent on stop: {out:?}"
        );
    }

    #[tokio::test]
    async fn reconnect_after_socket_drop_emits_second_connected() {
        let outbound = Arc::new(Mutex::new(Vec::new()));
        // Two cycles (Vec is popped from the end): cycle 1 join-ok then
        // EOF (recv ends → Err → backoff); cycle 2 join-ok then idle.
        let factory = Arc::new(FakeFactory {
            outbound: Arc::clone(&outbound),
            script: Mutex::new(vec![
                VecDeque::from(vec![join_ok()]), // cycle 2 (popped 2nd)
                VecDeque::from(vec![join_ok()]), // cycle 1 (popped 1st), then EOF
            ]),
            drained_blocks: false, // cycle 1 ends via EOF to force reconnect
        });
        let svc = Arc::new(WalletRealtimeService::new(
            "http://127.0.0.1:54321",
            "ANON",
            "u1".into(),
            Arc::new(StubJwt),
            factory,
        ));
        let mut rx = svc.subscribe();
        let runner = {
            let svc = Arc::clone(&svc);
            tokio::spawn(async move { svc.run().await })
        };

        let mut connected = 0;
        let collect = async {
            while connected < 2 {
                match futures_util::StreamExt::next(&mut rx).await {
                    Some(WalletRealtimeEvent::Connected) => connected += 1,
                    Some(_) => {}
                    None => break,
                }
            }
        };
        // Backoff[0] is 1000ms; allow generous slack.
        tokio::time::timeout(std::time::Duration::from_secs(8), collect)
            .await
            .expect("two Connected events (reconnect after drop)");
        assert_eq!(connected, 2, "reconnect produced a 2nd Connected");

        svc.stop();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), runner).await;
    }

    #[tokio::test]
    async fn token_provider_adapter_yields_jwt() {
        use agicash_traits::{AuthError, TokenProvider};
        struct P;
        #[async_trait::async_trait]
        impl TokenProvider for P {
            async fn get_jwt(&self) -> Result<String, AuthError> {
                Ok("JWT123".into())
            }
        }
        let a = TokenProviderJwtSource(Arc::new(P));
        assert_eq!(a.user_jwt().await.unwrap(), "JWT123");
    }
}
