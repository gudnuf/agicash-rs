//! Public row → rich-type conversion helpers.
//!
//! ## Why
//!
//! The Supabase wallet wire format splits each money-state record into
//! a public-column row (lifecycle state, ids, version, timestamps) and
//! an encrypted JSON blob (`encrypted_data`) that carries the
//! amount/quote-id/payment-request fields a row consumer actually needs.
//! Column-level encryption further wraps per-proof `amount` / `secret`
//! on the proof rows. The conversion logic that decrypts these and
//! folds them into a rich `agicash_cashu` / `agicash_domain` type lived
//! inline inside each storage trait impl (`row_to_quote`, `row_to_swap`,
//! …) and was therefore only reachable from the storage call sites.
//!
//! This module surfaces that conversion as **public, callable
//! helpers** named after the React-app repository pattern
//! (`toAccount`, `toMintQuote`, …). Two callers depend on them:
//!
//! 1. The existing storage impls (`SupabaseCashu*Storage::row_to_*`),
//!    refactored to delegate here rather than inline the logic.
//! 2. The wallet-side cache (in `agicash-wallet`, landed in a sibling
//!    lane) — it consumes the typed-row variants emitted by
//!    `agicash-realtime::WalletRealtimeEvent::Change` and needs the
//!    same row → rich conversion the storage layer uses.
//!
//! ## Naming
//!
//! Helpers preserve the existing rust domain names
//! (`CashuMintQuote` / `CashuMeltQuote` etc.). The broader rename to
//! match DB / React names (smell S9 in
//! `~/athanor/projects/agicash-rust/smells.md`) is a separate
//! mechanical lane this work deliberately does not touch.
//!
//! ## Decryption seam
//!
//! Every helper that touches encrypted fields takes
//! `&dyn ProofEncryption`. Callers supply the same encryption handle
//! the storage impl holds (`SupabaseStorage`'s
//! `Arc<dyn ProofEncryption>`). The cache layer constructs one once and
//! reuses it for every `Change` it folds into its in-memory state.
//!
//! ## Extraction status
//!
//! Five helpers are extracted: `to_account`, `to_cashu_mint_quote`,
//! `to_cashu_melt_quote`, `to_cashu_receive_swap`, and
//! `to_cashu_send_swap`. The send-swap helper takes the typed
//! `cashu_send_swaps` row PLUS a slice of joined `cashu_proofs` rows
//! (the storage call sites already embed them via postgrest `select=*,
//! cashu_proofs!cashu_send_swap_id(*)`), because the rich
//! `CashuSendSwap` carries `input_proofs` / `proofs_to_send` buckets
//! that are derived from the joined proofs — not from the swap row's
//! `encrypted_data`. Per-proof `amount` / `secret` are themselves
//! column-encrypted; the helper handles that internally. The bucket
//! classification mirrors what the TS-side `to_swap` does (see body).

use crate::generated::tables;
use agicash_cashu::{
    CashuMeltQuote, CashuMeltQuoteState, CashuMintQuote, CashuMintQuoteState, CashuReceiveSwap,
    CashuReceiveSwapState, CashuSendSwap, CashuSendSwapState, MeltQuoteStorageError,
    MintQuoteStorageError, OutputAmounts, ReceiveSwapStorageError, SendSwapStorageError,
    TokenProof,
};
use agicash_domain::Account;
use agicash_money::Money;
use agicash_traits::{EncryptionError, ProofEncryption};
use base64::engine::general_purpose;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `wallet.accounts` row → domain [`Account`].
///
/// `Account` deserializes column-by-column from the postgrest row JSON
/// (no encrypted blob, no embeds), so this is just a typed convenience
/// wrapper: callers that already hold a deserialized
/// [`tables::accounts::AccountsRow`] convert without re-decoding through
/// `serde_json::Value`. The broadcast trigger
/// `wallet.to_account_with_proofs` appends a `proofs` array onto the
/// account JSON; `Account` has no `proofs` field so the proofs sidecar
/// is ignored by serde here — pass the proofs array separately if the
/// caller needs it (the realtime crate's `AccountWithProofs` carries
/// both).
///
/// Pure (no async, no encryption). Mirrors the inline
/// `parse_account(value)` helpers each storage module previously
/// defined privately.
///
/// # Errors
///
/// Returns `serde_json::Error` if the row JSON doesn't decode into an
/// `Account` (e.g. a column added upstream the storage layer hasn't
/// adopted yet).
pub fn to_account(row: &tables::accounts::AccountsRow) -> Result<Account, serde_json::Error> {
    // The codegen row and domain `Account` are isomorphic by serde —
    // round-tripping through `Value` is the simplest typed bridge that
    // doesn't require a hand-written field-by-field copy (and survives
    // a future column-add on either side without code change).
    let v = serde_json::to_value(row)?;
    serde_json::from_value(v)
}

