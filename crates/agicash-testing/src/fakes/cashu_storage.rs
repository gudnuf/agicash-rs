//! The four in-memory cashu storages, shared by both tiers.
//!
//! Promoted from the proven `MemStorage` in
//! `agicash-cashu/tests/cdk_mint_money_flows.rs` (a full round-trip
//! `CashuMeltQuoteStorage` with the partial-unique-index `DuplicatePayment`
//! backstop + post-settle fault injection) and generalized to the other
//! three storage traits. House style: `parking_lot::Mutex<HashMap<Uuid,…>>`,
//! `#[derive(Debug, Default)]`, real state transitions, and the exact typed
//! errors the Supabase impls return (`NotFound`, `DuplicatePayment`,
//! `Concurrency`, `InvalidState`, `AlreadyClaimed`).

use agicash_cashu::{
    CashuMeltQuote, CashuMeltQuoteState, CashuMeltQuoteStorage, CashuMintQuote,
    CashuMintQuoteState, CashuMintQuoteStorage, CashuReceiveSwap, CashuReceiveSwapState,
    CashuReceiveSwapStorage, CashuSendSwap, CashuSendSwapState, CashuSendSwapStorage,
    CommitProofsToSend, CompleteMeltQuote, CompleteMeltQuoteResult, CompleteMintQuote,
    CompleteMintQuoteResult, CompleteReceiveSwapResult, CreateMeltQuote, CreateMeltQuoteResult,
    CreateMintQuote, CreateReceiveSwap, CreateReceiveSwapResult, CreateSendSwap,
    CreateSendSwapResult, MeltQuoteStorageError, MintQuoteStorageError, ProcessMintQuotePayment,
    ProcessMintQuotePaymentResult, ProofWithId, ReceiveSwapStorageError, SendSwapStorageError,
    TokenProof,
};
use agicash_domain::{Account, AccountId, UserId};
use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use uuid::Uuid;

use crate::fakes::user_storage::cashu_account;

/// Placeholder account returned alongside storage results. Tier 1 never
/// inspects it (M1/12c-3 short-circuit; 12c-7 only constructs the flow).
fn unused_account(user_id: UserId) -> Account {
    cashu_account(user_id, "http://unused", agicash_domain::Currency::Btc)
}

/// Echo back an account that carries the **real** id / owner / `mint_url` /
/// currency from a `create*` input. The real Supabase `create_*_swap`
/// RPCs return the genuine account row joined from the input's
/// `account_id`; the in-memory fake must do the same so the downstream
/// service (`swap_for_proofs_to_send` / `complete_swap`) routes its mint
/// connector at the real spawned mint, not a `http://unused` placeholder.
/// Tier 1 never reaches these paths (the `StubCashuProvider`
/// short-circuits), so the previous placeholder was inert there; Tier 2
/// drives them against the real mint and needs the real URL.
fn echo_account(
    account_id: AccountId,
    user_id: UserId,
    mint_url: &str,
    currency: agicash_domain::Currency,
) -> Account {
    let mut a = cashu_account(user_id, mint_url, currency);
    a.id = account_id;
    a
}

// ---------------------------------------------------------------------------
// Receive-swap
// ---------------------------------------------------------------------------

/// In-memory `CashuReceiveSwapStorage`. Keyed by `(token_hash, user_id)`,
/// mirroring the DB unique constraint that yields `AlreadyClaimed` (23505).
#[derive(Debug, Default)]
pub struct InMemoryReceiveSwapStorage {
    rows: Mutex<HashMap<(String, Uuid), CashuReceiveSwap>>,
}

