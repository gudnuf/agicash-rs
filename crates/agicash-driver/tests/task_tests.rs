//! Integration tests for `agicash-driver::task` — the Lane B trigger
//! loop.
//!
//! Strategy: instead of composing a real `WalletClient` (Lane A's
//! `tests/sweep_tests.rs` covers that surface end-to-end), we feed
//! the driver a **programmable [`Sweeper`]** that:
//!
//! - blocks until the test releases it (the in-flight latch
//!   assertion);
//! - returns a scripted sequence of `Result<SweepReport, WalletError>`
//!   per call (the pause-on-`Unauthenticated` assertion, the
//!   unbreakable-loop assertion).
//!
//! Triggers are driven by:
//! - pushing `WalletRealtimeEvent`s into an `async-broadcast::Sender`
//!   the driver subscribed to (same Sender/Receiver pair the
//!   production `WalletRealtimeService` would use); and
//! - calling `handle.notify_foreground()` for the foreground edge.
//!
//! All tests run on a multi-thread tokio runtime — single-thread
//! would deadlock the "block in-flight while we fire more triggers"
//! pattern because the blocking sweep would never yield.

#![allow(clippy::redundant_clone)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agicash_driver::{
    DriverConfig, ResumptionDriver, RetryConfig, SweepReport, Sweeper, TriggerReason,
};
use agicash_realtime::{RealtimeStatus, WalletEvent, WalletRealtimeEvent};
use agicash_wallet::WalletError;
use async_trait::async_trait;
use tokio::sync::Notify;

// ---------------------------------------------------------------------------
// Programmable sweeper: blocks on a Notify until the test releases,
// returns the next scripted result (or a default Ok), counts calls.
// ---------------------------------------------------------------------------

struct ProgrammableSweeper {
    /// If set, the sweep awaits this `Notify` BEFORE returning. Lets a
    /// test pin a sweep in-flight while firing more triggers.
    gate: Arc<Notify>,
    /// Whether the gate is "armed" — if true, sweeps wait on it; if
    /// false, sweeps return immediately.
    gated: AtomicBool,
    /// Scripted results, popped from the front per call. Default Ok
    /// when empty.
    results: tokio::sync::Mutex<Vec<Result<SweepReport, WalletError>>>,
    /// Total `sweep()` calls received.
    calls: AtomicUsize,
}

impl ProgrammableSweeper {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            gate: Arc::new(Notify::new()),
            gated: AtomicBool::new(false),
            results: tokio::sync::Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        })
    }

    fn arm_gate(&self) {
        self.gated.store(true, Ordering::Release);
    }

    fn release(&self) {
        self.gated.store(false, Ordering::Release);
        // Wake any sweep currently parked on the gate.
        self.gate.notify_waiters();
    }

    async fn push_result(&self, r: Result<SweepReport, WalletError>) {
        self.results.lock().await.push(r);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Acquire)
    }
}