/// `wallet.cashu_receive_quotes` row → domain [`CashuMintQuote`].
///
/// The row's `encrypted_data` is base64-then-cipher; this helper
/// decrypts it (via `encryption`), unpacks the per-state fields, and
/// folds them with the public row columns into a `CashuMintQuote`.
/// Mirrors `SupabaseCashuMintQuoteStorage::row_to_quote` exactly — see
/// that method's history for the wire-format intent.
///
/// `encryption` MUST be the same proof-encryption handle the storage
/// impl was constructed with (otherwise the decrypt yields garbage).
///
/// # Errors
///
/// Returns `MintQuoteStorageError::Backend` if the encrypted blob does
/// not decode, deserialize, or carry the per-state fields the state
/// machine requires. `MintQuoteStorageError::Encryption` if the
/// `encryption.decrypt` call itself fails.
pub async fn to_cashu_mint_quote(
    row: &tables::cashu_receive_quotes::CashuReceiveQuotesRow,
    encryption: &dyn ProofEncryption,
) -> Result<CashuMintQuote, MintQuoteStorageError> {
    let decoded = decrypt_blob::<MintQuoteStorageError>(encryption, &row.encrypted_data).await?;
    let receive: LightningReceiveData = serde_json::from_value(decoded)
        .map_err(|e| MintQuoteStorageError::Backend(format!("parse encrypted_data: {e}")))?;
    let version = u32::try_from(row.version).map_err(|_| {
        MintQuoteStorageError::Backend(format!("version out of u32 range: {}", row.version))
    })?;
    let state = match row.state {
        crate::generated::enums::CashuReceiveQuoteState::Unpaid => CashuMintQuoteState::Unpaid,
        crate::generated::enums::CashuReceiveQuoteState::Paid => CashuMintQuoteState::Paid {
            keyset_id: row.keyset_id.clone().unwrap_or_default(),
            keyset_counter: row
                .keyset_counter
                .and_then(|c| u32::try_from(c).ok())
                .unwrap_or(0),
            output_amounts: receive.output_amounts.clone().unwrap_or_default(),
        },
        crate::generated::enums::CashuReceiveQuoteState::Completed => {
            CashuMintQuoteState::Completed {
                keyset_id: row.keyset_id.clone().unwrap_or_default(),
                keyset_counter: row
                    .keyset_counter
                    .and_then(|c| u32::try_from(c).ok())
                    .unwrap_or(0),
                output_amounts: receive.output_amounts.clone().unwrap_or_default(),
            }
        }
        crate::generated::enums::CashuReceiveQuoteState::Expired => CashuMintQuoteState::Expired,
        crate::generated::enums::CashuReceiveQuoteState::Failed => CashuMintQuoteState::Failed {
            failure_reason: row
                .failure_reason
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        },
    };
    Ok(CashuMintQuote {
        id: row.id,
        quote_id: receive.mint_quote_id,
        user_id: agicash_domain::UserId::from(row.user_id),
        account_id: agicash_domain::AccountId::from(row.account_id),
        amount: receive.amount_received,
        description: receive.description,
        payment_request: receive.payment_request,
        payment_hash: row.payment_hash.clone(),
        locking_derivation_path: row.locking_derivation_path.clone(),
        transaction_id: row.transaction_id,
        minting_fee: receive.minting_fee,
        total_fee: receive.total_fee,
        created_at: row.created_at,
        expires_at: row.expires_at,
        version,
        state,
    })
}

/// `wallet.cashu_send_quotes` row → domain [`CashuMeltQuote`].
///
/// Mirrors `SupabaseCashuMeltQuoteStorage::row_to_quote`. Hardcodes
/// `proofs: Vec::new()` to match the existing inline behavior — the
/// broadcast trigger DOES emit `cashu_proofs` alongside the row, but
/// the storage layer's `CashuMeltQuote` has historically been built
/// without those proofs (they're walked separately for the
/// commit-proofs RPC). Preserving the existing contract avoids
/// rippling a behavioral change into this lane.
///
/// # Errors
///
/// Returns `MeltQuoteStorageError::Backend` for blob decode / parse
/// errors and missing PAID-state fields in `encrypted_data`;
/// `MeltQuoteStorageError::Encryption` if `encryption.decrypt` fails.
pub async fn to_cashu_melt_quote(
    row: &tables::cashu_send_quotes::CashuSendQuotesRow,
    encryption: &dyn ProofEncryption,
) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
    let decoded = decrypt_blob::<MeltQuoteStorageError>(encryption, &row.encrypted_data).await?;
    let send: LightningSendData = serde_json::from_value(decoded)
        .map_err(|e| MeltQuoteStorageError::Backend(format!("parse encrypted_data: {e}")))?;
    let version = u32::try_from(row.version).map_err(|_| {
        MeltQuoteStorageError::Backend(format!("version out of u32 range: {}", row.version))
    })?;
    let keyset_counter = u32::try_from(row.keyset_counter).map_err(|_| {
        MeltQuoteStorageError::Backend(format!(
            "keyset_counter out of u32 range: {}",
            row.keyset_counter
        ))
    })?;
    let number_of_change_outputs = u32::try_from(row.number_of_change_outputs).map_err(|_| {
        MeltQuoteStorageError::Backend(format!(
            "number_of_change_outputs out of u32 range: {}",
            row.number_of_change_outputs
        ))
    })?;
    let state = match row.state {
        crate::generated::enums::CashuSendQuoteState::Unpaid => CashuMeltQuoteState::Unpaid,
        crate::generated::enums::CashuSendQuoteState::Pending => CashuMeltQuoteState::Pending,
        crate::generated::enums::CashuSendQuoteState::Paid => CashuMeltQuoteState::Paid {
            payment_preimage: send.payment_preimage.clone().unwrap_or_default(),
            lightning_fee: send.lightning_fee.ok_or_else(|| {
                MeltQuoteStorageError::Backend(
                    "PAID quote missing lightning_fee in encrypted_data".into(),
                )
            })?,
            amount_spent: send.amount_spent.ok_or_else(|| {
                MeltQuoteStorageError::Backend(
                    "PAID quote missing amount_spent in encrypted_data".into(),
                )
            })?,
            total_fee: send.total_fee.ok_or_else(|| {
                MeltQuoteStorageError::Backend(
                    "PAID quote missing total_fee in encrypted_data".into(),
                )
            })?,
        },
        crate::generated::enums::CashuSendQuoteState::Expired => CashuMeltQuoteState::Expired,
        crate::generated::enums::CashuSendQuoteState::Failed => CashuMeltQuoteState::Failed {
            failure_reason: row
                .failure_reason
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        },
    };
    Ok(CashuMeltQuote {
        id: row.id,
        quote_id: send.melt_quote_id,
        user_id: agicash_domain::UserId::from(row.user_id),
        account_id: agicash_domain::AccountId::from(row.account_id),
        payment_request: send.payment_request,
        payment_hash: row.payment_hash.clone(),
        amount_requested: send.amount_requested,
        amount_requested_in_msat: send.amount_requested_in_msat,
        amount_received: send.amount_received,
        lightning_fee_reserve: send.lightning_fee_reserve,
        cashu_fee: send.cashu_send_fee,
        proofs: Vec::new(),
        amount_reserved: send.amount_reserved,
        keyset_id: row.keyset_id.clone(),
        keyset_counter,
        number_of_change_outputs,
        transaction_id: row.transaction_id,
        created_at: row.created_at,
        expires_at: row.expires_at,
        version,
        state,
    })
}

