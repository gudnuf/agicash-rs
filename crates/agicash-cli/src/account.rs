//! `account` subcommands.
//!
//! `account list` emits the raw `wallet.accounts` rows and `account
//! default` calls `update_user_defaults` — neither is on the
//! `WalletClient` facade surface (the facade's `set_default_account` is
//! `Unsupported` in slice 12 and `list_accounts` returns the reshaped
//! `AccountSummary`, not the raw row). Both run against the
//! shell-resident `UserStorage` handle the composition root builds from
//! the SAME endpoint config as the facade — there is no second
//! composition of the network stack. Behavior + stdout JSON are
//! byte-for-byte the pre-migration contract.

use crate::composition::CliDeps;
use agicash_domain::{AccountId, Currency, UserId};
use agicash_traits::{AuthError, StorageError, UpdateUserDefaults, UserStorage};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum AccountCmdError {
    #[error("not authenticated; run `agicash auth login`")]
    NotLoggedIn,
    #[error("invalid account id (expected UUID): {0}")]
    InvalidId(String),
    #[error("account not found: {0}")]
    NotFound(AccountId),
    #[error("currency {0:?} not supported for default-account selection (only BTC and USD)")]
    UnsupportedCurrency(Currency),
    #[error(transparent)]
    Auth(#[from] AuthError),
    #[error(transparent)]
    Storage(#[from] StorageError),
}

pub async fn cmd_list(deps: &CliDeps) -> Result<(), AccountCmdError> {
    let session = deps
        .keyring
        .load()
        .await?
        .ok_or(AccountCmdError::NotLoggedIn)?;
    let user_id = UserId::from(session.user_id);
    let accounts = deps.user_storage.list_accounts(user_id).await?;
    println!(
        "{}",
        serde_json::to_string(&accounts).expect("serialize accounts")
    );
    Ok(())
}

/// `account info <ID>` — detail for a single account.
///
/// A read sibling of `account list` / `account default`. Resolves the
/// account through the same ownership-scoped `list_accounts` path
/// `cmd_set_default` uses (the `UserStorage::get_account` trait method is
/// not user-scoped), so it emits the identical raw `Account` JSON shape
/// `account list` produces and a foreign account id surfaces as
/// `not-found` (exit 4) rather than leaking another user's row.
pub async fn cmd_info(deps: &CliDeps, id_str: &str) -> Result<(), AccountCmdError> {
    let session = deps
        .keyring
        .load()
        .await?
        .ok_or(AccountCmdError::NotLoggedIn)?;
    let user_id = UserId::from(session.user_id);

    let parsed = Uuid::parse_str(id_str).map_err(|_| AccountCmdError::InvalidId(id_str.into()))?;
    let account_id = AccountId::from(parsed);

    let accounts = deps.user_storage.list_accounts(user_id).await?;
    let account = accounts
        .into_iter()
        .find(|a| a.id == account_id)
        .ok_or(AccountCmdError::NotFound(account_id))?;

    println!(
        "{}",
        serde_json::to_string(&account).expect("serialize account")
    );
    Ok(())
}

pub async fn cmd_set_default(deps: &CliDeps, id_str: &str) -> Result<(), AccountCmdError> {
    let session = deps
        .keyring
        .load()
        .await?
        .ok_or(AccountCmdError::NotLoggedIn)?;
    let user_id = UserId::from(session.user_id);

    let parsed = Uuid::parse_str(id_str).map_err(|_| AccountCmdError::InvalidId(id_str.into()))?;
    let account_id = AccountId::from(parsed);

    // Look up the account to figure out which per-currency slot to set.
    let accounts = deps.user_storage.list_accounts(user_id).await?;
    let account = accounts
        .into_iter()
        .find(|a| a.id == account_id)
        .ok_or(AccountCmdError::NotFound(account_id))?;

    let patch = match account.currency {
        Currency::Btc => UpdateUserDefaults {
            default_btc_account_id: Some(Some(account_id)),
            ..Default::default()
        },
        Currency::Usd => UpdateUserDefaults {
            default_usd_account_id: Some(Some(account_id)),
            ..Default::default()
        },
        other @ Currency::Usdb => return Err(AccountCmdError::UnsupportedCurrency(other)),
    };

    let user = deps
        .user_storage
        .update_user_defaults(user_id, patch)
        .await?;
    println!("{}", serde_json::to_string(&user).expect("serialize user"));
    Ok(())
}
