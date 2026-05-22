//! Postgrest-backed [`CashuMintQuoteStorage`] implementation.
//!
//! Mirrors `app/features/receive/cashu-receive-quote-repository.ts`. The six
//! RPCs we call (`create_cashu_receive_quote`,
//! `process_cashu_receive_quote_payment`, `complete_cashu_receive_quote`,
//! `expire_cashu_receive_quote`, `fail_cashu_receive_quote`) live in the
//! `wallet` schema. The `mark_cashu_receive_quote_cashu_token_melt_initiated`
//! RPC is intentionally NOT wired — slice 7 does not produce CASHU_TOKEN-typed
//! quotes.
//!
//! Encryption is hidden inside this impl. We accept an
//! [`Arc<dyn ProofEncryption>`] dep at construction; slice 5 wires up
//! [`agicash_traits::PassthroughProofEncryption`] which encodes plaintext
//! JSON as base64 in the `encrypted_data` blob.

use crate::SupabaseStorage;
use agicash_cashu::{
    CashuMintQuote, CashuMintQuoteStorage, CompleteMintQuote, CompleteMintQuoteResult,
    CreateMintQuote, MintQuoteStorageError, ProcessMintQuotePayment, ProcessMintQuotePaymentResult,
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

/// Postgres-backed mint-quote storage.
pub struct SupabaseCashuMintQuoteStorage {
    base: Arc<SupabaseStorage>,
    encryption: Arc<dyn ProofEncryption>,
}

impl std::fmt::Debug for SupabaseCashuMintQuoteStorage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SupabaseCashuMintQuoteStorage")
            .finish_non_exhaustive()
    }
}

impl SupabaseCashuMintQuoteStorage {
    pub fn new(base: Arc<SupabaseStorage>, encryption: Arc<dyn ProofEncryption>) -> Self {
        Self { base, encryption }
    }

    async fn encrypt_to_base64(&self, value: &Value) -> Result<String, MintQuoteStorageError> {
        let bytes = serde_json::to_vec(value)
            .map_err(|e| MintQuoteStorageError::Backend(format!("encode encrypted_data: {e}")))?;
        let cipher = self.encryption.encrypt(&bytes).await?;
        Ok(general_purpose::STANDARD.encode(cipher))
    }
}

/// JSON inside `encrypted_data` (mirrors TS `CashuLightningReceiveDbDataSchema`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LightningReceiveData {
    payment_request: String,
    mint_quote_id: String,
    amount_received: Money,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    minting_fee: Option<Money>,
    /// Populated when the quote transitions to PAID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_amounts: Option<Vec<u64>>,
    total_fee: Money,
}