/// `wallet.cashu_receive_swaps` row → domain [`CashuReceiveSwap`].
///
/// Mirrors `SupabaseCashuReceiveSwapStorage::row_to_swap`. The receive
/// swap has no proof-row embeds (every proof tied to the swap lives
/// inside `encrypted_data` as a list of [`TokenProof`]); a single
/// encrypted-blob decrypt suffices.
///
/// # Errors
///
/// Returns `ReceiveSwapStorageError::Backend` for blob decode / parse
/// errors and out-of-range numeric columns;
/// `ReceiveSwapStorageError::Encryption` if `encryption.decrypt` fails.
pub async fn to_cashu_receive_swap(
    row: &tables::cashu_receive_swaps::CashuReceiveSwapsRow,
    encryption: &dyn ProofEncryption,
) -> Result<CashuReceiveSwap, ReceiveSwapStorageError> {
    let decoded = decrypt_blob::<ReceiveSwapStorageError>(encryption, &row.encrypted_data).await?;
    let receive: ReceiveData = serde_json::from_value(decoded)
        .map_err(|e| ReceiveSwapStorageError::Backend(format!("parse encrypted_data: {e}")))?;
    let state = match row.state {
        crate::generated::enums::CashuReceiveSwapState::Pending => CashuReceiveSwapState::Pending,
        crate::generated::enums::CashuReceiveSwapState::Completed => {
            CashuReceiveSwapState::Completed
        }
        crate::generated::enums::CashuReceiveSwapState::Failed => CashuReceiveSwapState::Failed {
            failure_reason: row
                .failure_reason
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
        },
    };
    let keyset_counter = u32::try_from(row.keyset_counter).map_err(|_| {
        ReceiveSwapStorageError::Backend(format!(
            "keyset_counter out of u32 range: {}",
            row.keyset_counter
        ))
    })?;
    let version = u32::try_from(row.version).map_err(|_| {
        ReceiveSwapStorageError::Backend(format!("version out of u32 range: {}", row.version))
    })?;
    Ok(CashuReceiveSwap {
        token_hash: row.token_hash.clone(),
        token_proofs: receive.token_proofs,
        token_description: receive.token_description,
        user_id: agicash_domain::UserId::from(row.user_id),
        account_id: agicash_domain::AccountId::from(row.account_id),
        input_amount: receive.token_amount,
        amount_received: receive.amount_received,
        fee_amount: receive.cashu_receive_fee,
        keyset_id: row.keyset_id.clone(),
        keyset_counter,
        output_amounts: receive.output_amounts,
        transaction_id: row.transaction_id,
        created_at: row.created_at,
        version,
        state,
    })
}

