//! Postgrest-backed [`CashuMeltQuoteStorage`] implementation.
//!
//! Mirrors `app/features/send/cashu-send-quote-repository.ts`. The five
//! RPCs we call (`create_cashu_send_quote`,
//! `mark_cashu_send_quote_as_pending`, `complete_cashu_send_quote`,
//! `expire_cashu_send_quote`, `fail_cashu_send_quote`) live in the
//! `wallet` schema.
//!
//! Encryption is hidden inside this impl. We accept an
//! [`Arc<dyn ProofEncryption>`] dep at construction; slice 5 wires up
//! [`agicash_traits::PassthroughProofEncryption`] which encodes plaintext
//! JSON as base64 in the `encrypted_data` blob.

use crate::SupabaseStorage;
use agicash_cashu::{
    CashuMeltQuote, CashuMeltQuoteStorage, CompleteMeltQuote, CompleteMeltQuoteResult,
    CreateMeltQuote, CreateMeltQuoteResult, MeltQuoteStorageError,
};
use agicash_domain::{Account, UserId};
use agicash_money::Money;
use agicash_traits::ProofEncryption;
use async_trait::async_trait;
use base64::engine::general_purpose;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

/// Postgres-backed melt-quote storage.
pub struct SupabaseCashuMeltQuoteStorage {
    base: Arc<SupabaseStorage>,
    encryption: Arc<dyn ProofEncryption>,
}

impl std::fmt::Debug for SupabaseCashuMeltQuoteStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupabaseCashuMeltQuoteStorage")
            .finish_non_exhaustive()
    }
}

impl SupabaseCashuMeltQuoteStorage {
    pub fn new(base: Arc<SupabaseStorage>, encryption: Arc<dyn ProofEncryption>) -> Self {
        Self { base, encryption }
    }

    async fn encrypt_to_base64(&self, value: &Value) -> Result<String, MeltQuoteStorageError> {
        let bytes = serde_json::to_vec(value)
            .map_err(|e| MeltQuoteStorageError::Backend(format!("encode encrypted_data: {e}")))?;
        let cipher = self.encryption.encrypt(&bytes).await?;
        Ok(general_purpose::STANDARD.encode(cipher))
    }
}

/// JSON inside `encrypted_data` (mirrors TS `CashuLightningSendDbDataSchema`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LightningSendData {
    payment_request: String,
    amount_requested: Money,
    amount_requested_in_msat: u64,
    amount_received: Money,
    lightning_fee_reserve: Money,
    cashu_send_fee: Money,
    melt_quote_id: String,
    amount_reserved: Money,
    /// Populated when the quote transitions to PAID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payment_preimage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lightning_fee: Option<Money>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    amount_spent: Option<Money>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total_fee: Option<Money>,
}