impl InMemoryReceiveSwapStorage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl CashuReceiveSwapStorage for InMemoryReceiveSwapStorage {
    async fn create(
        &self,
        input: CreateReceiveSwap,
    ) -> Result<CreateReceiveSwapResult, ReceiveSwapStorageError> {
        let key = (input.token_hash.clone(), input.user_id.as_uuid());
        // Capture the real routing fields before `input` is consumed, so
        // the echoed account points the downstream mint connector at the
        // real spawned mint (not the `http://unused` placeholder).
        let echoed = echo_account(
            input.account_id,
            input.user_id,
            &input.token_mint_url,
            input.amount_received.currency(),
        );
        let mut rows = self.rows.lock();
        if rows.contains_key(&key) {
            return Err(ReceiveSwapStorageError::AlreadyClaimed);
        }
        let swap = CashuReceiveSwap {
            token_hash: input.token_hash,
            token_proofs: input.token_proofs,
            token_description: input.token_description,
            user_id: input.user_id,
            account_id: input.account_id,
            input_amount: input.input_amount,
            amount_received: input.amount_received,
            fee_amount: input.fee_amount,
            keyset_id: input.keyset_id,
            keyset_counter: 0,
            output_amounts: input.output_amounts,
            transaction_id: input.reversed_transaction_id.unwrap_or_else(Uuid::new_v4),
            created_at: Utc::now(),
            version: 0,
            state: CashuReceiveSwapState::Pending,
        };
        rows.insert(key, swap.clone());
        Ok(CreateReceiveSwapResult {
            swap,
            account: echoed,
        })
    }

    async fn complete(
        &self,
        token_hash: &str,
        user_id: UserId,
        _proofs: Vec<TokenProof>,
    ) -> Result<CompleteReceiveSwapResult, ReceiveSwapStorageError> {
        let mut rows = self.rows.lock();
        let swap = rows
            .get_mut(&(token_hash.to_string(), user_id.as_uuid()))
            .ok_or(ReceiveSwapStorageError::NotFound)?;
        match &swap.state {
            // Idempotent on COMPLETED.
            CashuReceiveSwapState::Completed => {}
            CashuReceiveSwapState::Pending => {
                swap.state = CashuReceiveSwapState::Completed;
                swap.version += 1;
            }
            CashuReceiveSwapState::Failed { .. } => {
                return Err(ReceiveSwapStorageError::InvalidState(
                    "complete from FAILED".into(),
                ))
            }
        }
        let swap = swap.clone();
        Ok(CompleteReceiveSwapResult {
            swap,
            account: unused_account(user_id),
            added_proofs: vec![],
        })
    }

    async fn fail(
        &self,
        token_hash: &str,
        user_id: UserId,
        reason: &str,
    ) -> Result<CashuReceiveSwap, ReceiveSwapStorageError> {
        let mut rows = self.rows.lock();
        let swap = rows
            .get_mut(&(token_hash.to_string(), user_id.as_uuid()))
            .ok_or(ReceiveSwapStorageError::NotFound)?;
        match &swap.state {
            CashuReceiveSwapState::Failed { .. } => {}
            CashuReceiveSwapState::Completed => {
                return Err(ReceiveSwapStorageError::InvalidState(
                    "fail from COMPLETED".into(),
                ))
            }
            CashuReceiveSwapState::Pending => {
                swap.state = CashuReceiveSwapState::Failed {
                    failure_reason: reason.to_string(),
                };
                swap.version += 1;
            }
        }
        Ok(swap.clone())
    }
}

// ---------------------------------------------------------------------------
// Send-swap
// ---------------------------------------------------------------------------

/// In-memory `CashuSendSwapStorage`. Tracks swaps by id and a per-account
/// pool of UNSPENT proofs (`fund_account` seeds it).
#[derive(Debug, Default)]
pub struct InMemorySendSwapStorage {
    rows: Mutex<HashMap<Uuid, CashuSendSwap>>,
    /// `account_id` → unspent proofs available to spend as swap inputs.
    unspent: Mutex<HashMap<Uuid, Vec<ProofWithId>>>,
    /// `account_id` → next NUT-13 keyset counter. The real Supabase
    /// `create_send_swap` RPC reads + bumps `account.details
    /// .keyset_counters`; the in-memory fake must model the same
    /// monotonic counter so a Tier-2 swap-out against a *real* mint never
    /// reuses blinding factors (the mint rejects a replayed blinded
    /// message). Tier 1 never reaches the DRAFT swap path (the
    /// `StubCashuProvider` short-circuits), so this is inert there.
    keyset_counter: Mutex<HashMap<Uuid, u32>>,
}