/// `wallet.cashu_send_swaps` row + joined `cashu_proofs` rows →
/// domain [`CashuSendSwap`].
///
/// Mirrors `SupabaseCashuSendSwapStorage::row_to_swap_with_extra_proofs`.
/// Two-stage decryption:
///
/// 1. The swap row's `encrypted_data` blob → `SendData`
///    (`amount_received`, fees, `output_amounts`, …).
/// 2. Per-proof column-level decrypt of each joined `cashu_proofs`
///    row's `amount` + `secret` columns (these are base64-of-cipher
///    bytes wrapped by the SAME `ProofEncryption` handle as
///    `encrypted_data`).
///
/// Each joined proof is classified into one of three buckets:
///
/// - **input proof** (`p.cashu_send_swap_id != swap.id`): proof was
///   reserved as input to the swap but not produced by it.
/// - **proof to send** (`p.cashu_send_swap_id == swap.id` AND
///   `p.spending_cashu_send_swap_id == swap.id`): produced by the
///   input swap AND tagged as spent-by this swap → the proof the
///   receiver will claim.
/// - **change** (`p.spending_cashu_send_swap_id != swap.id` AND
///   `p.cashu_send_swap_id == swap.id`): produced by the input swap
///   but flowed back to the account; no bucket in the rich type.
///
/// Exact-amount path (`requires_input_proofs_swap = false`): no input
/// swap occurred, so the joined proofs ARE both the inputs AND the
/// proofs-to-send. They appear in both buckets (matches the TS-side
/// `to_swap` OR-condition).
///
/// # Errors
///
/// Returns `SendSwapStorageError::Backend` for blob decode / parse /
/// utf8 / out-of-range numeric errors;
/// `SendSwapStorageError::Encryption` if any decrypt call fails.
pub async fn to_cashu_send_swap(
    row: &tables::cashu_send_swaps::CashuSendSwapsRow,
    cashu_proofs: &[tables::cashu_proofs::CashuProofsRow],
    encryption: &dyn ProofEncryption,
) -> Result<CashuSendSwap, SendSwapStorageError> {
    let decoded = decrypt_blob::<SendSwapStorageError>(encryption, &row.encrypted_data).await?;
    let send: SendData = serde_json::from_value(decoded)
        .map_err(|e| SendSwapStorageError::Backend(format!("parse encrypted_data: {e}")))?;

    // Bucket the joined proofs. See the doc comment above for the
    // classification rule.
    let mut input_proofs: Vec<TokenProof> = Vec::new();
    let mut proofs_to_send: Vec<TokenProof> = Vec::new();
    for p in cashu_proofs {
        let is_swap_added = p.cashu_send_swap_id == Some(row.id);
        let is_swap_spending = p.spending_cashu_send_swap_id == Some(row.id);
        let proof = decrypt_proof_row(p, encryption).await?;
        if !row.requires_input_proofs_swap {
            // Exact-amount path: one set serves both buckets.
            input_proofs.push(proof.clone());
            proofs_to_send.push(proof);
        } else if is_swap_added && is_swap_spending {
            proofs_to_send.push(proof);
        } else if !is_swap_added {
            input_proofs.push(proof);
        }
        // else: change proof — owned by the account; no bucket here.
    }

    let state = match row.state {
        crate::generated::enums::CashuSendSwapState::Draft => CashuSendSwapState::Draft,
        crate::generated::enums::CashuSendSwapState::Pending => CashuSendSwapState::Pending {
            token_hash: row.token_hash.clone().unwrap_or_default(),
            proofs_to_send: proofs_to_send.clone(),
        },
        crate::generated::enums::CashuSendSwapState::Completed => CashuSendSwapState::Completed {
            token_hash: row.token_hash.clone().unwrap_or_default(),
            proofs_to_send: proofs_to_send.clone(),
        },
        crate::generated::enums::CashuSendSwapState::Failed => CashuSendSwapState::Failed {
            failure_reason: row
                .failure_reason
                .clone()
                .unwrap_or_else(|| "unknown".into()),
        },
        crate::generated::enums::CashuSendSwapState::Reversed => CashuSendSwapState::Reversed,
    };

    let keyset_counter = match row.keyset_counter {
        Some(c) => Some(u32::try_from(c).map_err(|_| {
            SendSwapStorageError::Backend(format!("keyset_counter out of u32 range: {c}"))
        })?),
        None => None,
    };
    let version = u32::try_from(row.version).map_err(|_| {
        SendSwapStorageError::Backend(format!("version out of u32 range: {}", row.version))
    })?;

    Ok(CashuSendSwap {
        id: row.id,
        account_id: agicash_domain::AccountId::from(row.account_id),
        user_id: agicash_domain::UserId::from(row.user_id),
        input_proofs,
        input_amount: send.amount_reserved,
        amount_received: send.amount_received,
        cashu_receive_fee: send.cashu_receive_fee,
        amount_to_send: send.amount_to_send,
        cashu_send_fee: send.cashu_send_fee,
        amount_spent: send.amount_spent,
        total_fee: send.total_fee,
        keyset_id: row.keyset_id.clone(),
        keyset_counter,
        output_amounts: send.output_amounts,
        transaction_id: row.transaction_id,
        created_at: row.created_at,
        version,
        state,
    })
}

/// Decrypt a joined `cashu_proofs` row's column-level encrypted
/// `amount` + `secret` and build a [`TokenProof`]. Other columns
/// (`unblinded_signature`, `dleq`, `witness`) are plaintext and copy
/// straight through.
async fn decrypt_proof_row(
    row: &tables::cashu_proofs::CashuProofsRow,
    encryption: &dyn ProofEncryption,
) -> Result<TokenProof, SendSwapStorageError> {
    let amount_bytes = general_purpose::STANDARD
        .decode(row.amount.as_bytes())
        .map_err(|e| SendSwapStorageError::Backend(format!("decode amount: {e}")))?;
    let secret_bytes = general_purpose::STANDARD
        .decode(row.secret.as_bytes())
        .map_err(|e| SendSwapStorageError::Backend(format!("decode secret: {e}")))?;
    let amount_plain = encryption.decrypt(&amount_bytes).await?;
    let secret_plain = encryption.decrypt(&secret_bytes).await?;
    let amount: u64 = std::str::from_utf8(&amount_plain)
        .map_err(|e| SendSwapStorageError::Backend(format!("amount utf8: {e}")))?
        .parse()
        .map_err(|e| SendSwapStorageError::Backend(format!("amount parse: {e}")))?;
    let secret = std::str::from_utf8(&secret_plain)
        .map_err(|e| SendSwapStorageError::Backend(format!("secret utf8: {e}")))?
        .to_string();
    Ok(TokenProof {
        id: row.keyset_id.clone(),
        amount,
        secret,
        c: row.unblinded_signature.clone(),
        dleq: row.dleq.clone(),
        witness: row.witness.clone(),
    })
}

// ===========================================================================
// Encrypted-blob shapes (intentionally private — they are an internal
// wire-format detail of `encrypted_data`, not a stable contract).
// ===========================================================================

/// JSON inside `encrypted_data` for a Cashu lightning-receive (mint) quote.
/// Mirrors TS `CashuLightningReceiveDbDataSchema`.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_amounts: Option<Vec<u64>>,
    total_fee: Money,
}

/// JSON inside `encrypted_data` for a Cashu lightning-send (melt) quote.
/// Mirrors TS `CashuLightningSendDbDataSchema`.
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    payment_preimage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lightning_fee: Option<Money>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    amount_spent: Option<Money>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    total_fee: Option<Money>,
}