/// Wire shape for one change-proof in `complete_cashu_send_quote`
/// (matches the `wallet.cashu_proof_input` composite type's camelCase
/// field names).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EncryptedProofInput {
    keyset_id: String,
    amount: String,
    secret: String,
    unblinded_signature: String,
    public_key_y: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    dleq: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    witness: Option<Value>,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl CashuMeltQuoteStorage for SupabaseCashuMeltQuoteStorage {
    async fn create(
        &self,
        input: CreateMeltQuote,
    ) -> Result<CreateMeltQuoteResult, MeltQuoteStorageError> {
        let data = LightningSendData {
            payment_request: input.payment_request.clone(),
            amount_requested: input.amount_requested,
            amount_requested_in_msat: input.amount_requested_in_msat,
            amount_received: input.amount_received,
            lightning_fee_reserve: input.lightning_fee_reserve,
            cashu_send_fee: input.cashu_fee,
            melt_quote_id: input.quote_id.clone(),
            amount_reserved: input.amount_reserved,
            payment_preimage: None,
            lightning_fee: None,
            amount_spent: None,
            total_fee: None,
        };
        let encrypted_data = self
            .encrypt_to_base64(
                &serde_json::to_value(&data).map_err(|e| {
                    MeltQuoteStorageError::Backend(format!("encode send data: {e}"))
                })?,
            )
            .await?;
        let quote_id_hash = sha256_hex(&input.quote_id);

        let body = serde_json::to_string(&json!({
            "p_user_id": input.user_id,
            "p_account_id": input.account_id,
            "p_currency": input.amount_received.currency(),
            "p_currency_requested": input.amount_requested.currency(),
            "p_expires_at": input.expires_at,
            "p_keyset_id": input.keyset_id,
            "p_number_of_change_outputs": input.number_of_change_outputs,
            "p_proofs_to_send": input.proof_ids,
            "p_encrypted_data": encrypted_data,
            "p_quote_id_hash": quote_id_hash,
            "p_payment_hash": input.payment_hash,
        }))
        .map_err(|e| MeltQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;

        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("create_cashu_send_quote", body)
            .execute()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            if text.contains("CONCURRENCY_ERROR") {
                return Err(MeltQuoteStorageError::Concurrency(text));
            }
            // The partial unique index
            // `cashu_send_quotes_payment_hash_active_unique` rejects a
            // second active (UNPAID/PENDING/PAID) quote for the same
            // `(user_id, payment_hash)`. Postgrest surfaces this as a
            // 23505 with the constraint name in the body. Detect the
            // constraint by name so we don't mis-classify the *other*
            // unique index on this table (`..._quote_id_hash_key`).
            // Mirrors the 23505 handling in
            // `cashu_receive_swap_storage.rs` (`AlreadyClaimed`).
            if is_duplicate_payment_violation(&text) {
                return Err(MeltQuoteStorageError::DuplicatePayment);
            }
            return Err(MeltQuoteStorageError::Backend(format!(
                "create_cashu_send_quote: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MeltQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let quote_value = value
            .get("quote")
            .cloned()
            .ok_or_else(|| MeltQuoteStorageError::Backend("missing quote field".into()))?;
        let account_value = value
            .get("account")
            .cloned()
            .ok_or_else(|| MeltQuoteStorageError::Backend("missing account field".into()))?;
        let quote = self.row_to_quote(quote_value).await?;
        let account = parse_account(account_value)?;
        Ok(CreateMeltQuoteResult { quote, account })
    }

    async fn mark_as_pending(
        &self,
        quote_id: Uuid,
    ) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let body = serde_json::to_string(&json!({ "p_quote_id": quote_id }))
            .map_err(|e| MeltQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("mark_cashu_send_quote_as_pending", body)
            .execute()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            if text.contains("NOT_FOUND") {
                return Err(MeltQuoteStorageError::NotFound);
            }
            if text.contains("INVALID_STATE") {
                return Err(MeltQuoteStorageError::InvalidState(text));
            }
            return Err(MeltQuoteStorageError::Backend(format!(
                "mark_cashu_send_quote_as_pending: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MeltQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let quote_value = value
            .get("quote")
            .cloned()
            .ok_or_else(|| MeltQuoteStorageError::Backend("missing quote field".into()))?;
        self.row_to_quote(quote_value).await
    }

    #[allow(clippy::too_many_lines)]
    async fn complete(
        &self,
        input: CompleteMeltQuote,
    ) -> Result<CompleteMeltQuoteResult, MeltQuoteStorageError> {
        // Re-encrypt the send-data blob with the resolved
        // preimage/fees/amount_spent populated.
        let cashu_fee = input.quote.cashu_fee;
        let amount_received = input.quote.amount_received;
        let amount_spent = input.amount_spent;
        let lightning_fee = sub_money(&sub_money(&amount_spent, &amount_received)?, &cashu_fee)?;
        let total_fee = add_money(&lightning_fee, &cashu_fee)?;
        let data = LightningSendData {
            payment_request: input.quote.payment_request.clone(),
            amount_requested: input.quote.amount_requested,
            amount_requested_in_msat: input.quote.amount_requested_in_msat,
            amount_received: input.quote.amount_received,
            lightning_fee_reserve: input.quote.lightning_fee_reserve,
            cashu_send_fee: input.quote.cashu_fee,
            melt_quote_id: input.quote.quote_id.clone(),
            amount_reserved: input.quote.amount_reserved,
            payment_preimage: Some(input.payment_preimage.clone()),
            lightning_fee: Some(lightning_fee),
            amount_spent: Some(amount_spent),
            total_fee: Some(total_fee),
        };
        let encrypted_data = self
            .encrypt_to_base64(
                &serde_json::to_value(&data).map_err(|e| {
                    MeltQuoteStorageError::Backend(format!("encode send data: {e}"))
                })?,
            )
            .await?;

        let mut encrypted_change: Vec<EncryptedProofInput> =
            Vec::with_capacity(input.change_proofs.len());
        for p in &input.change_proofs {
            let amount_enc = self
                .encryption
                .encrypt(p.amount.to_string().as_bytes())
                .await?;
            let secret_enc = self.encryption.encrypt(p.secret.as_bytes()).await?;
            encrypted_change.push(EncryptedProofInput {
                keyset_id: p.id.clone(),
                amount: general_purpose::STANDARD.encode(amount_enc),
                secret: general_purpose::STANDARD.encode(secret_enc),
                unblinded_signature: p.c.clone(),
                public_key_y: proof_to_y(&p.secret),
                dleq: p.dleq.clone(),
                witness: p.witness.clone(),
            });
        }

        let body = serde_json::to_string(&json!({
            "p_quote_id": input.quote.id,
            "p_change_proofs": encrypted_change,
            "p_encrypted_data": encrypted_data,
        }))
        .map_err(|e| MeltQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("complete_cashu_send_quote", body)
            .execute()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            if text.contains("NOT_FOUND") {
                return Err(MeltQuoteStorageError::NotFound);
            }
            if text.contains("INVALID_STATE") {
                return Err(MeltQuoteStorageError::InvalidState(text));
            }
            return Err(MeltQuoteStorageError::Backend(format!(
                "complete_cashu_send_quote: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MeltQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let quote_value = value
            .get("quote")
            .cloned()
            .ok_or_else(|| MeltQuoteStorageError::Backend("missing quote field".into()))?;
        let account_value = value
            .get("account")
            .cloned()
            .ok_or_else(|| MeltQuoteStorageError::Backend("missing account field".into()))?;
        let added_change_proofs = value
            .get("change_proofs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| {
                p.get("id")
                    .and_then(Value::as_str)
                    .map(std::string::ToString::to_string)
            })
            .collect();

        let quote = self.row_to_quote(quote_value).await?;
        let account = parse_account(account_value)?;
        Ok(CompleteMeltQuoteResult {
            quote,
            account,
            added_change_proofs,
        })
    }

    async fn expire(&self, quote_id: Uuid) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let body = serde_json::to_string(&json!({ "p_quote_id": quote_id }))
            .map_err(|e| MeltQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("expire_cashu_send_quote", body)
            .execute()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            if text.contains("NOT_FOUND") {
                return Err(MeltQuoteStorageError::NotFound);
            }
            if text.contains("INVALID_STATE") {
                return Err(MeltQuoteStorageError::InvalidState(text));
            }
            return Err(MeltQuoteStorageError::Backend(format!(
                "expire_cashu_send_quote: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MeltQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let quote_value = value
            .get("quote")
            .cloned()
            .ok_or_else(|| MeltQuoteStorageError::Backend("missing quote field".into()))?;
        self.row_to_quote(quote_value).await
    }

    async fn fail(
        &self,
        quote_id: Uuid,
        reason: &str,
    ) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let body = serde_json::to_string(&json!({
            "p_quote_id": quote_id,
            "p_failure_reason": reason,
        }))
        .map_err(|e| MeltQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("fail_cashu_send_quote", body)
            .execute()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            if text.contains("NOT_FOUND") {
                return Err(MeltQuoteStorageError::NotFound);
            }
            if text.contains("INVALID_STATE") {
                return Err(MeltQuoteStorageError::InvalidState(text));
            }
            return Err(MeltQuoteStorageError::Backend(format!(
                "fail_cashu_send_quote: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MeltQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let quote_value = value
            .get("quote")
            .cloned()
            .ok_or_else(|| MeltQuoteStorageError::Backend("missing quote field".into()))?;
        self.row_to_quote(quote_value).await
    }

    async fn get(&self, quote_id: Uuid) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .from("cashu_send_quotes")
            .select("*")
            .eq("id", quote_id.to_string())
            .execute()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(MeltQuoteStorageError::Backend(format!(
                "select cashu_send_quotes: HTTP {status}: {text}"
            )));
        }
        let rows: Vec<Value> = serde_json::from_str(&text)
            .map_err(|e| MeltQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let row = rows
            .into_iter()
            .next()
            .ok_or(MeltQuoteStorageError::NotFound)?;
        self.row_to_quote(row).await
    }

    async fn find_active_by_payment_hash(
        &self,
        user_id: UserId,
        payment_hash: &str,
    ) -> Result<Option<CashuMeltQuote>, MeltQuoteStorageError> {
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .from("cashu_send_quotes")
            .select("*")
            .eq("user_id", user_id.to_string())
            .eq("payment_hash", payment_hash)
            .in_("state", ["UNPAID", "PENDING", "PAID"])
            .execute()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(MeltQuoteStorageError::Backend(format!(
                "select cashu_send_quotes (by payment_hash): HTTP {status}: {text}"
            )));
        }
        let rows: Vec<Value> = serde_json::from_str(&text)
            .map_err(|e| MeltQuoteStorageError::Backend(format!("parse response: {e}")))?;
        match rows.into_iter().next() {
            Some(row) => Ok(Some(self.row_to_quote(row).await?)),
            None => Ok(None),
        }
    }

    async fn list_unresolved_for_user(
        &self,
        user_id: UserId,
    ) -> Result<Vec<CashuMeltQuote>, MeltQuoteStorageError> {
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .from("cashu_send_quotes")
            .select("*")
            .eq("user_id", user_id.to_string())
            .in_("state", ["UNPAID", "PENDING"])
            .execute()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MeltQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(MeltQuoteStorageError::Backend(format!(
                "select cashu_send_quotes (unresolved for user): HTTP {status}: {text}"
            )));
        }
        let rows: Vec<Value> = serde_json::from_str(&text)
            .map_err(|e| MeltQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(self.row_to_quote(row).await?);
        }
        Ok(out)
    }
}

impl SupabaseCashuMeltQuoteStorage {
    /// Delegates to `crate::conversions::to_cashu_melt_quote` after
    /// deserializing the postgrest row JSON into the codegen
    /// `CashuSendQuotesRow`. See `to_cashu_mint_quote`'s sibling doc
    /// in `cashu_mint_quote_storage.rs` for the extraction rationale.
    async fn row_to_quote(&self, value: Value) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let row: crate::generated::tables::cashu_send_quotes::CashuSendQuotesRow =
            serde_json::from_value(value).map_err(|e| {
                MeltQuoteStorageError::Backend(format!("parse cashu_send_quote row: {e}"))
            })?;
        crate::conversions::to_cashu_melt_quote(&row, self.encryption.as_ref()).await
    }
}

fn parse_account(value: Value) -> Result<Account, MeltQuoteStorageError> {
    serde_json::from_value::<Account>(value)
        .map_err(|e| MeltQuoteStorageError::Backend(format!("parse account row: {e}")))
}

#[allow(clippy::needless_pass_by_value)]
fn map_auth(err: agicash_traits::StorageError) -> MeltQuoteStorageError {
    MeltQuoteStorageError::Backend(format!("auth: {err}"))
}

/// True when a failed `create_cashu_send_quote` response body is the
/// Postgres `unique_violation` (23505) raised by the partial unique
/// index `cashu_send_quotes_payment_hash_active_unique` — i.e. an
/// active (UNPAID/PENDING/PAID) melt quote already exists for this
/// `(user_id, payment_hash)`. Keyed on the constraint name so the
/// *other* unique index on this table (`..._quote_id_hash_key`) is not
/// mis-classified as a duplicate payment.
fn is_duplicate_payment_violation(body: &str) -> bool {
    body.contains("cashu_send_quotes_payment_hash_active_unique")
}

fn sha256_hex(data: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data.as_bytes());
    hex::encode(hasher.finalize())
}

fn proof_to_y(secret: &str) -> String {
    use cdk::dhke::hash_to_curve;
    match hash_to_curve(secret.as_bytes()) {
        Ok(pk) => pk.to_hex(),
        Err(_) => String::new(),
    }
}

fn add_money(a: &Money, b: &Money) -> Result<Money, MeltQuoteStorageError> {
    if a.currency() != b.currency() {
        return Err(MeltQuoteStorageError::Backend(format!(
            "currency mismatch in add_money: {} vs {}",
            a.currency(),
            b.currency()
        )));
    }
    a.try_add(b)
        .map_err(|e| MeltQuoteStorageError::Backend(format!("add_money: {e}")))
}

fn sub_money(a: &Money, b: &Money) -> Result<Money, MeltQuoteStorageError> {
    if a.currency() != b.currency() {
        return Err(MeltQuoteStorageError::Backend(format!(
            "currency mismatch in sub_money: {} vs {}",
            a.currency(),
            b.currency()
        )));
    }
    a.try_sub(b)
        .map_err(|e| MeltQuoteStorageError::Backend(format!("sub_money: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agicash_domain::{AccountPurpose, AccountState, AccountType, Currency};
    use agicash_money::Unit;
    use agicash_traits::PassthroughProofEncryption;
    use rust_decimal::Decimal;
    use serde_json::json;

    struct StubTokens;

    #[async_trait::async_trait]
    impl agicash_traits::TokenProvider for StubTokens {
        async fn get_jwt(&self) -> Result<String, agicash_traits::AuthError> {
            Err(agicash_traits::AuthError::Unauthenticated)
        }
    }

    fn passthrough() -> Arc<dyn ProofEncryption> {
        Arc::new(PassthroughProofEncryption)
    }

    fn make_storage() -> SupabaseCashuMeltQuoteStorage {
        let cfg = crate::SupabaseStorageConfig {
            url: "https://test.supabase.co".into(),
            anon_key: "anon".into(),
        };
        let base = Arc::new(crate::SupabaseStorage::new(cfg, Arc::new(StubTokens)).unwrap());
        SupabaseCashuMeltQuoteStorage::new(base, passthrough())
    }

    // `row_to_quote` is now a thin delegation to
    // `crate::conversions::to_cashu_melt_quote`; its state-machine and
    // encrypted-data folding tests live next to that helper in
    // `conversions.rs`. Tests below cover what stays in this file:
    // write-path `encrypt_to_base64`, error classifiers, hash + key
    // helpers, and the account-row parser.

    #[tokio::test]
    async fn encrypt_to_base64_round_trips_through_passthrough() {
        let storage = make_storage();
        let value = json!({ "hello": "world" });
        let encoded = storage.encrypt_to_base64(&value).await.unwrap();
        let cipher = general_purpose::STANDARD
            .decode(encoded.as_bytes())
            .unwrap();
        let plain = storage.encryption.decrypt(&cipher).await.unwrap();
        let back: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(back, value);
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        let h = sha256_hex("abc");
        assert_eq!(
            h,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn duplicate_payment_violation_detected_by_constraint_name() {
        // Postgrest 23505 body shape for our partial unique index.
        let body = r#"{"code":"23505","details":"Key (user_id, payment_hash)=(...) already exists.","message":"duplicate key value violates unique constraint \"cashu_send_quotes_payment_hash_active_unique\""}"#;
        assert!(is_duplicate_payment_violation(body));
    }

    #[test]
    fn other_unique_violation_is_not_duplicate_payment() {
        // The *other* unique index on this table (mint quote_id_hash)
        // must NOT be mis-classified as a duplicate payment.
        let body = r#"{"code":"23505","message":"duplicate key value violates unique constraint \"cashu_send_quotes_quote_id_hash_key\""}"#;
        assert!(!is_duplicate_payment_violation(body));
        // A generic backend error is also not a duplicate payment.
        assert!(!is_duplicate_payment_violation(
            r#"{"code":"P0001","message":"CONCURRENCY_ERROR"}"#
        ));
    }

    #[test]
    fn proof_to_y_returns_hex_for_valid_secret() {
        let y = proof_to_y("0123456789abcdef");
        assert!(!y.is_empty());
        assert!(y.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn send_data_round_trips_through_json() {
        let data = LightningSendData {
            payment_request: "lnbc...".into(),
            amount_requested: Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            amount_requested_in_msat: 64_000,
            amount_received: Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            lightning_fee_reserve: Money::new(Decimal::from(1u64), Currency::Btc, Unit::Sat),
            cashu_send_fee: Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
            melt_quote_id: "qid".into(),
            amount_reserved: Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            payment_preimage: Some("pre".into()),
            lightning_fee: Some(Money::new(Decimal::from(1u64), Currency::Btc, Unit::Sat)),
            amount_spent: Some(Money::new(Decimal::from(65u64), Currency::Btc, Unit::Sat)),
            total_fee: Some(Money::new(Decimal::from(1u64), Currency::Btc, Unit::Sat)),
        };
        let json_value = serde_json::to_value(&data).unwrap();
        assert!(json_value.get("paymentRequest").is_some());
        assert!(json_value.get("meltQuoteId").is_some());
        assert!(json_value.get("amountRequestedInMsat").is_some());
        let back: LightningSendData = serde_json::from_value(json_value).unwrap();
        assert_eq!(back.payment_preimage, Some("pre".into()));
    }

    #[test]
    fn parse_account_strips_extra_cashu_proofs_field() {
        let raw = json!({
            "id": "11111111-2222-3333-4444-555555555555",
            "created_at": "2026-03-01T12:00:00Z",
            "user_id": "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "name": "Test",
            "type": "cashu",
            "purpose": "transactional",
            "currency": "BTC",
            "details": {"mint_url": "https://m"},
            "version": 0,
            "state": "active",
            "expires_at": null,
            "cashu_proofs": []
        });
        let acct = parse_account(raw).unwrap();
        assert_eq!(acct.account_type, AccountType::Cashu);
        assert_eq!(acct.currency, Currency::Btc);
        assert_eq!(acct.state, AccountState::Active);
        assert_eq!(acct.purpose, AccountPurpose::Transactional);
    }
}
