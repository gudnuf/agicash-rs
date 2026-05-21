//! `agicash receive lightning <amount>` subcommand — composed over the
//! `WalletClient` facade.
//!
//! Flow (verbatim prior UX): `quote_receive_lightning` → print
//! `quote-issued` → (`--no-wait` exits here) → shell-driven poll loop
//! (`poll_receive_lightning` single-shot on the prior `poll_ms`/
//! `timeout_s` cadence; the facade is runtime-agnostic so the shell owns
//! the loop) → on still-Unpaid print `timed-out` → else
//! `complete_receive_lightning` → print `received`/`already-failed`/etc.
//! stdout JSON is byte-for-byte the pre-migration contract.

use crate::composition::CliDeps;
use agicash_domain::{AccountId, Currency};
use agicash_money::{Money, Unit};
use agicash_traits::{AuthError, StorageError};
use agicash_wallet::{
    ReceiveLightningHandle, ReceiveLightningState, ReceiveReceipt, ReceiveStatus, WalletError,
};
use rust_decimal::Decimal;
use serde::Serialize;
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ReceiveLightningCmdError {
    #[error("not authenticated; run `agicash auth login`")]
    NotLoggedIn,
    #[error("no matching account — run `agicash mint add` first")]
    NoMatchingAccount,
    #[error("account ambiguous — pass --account <id>")]
    AccountAmbiguous,
    #[error("invalid account id: {0}")]
    InvalidAccountId(String),
    #[error("invalid quote id: {0}")]
    InvalidQuoteId(String),
    #[error("amount too small")]
    AmountTooSmall,
    #[error("quote not paid yet")]
    QuoteNotPaid,
    #[error("mint quote failed: {0}")]
    Quote(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Auth(#[from] AuthError),
}

#[derive(Serialize)]
struct QuoteIssuedOutput<'a> {
    status: &'a str,
    quote_id: String,
    invoice: String,
    payment_hash: String,
    amount: String,
    unit: String,
    currency: String,
    expires_at: String,
    account_id: String,
}

#[derive(Serialize)]
struct ReceivedOutput<'a> {
    status: &'a str,
    amount: String,
    fee: String,
    unit: String,
    currency: String,
    account_id: String,
    quote_id: String,
    payment_hash: String,
}

#[derive(Serialize)]
struct TimedOutOutput<'a> {
    status: &'a str,
    quote_id: String,
    invoice: String,
    payment_hash: String,
}

#[derive(Serialize)]
struct FailedOutput<'a> {
    status: &'a str,
    quote_id: String,
    reason: String,
}