impl InMemorySendSwapStorage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Test helper: credit `account_id` with UNSPENT `proofs`. Mirrors the
    /// `MemStorage::insert`-style seeding the proven template uses.
    pub fn fund_account(&self, account_id: AccountId, proofs: Vec<TokenProof>) {
        let mut pool = self.unspent.lock();
        let entry = pool.entry(account_id.as_uuid()).or_default();
        for proof in proofs {
            entry.push(ProofWithId {
                id: Uuid::new_v4(),
                proof,
            });
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl CashuSendSwapStorage for InMemorySendSwapStorage {
    async fn create(
        &self,
        input: CreateSendSwap,
    ) -> Result<CreateSendSwapResult, SendSwapStorageError> {
        // Echo the REAL account (real id / owner / mint_url / currency)
        // so `swap_for_proofs_to_send` routes its connector at the real
        // spawned mint, not the `http://unused` placeholder. Captured
        // before `input` is consumed below.
        let echoed = echo_account(
            input.account_id,
            input.user_id,
            &input.token_mint_url,
            input.amount_requested.currency(),
        );
        // Reserve the chosen input proof ids out of the account pool.
        {
            let mut pool = self.unspent.lock();
            if let Some(v) = pool.get_mut(&input.account_id.as_uuid()) {
                v.retain(|p| !input.input_proof_ids.contains(&p.id));
            }
        }
        // input_amount == amount_to_send → no input swap (PENDING with the
        // input proofs as the proofs-to-send). Otherwise DRAFT.
        let (state, keyset_counter) = if input.input_amount == input.amount_to_send {
            // Exact-proofs path: no mint swap, no blinded outputs → no
            // counter needed (matches the real RPC: PENDING straight away).
            (
                CashuSendSwapState::Pending {
                    token_hash: input.token_hash.clone().unwrap_or_default(),
                    proofs_to_send: input.input_proofs.clone(),
                },
                None,
            )
        } else {
            // Swap path (DRAFT): allocate the account's current NUT-13
            // counter, then advance it past every blinded output this swap
            // will request (send + change), so a subsequent swap-out on the
            // same account derives FRESH blinding factors. The real
            // Supabase `create_send_swap` does the identical read-then-bump
            // under its row lock; without this, a Tier-2 second send on the
            // same account replays counter 0 and the real mint rejects the
            // blinded message ("already signed").
            let outputs: u32 = input.output_amounts.as_ref().map_or(0, |o| {
                u32::try_from(o.send.len() + o.change.len()).unwrap_or(u32::MAX)
            });
            let mut counters = self.keyset_counter.lock();
            let slot = counters.entry(input.account_id.as_uuid()).or_insert(0);
            let start = *slot;
            *slot = slot.saturating_add(outputs.max(1));
            (CashuSendSwapState::Draft, Some(start))
        };
        let swap = CashuSendSwap {
            id: Uuid::new_v4(),
            account_id: input.account_id,
            user_id: input.user_id,
            input_proofs: input.input_proofs,
            input_amount: input.input_amount,
            amount_received: input.amount_requested,
            cashu_receive_fee: input.cashu_receive_fee,
            amount_to_send: input.amount_to_send,
            cashu_send_fee: input.cashu_send_fee,
            amount_spent: input.total_amount,
            total_fee: input.cashu_send_fee,
            keyset_id: input.keyset_id,
            keyset_counter,
            output_amounts: input.output_amounts,
            transaction_id: Uuid::new_v4(),
            created_at: Utc::now(),
            version: 0,
            state,
        };
        self.rows.lock().insert(swap.id, swap.clone());
        Ok(CreateSendSwapResult {
            swap,
            account: echoed,
        })
    }

    async fn commit_proofs_to_send(
        &self,
        input: CommitProofsToSend,
    ) -> Result<CashuSendSwap, SendSwapStorageError> {
        let mut rows = self.rows.lock();
        let swap = rows
            .get_mut(&input.swap_id)
            .ok_or(SendSwapStorageError::NotFound)?;
        if !matches!(swap.state, CashuSendSwapState::Draft) {
            return Err(SendSwapStorageError::InvalidState(format!(
                "commit_proofs_to_send from {:?}",
                swap.state
            )));
        }
        swap.state = CashuSendSwapState::Pending {
            token_hash: input.token_hash,
            proofs_to_send: input.proofs_to_send,
        };
        swap.version += 1;
        Ok(swap.clone())
    }

    async fn complete(&self, swap_id: Uuid) -> Result<CashuSendSwap, SendSwapStorageError> {
        let mut rows = self.rows.lock();
        let swap = rows
            .get_mut(&swap_id)
            .ok_or(SendSwapStorageError::NotFound)?;
        match swap.state.clone() {
            CashuSendSwapState::Completed { .. } => {}
            CashuSendSwapState::Pending {
                token_hash,
                proofs_to_send,
            } => {
                swap.state = CashuSendSwapState::Completed {
                    token_hash,
                    proofs_to_send,
                };
                swap.version += 1;
            }
            other => {
                return Err(SendSwapStorageError::InvalidState(format!(
                    "complete from {other:?}"
                )))
            }
        }
        Ok(swap.clone())
    }

    async fn fail(
        &self,
        swap_id: Uuid,
        reason: &str,
    ) -> Result<CashuSendSwap, SendSwapStorageError> {
        let mut rows = self.rows.lock();
        let swap = rows
            .get_mut(&swap_id)
            .ok_or(SendSwapStorageError::NotFound)?;
        match &swap.state {
            CashuSendSwapState::Failed { .. } => {}
            CashuSendSwapState::Draft => {
                swap.state = CashuSendSwapState::Failed {
                    failure_reason: reason.to_string(),
                };
                swap.version += 1;
            }
            other => {
                return Err(SendSwapStorageError::InvalidState(format!(
                    "fail from {other:?}"
                )))
            }
        }
        Ok(swap.clone())
    }

    async fn list_unspent_proofs(
        &self,
        account_id: AccountId,
    ) -> Result<Vec<ProofWithId>, SendSwapStorageError> {
        Ok(self
            .unspent
            .lock()
            .get(&account_id.as_uuid())
            .cloned()
            .unwrap_or_default())
    }

    async fn get(&self, swap_id: Uuid) -> Result<CashuSendSwap, SendSwapStorageError> {
        self.rows
            .lock()
            .get(&swap_id)
            .cloned()
            .ok_or(SendSwapStorageError::NotFound)
    }
}

// ---------------------------------------------------------------------------
// Mint-quote
// ---------------------------------------------------------------------------

/// In-memory `CashuMintQuoteStorage`. Faithful UNPAID→PAID→COMPLETED
/// transitions; idempotent on terminal states like the real Postgres fns.
#[derive(Debug, Default)]
pub struct InMemoryMintQuoteStorage {
    rows: Mutex<HashMap<Uuid, CashuMintQuote>>,
}

impl InMemoryMintQuoteStorage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl CashuMintQuoteStorage for InMemoryMintQuoteStorage {
    async fn create(
        &self,
        input: CreateMintQuote,
    ) -> Result<CashuMintQuote, MintQuoteStorageError> {
        let quote = CashuMintQuote {
            id: Uuid::new_v4(),
            quote_id: input.quote_id,
            user_id: input.user_id,
            account_id: input.account_id,
            amount: input.amount,
            description: input.description,
            payment_request: input.payment_request,
            payment_hash: input.payment_hash,
            locking_derivation_path: input.locking_derivation_path,
            transaction_id: Uuid::new_v4(),
            minting_fee: input.minting_fee,
            total_fee: input.total_fee,
            created_at: Utc::now(),
            expires_at: input.expires_at,
            version: 0,
            state: CashuMintQuoteState::Unpaid,
        };
        self.rows.lock().insert(quote.id, quote.clone());
        Ok(quote)
    }

    async fn process_payment(
        &self,
        input: ProcessMintQuotePayment,
    ) -> Result<ProcessMintQuotePaymentResult, MintQuoteStorageError> {
        let mut rows = self.rows.lock();
        let quote = rows
            .get_mut(&input.quote.id)
            .ok_or(MintQuoteStorageError::NotFound)?;
        match &quote.state {
            // Idempotent on PAID/COMPLETED.
            CashuMintQuoteState::Paid { .. } | CashuMintQuoteState::Completed { .. } => {}
            CashuMintQuoteState::Unpaid => {
                quote.state = CashuMintQuoteState::Paid {
                    keyset_id: input.keyset_id,
                    keyset_counter: 0,
                    output_amounts: input.output_amounts,
                };
                quote.version += 1;
            }
            other => {
                return Err(MintQuoteStorageError::InvalidState(format!(
                    "process_payment from {other:?}"
                )))
            }
        }
        let quote = quote.clone();
        let user_id = quote.user_id;
        Ok(ProcessMintQuotePaymentResult {
            quote,
            account: unused_account(user_id),
        })
    }

    async fn complete(
        &self,
        input: CompleteMintQuote,
    ) -> Result<CompleteMintQuoteResult, MintQuoteStorageError> {
        let mut rows = self.rows.lock();
        let quote = rows
            .get_mut(&input.quote_id)
            .ok_or(MintQuoteStorageError::NotFound)?;
        match quote.state.clone() {
            CashuMintQuoteState::Completed { .. } => {}
            CashuMintQuoteState::Paid {
                keyset_id,
                keyset_counter,
                output_amounts,
            } => {
                quote.state = CashuMintQuoteState::Completed {
                    keyset_id,
                    keyset_counter,
                    output_amounts,
                };
                quote.version += 1;
            }
            other => {
                return Err(MintQuoteStorageError::InvalidState(format!(
                    "complete from {other:?}"
                )))
            }
        }
        let quote = quote.clone();
        let user_id = quote.user_id;
        Ok(CompleteMintQuoteResult {
            quote,
            account: unused_account(user_id),
            added_proofs: vec![],
        })
    }

    async fn expire(&self, quote_id: Uuid) -> Result<CashuMintQuote, MintQuoteStorageError> {
        let mut rows = self.rows.lock();
        let quote = rows
            .get_mut(&quote_id)
            .ok_or(MintQuoteStorageError::NotFound)?;
        match &quote.state {
            CashuMintQuoteState::Expired => {}
            CashuMintQuoteState::Unpaid => {
                quote.state = CashuMintQuoteState::Expired;
                quote.version += 1;
            }
            other => {
                return Err(MintQuoteStorageError::InvalidState(format!(
                    "expire from {other:?}"
                )))
            }
        }
        Ok(quote.clone())
    }

    async fn fail(
        &self,
        quote_id: Uuid,
        reason: &str,
    ) -> Result<CashuMintQuote, MintQuoteStorageError> {
        let mut rows = self.rows.lock();
        let quote = rows
            .get_mut(&quote_id)
            .ok_or(MintQuoteStorageError::NotFound)?;
        match &quote.state {
            CashuMintQuoteState::Failed { .. } => {}
            CashuMintQuoteState::Unpaid => {
                quote.state = CashuMintQuoteState::Failed {
                    failure_reason: reason.to_string(),
                };
                quote.version += 1;
            }
            other => {
                return Err(MintQuoteStorageError::InvalidState(format!(
                    "fail from {other:?}"
                )))
            }
        }
        Ok(quote.clone())
    }

    async fn get(&self, quote_id: Uuid) -> Result<CashuMintQuote, MintQuoteStorageError> {
        self.rows
            .lock()
            .get(&quote_id)
            .cloned()
            .ok_or(MintQuoteStorageError::NotFound)
    }
}

// ---------------------------------------------------------------------------
// Melt-quote — verbatim promotion of the proven `MemStorage` template.
// ---------------------------------------------------------------------------

/// True for the active states the partial unique index
/// `cashu_send_quotes_payment_hash_active_unique` covers
/// (UNPAID/PENDING/PAID). FAILED/EXPIRED are excluded so a genuinely
/// failed/expired attempt stays legitimately retryable — identical
/// predicate to the supabase `find_active_by_payment_hash`.
fn is_active_state(state: &CashuMeltQuoteState) -> bool {
    matches!(
        state,
        CashuMeltQuoteState::Unpaid
            | CashuMeltQuoteState::Pending
            | CashuMeltQuoteState::Paid { .. }
    )
}

/// In-memory `CashuMeltQuoteStorage`. Faithful enough to drive the real
/// `CashuMeltQuoteService`, with the partial-unique-index `DuplicatePayment`
/// backstop and a single-shot post-settle `complete()` fault injection — the
/// proven P0 double-pay template lifted verbatim from
/// `cdk_mint_money_flows.rs`.
#[derive(Debug, Default)]
pub struct InMemoryMeltQuoteStorage {
    rows: Mutex<HashMap<Uuid, CashuMeltQuote>>,
    /// When > 0, the next `complete()` returns a Backend error and
    /// decrements. Simulates a crash AFTER the mint settled.
    fail_complete_times: AtomicUsize,
    complete_calls: AtomicUsize,
    /// Number of times `create()` actually inserted a fresh row.
    create_inserts: AtomicUsize,
}

impl InMemoryMeltQuoteStorage {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Test helper: seed a row directly.
    pub fn insert(&self, q: CashuMeltQuote) {
        self.rows.lock().insert(q.id, q);
    }

    /// Arm the next `times` `complete()` calls to fail post-settle.
    pub fn arm_complete_failure(&self, times: usize) {
        self.fail_complete_times.store(times, Ordering::SeqCst);
    }

    #[must_use]
    pub fn create_insert_count(&self) -> usize {
        self.create_inserts.load(Ordering::SeqCst)
    }

    #[must_use]
    pub fn complete_call_count(&self) -> usize {
        self.complete_calls.load(Ordering::SeqCst)
    }

    /// Test helper: the id of the single persisted melt quote, if exactly
    /// one exists. Used by the Tier 2 P0 no-double-pay test to recover the
    /// `quote_id` after `begin_send_lightning` returns `Err` (the
    /// post-settle storage-fault path propagates the error before the
    /// facade surfaces a handle, but the row is already persisted PENDING
    /// — exactly the reconcile entry point).
    #[must_use]
    pub fn single_quote_id(&self) -> Option<Uuid> {
        let rows = self.rows.lock();
        if rows.len() == 1 {
            rows.keys().next().copied()
        } else {
            None
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl CashuMeltQuoteStorage for InMemoryMeltQuoteStorage {
    async fn create(
        &self,
        input: CreateMeltQuote,
    ) -> Result<CreateMeltQuoteResult, MeltQuoteStorageError> {
        {
            let rows = self.rows.lock();
            if rows.values().any(|q| {
                q.user_id == input.user_id
                    && q.payment_hash == input.payment_hash
                    && is_active_state(&q.state)
            }) {
                return Err(MeltQuoteStorageError::DuplicatePayment);
            }
        }
        let quote_id = Uuid::new_v4();
        let quote = CashuMeltQuote {
            id: quote_id,
            quote_id: input.quote_id,
            user_id: input.user_id,
            account_id: input.account_id,
            payment_request: input.payment_request,
            payment_hash: input.payment_hash,
            amount_requested: input.amount_requested,
            amount_requested_in_msat: input.amount_requested_in_msat,
            amount_received: input.amount_received,
            lightning_fee_reserve: input.lightning_fee_reserve,
            cashu_fee: input.cashu_fee,
            proofs: input.proofs,
            amount_reserved: input.amount_reserved,
            keyset_id: input.keyset_id,
            keyset_counter: 0,
            number_of_change_outputs: input.number_of_change_outputs,
            transaction_id: Uuid::new_v4(),
            created_at: Utc::now(),
            expires_at: input.expires_at,
            version: 0,
            state: CashuMeltQuoteState::Unpaid,
        };
        let user_id = quote.user_id;
        self.rows.lock().insert(quote_id, quote.clone());
        self.create_inserts.fetch_add(1, Ordering::SeqCst);
        Ok(CreateMeltQuoteResult {
            quote,
            account: unused_account(user_id),
        })
    }

    async fn mark_as_pending(
        &self,
        quote_id: Uuid,
    ) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let mut rows = self.rows.lock();
        let q = rows
            .get_mut(&quote_id)
            .ok_or(MeltQuoteStorageError::NotFound)?;
        if matches!(q.state, CashuMeltQuoteState::Pending) {
            return Ok(q.clone());
        }
        if !matches!(q.state, CashuMeltQuoteState::Unpaid) {
            return Err(MeltQuoteStorageError::InvalidState(format!(
                "mark_as_pending from {:?}",
                q.state
            )));
        }
        q.state = CashuMeltQuoteState::Pending;
        q.version += 1;
        Ok(q.clone())
    }

    async fn complete(
        &self,
        input: CompleteMeltQuote,
    ) -> Result<CompleteMeltQuoteResult, MeltQuoteStorageError> {
        self.complete_calls.fetch_add(1, Ordering::SeqCst);
        let remaining = self.fail_complete_times.load(Ordering::SeqCst);
        if remaining > 0 {
            self.fail_complete_times
                .store(remaining - 1, Ordering::SeqCst);
            return Err(MeltQuoteStorageError::Backend(
                "injected post-settle storage failure".into(),
            ));
        }
        let mut rows = self.rows.lock();
        let q = rows
            .get_mut(&input.quote.id)
            .ok_or(MeltQuoteStorageError::NotFound)?;
        let user_id = q.user_id;
        // Idempotent on PAID (mirrors the real Postgres fn contract).
        if let CashuMeltQuoteState::Paid { .. } = q.state {
            return Ok(CompleteMeltQuoteResult {
                quote: q.clone(),
                account: unused_account(user_id),
                added_change_proofs: vec![],
            });
        }
        q.state = CashuMeltQuoteState::Paid {
            payment_preimage: input.payment_preimage.clone(),
            lightning_fee: input.quote.lightning_fee_reserve,
            amount_spent: input.amount_spent,
            total_fee: input.quote.lightning_fee_reserve,
        };
        q.version += 1;
        let quote = q.clone();
        drop(rows);
        Ok(CompleteMeltQuoteResult {
            quote,
            account: unused_account(user_id),
            added_change_proofs: input
                .change_proofs
                .iter()
                .map(|_| Uuid::new_v4().to_string())
                .collect(),
        })
    }

    async fn expire(&self, quote_id: Uuid) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let mut rows = self.rows.lock();
        let q = rows
            .get_mut(&quote_id)
            .ok_or(MeltQuoteStorageError::NotFound)?;
        q.state = CashuMeltQuoteState::Expired;
        Ok(q.clone())
    }

    async fn fail(
        &self,
        quote_id: Uuid,
        reason: &str,
    ) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let mut rows = self.rows.lock();
        let q = rows
            .get_mut(&quote_id)
            .ok_or(MeltQuoteStorageError::NotFound)?;
        q.state = CashuMeltQuoteState::Failed {
            failure_reason: reason.to_string(),
        };
        Ok(q.clone())
    }

    async fn get(&self, quote_id: Uuid) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        self.rows
            .lock()
            .get(&quote_id)
            .cloned()
            .ok_or(MeltQuoteStorageError::NotFound)
    }

    async fn find_active_by_payment_hash(
        &self,
        user_id: UserId,
        payment_hash: &str,
    ) -> Result<Option<CashuMeltQuote>, MeltQuoteStorageError> {
        Ok(self
            .rows
            .lock()
            .values()
            .find(|q| {
                q.user_id == user_id && q.payment_hash == payment_hash && is_active_state(&q.state)
            })
            .cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agicash_domain::Currency;
    use agicash_money::{Money, Unit};
    use rust_decimal::Decimal;

    fn money(n: u64) -> Money {
        Money::new(Decimal::from(n), Currency::Btc, Unit::Sat)
    }

    fn proof(amount: u64) -> TokenProof {
        TokenProof {
            id: "ks1".into(),
            amount,
            secret: format!("s{amount}"),
            c: format!("C{amount}"),
            dleq: None,
            witness: None,
        }
    }

    #[tokio::test]
    async fn send_swap_list_unspent_proofs_empty_account_is_ok_empty() {
        let s = InMemorySendSwapStorage::new();
        let proofs = s.list_unspent_proofs(AccountId::new()).await.unwrap();
        assert!(proofs.is_empty());
    }

    #[tokio::test]
    async fn send_swap_fund_account_then_list_returns_proofs() {
        let s = InMemorySendSwapStorage::new();
        let acct = AccountId::new();
        s.fund_account(acct, vec![proof(32), proof(16)]);
        let listed = s.list_unspent_proofs(acct).await.unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[tokio::test]
    async fn receive_swap_create_then_duplicate_is_already_claimed() {
        let s = InMemoryReceiveSwapStorage::new();
        let uid = UserId::new();
        let input = CreateReceiveSwap {
            token_hash: "hash".into(),
            token_proofs: vec![proof(64)],
            token_mint_url: "https://mint.example".into(),
            token_description: None,
            user_id: uid,
            account_id: AccountId::new(),
            keyset_id: "ks1".into(),
            input_amount: money(64),
            fee_amount: money(0),
            amount_received: money(64),
            output_amounts: vec![64],
            reversed_transaction_id: None,
        };
        assert!(s.create(input.clone()).await.is_ok());
        assert!(matches!(
            s.create(input).await,
            Err(ReceiveSwapStorageError::AlreadyClaimed)
        ));
    }

    #[tokio::test]
    async fn mint_quote_create_then_process_then_complete_transitions() {
        let s = InMemoryMintQuoteStorage::new();
        let q = s
            .create(CreateMintQuote {
                user_id: UserId::new(),
                account_id: AccountId::new(),
                amount: money(64),
                description: None,
                quote_id: "qid".into(),
                payment_request: "lnbc".into(),
                payment_hash: "hash".into(),
                expires_at: Utc::now(),
                locking_derivation_path: String::new(),
                minting_fee: None,
                total_fee: money(0),
            })
            .await
            .unwrap();
        assert!(matches!(q.state, CashuMintQuoteState::Unpaid));
        let paid = s
            .process_payment(ProcessMintQuotePayment {
                quote: q.clone(),
                keyset_id: "ks1".into(),
                output_amounts: vec![64],
            })
            .await
            .unwrap();
        assert!(matches!(paid.quote.state, CashuMintQuoteState::Paid { .. }));
        let done = s
            .complete(CompleteMintQuote {
                quote_id: q.id,
                proofs: vec![proof(64)],
            })
            .await
            .unwrap();
        assert!(matches!(
            done.quote.state,
            CashuMintQuoteState::Completed { .. }
        ));
    }

    #[tokio::test]
    async fn melt_quote_duplicate_payment_backstop_and_fault_injection() {
        let s = InMemoryMeltQuoteStorage::new();
        let uid = UserId::new();
        let mk = |hash: &str| CreateMeltQuote {
            user_id: uid,
            account_id: AccountId::new(),
            payment_request: "lnbc".into(),
            payment_hash: hash.into(),
            expires_at: Utc::now(),
            quote_id: "qid".into(),
            amount_requested: money(64),
            amount_requested_in_msat: 64_000,
            amount_received: money(64),
            lightning_fee_reserve: money(1),
            cashu_fee: money(0),
            proofs: vec![proof(64)],
            proof_ids: vec![Uuid::new_v4()],
            amount_reserved: money(64),
            keyset_id: "ks1".into(),
            number_of_change_outputs: 1,
        };
        let created = s.create(mk("h1")).await.unwrap();
        assert_eq!(s.create_insert_count(), 1);
        // Second create with same (user, payment_hash) → DuplicatePayment.
        assert!(matches!(
            s.create(mk("h1")).await,
            Err(MeltQuoteStorageError::DuplicatePayment)
        ));
        // Arm one post-settle complete() failure, then verify reconcile.
        s.arm_complete_failure(1);
        let complete = CompleteMeltQuote {
            quote: created.quote.clone(),
            payment_preimage: "preimage".into(),
            amount_spent: money(63),
            change_proofs: vec![],
        };
        assert!(matches!(
            s.complete(complete.clone()).await,
            Err(MeltQuoteStorageError::Backend(_))
        ));
        let done = s.complete(complete).await.unwrap();
        assert!(matches!(done.quote.state, CashuMeltQuoteState::Paid { .. }));
        assert_eq!(s.complete_call_count(), 2);
    }
}
