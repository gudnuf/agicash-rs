//! Long-lived trigger task (Lane B) — the driver's "engine".
//!
//! Per plan §3, the driver is **edge-triggered, not a busy poll.**
//! `ResumptionDriver::start` spawns a single async task that:
//!
//! 1. Subscribes to the [`agicash_realtime::WalletRealtimeService`]
//!    broadcast.
//! 2. Calls [`crate::sweep::run_sweep`] on every relevant trigger
//!    (start / `Connected` / relevant `Event` / foreground edge /
//!    fallback tick).
//! 3. Coalesces a burst of triggers down to **one in-flight sweep + at
//!    most one re-run** (the §3 single-in-flight latch with a dirty
//!    bit).
//! 4. Pauses on [`agicash_wallet::WalletError::Unauthenticated`] —
//!    a logged-out wallet has nothing to sweep — and resumes on the
//!    next foreground notify.
//! 5. Never dies on a `run_sweep` error: each sweep result is logged,
//!    the loop continues.
//!
//! ## Foreground-edge mechanism — `notify_foreground`
//!
//! The realtime broadcast does NOT carry the `set_online`/`set_active`
//! lifecycle (those are in-process `AtomicBool` flips on the service).
//! Two design options were on the table (plan §7 Lane B):
//! - (a) The driver exposes its own `notify_foreground()`; callers
//!   call it alongside `set_online(true)`/`set_active(true)`.
//! - (b) Wrap the realtime service so its lifecycle methods also poke
//!   the driver.
//!
//! We chose **(a)**: the driver's scope (plan §7) is explicitly
//! `crates/agicash-driver/**`. Wrapping the realtime service would
//! either modify another crate (out of scope) or interpose a new
//! type that hides every other method of `WalletRealtimeService`,
//! which is brittle. The `notify_foreground()` shape is a one-line
//! addition for Lane D (FFI) and Lane E (Leptos): wherever they
//! already call `set_realtime_online(true)` / `set_realtime_active(true)`,
//! they additionally call `handle.notify_foreground()`. That is the
//! React analog of `refetchOnWindowFocus` — the React app fires the
//! refetch when the focus event fires, not from inside the realtime
//! service.
//!
//! ## Subscribed-pauses-tick mechanism
//!
//! The realtime broadcast carries `WalletRealtimeEvent::StatusChanged`
//! events; the driver consumes the same broadcast it uses for
//! triggers and tracks `connected: bool` from the most recent
//! status. The fallback tick (`tokio::time::interval`, native-only —
//! Lane C swaps for wasm) is gated on `!connected`. When realtime is
//! `Subscribed`, the tick fires but produces no sweep — the tick is
//! the dead-socket backstop, not a polling loop.
//!
//! ## Native-vs-wasm runtime split
//!
//! The *logic* (subscribe + match + coalesce + retry) is
//! runtime-agnostic. Only the `spawn` primitive + the `interval`
//! timer are gated `#[cfg(not(target_arch = "wasm32"))]` — Lane C
//! swaps in `wasm_bindgen_futures::spawn_local` + a wasm timer.
//! Wasm builds of this crate compile today (the `task` module's
//! spawn entry-point is gated; tests are native-only).

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agicash_realtime::{RealtimeStatus, WalletRealtimeEvent};
use agicash_wallet::{WalletClient, WalletError};

use crate::sweep::{run_sweep_with_config, RetryConfig};
use crate::SweepReport;

/// Default fallback-tick cadence. Operator confirmed (plan §8 Q3):
/// ~90 s, paused while realtime is `Subscribed`.
pub const DEFAULT_FALLBACK_TICK: Duration = Duration::from_secs(90);

/// Default backoff after a sweep error (other than `Unauthenticated`,
/// which is its own pause). Keeps the loop from hot-spinning if the
/// initial `refresh_pending_state()` call is failing repeatedly.
const DEFAULT_ERROR_BACKOFF: Duration = Duration::from_secs(1);

