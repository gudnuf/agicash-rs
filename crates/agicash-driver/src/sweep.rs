//! [`run_sweep`] — one pass over the user's pending state.
//!
//! Calls [`agicash_wallet::WalletClient::refresh_pending_state`] then
//! dispatches the §2-table service method per row state. Plain
//! `async fn`; no spawn, no triggers, no fallback timer (Lane B/C).
//!
//! Per-row error handling:
//! - [`crate::ErrorClass::Benign`] → record `NoOp`, keep going.
//! - [`crate::ErrorClass::Transient`] → backoff + retry up to
//!   `RetryConfig::max_attempts`.
//! - [`crate::ErrorClass::Fatal`] → record `Failed`, keep going.
//!
//! One bad row never breaks the loop.

use std::time::Duration;

use agicash_cashu::{
    CashuMeltQuote, CashuMeltQuoteState, CashuMintQuote, CashuMintQuoteState, CashuReceiveSwap,
    CashuSendSwap, CashuSendSwapState,
};
use agicash_wallet::{PendingStateSnapshot, WalletClient, WalletError};
use chrono::Utc;
use uuid::Uuid;

use crate::error::{ClassifiedError, ErrorClass};
use crate::report::{RowKind, RowOutcome, RowStatus, SweepReport};

/// Retry configuration for transient per-row errors.
///
/// React's `useProcessX` mutations use `retry: 3` — the default mirrors
/// that. Default backoff is `100ms`, doubled per attempt (100ms /
/// 200ms / 400ms). The driver caps attempts; a row that exhausts its
/// budget is recorded as `Failed` and left for the next sweep.
#[derive(Debug, Clone, Copy)]
pub struct RetryConfig {
    /// Max attempts per row per sweep, including the first try. React
    /// default = 3 retries i.e. 4 attempts; we use 3 attempts total to
    /// keep the per-sweep latency bounded (a sweep with N transient
    /// rows × 4 attempts × 400ms is a poor reconnect experience).
    pub max_attempts: u8,
    /// Initial backoff before the *second* attempt. Doubled per
    /// subsequent attempt.
    pub base_backoff: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_backoff: Duration::from_millis(100),
        }
    }
}

/// Drive one resumption sweep over the signed-in user's pending state.
///
/// Single-shot: fetches the snapshot, dispatches per row, returns the
/// report. No spawn / no triggers / no fallback timer (Lane B/C).
///
/// Uses the default [`RetryConfig`]; use [`run_sweep_with_config`] to
/// override.
///
/// # Errors
/// Only the initial `refresh_pending_state` failure is propagated as a
/// `WalletError` — that single call is the "can we see the world?"
/// gate; if it fails, there's nothing to sweep. Per-row errors are
/// recorded in the report, never propagated.
pub async fn run_sweep(client: &WalletClient) -> Result<SweepReport, WalletError> {
    run_sweep_with_config(client, RetryConfig::default()).await
}

/// Like [`run_sweep`] but with an explicit retry budget.
pub async fn run_sweep_with_config(
    client: &WalletClient,
    cfg: RetryConfig,
) -> Result<SweepReport, WalletError> {
    let snapshot = client.refresh_pending_state().await?;
    Ok(sweep_snapshot(client, snapshot, cfg).await)
}

/// Visible for tests + Lane B: dispatch over an already-fetched
/// snapshot. Lets a test inject specific rows without driving the
/// four storage `list_*` methods; Lane B (the trigger task) reuses it
/// when a realtime `Event` already implies a specific row to act on.
pub async fn sweep_snapshot(
    client: &WalletClient,
    snapshot: PendingStateSnapshot,
    cfg: RetryConfig,
) -> SweepReport {
    let mut report = SweepReport::default();

    // Order: send_swap → receive_swap → mint_quote → melt_quote. Mirrors
    // the §2 table and React's `TaskProcessor` mount order. No
    // dependencies between row kinds — order is for log readability.
    for swap in snapshot.send_swaps {
        let outcome = handle_send_swap(client, swap, cfg).await;
        report.record(outcome);
    }
    for swap in snapshot.receive_swaps {
        let outcome = handle_receive_swap(client, swap, cfg).await;
        report.record(outcome);
    }
    for quote in snapshot.mint_quotes {
        let outcome = handle_mint_quote(client, quote, cfg).await;
        report.record(outcome);
    }
    for quote in snapshot.melt_quotes {
        let outcome = handle_melt_quote(client, quote, cfg).await;
        report.record(outcome);
    }

    report
}

// ---------------------------------------------------------------------------
// Per-row dispatchers — each maps a row's state to ONE facade call (the
// §2-table row-to-method mapping). Idempotency guards live in the
// services; we only classify the response.
// ---------------------------------------------------------------------------

