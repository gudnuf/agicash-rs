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