/// JSON inside `encrypted_data` for a Cashu receive swap. Mirrors TS
/// `CashuSwapReceiveDbDataSchema`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReceiveData {
    #[allow(dead_code)] // mint url isn't surfaced on the rich type today
    token_mint_url: String,
    token_amount: Money,
    token_proofs: Vec<TokenProof>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_description: Option<String>,
    amount_received: Money,
    output_amounts: Vec<u64>,
    cashu_receive_fee: Money,
}

/// JSON inside `encrypted_data` for a Cashu send swap. Mirrors TS
/// `CashuSwapSendDbDataSchema`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendData {
    #[allow(dead_code)] // mint url isn't surfaced on the rich type today
    token_mint_url: String,
    amount_received: Money,
    cashu_receive_fee: Money,
    amount_to_send: Money,
    cashu_send_fee: Money,
    amount_spent: Money,
    amount_reserved: Money,
    total_fee: Money,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    output_amounts: Option<OutputAmounts>,
}

// ===========================================================================
// Generic encryption helper (shared across the three quote/swap helpers).
// ===========================================================================

/// Trait so the generic decrypt helper can produce any of the three
/// storage error types from a single body. Each error enum already has a
/// `Backend(String)` variant + a `From<EncryptionError>` impl, so we
/// only need a uniform constructor for the parse-error branches.
trait BlobError {
    fn backend(msg: String) -> Self;
    fn from_encryption(err: EncryptionError) -> Self;
}

impl BlobError for MintQuoteStorageError {
    fn backend(msg: String) -> Self {
        MintQuoteStorageError::Backend(msg)
    }
    fn from_encryption(err: EncryptionError) -> Self {
        MintQuoteStorageError::Encryption(err)
    }
}
impl BlobError for MeltQuoteStorageError {
    fn backend(msg: String) -> Self {
        MeltQuoteStorageError::Backend(msg)
    }
    fn from_encryption(err: EncryptionError) -> Self {
        MeltQuoteStorageError::Encryption(err)
    }
}
impl BlobError for ReceiveSwapStorageError {
    fn backend(msg: String) -> Self {
        ReceiveSwapStorageError::Backend(msg)
    }
    fn from_encryption(err: EncryptionError) -> Self {
        ReceiveSwapStorageError::Encryption(err)
    }
}
impl BlobError for SendSwapStorageError {
    fn backend(msg: String) -> Self {
        SendSwapStorageError::Backend(msg)
    }
    fn from_encryption(err: EncryptionError) -> Self {
        SendSwapStorageError::Encryption(err)
    }
}