#[async_trait]
impl Sweeper for ProgrammableSweeper {
    async fn sweep(&self, _cfg: RetryConfig) -> Result<SweepReport, WalletError> {
        self.calls.fetch_add(1, Ordering::Release);
        // If gated, park until release. Notify is edge-triggered: we
        // grab the notified future BEFORE checking the flag so a
        // release that races our check still wakes us.
        loop {
            if !self.gated.load(Ordering::Acquire) {
                break;
            }
            let notified = self.gate.notified();
            tokio::pin!(notified);
            // Re-check after pin to avoid the lost-wakeup race.
            if !self.gated.load(Ordering::Acquire) {
                break;
            }
            notified.await;
        }
        let mut results = self.results.lock().await;
        if results.is_empty() {
            Ok(SweepReport::default())
        } else {
            results.remove(0)
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers — broadcast pair, event constructors, polling waits.
// ---------------------------------------------------------------------------

fn make_broadcast() -> (
    async_broadcast::Sender<WalletRealtimeEvent>,
    async_broadcast::Receiver<WalletRealtimeEvent>,
) {
    let (mut tx, rx) = async_broadcast::broadcast(64);
    tx.set_overflow(true);
    (tx, rx)
}

fn relevant_event() -> WalletRealtimeEvent {
    WalletRealtimeEvent::Event(WalletEvent {
        event: "CASHU_SEND_SWAP_UPDATED".into(),
        payload_json: "{}".into(),
    })
}

fn irrelevant_event() -> WalletRealtimeEvent {
    WalletRealtimeEvent::Event(WalletEvent {
        event: "ACCOUNT_UPDATED".into(),
        payload_json: "{}".into(),
    })
}

/// Poll a condition until it returns true or the timeout fires.
/// Polling beats fixed sleeps in test reliability — slow CI doesn't
/// flake.
async fn wait_for<F: Fn() -> bool>(timeout: Duration, f: F) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    f()
}

/// `DriverConfig` with the fallback tick **disabled** — every test
/// here drives triggers manually, no time-passes asserts.
fn test_config() -> DriverConfig {
    DriverConfig {
        retry: RetryConfig {
            max_attempts: 1,
            base_backoff: Duration::from_millis(1),
        },
        fallback_tick: None,
        error_backoff: Duration::from_millis(1),
    }
}

// ===========================================================================
// 1. Lifecycle — start is idempotent; stop drains and exits.
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn start_is_idempotent() {
    let sweeper = ProgrammableSweeper::new();
    let (_tx, rx) = make_broadcast();
    let driver = ResumptionDriver::new(sweeper.clone(), rx, test_config());

    let h1 = driver.start();
    let h2 = driver.start();

    // Both handles share the same shared state — observing one
    // observes the other.
    assert!(
        wait_for(Duration::from_secs(2), || h1.sweep_count() >= 1).await,
        "initial sweep must run at least once after start()"
    );
    let n = h1.sweep_count();
    assert_eq!(
        h1.sweep_count(),
        h2.sweep_count(),
        "both handles see the same sweep count"
    );

    h1.stop();
    assert!(h2.is_stopped(), "h2 sees the stop from h1");

    // After stop, no more sweeps should fire even if we wait.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(sweeper.calls(), n, "no sweeps after stop");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stop_drains_in_flight_then_exits() {
    let sweeper = ProgrammableSweeper::new();
    sweeper.arm_gate();
    let (_tx, rx) = make_broadcast();
    let driver = ResumptionDriver::new(sweeper.clone(), rx, test_config());

    let handle = driver.start();

    // Wait for the initial sweep to be in-flight (gated).
    assert!(
        wait_for(Duration::from_secs(2), || sweeper.calls() >= 1).await,
        "initial sweep should start"
    );

    // Request stop while the sweep is parked.
    handle.stop();

    // Release the gate — the in-flight sweep completes.
    sweeper.release();

    // Wait for is_stopped + observe no more sweeps fired.
    let calls_at_stop = sweeper.calls();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        sweeper.calls(),
        calls_at_stop,
        "no more sweeps after stop drains the in-flight one"
    );
    assert!(handle.is_stopped());
}

// ===========================================================================
// 2. Trigger routing — Connected, relevant Event, foreground all
//    fire; irrelevant Event does NOT.
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connected_event_fires_sweep() {
    let sweeper = ProgrammableSweeper::new();
    let (tx, rx) = make_broadcast();
    let driver = ResumptionDriver::new(sweeper.clone(), rx, test_config());
    let handle = driver.start();

    // Drain the initial-start sweep first.
    assert!(wait_for(Duration::from_secs(2), || sweeper.calls() >= 1).await);
    let n0 = sweeper.calls();

    tx.broadcast(WalletRealtimeEvent::Connected).await.unwrap();
    assert!(
        wait_for(Duration::from_secs(2), || sweeper.calls() > n0).await,
        "Connected must fire a sweep"
    );
    assert!(handle.has_seen(TriggerReason::RealtimeConnected));
    handle.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn relevant_event_fires_sweep_irrelevant_does_not() {
    let sweeper = ProgrammableSweeper::new();
    let (tx, rx) = make_broadcast();
    let driver = ResumptionDriver::new(sweeper.clone(), rx, test_config());
    let handle = driver.start();

    assert!(wait_for(Duration::from_secs(2), || sweeper.calls() >= 1).await);
    let n0 = sweeper.calls();

    // Irrelevant event first — must NOT fire a sweep.
    tx.broadcast(irrelevant_event()).await.unwrap();
    // Give the task a chance to process. If it WAS going to mark a
    // trigger, the sweep would happen within a few ms. We then assert
    // count unchanged.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        sweeper.calls(),
        n0,
        "irrelevant Event (ACCOUNT_UPDATED) must NOT fire a sweep"
    );
    assert!(
        !handle.has_seen(TriggerReason::RealtimeEvent),
        "irrelevant Event must not mark RealtimeEvent"
    );

    // Relevant event — must fire.
    tx.broadcast(relevant_event()).await.unwrap();
    assert!(
        wait_for(Duration::from_secs(2), || sweeper.calls() > n0).await,
        "relevant Event (CASHU_SEND_SWAP_UPDATED) must fire a sweep"
    );
    assert!(handle.has_seen(TriggerReason::RealtimeEvent));
    handle.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foreground_notify_fires_sweep() {
    let sweeper = ProgrammableSweeper::new();
    let (_tx, rx) = make_broadcast();
    let driver = ResumptionDriver::new(sweeper.clone(), rx, test_config());
    let handle = driver.start();

    assert!(wait_for(Duration::from_secs(2), || sweeper.calls() >= 1).await);
    let n0 = sweeper.calls();

    handle.notify_foreground();
    assert!(
        wait_for(Duration::from_secs(2), || sweeper.calls() > n0).await,
        "notify_foreground must fire a sweep"
    );
    assert!(handle.has_seen(TriggerReason::Foreground));
    handle.stop();
}

// ===========================================================================
// 3. Single-in-flight + dirty-bit coalescing.
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn coalesces_burst_into_single_rerun() {
    // Strategy: arm the gate so the very first (initial) sweep parks
    // in-flight. Fire MANY triggers — Connected + N relevant Events.
    // Release the gate. Assert: AT MOST one additional sweep runs
    // (the coalesced re-run). The contract is "N triggers during an
    // in-flight sweep collapse to one re-run."
    let sweeper = ProgrammableSweeper::new();
    sweeper.arm_gate();
    let (tx, rx) = make_broadcast();
    let driver = ResumptionDriver::new(sweeper.clone(), rx, test_config());
    let handle = driver.start();

    // Wait for the initial sweep to start (it will park on the gate).
    assert!(
        wait_for(Duration::from_secs(2), || sweeper.calls() >= 1).await,
        "initial sweep must enter the gate"
    );
    assert_eq!(sweeper.calls(), 1, "exactly one sweep in flight");

    // While the initial sweep is parked, fire a burst of triggers.
    tx.broadcast(WalletRealtimeEvent::Connected).await.unwrap();
    tx.broadcast(relevant_event()).await.unwrap();
    tx.broadcast(relevant_event()).await.unwrap();
    tx.broadcast(relevant_event()).await.unwrap();
    handle.notify_foreground();
    // Give the task a moment to receive each trigger and OR the
    // pending bit. They should all coalesce.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        sweeper.calls(),
        1,
        "no second sweep starts while the first is in flight"
    );

    // Release the in-flight sweep.
    sweeper.release();

    // Exactly ONE re-run should fire after release (the coalesced
    // dirty-bit re-run for all 5 triggers).
    assert!(
        wait_for(Duration::from_secs(2), || sweeper.calls() >= 2).await,
        "coalesced re-run must fire after the in-flight sweep completes"
    );
    // Wait a beat for any extra runs that should NOT exist.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let calls = sweeper.calls();
    assert_eq!(
        calls, 2,
        "exactly ONE re-run after release; got {calls} sweeps total"
    );
    assert!(handle.has_seen(TriggerReason::Coalesced));
    handle.stop();
}

// ===========================================================================
// 4. Logged-out pause: Unauthenticated halts; foreground notify
//    resumes.
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unauthenticated_pauses_until_foreground_resume() {
    let sweeper = ProgrammableSweeper::new();
    // First sweep returns Unauthenticated; subsequent sweeps return Ok.
    sweeper.push_result(Err(WalletError::Unauthenticated)).await;
    let (tx, rx) = make_broadcast();
    let driver = ResumptionDriver::new(sweeper.clone(), rx, test_config());
    let handle = driver.start();

    // Wait for the initial sweep + observe the pause flag.
    assert!(
        wait_for(Duration::from_secs(2), || handle.is_paused()).await,
        "Unauthenticated must set is_paused"
    );
    let n_after_pause = sweeper.calls();
    assert_eq!(n_after_pause, 1);

    // Fire triggers — they must NOT produce sweeps while paused.
    tx.broadcast(WalletRealtimeEvent::Connected).await.unwrap();
    tx.broadcast(relevant_event()).await.unwrap();
    tx.broadcast(relevant_event()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert_eq!(
        sweeper.calls(),
        n_after_pause,
        "no sweeps fire while paused on Unauthenticated"
    );
    assert!(handle.is_paused(), "still paused");

    // Foreground notify clears the pause AND fires a sweep.
    handle.notify_foreground();
    assert!(
        wait_for(Duration::from_secs(2), || sweeper.calls() > n_after_pause).await,
        "notify_foreground after pause must resume the loop"
    );
    assert!(!handle.is_paused(), "pause cleared by notify_foreground");
    handle.stop();
}

// ===========================================================================
// 5. Unbreakable loop: a per-sweep Err does NOT kill the task.
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sweep_error_does_not_kill_task() {
    let sweeper = ProgrammableSweeper::new();
    // Initial sweep returns Network error (transient, not auth).
    // The task should log + continue, NOT pause.
    sweeper
        .push_result(Err(WalletError::Network("simulated".into())))
        .await;
    let (tx, rx) = make_broadcast();
    let cfg = DriverConfig {
        retry: RetryConfig {
            max_attempts: 1,
            base_backoff: Duration::from_millis(1),
        },
        fallback_tick: None,
        // Short backoff so the test doesn't sit on the default 1s.
        error_backoff: Duration::from_millis(5),
    };
    let driver = ResumptionDriver::new(sweeper.clone(), rx, cfg);
    let handle = driver.start();

    // Wait for the first (failing) sweep.
    assert!(wait_for(Duration::from_secs(2), || sweeper.calls() >= 1).await);
    let n0 = sweeper.calls();
    assert!(
        !handle.is_paused(),
        "Network error must NOT pause (only Unauthenticated does)"
    );

    // Fire another trigger — the task should still be alive and
    // process it.
    tx.broadcast(WalletRealtimeEvent::Connected).await.unwrap();
    assert!(
        wait_for(Duration::from_secs(2), || sweeper.calls() > n0).await,
        "task must survive a per-sweep error and process the next trigger"
    );
    handle.stop();
}

// ===========================================================================
// 6. Subscribed status suppresses fallback tick.
//
// The fallback tick is the dead-socket backstop. When the realtime
// supervisor announces Subscribed, the tick must NOT mark a sweep.
// We drive this by: enabling a tight fallback tick, sending a
// `StatusChanged(Subscribed)` event, then asserting no extra sweep
// runs over a brief window.
// ===========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fallback_tick_suppressed_while_subscribed() {
    let sweeper = ProgrammableSweeper::new();
    let (tx, rx) = make_broadcast();
    // Tight tick so the test is fast.
    let cfg = DriverConfig {
        retry: RetryConfig {
            max_attempts: 1,
            base_backoff: Duration::from_millis(1),
        },
        fallback_tick: Some(Duration::from_millis(30)),
        error_backoff: Duration::from_millis(1),
    };
    let driver = ResumptionDriver::new(sweeper.clone(), rx, cfg);
    let handle = driver.start();

    // Drain initial sweep.
    assert!(wait_for(Duration::from_secs(2), || sweeper.calls() >= 1).await);

    // Announce we're connected. Status alone (not a Connected event)
    // doesn't mark a trigger, it just updates the connected flag.
    tx.broadcast(WalletRealtimeEvent::StatusChanged(
        RealtimeStatus::Subscribed,
    ))
    .await
    .unwrap();
    // Give the task a moment to process the status update.
    tokio::time::sleep(Duration::from_millis(20)).await;

    let n0 = sweeper.calls();

    // Sit for several tick periods. The tick should fire repeatedly
    // but produce NO sweeps because connected==true.
    tokio::time::sleep(Duration::from_millis(150)).await;

    let calls = sweeper.calls();
    assert_eq!(
        calls, n0,
        "fallback tick must NOT fire sweeps while Subscribed (got {calls} sweeps)"
    );

    // Now flip to disconnected — the tick should resume.
    tx.broadcast(WalletRealtimeEvent::StatusChanged(
        RealtimeStatus::Reconnecting,
    ))
    .await
    .unwrap();

    assert!(
        wait_for(Duration::from_secs(2), || sweeper.calls() > n0).await,
        "fallback tick must fire sweeps when NOT subscribed"
    );
    assert!(handle.has_seen(TriggerReason::FallbackTick));
    handle.stop();
}