/// Wire shape for one proof in `complete_cashu_receive_quote` (matches the
/// `wallet.cashu_proof_input` composite type's camelCase field names).
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
impl CashuMintQuoteStorage for SupabaseCashuMintQuoteStorage {
    async fn create(
        &self,
        input: CreateMintQuote,
    ) -> Result<CashuMintQuote, MintQuoteStorageError> {
        let data = LightningReceiveData {
            payment_request: input.payment_request.clone(),
            mint_quote_id: input.quote_id.clone(),
            amount_received: input.amount,
            description: input.description.clone(),
            minting_fee: input.minting_fee,
            output_amounts: None,
            total_fee: input.total_fee,
        };
        let encrypted_data =
            self.encrypt_to_base64(&serde_json::to_value(&data).map_err(|e| {
                MintQuoteStorageError::Backend(format!("encode receive data: {e}"))
            })?)
            .await?;
        let quote_id_hash = sha256_hex(&input.quote_id);

        let body = serde_json::to_string(&json!({
            "p_user_id": input.user_id,
            "p_account_id": input.account_id,
            "p_currency": input.amount.currency(),
            "p_expires_at": input.expires_at,
            "p_locking_derivation_path": input.locking_derivation_path,
            "p_receive_type": "LIGHTNING",
            "p_encrypted_data": encrypted_data,
            "p_quote_id_hash": quote_id_hash,
            "p_payment_hash": input.payment_hash,
        }))
        .map_err(|e| MintQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;

        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("create_cashu_receive_quote", body)
            .execute()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(MintQuoteStorageError::Backend(format!(
                "create_cashu_receive_quote: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MintQuoteStorageError::Backend(format!("parse response: {e}")))?;
        self.row_to_quote(value).await
    }

    async fn process_payment(
        &self,
        input: ProcessMintQuotePayment,
    ) -> Result<ProcessMintQuotePaymentResult, MintQuoteStorageError> {
        // Re-encrypt the receive-data blob with the new output_amounts.
        let existing_data = LightningReceiveData {
            payment_request: input.quote.payment_request.clone(),
            mint_quote_id: input.quote.quote_id.clone(),
            amount_received: input.quote.amount,
            description: input.quote.description.clone(),
            minting_fee: input.quote.minting_fee,
            output_amounts: Some(input.output_amounts.clone()),
            total_fee: input.quote.total_fee,
        };
        let encrypted_data =
            self.encrypt_to_base64(&serde_json::to_value(&existing_data).map_err(|e| {
                MintQuoteStorageError::Backend(format!("encode receive data: {e}"))
            })?)
            .await?;

        let body = serde_json::to_string(&json!({
            "p_quote_id": input.quote.id,
            "p_keyset_id": input.keyset_id,
            "p_number_of_outputs": input.output_amounts.len(),
            "p_encrypted_data": encrypted_data,
        }))
        .map_err(|e| MintQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;

        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("process_cashu_receive_quote_payment", body)
            .execute()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            if text.contains("NOT_FOUND") {
                return Err(MintQuoteStorageError::NotFound);
            }
            if text.contains("INVALID_STATE") {
                return Err(MintQuoteStorageError::InvalidState(text));
            }
            return Err(MintQuoteStorageError::Backend(format!(
                "process_cashu_receive_quote_payment: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MintQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let quote_value = value
            .get("quote")
            .cloned()
            .ok_or_else(|| MintQuoteStorageError::Backend("missing quote field".into()))?;
        let account_value = value
            .get("account")
            .cloned()
            .ok_or_else(|| MintQuoteStorageError::Backend("missing account field".into()))?;
        let quote = self.row_to_quote(quote_value).await?;
        let account = parse_account(account_value)?;
        Ok(ProcessMintQuotePaymentResult { quote, account })
    }

    async fn complete(
        &self,
        input: CompleteMintQuote,
    ) -> Result<CompleteMintQuoteResult, MintQuoteStorageError> {
        let mut encrypted_proofs = Vec::with_capacity(input.proofs.len());
        for p in &input.proofs {
            let amount_enc = self
                .encryption
                .encrypt(p.amount.to_string().as_bytes())
                .await?;
            let secret_enc = self.encryption.encrypt(p.secret.as_bytes()).await?;
            encrypted_proofs.push(EncryptedProofInput {
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
            "p_quote_id": input.quote_id,
            "p_proofs": encrypted_proofs,
        }))
        .map_err(|e| MintQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("complete_cashu_receive_quote", body)
            .execute()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            if text.contains("NOT_FOUND") {
                return Err(MintQuoteStorageError::NotFound);
            }
            if text.contains("INVALID_STATE") {
                return Err(MintQuoteStorageError::InvalidState(text));
            }
            return Err(MintQuoteStorageError::Backend(format!(
                "complete_cashu_receive_quote: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MintQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let quote_value = value
            .get("quote")
            .cloned()
            .ok_or_else(|| MintQuoteStorageError::Backend("missing quote field".into()))?;
        let account_value = value
            .get("account")
            .cloned()
            .ok_or_else(|| MintQuoteStorageError::Backend("missing account field".into()))?;
        let added_proofs = value
            .get("added_proofs")
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
        Ok(CompleteMintQuoteResult {
            quote,
            account,
            added_proofs,
        })
    }

    async fn expire(&self, quote_id: Uuid) -> Result<CashuMintQuote, MintQuoteStorageError> {
        let body = serde_json::to_string(&json!({ "p_quote_id": quote_id }))
            .map_err(|e| MintQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("expire_cashu_receive_quote", body)
            .execute()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            if text.contains("NOT_FOUND") {
                return Err(MintQuoteStorageError::NotFound);
            }
            if text.contains("INVALID_STATE") {
                return Err(MintQuoteStorageError::InvalidState(text));
            }
            return Err(MintQuoteStorageError::Backend(format!(
                "expire_cashu_receive_quote: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MintQuoteStorageError::Backend(format!("parse response: {e}")))?;
        self.row_to_quote(value).await
    }

    async fn fail(
        &self,
        quote_id: Uuid,
        reason: &str,
    ) -> Result<CashuMintQuote, MintQuoteStorageError> {
        let body = serde_json::to_string(&json!({
            "p_quote_id": quote_id,
            "p_failure_reason": reason,
        }))
        .map_err(|e| MintQuoteStorageError::Backend(format!("encode rpc body: {e}")))?;
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .rpc("fail_cashu_receive_quote", body)
            .execute()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            if text.contains("NOT_FOUND") {
                return Err(MintQuoteStorageError::NotFound);
            }
            if text.contains("INVALID_STATE") {
                return Err(MintQuoteStorageError::InvalidState(text));
            }
            return Err(MintQuoteStorageError::Backend(format!(
                "fail_cashu_receive_quote: HTTP {status}: {text}"
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| MintQuoteStorageError::Backend(format!("parse response: {e}")))?;
        self.row_to_quote(value).await
    }

    async fn get(&self, quote_id: Uuid) -> Result<CashuMintQuote, MintQuoteStorageError> {
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .from("cashu_receive_quotes")
            .select("*")
            .eq("id", quote_id.to_string())
            .execute()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(MintQuoteStorageError::Backend(format!(
                "select cashu_receive_quotes: HTTP {status}: {text}"
            )));
        }
        let rows: Vec<Value> = serde_json::from_str(&text)
            .map_err(|e| MintQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let row = rows
            .into_iter()
            .next()
            .ok_or(MintQuoteStorageError::NotFound)?;
        self.row_to_quote(row).await
    }

    async fn list_pending_for_user(
        &self,
        user_id: UserId,
    ) -> Result<Vec<CashuMintQuote>, MintQuoteStorageError> {
        let client = self.base.authenticated_client().await.map_err(map_auth)?;
        let response = client
            .from("cashu_receive_quotes")
            .select("*")
            .eq("user_id", user_id.to_string())
            .in_("state", ["UNPAID", "PAID"])
            .execute()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("postgrest: {e}")))?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| MintQuoteStorageError::Backend(format!("read body: {e}")))?;
        if !status.is_success() {
            return Err(MintQuoteStorageError::Backend(format!(
                "select cashu_receive_quotes (pending for user): HTTP {status}: {text}"
            )));
        }
        let rows: Vec<Value> = serde_json::from_str(&text)
            .map_err(|e| MintQuoteStorageError::Backend(format!("parse response: {e}")))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(self.row_to_quote(row).await?);
        }
        Ok(out)
    }
}

impl SupabaseCashuMintQuoteStorage {
    /// Delegates to `crate::conversions::to_cashu_mint_quote` after
    /// deserializing the postgrest row JSON into the codegen
    /// `CashuReceiveQuotesRow`. The conversion logic — including the
    /// `encrypted_data` decrypt and the state-machine fold — was
    /// extracted as a public helper so the wallet-side cache
    /// (`agicash-wallet`, sibling lane) can call it on the same
    /// typed row variants the realtime crate emits via
    /// `WalletRealtimeEvent::Change`.
    async fn row_to_quote(&self, value: Value) -> Result<CashuMintQuote, MintQuoteStorageError> {
        let row: crate::generated::tables::cashu_receive_quotes::CashuReceiveQuotesRow =
            serde_json::from_value(value).map_err(|e| {
                MintQuoteStorageError::Backend(format!("parse cashu_receive_quote row: {e}"))
            })?;
        crate::conversions::to_cashu_mint_quote(&row, self.encryption.as_ref()).await
    }
}

fn parse_account(value: Value) -> Result<Account, MintQuoteStorageError> {
    serde_json::from_value::<Account>(value)
        .map_err(|e| MintQuoteStorageError::Backend(format!("parse account row: {e}")))
}

#[allow(clippy::needless_pass_by_value)]
fn map_auth(err: agicash_traits::StorageError) -> MintQuoteStorageError {
    MintQuoteStorageError::Backend(format!("auth: {err}"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use agicash_domain::{AccountPurpose, AccountState, AccountType, Currency};
    use agicash_traits::PassthroughProofEncryption;
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

    fn make_storage() -> SupabaseCashuMintQuoteStorage {
        let cfg = crate::SupabaseStorageConfig {
            url: "https://test.supabase.co".into(),
            anon_key: "anon".into(),
        };
        let base = Arc::new(crate::SupabaseStorage::new(cfg, Arc::new(StubTokens)).unwrap());
        SupabaseCashuMintQuoteStorage::new(base, passthrough())
    }

    // `row_to_quote` is now a thin delegation to
    // `crate::conversions::to_cashu_mint_quote`; its state-machine and
    // encrypted-data folding tests live next to that helper in
    // `conversions.rs`. Tests below cover the parts that stay here:
    // the write-path `encrypt_to_base64` round-trip, the hash + key
    // helpers, and the account-row parser.

    #[tokio::test]
    async fn encrypt_to_base64_round_trips_through_passthrough() {
        let storage = make_storage();
        let value = json!({ "hello": "world" });
        // Round-trip via the conversion helper's matching decrypt path
        // (it reads what `encrypt_to_base64` writes).
        let encoded = storage.encrypt_to_base64(&value).await.unwrap();
        // Decode + decrypt with the same passthrough impl.
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
    fn proof_to_y_returns_hex_for_valid_secret() {
        let y = proof_to_y("0123456789abcdef");
        assert!(!y.is_empty());
        assert!(y.chars().all(|c| c.is_ascii_hexdigit()));
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
