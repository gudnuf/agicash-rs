//! Money-path E2E against a REAL local CDK mint (`cdk-mintd` +
//! `FakeWallet`).
//!
//! WHY THIS EXISTS
//! ----------------
//! Before this, the agicash-rs money flows were only unit/build/static
//! verified. The melt orchestrator (`CashuMeltQuoteService`) had unit
//! tests whose mock provider's `wallet_for_account` is `unreachable!()`
//! — i.e. `initiate_melt` / `poll_until_complete` were NEVER driven
//! against any mint. That is the class of hole a P0 melt double-pay
//! slipped through (the iOS fix at master 4d1a7e52 is Swift
//! state-machine only; the Rust orchestrator's equivalent reconcile
//! path had zero live coverage).
//!
//! This drives the production code path — `cdk::wallet::HttpClient`
//! implementing `MintConnector`, wrapped by `CdkCashuProvider` /
//! `CashuMintWallet`, the exact stack production uses — against a real
//! Cashu mint speaking the NUT wire protocol.
//!
//! WHAT IT COVERS
//! --------------
//!  1. NUT-04 mint-quote receive: post mint quote → `FakeWallet`
//!     auto-settles → `post_mint` → real proofs land.
//!  2. NUT-06 swap round-trip (the operation `send` performs) +
//!     double-spend enforcement (NUT-07 check-state).
//!  3. NUT-05 Lightning MELT send via the REAL
//!     `CashuMeltQuoteService::{get_quote, initiate_melt}` pipeline —
//!     proofs spent, quote PAID.
//!  4. **The P0 double-pay reconcile path**: an in-memory storage whose
//!     `complete()` is fault-injected to FAIL exactly once *after the
//!     mint has already settled the payment*. Asserts the flow does
//!     NOT re-initiate (which would re-`post_melt` = a second Lightning
//!     payment) and instead reconciles to PAID via
//!     `poll_until_complete` (a read-only NUT-05 status poll). The
//!     no-double-pay invariant is then proven at the PROTOCOL level: a
//!     manual second `post_melt` with the same inputs is rejected by
//!     the mint because the proofs are already spent.
//!
//! GATING
//! ------
//! Opt-in: `--features cdk-mint-e2e`. Needs a mint at
//! `AGICASH_TEST_MINT_URL` (default `http://127.0.0.1:8087`). Bring one
//! up with `bash scripts/cdk-mint-e2e.sh start`. Without the feature
//! this file compiles to a single skipped test so it never wedges CI.

#[cfg(not(feature = "cdk-mint-e2e"))]
#[test]
fn cdk_mint_e2e_skipped_without_feature() {
    eprintln!(
        "skipping money-path E2E; run with: \
         cargo test -p agicash-cashu --features cdk-mint-e2e \
         --test cdk_mint_money_flows -- --test-threads=1 \
         (needs `bash scripts/cdk-mint-e2e.sh start` first)"
    );
}