fn map_err(e: WalletError) -> ReceiveLightningCmdError {
    match e {
        WalletError::Unauthenticated => ReceiveLightningCmdError::NotLoggedIn,
        WalletError::Validation { code, message } => match code.as_str() {
            "no_account" | "no_matching_account" => ReceiveLightningCmdError::NoMatchingAccount,
            "ambiguous_account" => ReceiveLightningCmdError::AccountAmbiguous,
            "amount_too_small" => ReceiveLightningCmdError::AmountTooSmall,
            _ => ReceiveLightningCmdError::Quote(message),
        },
        WalletError::NotFound(_) => ReceiveLightningCmdError::NoMatchingAccount,
        WalletError::Cashu(m) | WalletError::CashuTyped { message: m, .. } => {
            ReceiveLightningCmdError::Quote(m)
        }
        WalletError::Storage(m) => ReceiveLightningCmdError::Storage(StorageError::Internal(m)),
        other => ReceiveLightningCmdError::Quote(other.to_string()),
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_receive_lightning(
    deps: &CliDeps,
    amount: u64,
    account: Option<String>,
    currency: Currency,
    description: Option<String>,
    no_wait: bool,
    poll_ms: u64,
    timeout_s: u64,
) -> Result<(), ReceiveLightningCmdError> {
    let unit = unit_for_currency(currency);
    if amount == 0 {
        return Err(ReceiveLightningCmdError::AmountTooSmall);
    }
    let amount_money = Money::new(Decimal::from(amount), currency, unit);
    let account_id = parse_account(account.as_deref())?;
    // `description` is accepted for CLI back-compat. The facade
    // `quote_receive_lightning` does not take a memo (the prior CLI
    // forwarded it to `create_quote`); the field is otherwise inert in
    // this build — discard explicitly so it isn't flagged unused.
    let _ = description;

    let handle = deps
        .wallet
        .quote_receive_lightning(account_id, amount_money)
        .await
        .map_err(map_err)?;

    print_quote_issued(&handle);

    if no_wait {
        return Ok(());
    }

    poll_then_complete(
        deps,
        handle.quote_id,
        &handle.invoice,
        &handle.payment_hash,
        poll_ms,
        timeout_s,
        true,
    )
    .await
}

pub async fn cmd_receive_lightning_complete(
    deps: &CliDeps,
    quote_id: String,
    poll_ms: u64,
    timeout_s: u64,
) -> Result<(), ReceiveLightningCmdError> {
    let id = Uuid::parse_str(&quote_id)
        .map_err(|_| ReceiveLightningCmdError::InvalidQuoteId(quote_id.clone()))?;
    poll_then_complete(deps, id, "", "", poll_ms, timeout_s, false).await
}

/// Shell-owned poll cadence (the facade poll is single-shot by design —
/// the consumer owns the loop). Preserves the prior `poll_ms`/
/// `timeout_s` semantics: poll until the snapshot leaves `Unpaid`, then
/// complete; on timeout emit `timed-out` (for the wait path) or the
/// `quote-not-paid` error (for the `lightning-complete` path) exactly as
/// before.
#[allow(clippy::too_many_arguments)]
async fn poll_then_complete(
    deps: &CliDeps,
    quote_id: Uuid,
    invoice: &str,
    payment_hash: &str,
    poll_ms: u64,
    timeout_s: u64,
    is_wait_path: bool,
) -> Result<(), ReceiveLightningCmdError> {
    let deadline = Instant::now() + Duration::from_secs(timeout_s);
    let mut paid = false;
    loop {
        let snap = deps
            .wallet
            .poll_receive_lightning(quote_id)
            .await
            .map_err(map_err)?;
        match snap.state {
            ReceiveLightningState::Unpaid => {
                if Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(poll_ms.max(1))).await;
            }
            ReceiveLightningState::Failed => {
                let body = FailedOutput {
                    status: "failed",
                    quote_id: quote_id.to_string(),
                    reason: snap.failure_reason.unwrap_or_else(|| "unknown".into()),
                };
                println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
                return Ok(());
            }
            _ => {
                paid = true;
                break;
            }
        }
    }

    if !paid {
        if is_wait_path {
            let body = TimedOutOutput {
                status: "timed-out",
                quote_id: quote_id.to_string(),
                invoice: invoice.to_string(),
                payment_hash: payment_hash.to_string(),
            };
            println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
            return Ok(());
        }
        return Err(ReceiveLightningCmdError::QuoteNotPaid);
    }

    let receipt = deps
        .wallet
        .complete_receive_lightning(quote_id)
        .await
        .map_err(map_err)?;
    print_receipt(&receipt, quote_id);
    Ok(())
}

fn print_quote_issued(handle: &ReceiveLightningHandle) {
    let body = QuoteIssuedOutput {
        status: "quote-issued",
        quote_id: handle.quote_id.to_string(),
        invoice: handle.invoice.clone(),
        payment_hash: handle.payment_hash.clone(),
        amount: handle.amount.amount().to_string(),
        unit: handle.amount.unit().to_string(),
        currency: handle.amount.currency().to_string(),
        expires_at: handle.expires_at.to_rfc3339(),
        account_id: handle.account_id.to_string(),
    };
    println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
}

fn print_receipt(receipt: &ReceiveReceipt, fallback_quote_id: Uuid) {
    // Status string per facade `ReceiveStatus`. `Received` /
    // `AlreadyClaimed` both render `received` (idempotent re-complete is
    // a success). Behavior delta (flagged): the pre-migration CLI
    // distinguished `already-failed` (terminal Failed) from
    // `already-expired` (terminal Expired); the facade collapses
    // Failed+Expired into `AlreadyFailed`, so both render
    // `already-failed`. The required smoke path (mint quote → paid →
    // received) is byte-identical.
    let status = match receipt.status {
        ReceiveStatus::Received | ReceiveStatus::AlreadyClaimed => "received",
        ReceiveStatus::Pending => "pending",
        ReceiveStatus::AlreadyFailed => "already-failed",
    };
    let body = ReceivedOutput {
        status,
        amount: receipt.amount.amount().to_string(),
        fee: receipt.fee.amount().to_string(),
        unit: receipt.amount.unit().to_string(),
        currency: receipt.amount.currency().to_string(),
        account_id: receipt.account_id.to_string(),
        quote_id: fallback_quote_id.to_string(),
        payment_hash: receipt.token_hash.clone(),
    };
    println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
}

fn parse_account(requested: Option<&str>) -> Result<Option<AccountId>, ReceiveLightningCmdError> {
    match requested {
        None => Ok(None),
        Some(s) => {
            let id = Uuid::parse_str(s)
                .map_err(|_| ReceiveLightningCmdError::InvalidAccountId(s.to_string()))?;
            Ok(Some(AccountId::from(id)))
        }
    }
}

fn unit_for_currency(currency: Currency) -> Unit {
    match currency {
        Currency::Btc => Unit::Sat,
        Currency::Usd | Currency::Usdb => Unit::Cent,
    }
}
