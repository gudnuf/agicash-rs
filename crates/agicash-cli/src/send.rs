//! `agicash send token <amount>` subcommand — composed over the
//! `WalletClient` facade.
//!
//! `--dry-run` → `quote_send_token`; commit → `send_token` (the facade
//! owns proof-select → NUT-03 swap → V3/V4 encode, the same
//! `SendSwapService` path the CLI drove inline). The dry-run body needs
//! the account's `mint_url`, which `SendTokenQuote` does not carry, so
//! the shell resolves it from `list_accounts()` by `account_id` — one
//! extra read, output byte-for-byte the pre-migration contract.

use crate::composition::CliDeps;
use agicash_domain::{AccountId, Currency};
use agicash_money::{Money, Unit};
use agicash_traits::{AuthError, StorageError};
use agicash_wallet::{CashuDiscriminator, TokenVersion, WalletError};
use rust_decimal::Decimal;
use serde::Serialize;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SendCmdError {
    #[error("not logged in")]
    NotLoggedIn,
    #[error("no matching account")]
    NoMatchingAccount,
    #[error("account ambiguous — pass --account <id>")]
    AccountAmbiguous,
    #[error("invalid account id: {0}")]
    InvalidAccountId(String),
    #[error("unsupported token version: {0}")]
    UnsupportedTokenVersion(u8),
    #[error("token encode error: {0}")]
    TokenEncode(String),
    #[error("insufficient balance: {0}")]
    InsufficientBalance(String),
    #[error("send failed: {0}")]
    Send(String),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Auth(#[from] AuthError),
}

#[derive(Serialize)]
struct SendOutput<'a> {
    status: &'a str,
    token: String,
    amount: String,
    fee: String,
    unit: String,
    currency: String,
    account_id: String,
    mint_url: String,
    swap_id: String,
    token_hash: String,
}

#[derive(Serialize)]
struct QuoteOutput<'a> {
    status: &'a str,
    amount_requested: String,
    amount_to_send: String,
    total_amount: String,
    total_fee: String,
    cashu_send_fee: String,
    cashu_receive_fee: String,
    unit: String,
    currency: String,
    account_id: String,
    mint_url: String,
}

fn map_err(e: WalletError) -> SendCmdError {
    match e {
        WalletError::Unauthenticated => SendCmdError::NotLoggedIn,
        WalletError::Validation { code, message } => match code.as_str() {
            "no_account" => SendCmdError::NoMatchingAccount,
            "ambiguous_account" => SendCmdError::AccountAmbiguous,
            _ => SendCmdError::Send(message),
        },
        WalletError::NotFound(_) => SendCmdError::NoMatchingAccount,
        WalletError::CashuTyped {
            discriminator: CashuDiscriminator::InsufficientBalance,
            message,
        } => SendCmdError::InsufficientBalance(message),
        WalletError::Cashu(m) | WalletError::CashuTyped { message: m, .. } => {
            if m.contains("encode") || m.contains("proof decode") {
                SendCmdError::TokenEncode(m)
            } else {
                SendCmdError::Send(m)
            }
        }
        WalletError::Network(m) => SendCmdError::Send(m),
        WalletError::Storage(m) => SendCmdError::Storage(StorageError::Internal(m)),
        WalletError::Internal(m) => SendCmdError::TokenEncode(m),
        other => SendCmdError::Send(other.to_string()),
    }
}

async fn mint_url_for(deps: &CliDeps, account_id: AccountId) -> Result<String, SendCmdError> {
    let accounts = deps.wallet.list_accounts().await.map_err(map_err)?;
    accounts
        .into_iter()
        .find(|a| a.id == account_id)
        .and_then(|a| a.mint_url)
        .ok_or_else(|| {
            SendCmdError::Storage(StorageError::Internal(
                "account.details missing mint_url".into(),
            ))
        })
}

#[allow(clippy::too_many_arguments)]
pub async fn cmd_send(
    deps: &CliDeps,
    amount: u64,
    account: Option<String>,
    token_version: u8,
    dry_run: bool,
) -> Result<(), SendCmdError> {
    if token_version != 3 && token_version != 4 {
        return Err(SendCmdError::UnsupportedTokenVersion(token_version));
    }
    let account_id = parse_account(account.as_deref())?;

    // Cashu token send always settles a BTC Cashu account in the prior
    // CLI (it derived the unit from the picked account's currency, which
    // for `send token` is BTC in every supported path). The facade's
    // `quote_send_token`/`send_token` pick by the amount's currency.
    let amount_money = Money::new(Decimal::from(amount), Currency::Btc, Unit::Sat);

    if dry_run {
        let quote = deps
            .wallet
            .quote_send_token(account_id, amount_money)
            .await
            .map_err(map_err)?;
        let mint_url = mint_url_for(deps, quote.account_id).await?;
        let body = QuoteOutput {
            status: "quote",
            amount_requested: quote.amount_requested.amount().to_string(),
            amount_to_send: quote.amount_to_send.amount().to_string(),
            total_amount: quote.total_amount.amount().to_string(),
            total_fee: quote.total_fee.amount().to_string(),
            cashu_send_fee: quote.cashu_send_fee.amount().to_string(),
            cashu_receive_fee: quote.cashu_receive_fee.amount().to_string(),
            unit: quote.amount_to_send.unit().to_string(),
            currency: quote.amount_to_send.currency().to_string(),
            account_id: quote.account_id.to_string(),
            mint_url,
        };
        println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
        return Ok(());
    }

    let tv = if token_version == 3 {
        TokenVersion::V3
    } else {
        TokenVersion::V4
    };
    let receipt = deps
        .wallet
        .send_token(account_id, amount_money, tv)
        .await
        .map_err(map_err)?;

    let body = SendOutput {
        status: "sent",
        token: receipt.token.clone(),
        amount: receipt.amount.amount().to_string(),
        fee: receipt.fee.amount().to_string(),
        unit: receipt.amount.unit().to_string(),
        currency: receipt.amount.currency().to_string(),
        account_id: receipt.account_id.to_string(),
        mint_url: receipt.mint_url.clone(),
        swap_id: receipt.swap_id.to_string(),
        token_hash: receipt.token_hash.clone(),
    };
    println!("{}", serde_json::to_string(&body).expect("serialize JSON"));
    Ok(())
}

fn parse_account(requested: Option<&str>) -> Result<Option<AccountId>, SendCmdError> {
    match requested {
        None => Ok(None),
        Some(s) => {
            let id =
                Uuid::parse_str(s).map_err(|_| SendCmdError::InvalidAccountId(s.to_string()))?;
            Ok(Some(AccountId::from(id)))
        }
    }
}
