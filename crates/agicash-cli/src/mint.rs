//! `mint` and `balance` subcommands — composed over `WalletClient`.
//!
//! `mint add` → `WalletClient::add_mint` (the de-duplicated
//! `add_mint_account` primitive: NUT-06 discovery + user-row
//! preservation + brand-new-guest Spark workaround, identical to the
//! pre-migration inline body). `balance` reconstructs the exact prior
//! per-account JSON from `WalletClient::balance` + `WalletClient::
//! exchange_rate` — same fields, same per-account-non-fatal rate
//! handling. stdout JSON is byte-for-byte the pre-migration contract.

use crate::composition::CliDeps;
use agicash_domain::Currency;
use agicash_traits::{AuthError, StorageError};
use agicash_wallet::WalletError;
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum MintCmdError {
    #[error("not authenticated; run `agicash auth login`")]
    NotLoggedIn,
    #[error("invalid mint URL: {0}")]
    InvalidUrl(String),
    #[error("mint unreachable: {0}")]
    MintUnreachable(String),
    #[error("mint protocol error: {0}")]
    MintError(String),
    #[error("unsupported currency: {0}")]
    UnsupportedCurrency(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Auth(#[from] AuthError),
}

/// Map a facade `WalletError` from `add_mint` / `balance` back onto the
/// CLI's `MintCmdError` so the existing error-code + exit-code contract
/// (`classify_error`) is preserved byte-for-byte.
fn map_mint_err(e: WalletError) -> MintCmdError {
    match e {
        WalletError::Unauthenticated => MintCmdError::NotLoggedIn,
        WalletError::Validation { code, message } if code == "bad_url" => {
            MintCmdError::InvalidUrl(message)
        }
        WalletError::Validation { message, .. } => MintCmdError::UnsupportedCurrency(message),
        WalletError::Network(m) => MintCmdError::MintUnreachable(m),
        WalletError::Cashu(m) => MintCmdError::MintError(m),
        // Preserve the pre-migration storage error codes: a storage
        // `NotFound` classified as `not-found`; other storage errors as
        // `storage-backend-error` (`classify_storage`, main.rs). NOTE
        // (flagged delta): the facade flattens `StorageError` to
        // `WalletError::Storage(String)`, erasing the
        // Backend/Internal/Network distinction, so a storage `Internal`
        // or `Network` now classifies as `storage-backend-error` instead
        // of `internal-error`/`network-error`. The dominant real storage
        // failure is `Backend`; this preserves that + the not-found case.
        WalletError::NotFound(_) => MintCmdError::Storage(StorageError::NotFound),
        WalletError::Storage(m) => MintCmdError::Storage(StorageError::Backend(m)),
        other => MintCmdError::Storage(StorageError::Backend(other.to_string())),
    }
}

#[derive(Serialize)]
struct MintAddOutput<'a> {
    status: &'a str,
    account_id: String,
    mint_name: String,
    mint_url: String,
}

pub async fn cmd_mint_add(
    deps: &CliDeps,
    url: &str,
    currency: Currency,
) -> Result<(), MintCmdError> {
    let summary = deps
        .wallet
        .add_mint(url.to_string(), currency)
        .await
        .map_err(map_mint_err)?;

    print_json(&MintAddOutput {
        status: "added",
        account_id: summary.id.to_string(),
        mint_name: summary.name.clone(),
        mint_url: summary.mint_url.clone().unwrap_or_default(),
    });
    Ok(())
}

/// `mint list` — list every configured Cashu mint.
///
/// Wraps [`WalletClient::list_mints`], which groups the user's Cashu
/// accounts by mint URL. The facade's `MintSummary` is already
/// `Serialize`, so it is emitted verbatim — a JSON array, one object per
/// mint (`mint_url`, `mint_name`, `accounts`).
pub async fn cmd_mint_list(deps: &CliDeps) -> Result<(), MintCmdError> {
    let mints = deps.wallet.list_mints().await.map_err(map_mint_err)?;
    print_json(&mints);
    Ok(())
}

#[derive(Serialize)]
struct BalanceEntry {
    account_id: String,
    name: String,
    currency: String,
    balance: String,
    unit: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    btc_equivalent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rate_btc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    btc_equivalent_error: Option<String>,
}

pub async fn cmd_balance(deps: &CliDeps) -> Result<(), MintCmdError> {
    let summary = deps.wallet.balance(None).await.map_err(map_mint_err)?;

    let mut entries: Vec<BalanceEntry> = Vec::with_capacity(summary.per_account.len());
    for acct in &summary.per_account {
        let unit = match acct.currency {
            Currency::Btc => "sat".to_string(),
            Currency::Usd | Currency::Usdb => "cent".to_string(),
        };

        let mut entry = BalanceEntry {
            account_id: acct.id.to_string(),
            name: acct.name.clone(),
            currency: acct.currency.to_string(),
            balance: acct.balance.clone(),
            unit,
            btc_equivalent: None,
            rate_btc: None,
            btc_equivalent_error: None,
        };

        // Non-BTC accounts: fetch a BTC-equivalent for display. Failures
        // are surfaced per-account, not as a top-level error — a down
        // rate provider must not crash `balance` (verbatim prior
        // semantics; the prior code exposed the rate verbatim + echoed
        // the raw balance amount as `btc_equivalent`).
        if acct.currency != Currency::Btc {
            match deps
                .wallet
                .exchange_rate(acct.currency, Currency::Btc)
                .await
            {
                Ok(snapshot) => {
                    entry.btc_equivalent = Some(acct.balance.clone());
                    entry.rate_btc = Some(snapshot.rate.to_string());
                }
                Err(e) => {
                    entry.btc_equivalent_error = Some(classify_rate_error(&e));
                }
            }
        }

        entries.push(entry);
    }

    print_json(&entries);
    Ok(())
}

/// Preserve the pre-migration rate-error discriminators. The facade
/// flattens `ExchangeRateError` into `WalletError`; recover the same
/// three strings the CLI emitted before.
fn classify_rate_error(e: &WalletError) -> String {
    match e {
        WalletError::Network(_) => "network-error".into(),
        WalletError::ExchangeRate(m) | WalletError::Validation { message: m, .. } => {
            if m.contains("unsupported") || m.contains("UnsupportedPair") {
                "unsupported-pair".into()
            } else {
                "invalid-response".into()
            }
        }
        WalletError::Unsupported(_) => "unsupported-pair".into(),
        _ => "invalid-response".into(),
    }
}

fn print_json<T: Serialize>(value: &T) {
    println!("{}", serde_json::to_string(value).expect("serialize JSON"));
}