async fn handle_send_swap(
    client: &WalletClient,
    swap: CashuSendSwap,
    cfg: RetryConfig,
) -> RowOutcome {
    let row_id = swap.id;
    let (kind, status) = match &swap.state {
        CashuSendSwapState::Draft => {
            let status = retry(cfg, || async {
                client.resume_send_swap_draft(row_id).await.map(|_| ())
            })
            .await;
            (RowKind::SendSwapDraft, status)
        }
        CashuSendSwapState::Pending { .. } => {
            // NUT-07 claim check; on all-SPENT it flips PENDING → COMPLETED.
            // Re-fires are safe — `check_send_token_claimed` fast-paths
            // terminal states with no mint round-trip.
            let status = retry(cfg, || async {
                client.check_send_token_claimed(row_id).await.map(|_| ())
            })
            .await;
            (RowKind::SendSwapPending, status)
        }
        // The `list_unresolved_send_swaps` reader only emits DRAFT +
        // PENDING; terminal states would be a contract bug. Record as a
        // benign no-op rather than break the sweep.
        _ => (
            RowKind::SendSwapPending,
            RowStatus::NoOp {
                reason: format!(
                    "send_swap in unexpected state {:?} surfaced by list_unresolved",
                    swap.state
                ),
            },
        ),
    };
    RowOutcome {
        row_id,
        kind,
        status,
    }
}

async fn handle_receive_swap(
    client: &WalletClient,
    swap: CashuReceiveSwap,
    cfg: RetryConfig,
) -> RowOutcome {
    let row_id = transaction_id_for_receive(&swap);
    // The receive-swap storage indexes by token_hash, not by `id` —
    // there is no `id` field on `CashuReceiveSwap`. We surface the
    // transaction_id in the report for traceability (it's the wallet-
    // wide handle the React UI uses).
    //
    // `resume_receive_swap` takes the row by value (the storage trait
    // has no `get(id)`; the driver already has the row from the
    // snapshot, so no read is wasted).
    let status = retry(cfg, || {
        let swap = swap.clone();
        async move { client.resume_receive_swap(swap).await.map(|_| ()) }
    })
    .await;
    RowOutcome {
        row_id,
        kind: RowKind::ReceiveSwapPending,
        status,
    }
}

async fn handle_mint_quote(
    client: &WalletClient,
    quote: CashuMintQuote,
    cfg: RetryConfig,
) -> RowOutcome {
    let row_id = quote.id;
    // Expiry first: an UNPAID quote past `expires_at` should be
    // expired, not polled. Mirrors React's `expire` mutation in
    // `useProcessCashuReceiveQuoteTasks`. Cheap; no mint round-trip.
    if matches!(quote.state, CashuMintQuoteState::Unpaid) && quote.expires_at < Utc::now() {
        let status = retry(cfg, || async {
            client.expire_mint_quote(row_id).await.map(|_| ())
        })
        .await;
        return RowOutcome {
            row_id,
            kind: RowKind::MintQuoteExpire,
            status,
        };
    }
    let (kind, status) = match &quote.state {
        // UNPAID & not yet expired — cheap poll to catch the
        // UNPAID→PAID edge promptly. `poll_receive_lightning` fast-paths
        // past UNPAID by reading the persisted state, then does ONE
        // mint round-trip with zero timeout (the existing facade
        // semantics). On PAID the *next* sweep will pick it up and
        // call `complete_receive_lightning` — keeps each handler
        // single-step.
        CashuMintQuoteState::Unpaid => {
            let status = retry(cfg, || async {
                client.poll_receive_lightning(row_id).await.map(|_| ())
            })
            .await;
            (RowKind::MintQuoteUnpaid, status)
        }
        // PAID → drive to COMPLETED via the real mint call.
        CashuMintQuoteState::Paid { .. } => {
            let status = retry(cfg, || async {
                client.complete_receive_lightning(row_id).await.map(|_| ())
            })
            .await;
            (RowKind::MintQuotePaid, status)
        }
        // Terminal states should not surface from
        // `list_pending_mint_quotes`; benign no-op if they do.
        _ => (
            RowKind::MintQuotePaid,
            RowStatus::NoOp {
                reason: format!(
                    "mint_quote in unexpected state {:?} surfaced by list_pending",
                    quote.state
                ),
            },
        ),
    };
    RowOutcome {
        row_id,
        kind,
        status,
    }
}

