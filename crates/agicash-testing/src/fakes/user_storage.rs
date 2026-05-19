//! `InMemoryUserStorage` + a `cashu_account` builder.
//!
//! Round-trips `User` rows and `Account` rows in `parking_lot::Mutex`
//! maps (house style). The trait's RPC-shaped surface
//! (`upsert_user_with_accounts`, `get_user`, `list_accounts`,
//! `get_account`, `update_user_defaults`) is implemented faithfully enough
//! for the facade glue under test; `insert_account` / `insert_user` are
//! test-only helpers for seeding state directly.

use agicash_domain::{
    Account, AccountId, AccountPurpose, AccountState, AccountType, Currency, User, UserId,
};
use agicash_traits::{
    StorageError, UpdateUserDefaults, UpsertUserInput, UpsertUserResult, UserStorage,
};
use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;
use serde_json::json;
use std::collections::HashMap;
use uuid::Uuid;

/// Build a minimal active Cashu [`Account`] for `user_id`, with
/// `mint_url` landing in `details["mint_url"]` (the JSONB key the cashu
/// provider extracts).
#[must_use]
pub fn cashu_account(user_id: UserId, mint_url: &str, currency: Currency) -> Account {
    Account {
        id: AccountId::new(),
        created_at: Utc::now(),
        user_id,
        name: "Test Mint".into(),
        account_type: AccountType::Cashu,
        purpose: AccountPurpose::Transactional,
        currency,
        details: json!({ "mint_url": mint_url, "keyset_counters": {} }),
        version: 0,
        state: AccountState::Active,
        expires_at: None,
    }
}

/// Build a minimal `User` row for `user_id`.
#[must_use]
fn bare_user(user_id: UserId) -> User {
    User {
        id: user_id,
        created_at: Utc::now(),
        email: None,
        email_verified: false,
        username: format!("user-{user_id}"),
        default_btc_account_id: None,
        default_usd_account_id: None,
        default_currency: Currency::Btc,
        cashu_locking_xpub: "xpub-test".into(),
        encryption_public_key: "enc-test".into(),
        spark_identity_public_key: "spark-test".into(),
        terms_accepted_at: None,
        gift_card_mint_terms_accepted_at: None,
    }
}

/// In-memory `UserStorage`. No network. Deterministic.
#[derive(Debug, Default)]
pub struct InMemoryUserStorage {
    users: Mutex<HashMap<Uuid, User>>,
    accounts: Mutex<HashMap<Uuid, Account>>,
}

impl InMemoryUserStorage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Test helper: seed an account row directly (also materializes a bare
    /// `User` row for its owner if none exists yet).
    pub fn insert_account(&self, account: Account) {
        self.users
            .lock()
            .entry(account.user_id.as_uuid())
            .or_insert_with(|| bare_user(account.user_id));
        self.accounts
            .lock()
            .insert(account.id.as_uuid(), account);
    }

    /// Test helper: seed a user row directly.
    pub fn insert_user(&self, user: User) {
        self.users.lock().insert(user.id.as_uuid(), user);
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl UserStorage for InMemoryUserStorage {
    async fn upsert_user_with_accounts(
        &self,
        input: UpsertUserInput,
    ) -> Result<UpsertUserResult, StorageError> {
        let mut user = bare_user(input.user_id);
        user.email = input.email;
        user.email_verified = input.email_verified;
        user.cashu_locking_xpub = input.cashu_locking_xpub;
        user.encryption_public_key = input.encryption_public_key;
        user.spark_identity_public_key = input.spark_identity_public_key;
        user.terms_accepted_at = input.terms_accepted_at;
        user.gift_card_mint_terms_accepted_at = input.gift_card_mint_terms_accepted_at;

        let accounts: Vec<Account> = input
            .accounts
            .into_iter()
            .map(|a| Account {
                id: AccountId::new(),
                created_at: Utc::now(),
                user_id: input.user_id,
                name: a.name,
                account_type: a.account_type,
                purpose: a.purpose,
                currency: a.currency,
                details: a.details,
                version: 0,
                state: AccountState::Active,
                expires_at: None,
            })
            .collect();

        {
            let mut users = self.users.lock();
            users.insert(input.user_id.as_uuid(), user.clone());
        }
        {
            let mut store = self.accounts.lock();
            for acc in &accounts {
                store.insert(acc.id.as_uuid(), acc.clone());
            }
        }
        Ok(UpsertUserResult { user, accounts })
    }

    async fn get_user(&self, user_id: UserId) -> Result<Option<User>, StorageError> {
        Ok(self.users.lock().get(&user_id.as_uuid()).cloned())
    }

    async fn list_accounts(&self, user_id: UserId) -> Result<Vec<Account>, StorageError> {
        Ok(self
            .accounts
            .lock()
            .values()
            .filter(|a| a.user_id == user_id && a.state == AccountState::Active)
            .cloned()
            .collect())
    }

    async fn get_account(
        &self,
        account_id: AccountId,
    ) -> Result<Option<Account>, StorageError> {
        Ok(self.accounts.lock().get(&account_id.as_uuid()).cloned())
    }

    async fn update_user_defaults(
        &self,
        user_id: UserId,
        patch: UpdateUserDefaults,
    ) -> Result<User, StorageError> {
        let mut users = self.users.lock();
        let user = users
            .get_mut(&user_id.as_uuid())
            .ok_or(StorageError::NotFound)?;
        if let Some(v) = patch.default_btc_account_id {
            user.default_btc_account_id = v;
        }
        if let Some(v) = patch.default_usd_account_id {
            user.default_usd_account_id = v;
        }
        if let Some(c) = patch.default_currency {
            user.default_currency = c;
        }
        Ok(user.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn insert_account_then_get_and_list_round_trip() {
        let s = InMemoryUserStorage::new();
        let uid = UserId::new();
        let acc = cashu_account(uid, "https://mint.example", Currency::Btc);
        let acc_id = acc.id;
        s.insert_account(acc);

        let got = s.get_account(acc_id).await.unwrap().expect("present");
        assert_eq!(got.user_id, uid);
        assert_eq!(
            got.details.get("mint_url").and_then(|v| v.as_str()),
            Some("https://mint.example")
        );
        assert_eq!(s.list_accounts(uid).await.unwrap().len(), 1);
        // Owner's bare user row was materialized.
        assert!(s.get_user(uid).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn get_account_for_missing_id_is_none() {
        let s = InMemoryUserStorage::new();
        assert!(s.get_account(AccountId::new()).await.unwrap().is_none());
    }
}
