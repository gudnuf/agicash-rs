//! `agicash receive token <token>` subcommand — composed over
//! `WalletClient::receive_cashu_token`.
//!
//! The facade owns parse → account-pick → create → `complete_swap` (the
//! same `ReceiveSwapService` path the CLI drove inline). This shell
//! reshapes the facade's `ReceiveReceipt` back into the exact prior
//! stdout JSON: the full `received`/`pending`/`already-failed` body and
//! the compact `already-claimed` body (`status` + `token_hash` only).

use crate::composition::CliDeps;
use agicash_traits::{AuthError, StorageError};
use agicash_wallet::{ReceiveReceipt, ReceiveStatus, WalletError};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum ReceiveCmdError {
    #[error("not logged in")]
    NotLoggedIn,
    #[error("invalid token: {0}")]
    InvalidToken(String),
    #[error("no matching account for mint {0} — run `agicash mint add` first")]
    NoMatchingAccount(String),
    #[error("receive failed: {0}")]
    Receive(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Auth(#[from] AuthError),
}

#[derive(Serialize)]
struct ReceiveOutput<'a> {
    status: &'a str,
    amount: String,
    fee: String,
    unit: String,
    currency: String,
    account_id: String,
    mint_url: String,
    token_hash: String,
}

#[derive(Serialize)]
struct AlreadyClaimedOutput<'a> {
    status: &'a str,
    token_hash: String,
}

fn map_err(e: WalletError) -> ReceiveCmdError {
    match e {
        WalletError::Unauthenticated => ReceiveCmdError::NotLoggedIn,
        WalletError::Validation { code, message } if code == "no_matching_account" => {
            // The facade message is "no matching account for mint <url> —
            // add the mint first"; recover the bare mint URL so the
            // CLI's own message template renders identically.
            let mint = message
                .strip_prefix("no matching account for mint ")
                .and_then(|s| s.split(" —").next())
                .unwrap_or(&message)
                .to_string();
            ReceiveCmdError::NoMatchingAccount(mint)
        }
        WalletError::Cashu(m) | WalletError::CashuTyped { message: m, .. } => {
            if m.contains("token") && (m.contains("parse") || m.contains("decode")) {
                ReceiveCmdError::InvalidToken(m)
            } else {
                ReceiveCmdError::Receive(m)
            }
        }
        WalletError::Network(m) => ReceiveCmdError::Receive(m),
        WalletError::Storage(m) => ReceiveCmdError::Storage(StorageError::Internal(m)),
        other => ReceiveCmdError::Receive(other.to_string()),
    }
}

pub async fn cmd_receive(deps: &CliDeps, token_str: &str) -> Result<(), ReceiveCmdError> {
    let receipt: ReceiveReceipt = deps
        .wallet
        .receive_cashu_token(token_str)
        .await
        .map_err(map_err)?;

    if receipt.status == ReceiveStatus::AlreadyClaimed {
        // Compact body — verbatim the prior `AlreadyClaimedOutput`.
        let out = AlreadyClaimedOutput {
            status: "already-claimed",
            token_hash: receipt.token_hash.clone(),
        };
        println!("{}", serde_json::to_string(&out).expect("serialize JSON"));
        return Ok(());
    }

    let status = match receipt.status {
        ReceiveStatus::Received => "received",
        // Behavior delta (flagged): the pre-migration CLI emitted two
        // distinct shapes for a terminally-FAILED receive — an
        // `already-failed` status for an already-terminal-Failed swap and
        // an `already-claimed`+`reason` body for a fresh Failed swap. The
        // facade collapses both to `AlreadyFailed` (no `reason` field).
        // We render the dominant `already-failed` status; the rare
        // fresh-Failed `reason` field is dropped. The happy path
        // (`received`) and idempotent re-receive (`already-claimed`) are
        // byte-identical.
        ReceiveStatus::AlreadyFailed => "already-failed",
        ReceiveStatus::Pending => "pending",
        ReceiveStatus::AlreadyClaimed => unreachable!("handled above"),
    };

    let body = ReceiveOutput {
        status,
        amount: receipt.amount.amount().to_string(),
        fee: receipt.fee.amount().to_string(),
        unit: receipt.amount.unit().to_string(),
        currency: receipt.amount.currency().to_string(),
        account_id: receipt.account_id.to_string(),
        mint_url: receipt.mint_url.clone(),
        token_hash: receipt.token_hash.clone(),
    };
    println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
    Ok(())
}