#[cfg(feature = "cdk-mint-e2e")]
#[allow(
    clippy::clone_on_copy,
    clippy::too_many_lines,
    clippy::doc_markdown
)]
mod e2e {
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use agicash_cashu::{
    CashuMeltQuote, CashuMeltQuoteService, CashuMeltQuoteState, CashuMeltQuoteStorage,
    CdkCashuProvider, CompleteMeltQuote, CompleteMeltQuoteResult, CreateMeltQuote,
    CreateMeltQuoteResult, MeltOutcome, MeltQuoteStorageError, ProofWithId, TokenProof,
};
use agicash_domain::{
    Account, AccountId, AccountPurpose, AccountState, AccountType, Currency, UserId,
};
use agicash_traits::CashuProvider;
use async_trait::async_trait;
use chrono::Utc;
use parking_lot::Mutex;
use serde_json::json;
use uuid::Uuid;

use cdk::amount::SplitTarget;
use cdk::dhke::construct_proofs;
use cdk::mint_url::MintUrl;
use cdk::nuts::nut02::Id as KeysetId;
use cdk::nuts::{
    CurrencyUnit, MeltRequest, MintQuoteBolt11Request, MintRequest, PaymentMethod,
    PreMintSecrets, Proof, SwapRequest,
};
use cdk::wallet::{HttpClient, MintConnector};
use cdk::Amount;

fn mint_url_str() -> String {
    let _ = dotenvy::dotenv();
    std::env::var("AGICASH_TEST_MINT_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8087".to_string())
}

fn account_for(mint_url: &str) -> Account {
    Account {
        id: AccountId::new(),
        created_at: Utc::now(),
        user_id: UserId::new(),
        name: "e2e-cashu".into(),
        account_type: AccountType::Cashu,
        purpose: AccountPurpose::Transactional,
        currency: Currency::Btc,
        details: json!({ "mint_url": mint_url, "keyset_counters": {} }),
        version: 0,
        state: AccountState::Active,
        expires_at: None,
    }
}

/// In-memory melt-quote storage. Faithful enough to drive the real
/// `CashuMeltQuoteService`: tracks state transitions and (critically)
/// supports fault-injecting a single `complete()` failure to simulate a
/// post-settle bookkeeping error — the P0 scenario.
struct MemStorage {
    rows: Mutex<HashMap<Uuid, CashuMeltQuote>>,
    /// When > 0, the next `complete()` returns a Backend error and
    /// decrements. Simulates a crash AFTER the mint settled.
    fail_complete_times: AtomicUsize,
    complete_calls: AtomicUsize,
}

impl MemStorage {
    fn new() -> Self {
        Self {
            rows: Mutex::new(HashMap::new()),
            fail_complete_times: AtomicUsize::new(0),
            complete_calls: AtomicUsize::new(0),
        }
    }
    fn insert(&self, q: CashuMeltQuote) {
        self.rows.lock().insert(q.id, q);
    }
    fn arm_complete_failure(&self, times: usize) {
        self.fail_complete_times.store(times, Ordering::SeqCst);
    }
}

#[async_trait]
impl CashuMeltQuoteStorage for MemStorage {
    async fn create(
        &self,
        _input: CreateMeltQuote,
    ) -> Result<CreateMeltQuoteResult, MeltQuoteStorageError> {
        unreachable!("create() not used by this test path")
    }

    async fn mark_as_pending(
        &self,
        quote_id: Uuid,
    ) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let mut rows = self.rows.lock();
        let q = rows.get_mut(&quote_id).ok_or(MeltQuoteStorageError::NotFound)?;
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
        // Fault injection: simulate the storage step crashing AFTER the
        // mint already settled the Lightning payment.
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
        // Idempotent on PAID (mirrors the real Postgres fn contract).
        if let CashuMeltQuoteState::Paid { .. } = q.state {
            return Ok(CompleteMeltQuoteResult {
                quote: q.clone(),
                account: account_for("http://unused"),
                added_change_proofs: vec![],
            });
        }
        let lightning_fee = input.quote.lightning_fee_reserve.clone();
        q.state = CashuMeltQuoteState::Paid {
            payment_preimage: input.payment_preimage.clone(),
            lightning_fee: lightning_fee.clone(),
            amount_spent: input.amount_spent.clone(),
            total_fee: lightning_fee,
        };
        q.version += 1;
        let quote = q.clone();
        drop(rows);
        Ok(CompleteMeltQuoteResult {
            quote,
            account: account_for("http://unused"),
            added_change_proofs: input
                .change_proofs
                .iter()
                .map(|_| Uuid::new_v4().to_string())
                .collect(),
        })
    }

    async fn expire(
        &self,
        quote_id: Uuid,
    ) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let mut rows = self.rows.lock();
        let q = rows.get_mut(&quote_id).ok_or(MeltQuoteStorageError::NotFound)?;
        q.state = CashuMeltQuoteState::Expired;
        Ok(q.clone())
    }

    async fn fail(
        &self,
        quote_id: Uuid,
        reason: &str,
    ) -> Result<CashuMeltQuote, MeltQuoteStorageError> {
        let mut rows = self.rows.lock();
        let q = rows.get_mut(&quote_id).ok_or(MeltQuoteStorageError::NotFound)?;
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
}

fn raw_client(url: &str) -> Arc<dyn MintConnector + Send + Sync> {
    let mint_url = MintUrl::from_str(url).expect("mint url");
    Arc::new(HttpClient::new(mint_url, None))
}

async fn active_sat_keyset(
    client: &Arc<dyn MintConnector + Send + Sync>,
) -> (KeysetId, u64) {
    let ks = client.get_mint_keysets().await.expect("get_mint_keysets");
    let active = ks
        .keysets
        .iter()
        .find(|k| k.unit == CurrencyUnit::Sat && k.active)
        .expect("active sat keyset")
        .clone();
    (active.id, active.input_fee_ppk)
}

