//! `agicash send lightning <bolt11>` subcommand — composed over the
//! `WalletClient` facade's reconcile-aware send surface (P0-1).
//!
//! - `--dry-run` → `quote_send_lightning` (preview, no commit) →
//!   `quote` body, byte-for-byte the prior `print_dry_run`.
//! - commit → `begin_send_lightning` (create + NUT-05 `post_melt` in one
//!   reconcile-aware call) → on `InFlight` a shell-driven
//!   `poll_send_lightning` loop on the prior `poll_ms`/`timeout_s`
//!   cadence → `paid` / `failed` / `timed-out` bodies byte-for-byte the
//!   prior `print_outcome`.
//!
//! FLAGGED BEHAVIOR DELTA (facade-gap, NOT in the required smoke path):
//! the pre-migration CLI emitted an intermediate `quote-issued` line
//! (persisted quote, before `post_melt`) and `--no-wait` exited there.
//! The slice-12 P0-1 redesign deliberately fused create+melt into the
//! reconcile-aware `begin_send_lightning` (removing exactly the
//! un-reconciled create-then-pay seam that was the double-pay vector),
//! so there is no public facade method that persists a melt quote
//! WITHOUT firing `post_melt`. The `quote-issued` line and the
//! `--no-wait` early-exit are therefore not reproduced; `--no-wait`
//! degrades to the same begin+single-reconcile-poll the wait path runs.
//! Modifying the facade to re-expose that seam is explicitly out of
//! scope (and would reintroduce the closed double-pay vector).

use crate::composition::CliDeps;
use agicash_domain::AccountId;
use agicash_traits::{AuthError, StorageError};
use agicash_wallet::types::SendLightningStatus;
use agicash_wallet::{SendLightningQuote, WalletError};
use serde::Serialize;
use std::str::FromStr;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SendLightningCmdError {
    #[error("not logged in")]
    NotLoggedIn,
    #[error("no matching account")]
    NoMatchingAccount,
    #[error("account ambiguous — pass --account <id>")]
    AccountAmbiguous,
    #[error("invalid account id: {0}")]
    InvalidAccountId(String),
    #[error("invalid quote id: {0}")]
    InvalidQuoteId(String),
    #[error("melt quote failed: {0}")]
    Quote(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Auth(#[from] AuthError),
}

#[derive(Serialize)]
struct QuoteOutput<'a> {
    status: &'a str,
    amount: String,
    lightning_fee_reserve: String,
    cashu_fee: String,
    total_fee: String,
    total_amount: String,
    unit: String,
    currency: String,
    account_id: String,
    payment_hash: String,
}

#[derive(Serialize)]
struct PaidOutput<'a> {
    status: &'a str,
    quote_id: String,
    amount: String,
    lightning_fee: String,
    cashu_fee: String,
    total_fee: String,
    amount_spent: String,
    payment_preimage: String,
    account_id: String,
    payment_hash: String,
}

#[derive(Serialize)]
struct TimedOutOutput<'a> {
    status: &'a str,
    quote_id: String,
    payment_hash: String,
}

#[derive(Serialize)]
struct FailedOutput<'a> {
    status: &'a str,
    quote_id: String,
    reason: String,
}