/// Configuration for the trigger task.
///
/// The `RetryConfig` field is forwarded to [`run_sweep_with_config`]
/// for the per-row retry budget (Lane A's contract).
#[derive(Debug, Clone, Copy)]
pub struct DriverConfig {
    /// Per-row retry budget for each sweep (forwarded to Lane A).
    pub retry: RetryConfig,
    /// Fallback-tick cadence — the dead-socket backstop. Paused while
    /// realtime status is `Subscribed`. Set to `None` to disable
    /// entirely (operator opt-out — plan §8 Q3).
    pub fallback_tick: Option<Duration>,
    /// Backoff after a sweep error (other than `Unauthenticated`).
    /// Prevents hot-spinning a failing refresh.
    pub error_backoff: Duration,
}

impl Default for DriverConfig {
    fn default() -> Self {
        Self {
            retry: RetryConfig::default(),
            fallback_tick: Some(DEFAULT_FALLBACK_TICK),
            error_backoff: DEFAULT_ERROR_BACKOFF,
        }
    }
}

/// Abstraction over "the thing the driver invokes on each trigger" so
/// the trigger-task tests can run hermetically without a real
/// `WalletClient`. Lane A's `run_sweep_with_config(&WalletClient, _)`
/// is the production impl ([`WalletClientSweeper`]); tests use a
/// programmable stub.
///
/// **Public** so Lane C / future lanes can plug an alternative impl
/// (e.g. a wasm-specific sweeper) without modifying this crate.
#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
pub trait Sweeper: SweeperBounds {
    /// Run one sweep with the given retry config. Mirrors
    /// `run_sweep_with_config`'s signature — same error semantics.
    /// The driver classifies `Err(WalletError::Unauthenticated)` as
    /// the pause signal; all other errors are logged and the loop
    /// continues.
    async fn sweep(&self, cfg: RetryConfig) -> Result<SweepReport, WalletError>;
}

#[cfg(not(target_arch = "wasm32"))]
pub trait SweeperBounds: Send + Sync {}
#[cfg(not(target_arch = "wasm32"))]
impl<T: Send + Sync> SweeperBounds for T {}

#[cfg(target_arch = "wasm32")]
pub trait SweeperBounds {}
#[cfg(target_arch = "wasm32")]
impl<T> SweeperBounds for T {}

/// The production [`Sweeper`] — wraps a `WalletClient` and calls
/// [`run_sweep_with_config`]. Owned by an `Arc` so the driver task
/// and the caller can both hold it (the driver only needs `&self`).
pub struct WalletClientSweeper {
    client: Arc<WalletClient>,
}

impl std::fmt::Debug for WalletClientSweeper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletClientSweeper")
            .finish_non_exhaustive()
    }
}

impl WalletClientSweeper {
    /// Construct from a shared `WalletClient`. The same `Arc` the
    /// caller uses (FFI / Leptos / CLI) — no clone of the underlying
    /// facade.
    #[must_use]
    pub fn new(client: Arc<WalletClient>) -> Self {
        Self { client }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl Sweeper for WalletClientSweeper {
    async fn sweep(&self, cfg: RetryConfig) -> Result<SweepReport, WalletError> {
        run_sweep_with_config(self.client.as_ref(), cfg).await
    }
}

/// The reason a sweep was scheduled — surfaced to the test harness
/// (and tracing logs) so a test can assert "the `Connected` trigger
/// fired a sweep; the irrelevant `Event` did not."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerReason {
    /// Initial sweep on `start()` — the post-process-death catch-up.
    InitialStart,
    /// Realtime (re)connect — `WalletRealtimeEvent::Connected`.
    RealtimeConnected,
    /// A relevant realtime `Event` — server-side row changed.
    RealtimeEvent,
    /// Foreground edge — caller invoked `notify_foreground`.
    Foreground,
    /// Fallback periodic tick (dead-socket backstop).
    FallbackTick,
    /// The dirty-bit re-run that fires once after an in-flight sweep
    /// completes if any trigger arrived during it.
    Coalesced,
}

/// Shared state between the spawned task and the driver handle. Kept
/// behind an `Arc` so `stop()` and `notify_foreground()` (called from
/// the consumer) can poke a task running on another thread.
struct Shared {
    /// Stop flag — set by `stop()`, observed at every loop top and
    /// before each await point.
    stop: AtomicBool,
    /// "A trigger arrived; please run a sweep." The task drains this
    /// when it picks up a trigger; the dirty-bit re-run path sets it
    /// from the in-flight completion.
    pending: AtomicBool,
    /// "The wallet is logged out; pause." Set when a sweep returns
    /// `Err(WalletError::Unauthenticated)`. Cleared on
    /// `notify_foreground()` (the user came back to the app — likely
    /// signed in again).
    paused: AtomicBool,
    /// Counters — observability + test asserts.
    sweep_count: AtomicUsize,
    /// Bit field of `TriggerReason`s seen; tests poll this to assert
    /// trigger routing. Encoded by `reason_bit`.
    reasons_seen: AtomicU64,
    /// Async waker used by the task to park when no trigger is
    /// pending. A pulse on this channel is the "wake up, check the
    /// `pending` flag" signal — the same pattern `agicash-realtime`
    /// uses for `stop_tx` (see `crates/agicash-realtime/src/service.rs`).
    waker: async_broadcast::Sender<()>,
    /// Kept-alive inactive receiver so `Sender::new_receiver()` always
    /// has a live counterpart. async-broadcast disconnects when the
    /// last receiver is dropped.
    #[allow(dead_code)]
    waker_keepalive: async_broadcast::InactiveReceiver<()>,
}

impl Shared {
    fn new() -> Self {
        let (mut waker, waker_rx) = async_broadcast::broadcast(1);
        waker.set_overflow(true);
        let waker_keepalive = waker_rx.deactivate();
        Self {
            stop: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            sweep_count: AtomicUsize::new(0),
            reasons_seen: AtomicU64::new(0),
            waker,
            waker_keepalive,
        }
    }