async fn handle_melt_quote(
    client: &WalletClient,
    quote: CashuMeltQuote,
    cfg: RetryConfig,
) -> RowOutcome {
    let row_id = quote.id;
    let (kind, status) = match &quote.state {
        // UNPAID: the driver MUST NOT call `initiate_melt` (plan §6.2,
        // the double-pay bug). Only expiry is in scope. If not yet
        // expired, there is no resumption action — leave the row as-is
        // until either the user re-initiates or `expires_at` passes.
        CashuMeltQuoteState::Unpaid => {
            if quote.expires_at < Utc::now() {
                let status = retry(cfg, || async {
                    client.expire_melt_quote(row_id).await.map(|_| ())
                })
                .await;
                (RowKind::MeltQuoteUnpaidExpire, status)
            } else {
                (
                    RowKind::MeltQuoteUnpaidExpire,
                    RowStatus::NoOp {
                        reason: "melt_quote UNPAID, not yet expired — driver does not initiate"
                            .into(),
                    },
                )
            }
        }
        // PENDING: reconcile via the existing `poll_send_lightning`
        // facade — calls `melt_quote_service::poll_until_complete`,
        // which polls the mint's view of an already-initiated payment
        // and settles the row. **NEVER** calls `initiate_melt`. This is
        // the slice-29 §2.4 no-double-pay discipline enforced in core.
        CashuMeltQuoteState::Pending => {
            let status = retry(cfg, || async {
                client.poll_send_lightning(row_id).await.map(|_| ())
            })
            .await;
            (RowKind::MeltQuotePending, status)
        }
        // Terminal states should not surface from
        // `list_unresolved_melt_quotes`; benign no-op if they do.
        _ => (
            RowKind::MeltQuotePending,
            RowStatus::NoOp {
                reason: format!(
                    "melt_quote in unexpected state {:?} surfaced by list_unresolved",
                    quote.state
                ),
            },
        ),
    };
    RowOutcome {
        row_id,
        kind,
        status,
    }
}

// ---------------------------------------------------------------------------
// Retry shim — runs the closure, classifies the error, retries on
// `Transient`, stops on `Benign` / `Fatal` / success.
// ---------------------------------------------------------------------------

async fn retry<F, Fut>(cfg: RetryConfig, mut f: F) -> RowStatus
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<(), WalletError>>,
{
    let mut last_err: Option<ClassifiedError> = None;
    let mut backoff = cfg.base_backoff;
    for attempt in 1..=cfg.max_attempts {
        match f().await {
            Ok(()) => return RowStatus::Advanced,
            Err(e) => {
                let classified = ClassifiedError::from_wallet_error(e);
                match classified.class {
                    ErrorClass::Benign => {
                        return RowStatus::NoOp {
                            reason: classified.source.to_string(),
                        };
                    }
                    ErrorClass::Fatal => {
                        return RowStatus::Failed {
                            error: classified,
                            attempts: attempt,
                        };
                    }
                    ErrorClass::Transient => {
                        last_err = Some(classified);
                        if attempt < cfg.max_attempts {
                            sleep_for(backoff).await;
                            backoff = backoff.saturating_mul(2);
                        }
                    }
                }
            }
        }
    }
    // Exhausted the retry budget on transient errors. Unwrap is safe:
    // we only reach here after at least one transient error was
    // recorded in `last_err`.
    RowStatus::Failed {
        error: last_err.expect("transient path always populates last_err"),
        attempts: cfg.max_attempts,
    }
}

// Tiny cfg-split sleep — wasm32 has no `tokio::time::sleep`.
#[cfg(not(target_arch = "wasm32"))]
async fn sleep_for(d: Duration) {
    tokio::time::sleep(d).await;
}

// Wasm sleep: wraps the JS global `setTimeout` via `js-sys` +
// `wasm-bindgen-futures`. Same primitive `agicash-realtime` settled
// on (it deliberately avoids `gloo-timers` — we follow). Duplicate of
// `task::wasm_sleep_ms`; pulled inline here so `retry()`'s sleep
// actually sleeps on wasm. ~15 LOC of duplication is cheaper than a
// pub module item to share across `sweep.rs` and `task.rs`.
#[cfg(target_arch = "wasm32")]
async fn sleep_for(d: Duration) {
    use wasm_bindgen::{closure::Closure, JsCast, JsValue};
    let ms = u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let set_timeout = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("setTimeout"))
            .ok()
            .and_then(|v| v.dyn_into::<js_sys::Function>().ok());
        if let Some(set_timeout) = set_timeout {
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
            let _ = resolve.call0(&JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

/// The receive-swap "id" we surface in the report. The row is keyed in
/// storage by `token_hash`, but the wallet exposes the row's
/// `transaction_id` (a uuid) as the cross-store handle — that's what
/// the React UI uses. We use it solely for traceability in the report.
fn transaction_id_for_receive(swap: &CashuReceiveSwap) -> Uuid {
    swap.transaction_id
}