fn map_err(e: WalletError) -> SendLightningCmdError {
    match e {
        WalletError::Unauthenticated => SendLightningCmdError::NotLoggedIn,
        WalletError::Validation { code, message } => match code.as_str() {
            "no_account" => SendLightningCmdError::NoMatchingAccount,
            "ambiguous_account" => SendLightningCmdError::AccountAmbiguous,
            _ => SendLightningCmdError::Quote(message),
        },
        WalletError::NotFound(_) => SendLightningCmdError::NoMatchingAccount,
        WalletError::Cashu(m)
        | WalletError::CashuTyped { message: m, .. }
        | WalletError::Concurrency(m)
        | WalletError::Network(m) => SendLightningCmdError::Quote(m),
        WalletError::Storage(m) => {
            SendLightningCmdError::Storage(StorageError::Internal(m))
        }
        other => SendLightningCmdError::Quote(other.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_send_lightning(
    deps: &CliDeps,
    invoice: String,
    account: Option<String>,
    dry_run: bool,
    no_wait: bool,
    poll_ms: u64,
    timeout_s: u64,
) -> Result<(), SendLightningCmdError> {
    let account_id = parse_account(account.as_deref())?;

    if dry_run {
        let quote = deps
            .wallet
            .quote_send_lightning(account_id, invoice)
            .await
            .map_err(map_err)?;
        print_dry_run(&quote);
        return Ok(());
    }

    // FLAGGED DELTA: no facade method persists a melt quote without
    // firing post_melt, so `--no-wait` cannot print `quote-issued` and
    // exit pre-melt. It degrades to begin + one reconcile poll (same as
    // the wait path with a minimal cadence).
    let _ = no_wait;

    let status = deps
        .wallet
        .begin_send_lightning(account_id, invoice)
        .await
        .map_err(map_err)?;

    drive_to_terminal(deps, status, poll_ms, timeout_s).await
}

pub async fn cmd_send_lightning_complete(
    deps: &CliDeps,
    quote_id: String,
    poll_ms: u64,
    timeout_s: u64,
) -> Result<(), SendLightningCmdError> {
    let id = Uuid::from_str(&quote_id)
        .map_err(|_| SendLightningCmdError::InvalidQuoteId(quote_id.clone()))?;
    // Resume an in-flight melt by its quote id (the facade poll is
    // keyed by quote_id and is reconcile-aware).
    let status = deps
        .wallet
        .poll_send_lightning(id)
        .await
        .map_err(map_err)?;
    drive_to_terminal(deps, status, poll_ms, timeout_s).await
}

/// Shell-owned poll cadence over the facade's single-shot
/// `poll_send_lightning`. An `InFlight` is the typed, non-error signal
/// (P0-1) — poll, never re-`begin`. Preserves the prior `poll_ms`/
/// `timeout_s` semantics + the `paid`/`failed`/`timed-out` bodies.
async fn drive_to_terminal(
    deps: &CliDeps,
    initial: SendLightningStatus,
    poll_ms: u64,
    timeout_s: u64,
) -> Result<(), SendLightningCmdError> {
    let mut status = initial;
    let deadline = Instant::now() + Duration::from_secs(timeout_s);
    loop {
        match status {
            SendLightningStatus::Paid(receipt) => {
                let body = PaidOutput {
                    status: "paid",
                    quote_id: receipt.quote_id.to_string(),
                    amount: receipt.amount.amount().to_string(),
                    lightning_fee: receipt.lightning_fee.amount().to_string(),
                    cashu_fee: receipt.cashu_fee.amount().to_string(),
                    total_fee: receipt.total_fee.amount().to_string(),
                    amount_spent: receipt.amount_spent.amount().to_string(),
                    payment_preimage: receipt.payment_preimage.clone(),
                    account_id: receipt.account_id.to_string(),
                    payment_hash: receipt.payment_hash.clone(),
                };
                println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
                return Ok(());
            }
            SendLightningStatus::Failed { quote_id, reason } => {
                let body = FailedOutput {
                    status: "failed",
                    quote_id: quote_id.to_string(),
                    reason,
                };
                println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
                return Ok(());
            }
            SendLightningStatus::InFlight { quote_id } => {
                if Instant::now() >= deadline {
                    let body = TimedOutOutput {
                        status: "timed-out",
                        quote_id: quote_id.to_string(),
                        payment_hash: String::new(),
                    };
                    println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(poll_ms.max(1))).await;
                status = deps
                    .wallet
                    .poll_send_lightning(quote_id)
                    .await
                    .map_err(map_err)?;
            }
        }
    }
}

fn print_dry_run(quote: &SendLightningQuote) {
    let body = QuoteOutput {
        status: "quote",
        amount: quote.amount.amount().to_string(),
        lightning_fee_reserve: quote.lightning_fee_reserve.amount().to_string(),
        cashu_fee: quote.cashu_fee.amount().to_string(),
        total_fee: quote.total_fee.amount().to_string(),
        total_amount: quote.total_amount.amount().to_string(),
        unit: quote.amount.unit().to_string(),
        currency: quote.amount.currency().to_string(),
        account_id: quote.account_id.to_string(),
        payment_hash: quote.payment_hash.clone(),
    };
    println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
}

fn parse_account(
    requested: Option<&str>,
) -> Result<Option<AccountId>, SendLightningCmdError> {
    match requested {
        None => Ok(None),
        Some(s) => {
            let id = Uuid::from_str(s)
                .map_err(|_| SendLightningCmdError::InvalidAccountId(s.to_string()))?;
            Ok(Some(AccountId::from(id)))
        }
    }
}