    /// Pulse the task waker. Cheap; never blocks; never errors (the
    /// broadcast is overflow=true so a missed pulse is harmless —
    /// `pending` is the durable signal, `waker` is just the wake-up
    /// poke).
    fn poke(&self) {
        let _ = self.waker.try_broadcast(());
    }

    fn mark(&self, reason: TriggerReason) {
        let bit = reason_bit(reason);
        self.reasons_seen.fetch_or(bit, Ordering::Relaxed);
        self.pending.store(true, Ordering::Release);
        self.poke();
    }
}

fn reason_bit(reason: TriggerReason) -> u64 {
    1u64 << match reason {
        TriggerReason::InitialStart => 0,
        TriggerReason::RealtimeConnected => 1,
        TriggerReason::RealtimeEvent => 2,
        TriggerReason::Foreground => 3,
        TriggerReason::FallbackTick => 4,
        TriggerReason::Coalesced => 5,
    }
}

/// Handle returned by [`ResumptionDriver::start`].
///
/// `Arc`-wrapped, `Send + Sync` on native — suitable for FFI (the
/// Lane D handoff) and Leptos consumption (Lane E will use it under
/// `#[cfg(target_arch = "wasm32")]` once Lane C ships the wasm
/// runtime split; the *type* of the handle is uniform either way).
///
/// `stop()` is idempotent and drains the in-flight sweep gracefully
/// (the loop checks `stop` at every await point; the in-flight
/// sweep itself runs to completion).
pub struct DriverHandle {
    shared: Arc<Shared>,
}

impl DriverHandle {
    /// Idempotent stop. Sets the stop flag, wakes the task. The
    /// in-flight sweep (if any) runs to completion; the loop exits
    /// at its next check.
    ///
    /// Calling this on an already-stopped handle is a no-op.
    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.poke();
    }

    /// Notify the driver of a foreground / online edge. This is the
    /// React `refetchOnWindowFocus` analog and the design point
    /// documented in this module's header:
    ///
    /// - Lane D (FFI): wire `set_realtime_online(true)` and
    ///   `set_realtime_active(true)` to also call this.
    /// - Lane E (Leptos): wire the `visibilitychange` / `online` /
    ///   `offline` listeners to also call this.
    ///
    /// A foreground notify ALSO clears the `paused` flag — if the
    /// previous sweep saw `Unauthenticated`, this is the "the user
    /// came back; try again" signal (likely they signed back in).
    pub fn notify_foreground(&self) {
        // Clear pause first: a foreground edge is the resume signal.
        self.shared.paused.store(false, Ordering::Release);
        self.shared.mark(TriggerReason::Foreground);
    }