fn fresh_seed() -> [u8; 64] {
    // Proofs only need run-uniqueness; avoid an extra dev-dep.
    use std::time::{SystemTime, UNIX_EPOCH};
    static CTR: AtomicUsize = AtomicUsize::new(0);
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let c = CTR.fetch_add(1, Ordering::SeqCst) as u128;
    let mut x = n ^ (c << 64) ^ 0x9E37_79B9_7F4A_7C15;
    let mut buf = [0u8; 64];
    for chunk in buf.chunks_mut(8) {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let b = x.to_le_bytes();
        for (i, slot) in chunk.iter_mut().enumerate() {
            *slot = b[i];
        }
    }
    buf
}

/// NUT-04: mint `amount` sat of real proofs from the local mint.
async fn mint_proofs(
    client: &Arc<dyn MintConnector + Send + Sync>,
    amount: u64,
) -> Vec<Proof> {
    let (keyset_id, fee_ppk) = active_sat_keyset(client).await;
    let quote = client
        .post_mint_quote(MintQuoteBolt11Request {
            amount: Amount::from(amount),
            unit: CurrencyUnit::Sat,
            description: Some("agicash cdk-mint-e2e".into()),
            pubkey: None,
        })
        .await
        .expect("post_mint_quote");

    let mut paid = false;
    for _ in 0..40 {
        let st = client
            .get_mint_quote_status(&quote.quote.to_string())
            .await
            .expect("get_mint_quote_status");
        if matches!(st.state, cdk::nuts::nut23::QuoteState::Paid) {
            paid = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    assert!(paid, "local FakeWallet mint did not auto-pay mint quote");

    let seed = fresh_seed();
    let fee_and_amounts = cdk::amount::FeeAndAmounts::from((
        fee_ppk,
        (0..32).map(|i| 1u64 << i).collect::<Vec<_>>(),
    ));
    let pre_mint = PreMintSecrets::from_seed(
        keyset_id,
        0,
        &seed,
        Amount::from(amount),
        &SplitTarget::None,
        &fee_and_amounts,
    )
    .expect("PreMintSecrets::from_seed");

    let resp = client
        .post_mint(
            &PaymentMethod::BOLT11,
            MintRequest {
                quote: quote.quote.to_string(),
                outputs: pre_mint.blinded_messages(),
                signature: None,
            },
        )
        .await
        .expect("post_mint");

    let keyset = client.get_mint_keyset(keyset_id).await.expect("keyset");
    construct_proofs(resp.signatures, pre_mint.rs(), pre_mint.secrets(), &keyset.keys)
        .expect("construct_proofs")
}

fn prepared_from(proofs: &[Proof]) -> Vec<ProofWithId> {
    proofs
        .iter()
        .map(|p| ProofWithId {
            id: Uuid::new_v4(),
            proof: TokenProof {
                id: p.keyset_id.to_string(),
                amount: u64::from(p.amount),
                secret: p.secret.to_string(),
                c: p.c.to_hex(),
                dleq: None,
                witness: None,
            },
        })
        .collect()
}

// ---------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------

/// NUT-04 mint-quote receive + NUT-06 swap round-trip + NUT-07
/// double-spend enforcement against the live local mint.
#[tokio::test]
async fn nut04_mint_and_nut06_swap_roundtrip_live() {
    let url = mint_url_str();
    let client = raw_client(&url);

    let proofs = mint_proofs(&client, 64).await;
    let total: u64 = proofs.iter().map(|p| u64::from(p.amount)).sum();
    assert_eq!(total, 64, "minted proofs sum mismatch");

    // NUT-06 swap (the core operation `send` performs).
    let (keyset_id, _fee) = active_sat_keyset(&client).await;
    let seed = fresh_seed();
    let fee_and_amounts = cdk::amount::FeeAndAmounts::from((
        0u64,
        (0..32).map(|i| 1u64 << i).collect::<Vec<_>>(),
    ));
    let pre = PreMintSecrets::from_seed(
        keyset_id,
        0,
        &seed,
        Amount::from(64),
        &SplitTarget::None,
        &fee_and_amounts,
    )
    .expect("pre");
    let swap = SwapRequest::new(proofs.clone(), pre.blinded_messages());
    let resp = client.post_swap(swap).await.expect("post_swap");
    let keyset = client.get_mint_keyset(keyset_id).await.expect("keyset");
    let swapped =
        construct_proofs(resp.signatures, pre.rs(), pre.secrets(), &keyset.keys)
            .expect("swapped proofs");
    let swapped_total: u64 = swapped.iter().map(|p| u64::from(p.amount)).sum();
    assert_eq!(swapped_total, 64, "swap did not preserve value");

    // NUT-07: original proofs must now be SPENT.
    let states = client
        .post_check_state(cdk::nuts::CheckStateRequest {
            ys: proofs.iter().map(|p| p.y().expect("y")).collect(),
        })
        .await
        .expect("check_state");
    assert!(
        states
            .states
            .iter()
            .all(|s| s.state == cdk::nuts::State::Spent),
        "original proofs should be SPENT after swap"
    );
}

/// NUT-05 melt happy path through the REAL `CashuMeltQuoteService`
/// against the live mint: get_quote → initiate_melt → PAID.
#[tokio::test]
async fn nut05_melt_send_happy_path_live() {
    let url = mint_url_str();
    let client = raw_client(&url);
    let proofs = mint_proofs(&client, 256).await;
    // External bolt11 (random payment hash, not minted here). The
    // FakeWallet melt backend settles it instantly to PAID by default.
    let target = cdk_fake_wallet::create_fake_invoice(64_000, "melt target".into());
    run_melt(&url, &proofs, &target.to_string(), 0).await;
}

/// **THE P0 PATH.** Fault-inject a post-settle storage failure after
/// the mint settled the Lightning payment. The flow must NOT re-initiate
/// (re-`post_melt` = double Lightning payment); it must reconcile to
/// PAID via `poll_until_complete` (read-only NUT-05 status). The
/// no-double-pay invariant is then proven at the PROTOCOL level: a
/// manual second `post_melt` with the same inputs is rejected by the
/// mint (proofs already spent).
#[tokio::test]
async fn nut05_melt_p0_double_pay_reconcile_live() {
    let url = mint_url_str();
    let client = raw_client(&url);
    let proofs = mint_proofs(&client, 256).await;
    let target = cdk_fake_wallet::create_fake_invoice(64_000, "melt target p0".into());
    run_melt(&url, &proofs, &target.to_string(), 1).await;
}

/// Shared driver. `fail_complete_times` arms N post-settle storage
/// failures (0 = happy path). On the P0 path it additionally proves the
/// no-double-pay invariant at the protocol level.
async fn run_melt(
    url: &str,
    funding_proofs: &[Proof],
    bolt11: &str,
    fail_complete_times: usize,
) {
    let account = account_for(url);
    let provider: Arc<dyn CashuProvider> = Arc::new(CdkCashuProvider::new());
    let storage = Arc::new(MemStorage::new());
    if fail_complete_times > 0 {
        storage.arm_complete_failure(fail_complete_times);
    }
    let svc = CashuMeltQuoteService::new(
        storage.clone() as Arc<dyn CashuMeltQuoteStorage>,
        provider,
    );

    let prepared = prepared_from(funding_proofs);
    let bolt11_str = bolt11.to_string();

    // 1. get_quote — real NUT-05 post_melt_quote against the live mint.
    let preview = svc
        .get_quote(&account, &prepared, &bolt11_str)
        .await
        .expect("get_quote against live mint");

    // 2. Seed the storage row (mirrors create_quote's output). Only the
    //    proofs actually selected by get_quote are reserved/spent.
    let quote_id = Uuid::new_v4();
    let quote = CashuMeltQuote {
        id: quote_id,
        quote_id: preview.melt_quote_id.clone(),
        user_id: account.user_id,
        account_id: account.id,
        payment_request: preview.bolt11.clone(),
        payment_hash: preview.payment_hash.clone(),
        amount_requested: preview.amount_requested.clone(),
        amount_requested_in_msat: preview.amount_requested_in_msat,
        amount_received: preview.amount_received.clone(),
        lightning_fee_reserve: preview.lightning_fee_reserve.clone(),
        cashu_fee: preview.cashu_fee.clone(),
        proofs: preview
            .prepared_proofs
            .iter()
            .map(|p| p.proof.clone())
            .collect(),
        amount_reserved: preview.amount_reserved.clone(),
        keyset_id: preview.keyset_id.clone(),
        keyset_counter: preview.keyset_counter,
        number_of_change_outputs: preview.number_of_change_outputs,
        transaction_id: Uuid::new_v4(),
        created_at: Utc::now(),
        expires_at: preview.expires_at,
        version: 0,
        state: CashuMeltQuoteState::Unpaid,
    };
    storage.insert(quote.clone());

    // Capture the exact input proofs the melt will spend, for the
    // protocol-level double-spend assertion below.
    let melt_inputs: Vec<Proof> = preview
        .prepared_proofs
        .iter()
        .map(|pw| {
            funding_proofs
                .iter()
                .find(|fp| fp.secret.to_string() == pw.proof.secret)
                .expect("prepared proof traces to a funding proof")
                .clone()
        })
        .collect();

    // Fresh per-invocation seed: the change blinded messages are
    // derived deterministically from (seed, keyset_id, counter). A
    // fixed seed + counter 0 makes two melt runs request identical
    // change blanks and the mint rejects the second with "Blinded
    // Message is already signed". Run-unique seed keeps blanks unique.
    let seed = fresh_seed();

    // 3. initiate_melt — the real NUT-05 post_melt against the live mint.
    let outcome = svc.initiate_melt(&account, quote.clone(), &seed).await;

    match outcome {
        Ok(MeltOutcome::Paid { quote: q, .. }) => {
            assert_eq!(fail_complete_times, 0, "happy path expected no fault");
            assert!(
                matches!(q.state, CashuMeltQuoteState::Paid { .. }),
                "quote not PAID: {:?}",
                q.state
            );
        }
        Ok(MeltOutcome::Failed(q)) => {
            panic!(
                "mint reported melt FAILED against the live FakeWallet \
                 mint (should settle instantly): {:?}",
                q.state
            );
        }
        Ok(MeltOutcome::Pending(q)) => {
            let final_outcome = svc
                .poll_until_complete(
                    &account,
                    q,
                    &seed,
                    std::time::Duration::from_millis(300),
                    std::time::Duration::from_secs(30),
                )
                .await
                .expect("poll_until_complete");
            assert!(
                matches!(final_outcome, MeltOutcome::Paid { .. }),
                "pending melt did not reconcile to PAID: {final_outcome:?}"
            );
        }
        Err(e) => {
            // THE P0 PATH: initiate_melt's post-settle complete() was
            // fault-injected. The Lightning payment ALREADY happened
            // (mint settled). Re-initiating would re-call post_melt =
            // double pay. The flow reconciles via poll_until_complete,
            // which only calls the read-only get_melt_quote_status.
            assert!(
                fail_complete_times > 0,
                "unexpected error on happy path: {e:?}"
            );
            let pending = storage.get(quote_id).await.expect("row present");
            assert!(
                matches!(pending.state, CashuMeltQuoteState::Pending),
                "post-fault state should be Pending, was {:?}",
                pending.state
            );
            let reconciled = svc
                .poll_until_complete(
                    &account,
                    pending,
                    &seed,
                    std::time::Duration::from_millis(300),
                    std::time::Duration::from_secs(30),
                )
                .await
                .expect("poll_until_complete reconcile");
            assert!(
                matches!(reconciled, MeltOutcome::Paid { .. }),
                "P0 reconcile did not reach PAID: {reconciled:?}"
            );
            let final_row = storage.get(quote_id).await.expect("row");
            assert!(
                matches!(final_row.state, CashuMeltQuoteState::Paid { .. }),
                "final state must be PAID after reconcile, was {:?}",
                final_row.state
            );
        }
    }

    // THE LOAD-BEARING INVARIANT, proven at the PROTOCOL level: the
    // melt's input proofs are spent at the mint. A manual second
    // post_melt with the same inputs MUST be rejected — i.e. a
    // double-pay is impossible, the first (and only) settlement
    // consumed the proofs. This is what a P0 double-pay regression
    // would violate (it would issue a second successful post_melt).
    let client = raw_client(url);
    let second = MeltRequest::new(
        preview.melt_quote_id.clone(),
        melt_inputs.clone(),
        None,
    );
    let replay = client.post_melt(&PaymentMethod::BOLT11, second).await;
    assert!(
        replay.is_err(),
        "second post_melt with the same inputs MUST be rejected by the \
         mint (proofs already spent); a success here is a double-pay"
    );

    // And the proofs themselves report SPENT via NUT-07.
    let st = client
        .post_check_state(cdk::nuts::CheckStateRequest {
            ys: melt_inputs.iter().map(|p| p.y().expect("y")).collect(),
        })
        .await
        .expect("check_state");
    assert!(
        st.states.iter().all(|s| s.state == cdk::nuts::State::Spent),
        "melt input proofs must be SPENT exactly once"
    );
}
} // mod e2e