/// Decode base64 → cipher bytes, decrypt, parse JSON.
async fn decrypt_blob<E: BlobError>(
    encryption: &dyn ProofEncryption,
    encoded: &str,
) -> Result<Value, E> {
    let cipher = general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .map_err(|e| E::backend(format!("decode encrypted_data: {e}")))?;
    let plain = encryption
        .decrypt(&cipher)
        .await
        .map_err(E::from_encryption)?;
    serde_json::from_slice::<Value>(&plain)
        .map_err(|e| E::backend(format!("encrypted_data not JSON: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agicash_domain::{AccountPurpose, AccountState, AccountType, Currency};
    use agicash_money::Unit;
    use agicash_traits::PassthroughProofEncryption;
    use rust_decimal::Decimal;
    use serde_json::json;
    use std::sync::Arc;
    use uuid::Uuid;

    fn passthrough() -> Arc<dyn ProofEncryption> {
        Arc::new(PassthroughProofEncryption)
    }

    async fn encrypt_blob(value: &Value) -> String {
        let enc = passthrough();
        let bytes = serde_json::to_vec(value).unwrap();
        let cipher = enc.encrypt(&bytes).await.unwrap();
        general_purpose::STANDARD.encode(cipher)
    }

    /// Smoke: account row → domain Account preserves every field.
    #[test]
    fn to_account_preserves_columns() {
        let row = tables::accounts::AccountsRow {
            id: Uuid::nil(),
            created_at: chrono::Utc::now(),
            user_id: Uuid::nil(),
            name: "test".into(),
            r#type: crate::generated::enums::AccountType::Cashu,
            purpose: crate::generated::enums::AccountPurpose::Transactional,
            currency: crate::generated::enums::Currency::Btc,
            details: json!({"mint_url": "https://m"}),
            version: 0,
            expires_at: None,
            state: crate::generated::enums::AccountState::Active,
        };
        let acct = to_account(&row).expect("to_account on a well-formed row");
        assert_eq!(acct.name, "test");
        assert_eq!(acct.account_type, AccountType::Cashu);
        assert_eq!(acct.purpose, AccountPurpose::Transactional);
        assert_eq!(acct.currency, Currency::Btc);
        assert_eq!(acct.state, AccountState::Active);
    }

    /// UNPAID mint quote decrypts and folds. Encrypted-data wiring proven.
    #[tokio::test]
    async fn to_cashu_mint_quote_unpaid() {
        let blob = json!({
            "paymentRequest": "lnbc...",
            "mintQuoteId": "qid",
            "amountReceived": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "totalFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
        });
        let encrypted = encrypt_blob(&blob).await;
        let row = tables::cashu_receive_quotes::CashuReceiveQuotesRow {
            id: Uuid::nil(),
            created_at: chrono::Utc::now(),
            account_id: Uuid::nil(),
            user_id: Uuid::nil(),
            expires_at: chrono::Utc::now(),
            state: crate::generated::enums::CashuReceiveQuoteState::Unpaid,
            keyset_id: None,
            keyset_counter: None,
            version: 0,
            transaction_id: Uuid::nil(),
            r#type: crate::generated::enums::ReceiveQuoteType::Lightning,
            locking_derivation_path: String::new(),
            failure_reason: None,
            encrypted_data: encrypted,
            payment_hash: "ph".into(),
            quote_id_hash: "qh".into(),
            cashu_token_melt_initiated: None,
        };
        let q = to_cashu_mint_quote(&row, passthrough().as_ref())
            .await
            .expect("UNPAID quote decodes");
        assert_eq!(q.quote_id, "qid");
        assert!(matches!(q.state, CashuMintQuoteState::Unpaid));
    }

    /// PAID mint quote pulls `output_amounts` from the decrypted blob,
    /// `keyset_id` / `keyset_counter` from the public columns.
    #[tokio::test]
    async fn to_cashu_mint_quote_paid_folds_keyset_with_blob_outputs() {
        let blob = json!({
            "paymentRequest": "lnbc...",
            "mintQuoteId": "qid",
            "amountReceived": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "totalFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
            "outputAmounts": [64],
        });
        let encrypted = encrypt_blob(&blob).await;
        let row = tables::cashu_receive_quotes::CashuReceiveQuotesRow {
            id: Uuid::nil(),
            created_at: chrono::Utc::now(),
            account_id: Uuid::nil(),
            user_id: Uuid::nil(),
            expires_at: chrono::Utc::now(),
            state: crate::generated::enums::CashuReceiveQuoteState::Paid,
            keyset_id: Some("00abcdef".into()),
            keyset_counter: Some(7),
            version: 1,
            transaction_id: Uuid::nil(),
            r#type: crate::generated::enums::ReceiveQuoteType::Lightning,
            locking_derivation_path: String::new(),
            failure_reason: None,
            encrypted_data: encrypted,
            payment_hash: "ph".into(),
            quote_id_hash: "qh".into(),
            cashu_token_melt_initiated: None,
        };
        let q = to_cashu_mint_quote(&row, passthrough().as_ref())
            .await
            .unwrap();
        match q.state {
            CashuMintQuoteState::Paid {
                keyset_id,
                keyset_counter,
                output_amounts,
            } => {
                assert_eq!(keyset_id, "00abcdef");
                assert_eq!(keyset_counter, 7);
                assert_eq!(output_amounts, vec![64]);
            }
            other => panic!("unexpected state: {other:?}"),
        }
    }

    /// FAILED mint quote pulls `failure_reason` from the public column.
    #[tokio::test]
    async fn to_cashu_mint_quote_failed_uses_public_failure_reason() {
        let blob = json!({
            "paymentRequest": "lnbc...",
            "mintQuoteId": "qid",
            "amountReceived": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "totalFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
        });
        let encrypted = encrypt_blob(&blob).await;
        let row = tables::cashu_receive_quotes::CashuReceiveQuotesRow {
            id: Uuid::nil(),
            created_at: chrono::Utc::now(),
            account_id: Uuid::nil(),
            user_id: Uuid::nil(),
            expires_at: chrono::Utc::now(),
            state: crate::generated::enums::CashuReceiveQuoteState::Failed,
            keyset_id: None,
            keyset_counter: None,
            version: 1,
            transaction_id: Uuid::nil(),
            r#type: crate::generated::enums::ReceiveQuoteType::Lightning,
            locking_derivation_path: String::new(),
            failure_reason: Some("Boom".into()),
            encrypted_data: encrypted,
            payment_hash: "ph".into(),
            quote_id_hash: "qh".into(),
            cashu_token_melt_initiated: None,
        };
        let q = to_cashu_mint_quote(&row, passthrough().as_ref())
            .await
            .unwrap();
        match q.state {
            CashuMintQuoteState::Failed { failure_reason } => assert_eq!(failure_reason, "Boom"),
            other => panic!("unexpected state: {other:?}"),
        }
    }

    /// UNPAID melt quote decrypts and folds; `proofs` always empty per
    /// the storage-layer contract.
    #[tokio::test]
    async fn to_cashu_melt_quote_unpaid_proofs_empty() {
        let blob = json!({
            "paymentRequest": "lnbc...",
            "amountRequested": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "amountRequestedInMsat": 64_000u64,
            "amountReceived": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "lightningFeeReserve": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
            "cashuSendFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
            "meltQuoteId": "mqid",
            "amountReserved": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
        });
        let encrypted = encrypt_blob(&blob).await;
        let row = tables::cashu_send_quotes::CashuSendQuotesRow {
            id: Uuid::nil(),
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now(),
            user_id: Uuid::nil(),
            account_id: Uuid::nil(),
            currency_requested: crate::generated::enums::Currency::Btc,
            keyset_id: "00abcdef".into(),
            keyset_counter: 0,
            number_of_change_outputs: 0,
            state: crate::generated::enums::CashuSendQuoteState::Unpaid,
            failure_reason: None,
            version: 0,
            transaction_id: Uuid::nil(),
            encrypted_data: encrypted,
            payment_hash: "ph".into(),
            quote_id_hash: "qh".into(),
        };
        let q = to_cashu_melt_quote(&row, passthrough().as_ref())
            .await
            .unwrap();
        assert_eq!(q.quote_id, "mqid");
        assert!(matches!(q.state, CashuMeltQuoteState::Unpaid));
        assert!(q.proofs.is_empty(), "proofs always empty for melt quote");
    }

    /// PAID melt quote without the required side fields → Backend error
    /// (matches existing inline behavior).
    #[tokio::test]
    async fn to_cashu_melt_quote_paid_missing_lightning_fee_errors() {
        let blob = json!({
            "paymentRequest": "lnbc...",
            "amountRequested": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "amountRequestedInMsat": 64_000u64,
            "amountReceived": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "lightningFeeReserve": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
            "cashuSendFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
            "meltQuoteId": "mqid",
            "amountReserved": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            // intentionally no `lightningFee` / `amountSpent` / `totalFee`
        });
        let encrypted = encrypt_blob(&blob).await;
        let row = tables::cashu_send_quotes::CashuSendQuotesRow {
            id: Uuid::nil(),
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now(),
            user_id: Uuid::nil(),
            account_id: Uuid::nil(),
            currency_requested: crate::generated::enums::Currency::Btc,
            keyset_id: "00abcdef".into(),
            keyset_counter: 0,
            number_of_change_outputs: 0,
            state: crate::generated::enums::CashuSendQuoteState::Paid,
            failure_reason: None,
            version: 1,
            transaction_id: Uuid::nil(),
            encrypted_data: encrypted,
            payment_hash: "ph".into(),
            quote_id_hash: "qh".into(),
        };
        let err = to_cashu_melt_quote(&row, passthrough().as_ref())
            .await
            .expect_err("PAID without lightning_fee must error");
        let msg = err.to_string();
        assert!(
            msg.contains("lightning_fee"),
            "error mentions the missing field: {msg}"
        );
    }

    /// PENDING receive swap decrypts.
    #[tokio::test]
    async fn to_cashu_receive_swap_pending_decodes() {
        let blob = json!({
            "tokenMintUrl": "https://m",
            "tokenAmount": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "tokenProofs": [],
            "amountReceived": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "outputAmounts": [64u64],
            "cashuReceiveFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
        });
        let encrypted = encrypt_blob(&blob).await;
        let row = tables::cashu_receive_swaps::CashuReceiveSwapsRow {
            token_hash: "abcd".into(),
            created_at: chrono::Utc::now(),
            account_id: Uuid::nil(),
            user_id: Uuid::nil(),
            keyset_id: "00abcdef".into(),
            keyset_counter: 1,
            state: crate::generated::enums::CashuReceiveSwapState::Pending,
            version: 0,
            failure_reason: None,
            transaction_id: Uuid::nil(),
            encrypted_data: encrypted,
        };
        let s = to_cashu_receive_swap(&row, passthrough().as_ref())
            .await
            .unwrap();
        assert_eq!(s.token_hash, "abcd");
        assert!(matches!(s.state, CashuReceiveSwapState::Pending));
    }

    /// FAILED receive swap pulls `failure_reason` from the public column.
    #[tokio::test]
    async fn to_cashu_receive_swap_failed_uses_public_failure_reason() {
        let blob = json!({
            "tokenMintUrl": "https://m",
            "tokenAmount": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "tokenProofs": [],
            "amountReceived": Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat),
            "outputAmounts": [64u64],
            "cashuReceiveFee": Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat),
        });
        let encrypted = encrypt_blob(&blob).await;
        let row = tables::cashu_receive_swaps::CashuReceiveSwapsRow {
            token_hash: "abcd".into(),
            created_at: chrono::Utc::now(),
            account_id: Uuid::nil(),
            user_id: Uuid::nil(),
            keyset_id: "00abcdef".into(),
            keyset_counter: 1,
            state: crate::generated::enums::CashuReceiveSwapState::Failed,
            version: 1,
            failure_reason: Some("token already spent".into()),
            transaction_id: Uuid::nil(),
            encrypted_data: encrypted,
        };
        let s = to_cashu_receive_swap(&row, passthrough().as_ref())
            .await
            .unwrap();
        match s.state {
            CashuReceiveSwapState::Failed { failure_reason } => {
                assert_eq!(failure_reason, "token already spent");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    // -------- to_cashu_send_swap --------

    /// Build a `SendData` JSON blob with all required side fields. Money
    /// amounts are uniform — these tests exercise the bucket / state
    /// machine logic, not the encrypted-data wiring.
    fn send_blob() -> Value {
        let money = Money::new(Decimal::from(64u64), Currency::Btc, Unit::Sat);
        let zero = Money::new(Decimal::from(0u64), Currency::Btc, Unit::Sat);
        json!({
            "tokenMintUrl": "https://m",
            "amountReceived": money,
            "cashuReceiveFee": zero,
            "amountToSend": money,
            "cashuSendFee": zero,
            "amountSpent": money,
            "amountReserved": money,
            "totalFee": zero,
        })
    }

    /// Build a send-swap row in the given state. `requires_input_proofs_swap`
    /// is wired in so the bucket logic in the helper sees the right shape.
    async fn send_swap_row(
        swap_id: Uuid,
        state: crate::generated::enums::CashuSendSwapState,
        requires_input_proofs_swap: bool,
    ) -> tables::cashu_send_swaps::CashuSendSwapsRow {
        tables::cashu_send_swaps::CashuSendSwapsRow {
            id: swap_id,
            user_id: Uuid::nil(),
            account_id: Uuid::nil(),
            transaction_id: Uuid::nil(),
            keyset_id: None,
            keyset_counter: None,
            token_hash: Some("th".into()),
            state,
            version: 1,
            created_at: chrono::Utc::now(),
            failure_reason: None,
            encrypted_data: encrypt_blob(&send_blob()).await,
            requires_input_proofs_swap,
        }
    }

    /// Build a column-encrypted proof row with the given send-swap
    /// linkage. `cashu_send_swap_id` set → produced by the swap;
    /// `spending_cashu_send_swap_id` set → spent by the swap. Set both
    /// for a proof-to-send, only the latter NOT set for a change proof,
    /// and neither for an input-only proof.
    async fn proof_row(
        secret: &str,
        amount: u64,
        cashu_send_swap_id: Option<Uuid>,
        spending_cashu_send_swap_id: Option<Uuid>,
    ) -> tables::cashu_proofs::CashuProofsRow {
        let enc = passthrough();
        let amount_cipher = enc.encrypt(amount.to_string().as_bytes()).await.unwrap();
        let secret_cipher = enc.encrypt(secret.as_bytes()).await.unwrap();
        tables::cashu_proofs::CashuProofsRow {
            id: Uuid::new_v4(),
            user_id: Uuid::nil(),
            account_id: Uuid::nil(),
            keyset_id: "ks1".into(),
            amount: general_purpose::STANDARD.encode(amount_cipher),
            secret: general_purpose::STANDARD.encode(secret_cipher),
            unblinded_signature: "C1".into(),
            public_key_y: "02deadbeef".into(),
            dleq: None,
            witness: None,
            state: crate::generated::enums::CashuProofState::Reserved,
            version: 0,
            created_at: chrono::Utc::now(),
            reserved_at: None,
            spent_at: None,
            cashu_receive_quote_id: None,
            cashu_receive_swap_token_hash: None,
            cashu_send_quote_id: None,
            spending_cashu_send_quote_id: None,
            cashu_send_swap_id,
            spending_cashu_send_swap_id,
        }
    }

    /// Exact-amount path (`requires_input_proofs_swap = false`): a single
    /// joined proof is both the input AND the proof-to-send. The helper
    /// double-counts (matches the TS-side OR condition).
    #[tokio::test]
    async fn to_cashu_send_swap_exact_amount_path_double_buckets_proof() {
        let swap_id = Uuid::new_v4();
        let row = send_swap_row(
            swap_id,
            crate::generated::enums::CashuSendSwapState::Pending,
            false,
        )
        .await;
        // For the exact-amount path the proof has neither linkage
        // (storage attaches it via a different path) — bucketing
        // ignores the linkage flags entirely.
        let proofs = vec![proof_row("sec1", 64, None, None).await];
        let swap = to_cashu_send_swap(&row, &proofs, passthrough().as_ref())
            .await
            .unwrap();
        assert_eq!(swap.input_proofs.len(), 1);
        assert_eq!(swap.input_proofs[0].amount, 64);
        match swap.state {
            CashuSendSwapState::Pending {
                token_hash,
                proofs_to_send,
            } => {
                assert_eq!(token_hash, "th");
                assert_eq!(proofs_to_send.len(), 1);
                assert_eq!(proofs_to_send[0].secret, "sec1");
            }
            other => panic!("expected Pending, got: {other:?}"),
        }
    }

    /// Input-swap path with a proof-to-send: `cashu_send_swap_id =
    /// swap.id` AND `spending_cashu_send_swap_id = swap.id` → goes into
    /// the `proofs_to_send` bucket only.
    #[tokio::test]
    async fn to_cashu_send_swap_input_swap_classifies_proof_to_send() {
        let swap_id = Uuid::new_v4();
        let row = send_swap_row(
            swap_id,
            crate::generated::enums::CashuSendSwapState::Pending,
            true,
        )
        .await;
        // One input proof (not produced by this swap) + one
        // proof-to-send (produced AND spent by this swap).
        let proofs = vec![
            proof_row("input1", 64, None, None).await,
            proof_row("send1", 32, Some(swap_id), Some(swap_id)).await,
        ];
        let swap = to_cashu_send_swap(&row, &proofs, passthrough().as_ref())
            .await
            .unwrap();
        assert_eq!(swap.input_proofs.len(), 1);
        assert_eq!(swap.input_proofs[0].secret, "input1");
        match swap.state {
            CashuSendSwapState::Pending { proofs_to_send, .. } => {
                assert_eq!(proofs_to_send.len(), 1);
                assert_eq!(proofs_to_send[0].secret, "send1");
                assert_eq!(proofs_to_send[0].amount, 32);
            }
            other => panic!("expected Pending, got: {other:?}"),
        }
    }

    /// Input-swap path with a change proof: `cashu_send_swap_id =
    /// swap.id` but `spending_cashu_send_swap_id != swap.id` (None
    /// here — flowed back to the account). Falls into NO bucket on the
    /// rich type.
    #[tokio::test]
    async fn to_cashu_send_swap_input_swap_change_proof_in_no_bucket() {
        let swap_id = Uuid::new_v4();
        let row = send_swap_row(
            swap_id,
            crate::generated::enums::CashuSendSwapState::Pending,
            true,
        )
        .await;
        // Input proof + proof-to-send + change proof (produced by swap
        // but not spent by it).
        let proofs = vec![
            proof_row("input1", 64, None, None).await,
            proof_row("send1", 32, Some(swap_id), Some(swap_id)).await,
            proof_row("change1", 16, Some(swap_id), None).await,
        ];
        let swap = to_cashu_send_swap(&row, &proofs, passthrough().as_ref())
            .await
            .unwrap();
        assert_eq!(swap.input_proofs.len(), 1, "change must not be input");
        assert_eq!(swap.input_proofs[0].secret, "input1");
        match swap.state {
            CashuSendSwapState::Pending { proofs_to_send, .. } => {
                assert_eq!(
                    proofs_to_send.len(),
                    1,
                    "change must not be in proofs_to_send"
                );
                assert_eq!(proofs_to_send[0].secret, "send1");
            }
            other => panic!("expected Pending, got: {other:?}"),
        }
    }

    /// FAILED state pulls `failure_reason` from the public column.
    #[tokio::test]
    async fn to_cashu_send_swap_failed_uses_public_failure_reason() {
        let swap_id = Uuid::new_v4();
        let mut row = send_swap_row(
            swap_id,
            crate::generated::enums::CashuSendSwapState::Failed,
            true,
        )
        .await;
        row.failure_reason = Some("mint rejected".into());
        let swap = to_cashu_send_swap(&row, &[], passthrough().as_ref())
            .await
            .unwrap();
        match swap.state {
            CashuSendSwapState::Failed { failure_reason } => {
                assert_eq!(failure_reason, "mint rejected");
            }
            other => panic!("expected Failed, got: {other:?}"),
        }
    }
}