    /// Whether the driver is currently paused on `Unauthenticated`.
    /// Test/observability helper — not in the React API.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.shared.paused.load(Ordering::Acquire)
    }

    /// Count of sweeps actually executed (not counting trigger
    /// arrivals — coalescing means N triggers ≤ N+1 sweeps). Test
    /// hook.
    #[must_use]
    pub fn sweep_count(&self) -> usize {
        self.shared.sweep_count.load(Ordering::Acquire)
    }

    /// Whether the driver has stopped. Test hook.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        self.shared.stop.load(Ordering::Acquire)
    }

    /// Whether a given trigger reason has fired at least once since
    /// `start()`. Test hook used to assert routing ("relevant `Event`
    /// produced a sweep; irrelevant `Event` did not").
    #[must_use]
    pub fn has_seen(&self, reason: TriggerReason) -> bool {
        let bit = reason_bit(reason);
        self.shared.reasons_seen.load(Ordering::Acquire) & bit != 0
    }
}

impl std::fmt::Debug for DriverHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DriverHandle")
            .field("stopped", &self.is_stopped())
            .field("paused", &self.is_paused())
            .field("sweep_count", &self.sweep_count())
            .finish_non_exhaustive()
    }
}

/// The resumption driver — the §3 trigger task wrapper.
///
/// Construction is split from `start()` so:
/// - tests can build a `ResumptionDriver` with a stub sweeper +
///   manually-driven broadcast channel; and
/// - the same code path serves both the production wiring
///   (`agicash-realtime` broadcast) and the test wiring (a hand-built
///   channel) without a conditional.
///
/// The `start()` method:
/// - is idempotent (calling twice does NOT spawn twice; the second
///   call is a no-op and returns the *existing* handle); and
/// - returns a [`DriverHandle`] for the caller to control the task.
///
/// **Native-only**: the spawn uses `tokio::spawn`. Lane C swaps for
/// `wasm_bindgen_futures::spawn_local` (and the fallback tick for a
/// wasm-friendly timer). Wasm `cargo check` of this crate compiles
/// because the spawn entry-point is gated.
pub struct ResumptionDriver<S: Sweeper + 'static> {
    sweeper: Arc<S>,
    rx: async_broadcast::Receiver<WalletRealtimeEvent>,
    config: DriverConfig,
    /// Set on the first `start()`; idempotent.
    handle: std::sync::Mutex<Option<DriverHandle>>,
}

impl<S: Sweeper + 'static> std::fmt::Debug for ResumptionDriver<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResumptionDriver")
            .field("config", &self.config)
            .field("started", &self.handle.lock().ok().map(|s| s.is_some()))
            .finish_non_exhaustive()
    }
}

impl<S: Sweeper + 'static> ResumptionDriver<S> {
    /// Construct from a sweeper + a fresh broadcast receiver (one per
    /// driver — the realtime service's `subscribe()` clones a
    /// receiver, and each consumer sees every event from that point
    /// on).
    #[must_use]
    pub fn new(
        sweeper: Arc<S>,
        rx: async_broadcast::Receiver<WalletRealtimeEvent>,
        config: DriverConfig,
    ) -> Self {
        Self {
            sweeper,
            rx,
            config,
            handle: std::sync::Mutex::new(None),
        }
    }

    /// Spawn the trigger task. Idempotent: a second call is a no-op
    /// and returns a *new* handle pointing at the *same* shared
    /// state. The first `stop()` on any handle is the one that
    /// counts.
    ///
    /// `#[cfg(not(target_arch = "wasm32"))]` — Lane C provides the
    /// wasm variant (`spawn_local`).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn start(&self) -> DriverHandle {
        let mut slot = self
            .handle
            .lock()
            .expect("ResumptionDriver handle slot poisoned");
        if let Some(existing) = slot.as_ref() {
            // Idempotent: hand the caller a fresh handle over the
            // same shared state.
            return DriverHandle {
                shared: Arc::clone(&existing.shared),
            };
        }
        let shared = Arc::new(Shared::new());
        let handle = DriverHandle {
            shared: Arc::clone(&shared),
        };
        let rx = self.rx.clone();
        let sweeper = Arc::clone(&self.sweeper);
        let config = self.config;
        let shared_for_task = Arc::clone(&shared);

        // Initial-start trigger BEFORE the spawn so a test's
        // `start() → stop()` immediately observes at least one sweep
        // request even if the runtime hasn't yet polled the task.
        shared_for_task.mark(TriggerReason::InitialStart);

        tokio::spawn(async move {
            run_task(sweeper, rx, config, shared_for_task).await;
        });

        *slot = Some(DriverHandle {
            shared: Arc::clone(&shared),
        });
        handle
    }
}

/// The actual trigger-task body. Pulled out of `start()` so it can be
/// driven directly by tests on a `LocalSet` if they want explicit
/// control over the task lifecycle.
///
/// Lane C will call this same `run_task` from the wasm `spawn_local`
/// path — the *logic* is runtime-agnostic; only the spawn primitive +
/// the interval timer are cfg-gated.
async fn run_task<S: Sweeper + 'static>(
    sweeper: Arc<S>,
    rx: async_broadcast::Receiver<WalletRealtimeEvent>,
    config: DriverConfig,
    shared: Arc<Shared>,
) {
    use futures_util::StreamExt;

    let mut rx = rx;
    let mut waker_rx = shared.waker.new_receiver();

    // Native-only fallback-tick interval. Lane C swaps for the wasm
    // timer primitive. We *create* the interval up front even if
    // `fallback_tick == None`; the `tick()` branch is then disabled
    // by branching on `Option`. (`Option<Interval>` would be tidier
    // but `tokio::select!`'s arm gating works on bool guards, so a
    // dummy never-fires interval is simplest.)
    #[cfg(not(target_arch = "wasm32"))]
    let mut tick = match config.fallback_tick {
        Some(period) => {
            let mut iv = tokio::time::interval(period);
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            // Consume the immediate first tick — we don't want the
            // very-first interval tick to race the initial sweep.
            iv.tick().await;
            Some(iv)
        }
        None => None,
    };

    // Track realtime-connection status so we can suppress the
    // fallback tick while Subscribed (the dead-socket backstop is
    // ONLY for the case where realtime is down). Default to NOT
    // connected — until we see `Connected` or `Subscribed`, the tick
    // is enabled.
    let mut connected = false;

    while !shared.stop.load(Ordering::Acquire) {
        // 1. If a trigger is pending, run a sweep (and handle the
        //    dirty-bit re-run via the atomic swap).
        if shared.pending.load(Ordering::Acquire) {
            // Snapshot + clear pending BEFORE the sweep starts. Any
            // trigger that arrives DURING the sweep re-sets pending
            // — that becomes the coalesced re-run on the next loop
            // iteration. This is the §3 single-in-flight latch.
            shared.pending.store(false, Ordering::Release);

            // Run the sweep. The loop is unbreakable: a sweep error
            // is logged and we continue. The one exception is
            // `Unauthenticated`, which sets the `paused` flag — the
            // task stops firing sweeps until a foreground notify
            // clears it.
            //
            // NOTE: `pending` cleared *before* the await. If a
            // trigger fires during the await it will set `pending`
            // back to true; we'll loop and run another sweep. This
            // is the "at most one re-run after the in-flight one"
            // §3 discipline — N concurrent triggers collapse to one
            // sweep + at most one re-run.
            let result = sweeper.sweep(config.retry).await;
            shared.sweep_count.fetch_add(1, Ordering::Release);

            match result {
                Ok(report) => {
                    tracing::debug!(
                        target: "agicash_driver::task",
                        rows_seen = report.rows_seen,
                        advanced = report.advanced,
                        no_op = report.no_op,
                        failed = report.failed,
                        "sweep complete"
                    );
                }
                Err(WalletError::Unauthenticated) => {
                    // The wallet is logged out. Pause the loop —
                    // re-firing on every trigger would just produce
                    // a stream of `Unauthenticated`s in the log.
                    // Resume when `notify_foreground()` is called
                    // (the user-came-back signal — they may have
                    // re-signed-in).
                    tracing::info!(
                        target: "agicash_driver::task",
                        "sweep returned Unauthenticated; pausing until foreground edge"
                    );
                    shared.paused.store(true, Ordering::Release);
                }
                Err(e) => {
                    // Any other error — log + continue. The loop
                    // never dies on a single failed sweep.
                    tracing::warn!(
                        target: "agicash_driver::task",
                        error = %e,
                        "sweep failed; will retry on next trigger"
                    );
                    // Brief backoff so a persistent failure doesn't
                    // hot-spin if triggers keep firing.
                    #[cfg(not(target_arch = "wasm32"))]
                    tokio::time::sleep(config.error_backoff).await;
                }
            }
            // After a sweep, check if any new trigger arrived during
            // it (the dirty bit). If so, loop and sweep again — but
            // mark it as `Coalesced` for observability.
            if shared.pending.load(Ordering::Acquire) {
                shared
                    .reasons_seen
                    .fetch_or(reason_bit(TriggerReason::Coalesced), Ordering::Relaxed);
            }
            continue;
        }

        // 2. No pending trigger — park on the broadcast / waker /
        //    fallback tick / stop signal.

        #[cfg(not(target_arch = "wasm32"))]
        {
            // tokio::select! over: realtime broadcast / waker pulse /
            // fallback tick / stop wake.
            let tick_fut = async {
                if let Some(iv) = tick.as_mut() {
                    iv.tick().await;
                } else {
                    // Never resolves — fallback tick disabled.
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                biased;
                // Wake-on-stop. The `notify_foreground` / `mark`
                // path pulses the same waker, so this also catches
                // foreground edges that arrived between the loop
                // top check and entering the select.
                _ = waker_rx.next() => {
                    // Loop: top will re-check `pending` / `stop`.
                }
                ev = rx.next() => {
                    match ev {
                        Some(ev) => handle_realtime_event(ev, &shared, &mut connected),
                        None => {
                            // Broadcast sender dropped — the realtime
                            // service is gone. Wait for stop / waker;
                            // the task otherwise has nothing to do.
                            tracing::debug!(
                                target: "agicash_driver::task",
                                "realtime broadcast closed; driver continues on waker triggers only"
                            );
                        }
                    }
                }
                () = tick_fut => {
                    // Fallback tick fired. If realtime is currently
                    // Subscribed, this is the suppressed-while-
                    // connected case — do NOT mark a trigger. The
                    // tick exists ONLY as a dead-socket backstop.
                    if !connected && !shared.paused.load(Ordering::Acquire) {
                        shared.mark(TriggerReason::FallbackTick);
                    }
                }
            }
        }
        #[cfg(target_arch = "wasm32")]
        {
            // Lane C will replace this with a wasm select primitive.
            // For now (Lane B is native-only per §7) the task body
            // is unreachable on wasm — `start()` is cfg-gated and
            // there is no spawn entry-point.
            let _ = (&mut rx, &mut waker_rx, &connected);
            std::future::pending::<()>().await;
        }
    }
    tracing::debug!(target: "agicash_driver::task", "trigger task exiting");
}

/// Inspect a `WalletRealtimeEvent` and decide whether it should mark
/// a trigger. Pulled out so tests can drive it directly through a
/// `(Sender, Receiver)` pair.
fn handle_realtime_event(ev: WalletRealtimeEvent, shared: &Shared, connected: &mut bool) {
    match ev {
        WalletRealtimeEvent::Connected => {
            *connected = true;
            // Suppression of triggers while paused is intentional:
            // a paused driver got `Unauthenticated` last sweep; a
            // bare `Connected` does not mean the user re-signed-in.
            // Foreground notify is the only resume signal.
            if !shared.paused.load(Ordering::Acquire) {
                shared.mark(TriggerReason::RealtimeConnected);
            }
        }
        WalletRealtimeEvent::Event(e) => {
            if is_relevant_event(&e.event) && !shared.paused.load(Ordering::Acquire) {
                shared.mark(TriggerReason::RealtimeEvent);
            }
        }
        WalletRealtimeEvent::StatusChanged(status) => {
            // Track connection status for the fallback-tick gate.
            *connected = matches!(status, RealtimeStatus::Subscribed);
        }
        WalletRealtimeEvent::Error(_) => {
            // Recoverable transport error from the realtime supervisor.
            // The supervisor is retrying internally; we have nothing
            // to do here. (A `TerminalError` arrives as a
            // `StatusChanged`, which the above branch sees.)
        }
    }
}

/// The §3 event-name allowlist. Mirrors the React app's mapping of
/// realtime event names to driver-relevant rows.
///
/// **Source of truth** (plan §3 row 4 + lane-B prompt): these four
/// events indicate a row the driver acts on. Every other realtime
/// event (`ACCOUNT_*`, `TRANSACTION_*`, ...) is observed by the
/// resumption store but is NOT a driver trigger — the driver only
/// resolves the four state machines (`send_swap` / `receive_swap` /
/// `mint_quote` / `melt_quote`).
fn is_relevant_event(name: &str) -> bool {
    matches!(
        name,
        "CASHU_SEND_SWAP_UPDATED"
            | "CASHU_RECEIVE_SWAP_UPDATED"
            | "CASHU_RECEIVE_QUOTE_UPDATED"
            | "CASHU_SEND_QUOTE_UPDATED"
    )
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    //! Unit tests for the §3 trigger plumbing — no real wallet,
    //! no real realtime service. Integration tests against the
    //! `ProgrammableProvider` harness live in `tests/task_tests.rs`.
    use super::*;

    #[test]
    fn relevant_events_listed() {
        assert!(is_relevant_event("CASHU_SEND_SWAP_UPDATED"));
        assert!(is_relevant_event("CASHU_RECEIVE_SWAP_UPDATED"));
        assert!(is_relevant_event("CASHU_RECEIVE_QUOTE_UPDATED"));
        assert!(is_relevant_event("CASHU_SEND_QUOTE_UPDATED"));
    }

    #[test]
    fn irrelevant_events_excluded() {
        assert!(!is_relevant_event("ACCOUNT_UPDATED"));
        assert!(!is_relevant_event("TRANSACTION_CREATED"));
        assert!(!is_relevant_event("TRANSACTION_UPDATED"));
        assert!(!is_relevant_event(""));
        assert!(!is_relevant_event("cashu_send_swap_updated")); // case-sensitive
    }

    #[test]
    fn reason_bits_distinct() {
        // All six reasons map to distinct bits in the u64 mask.
        let bits = [
            reason_bit(TriggerReason::InitialStart),
            reason_bit(TriggerReason::RealtimeConnected),
            reason_bit(TriggerReason::RealtimeEvent),
            reason_bit(TriggerReason::Foreground),
            reason_bit(TriggerReason::FallbackTick),
            reason_bit(TriggerReason::Coalesced),
        ];
        for (i, a) in bits.iter().enumerate() {
            for b in bits.iter().skip(i + 1) {
                assert_ne!(a, b, "reason bits must be distinct");
            }
        }
    }

    #[test]
    fn handle_realtime_event_updates_connected_on_subscribed() {
        let shared = Shared::new();
        let mut connected = false;
        handle_realtime_event(
            WalletRealtimeEvent::StatusChanged(RealtimeStatus::Subscribed),
            &shared,
            &mut connected,
        );
        assert!(connected, "Subscribed must set connected=true");
        handle_realtime_event(
            WalletRealtimeEvent::StatusChanged(RealtimeStatus::Reconnecting),
            &shared,
            &mut connected,
        );
        assert!(!connected, "Reconnecting must set connected=false");
    }

    #[test]
    fn handle_realtime_event_marks_only_relevant() {
        let shared = Shared::new();
        let mut connected = false;

        handle_realtime_event(
            WalletRealtimeEvent::Event(agicash_realtime::WalletEvent {
                event: "ACCOUNT_UPDATED".into(),
                payload_json: "{}".into(),
            }),
            &shared,
            &mut connected,
        );
        assert!(
            !shared.pending.load(Ordering::Acquire),
            "ACCOUNT_UPDATED must not mark"
        );

        handle_realtime_event(
            WalletRealtimeEvent::Event(agicash_realtime::WalletEvent {
                event: "CASHU_RECEIVE_SWAP_UPDATED".into(),
                payload_json: "{}".into(),
            }),
            &shared,
            &mut connected,
        );
        assert!(
            shared.pending.load(Ordering::Acquire),
            "CASHU_RECEIVE_SWAP_UPDATED must mark"
        );
    }

    #[test]
    fn handle_realtime_event_suppressed_while_paused() {
        let shared = Shared::new();
        shared.paused.store(true, Ordering::Release);
        let mut connected = false;

        handle_realtime_event(WalletRealtimeEvent::Connected, &shared, &mut connected);
        assert!(
            !shared.pending.load(Ordering::Acquire),
            "paused: Connected must NOT mark"
        );
        assert!(connected, "Connected still tracks status while paused");

        handle_realtime_event(
            WalletRealtimeEvent::Event(agicash_realtime::WalletEvent {
                event: "CASHU_SEND_SWAP_UPDATED".into(),
                payload_json: "{}".into(),
            }),
            &shared,
            &mut connected,
        );
        assert!(
            !shared.pending.load(Ordering::Acquire),
            "paused: Event must NOT mark"
        );
    }
}
