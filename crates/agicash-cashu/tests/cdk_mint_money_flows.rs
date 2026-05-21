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
#[allow(clippy::clone_on_copy, clippy::too_many_lines, clippy::doc_markdown)]
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
    ///
    /// `create()` is a *real* in-memory insert (not `unreachable!()`) so the
    /// production `create_quote` path — including Lane T's
    /// `find_active_by_payment_hash` pre-check and the
    /// `DuplicatePayment` (DB partial-unique-index) race-safe backstop — is
    /// genuinely exercised end-to-end. The backstop is modelled in-memory
    /// because this gate has no Postgres: on `create()` we re-check for an
    /// active (UNPAID/PENDING/PAID) row with the same `(user_id,
    /// payment_hash)` and return [`MeltQuoteStorageError::DuplicatePayment`],
    /// exactly as the `cashu_send_quotes_payment_hash_active_unique` partial
    /// unique index does via a 23505 → `DuplicatePayment` in
    /// `agicash-storage-supabase`.
    struct MemStorage {
        rows: Mutex<HashMap<Uuid, CashuMeltQuote>>,
        /// When > 0, the next `complete()` returns a Backend error and
        /// decrements. Simulates a crash AFTER the mint settled.
        fail_complete_times: AtomicUsize,
        complete_calls: AtomicUsize,
        /// Number of times `create()` actually inserted a fresh row. The
        /// `find_active_by_payment_hash` pre-check must keep this at 1 for a
        /// re-quoted invoice (a 2nd insert is what would drive a 2nd
        /// `post_melt` = double-pay).
        create_inserts: AtomicUsize,
    }

    impl MemStorage {
        fn new() -> Self {
            Self {
                rows: Mutex::new(HashMap::new()),
                fail_complete_times: AtomicUsize::new(0),
                complete_calls: AtomicUsize::new(0),
                create_inserts: AtomicUsize::new(0),
            }
        }
        fn insert(&self, q: CashuMeltQuote) {
            self.rows.lock().insert(q.id, q);
        }
        fn arm_complete_failure(&self, times: usize) {
            self.fail_complete_times.store(times, Ordering::SeqCst);
        }
        fn create_insert_count(&self) -> usize {
            self.create_inserts.load(Ordering::SeqCst)
        }
    }

    /// True for the active states the partial unique index
    /// `cashu_send_quotes_payment_hash_active_unique` covers
    /// (UNPAID/PENDING/PAID). FAILED/EXPIRED are excluded so a genuinely
    /// failed/expired attempt stays legitimately retryable — identical
    /// predicate to Lane T's migration and the supabase
    /// `find_active_by_payment_hash`.
    fn is_active_state(state: &CashuMeltQuoteState) -> bool {
        matches!(
            state,
            CashuMeltQuoteState::Unpaid
                | CashuMeltQuoteState::Pending
                | CashuMeltQuoteState::Paid { .. }
        )
    }

    #[async_trait]
    impl CashuMeltQuoteStorage for MemStorage {
        async fn create(
            &self,
            input: CreateMeltQuote,
        ) -> Result<CreateMeltQuoteResult, MeltQuoteStorageError> {
            // Race-safe backstop = the partial unique index
            // `cashu_send_quotes_payment_hash_active_unique`. Even if the
            // `create_quote` pre-check missed an active row (TOCTOU), the
            // DB rejects a 2nd active (UNPAID/PENDING/PAID) row for the same
            // `(user_id, payment_hash)` with a 23505 that
            // `agicash-storage-supabase` maps to
            // `MeltQuoteStorageError::DuplicatePayment`. Model that here so
            // the typed-error path is exercised without Postgres.
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
                // The real DB fn re-derives the authoritative pre-bump
                // counter; the preview's pass-through value is fine here
                // because the change blanks are rebuilt from (seed,
                // keyset_id, counter) and the test uses counter 0.
                keyset_counter: 0,
                number_of_change_outputs: input.number_of_change_outputs,
                transaction_id: Uuid::new_v4(),
                created_at: Utc::now(),
                expires_at: input.expires_at,
                version: 0,
                state: CashuMeltQuoteState::Unpaid,
            };
            self.rows.lock().insert(quote_id, quote.clone());
            self.create_inserts.fetch_add(1, Ordering::SeqCst);
            Ok(CreateMeltQuoteResult {
                quote,
                account: account_for("http://unused"),
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

        /// Real in-memory mirror of the supabase
        /// `SupabaseCashuMeltQuoteStorage::find_active_by_payment_hash`
        /// contract: return the stored quote for this `(user_id,
        /// payment_hash)` whose state is active (UNPAID/PENDING/PAID), else
        /// `None`. FAILED/EXPIRED rows are intentionally NOT matched so a
        /// genuinely failed/expired attempt is still retryable — identical
        /// to the supabase `.in_("state", ["UNPAID","PENDING","PAID"])`
        /// filter and Lane T's migration predicate. This is the
        /// defense-in-depth pre-check `create_quote` calls before issuing a
        /// 2nd `post_melt`.
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
                    q.user_id == user_id
                        && q.payment_hash == payment_hash
                        && is_active_state(&q.state)
                })
                .cloned())
        }

        async fn list_unresolved_for_user(
            &self,
            user_id: UserId,
        ) -> Result<Vec<CashuMeltQuote>, MeltQuoteStorageError> {
            Ok(self
                .rows
                .lock()
                .values()
                .filter(|q| {
                    q.user_id == user_id
                        && matches!(
                            q.state,
                            CashuMeltQuoteState::Unpaid | CashuMeltQuoteState::Pending
                        )
                })
                .cloned()
                .collect())
        }
    }

    fn raw_client(url: &str) -> Arc<dyn MintConnector + Send + Sync> {
        let mint_url = MintUrl::from_str(url).expect("mint url");
        Arc::new(HttpClient::new(mint_url, None))
    }

    async fn active_sat_keyset(client: &Arc<dyn MintConnector + Send + Sync>) -> (KeysetId, u64) {
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
    async fn mint_proofs(client: &Arc<dyn MintConnector + Send + Sync>, amount: u64) -> Vec<Proof> {
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
        construct_proofs(
            resp.signatures,
            pre_mint.rs(),
            pre_mint.secrets(),
            &keyset.keys,
        )
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
        let swapped = construct_proofs(resp.signatures, pre.rs(), pre.secrets(), &keyset.keys)
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
        let svc =
            CashuMeltQuoteService::new(storage.clone() as Arc<dyn CashuMeltQuoteStorage>, provider);

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
        let second = MeltRequest::new(preview.melt_quote_id.clone(), melt_inputs.clone(), None);
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

    // =====================================================================
    // Lane T proof harness: fault-injecting MintConnector + provider.
    // =====================================================================
    //
    // `MemStorage` + `CdkCashuProvider` drive the *unfaulted* path. To prove
    // Lane T Fix 1 (ambiguous post_melt => stay PENDING, not FAILED) we need
    // to inject a fault on the `post_melt` *response* while letting the mint
    // actually settle the invoice — exactly the real-world hazard (the mint
    // pays; our HTTP read of the answer is lost). `FaultConnector` wraps the
    // real CDK `HttpClient`, delegates every NUT call to it, and rewrites
    // only `post_melt` per the configured `MeltFault`.

    // NOTE: `MeltRequest`, `MintQuoteBolt11Request`, `MintRequest`,
    // `PaymentMethod`, `SwapRequest`, `MintConnector`, `MintUrl` and the
    // `KeysetId` alias are already imported by the top-of-module `use`
    // blocks (this whole `mod e2e` shares one namespace) — do NOT re-import
    // them here or rustc errors with E0252.
    use cdk::nuts::{
        CheckStateRequest, CheckStateResponse, KeySet, KeysetResponse, MeltQuoteBolt11Request,
        MeltQuoteBolt11Response, MeltQuoteBolt12Request, MeltQuoteBolt12Response,
        MeltQuoteCustomRequest, MeltQuoteCustomResponse, MintInfo, MintQuoteBolt11Response,
        MintQuoteBolt12Request, MintQuoteBolt12Response, MintQuoteCustomRequest,
        MintQuoteCustomResponse, MintResponse, RestoreRequest, RestoreResponse, SwapResponse,
    };
    use cdk::wallet::{AuthWallet, LnurlPayInvoiceResponse, LnurlPayResponse};

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum MeltFault {
        /// Forward `post_melt` to the real mint (it settles the invoice),
        /// then DROP the success response and return an *ambiguous* network
        /// error (`HttpError(None, _)` — `is_definitive_failure() == false`).
        /// Models "the mint paid but our read of the answer was lost". Lane T
        /// Fix 1 MUST keep the quote PENDING (proofs RESERVED) here.
        AmbiguousAfterSettle,
        /// Return a *definitive* mint rejection (`HttpError(Some(400), _)` —
        /// `is_definitive_failure() == true`) WITHOUT forwarding, so the mint
        /// never sees the request and provably does not pay. Lane T leaves
        /// this branch unchanged: FAIL + release proofs.
        DefinitiveBeforeSettle,
    }

    #[derive(Debug)]
    struct FaultConnector {
        inner: Arc<dyn MintConnector + Send + Sync>,
        fault: parking_lot::Mutex<Option<MeltFault>>,
        /// `post_melt` calls that actually reached the real mint.
        real_post_melt_calls: AtomicUsize,
    }

    impl FaultConnector {
        fn new(inner: Arc<dyn MintConnector + Send + Sync>) -> Self {
            Self {
                inner,
                fault: parking_lot::Mutex::new(None),
                real_post_melt_calls: AtomicUsize::new(0),
            }
        }
        fn arm(&self, f: MeltFault) {
            *self.fault.lock() = Some(f);
        }
        /// Disarm so subsequent post_melt calls behave normally.
        fn disarm(&self) {
            *self.fault.lock() = None;
        }
        fn real_post_melt_count(&self) -> usize {
            self.real_post_melt_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl MintConnector for FaultConnector {
        async fn fetch_lnurl_pay_request(&self, url: &str) -> Result<LnurlPayResponse, cdk::Error> {
            self.inner.fetch_lnurl_pay_request(url).await
        }
        async fn fetch_lnurl_invoice(
            &self,
            url: &str,
        ) -> Result<LnurlPayInvoiceResponse, cdk::Error> {
            self.inner.fetch_lnurl_invoice(url).await
        }
        async fn get_mint_keys(&self) -> Result<Vec<KeySet>, cdk::Error> {
            self.inner.get_mint_keys().await
        }
        async fn get_mint_keyset(&self, keyset_id: KeysetId) -> Result<KeySet, cdk::Error> {
            self.inner.get_mint_keyset(keyset_id).await
        }
        async fn get_mint_keysets(&self) -> Result<KeysetResponse, cdk::Error> {
            self.inner.get_mint_keysets().await
        }
        async fn post_mint_quote(
            &self,
            request: MintQuoteBolt11Request,
        ) -> Result<MintQuoteBolt11Response<String>, cdk::Error> {
            self.inner.post_mint_quote(request).await
        }
        async fn get_mint_quote_status(
            &self,
            quote_id: &str,
        ) -> Result<MintQuoteBolt11Response<String>, cdk::Error> {
            self.inner.get_mint_quote_status(quote_id).await
        }
        async fn post_mint(
            &self,
            method: &PaymentMethod,
            request: MintRequest<String>,
        ) -> Result<MintResponse, cdk::Error> {
            self.inner.post_mint(method, request).await
        }
        async fn post_melt_quote(
            &self,
            request: MeltQuoteBolt11Request,
        ) -> Result<MeltQuoteBolt11Response<String>, cdk::Error> {
            self.inner.post_melt_quote(request).await
        }
        async fn get_melt_quote_status(
            &self,
            quote_id: &str,
        ) -> Result<MeltQuoteBolt11Response<String>, cdk::Error> {
            self.inner.get_melt_quote_status(quote_id).await
        }
        async fn post_melt(
            &self,
            method: &PaymentMethod,
            request: MeltRequest<String>,
        ) -> Result<MeltQuoteBolt11Response<String>, cdk::Error> {
            let fault = *self.fault.lock();
            match fault {
                None => {
                    self.real_post_melt_calls.fetch_add(1, Ordering::SeqCst);
                    self.inner.post_melt(method, request).await
                }
                Some(MeltFault::AmbiguousAfterSettle) => {
                    // Let the mint ACTUALLY settle (real Lightning payment
                    // via FakeWallet) — then drop the answer and surface an
                    // ambiguous network error, exactly as a lost HTTP read
                    // would. The melt DID happen at the mint.
                    self.real_post_melt_calls.fetch_add(1, Ordering::SeqCst);
                    let _settled = self.inner.post_melt(method, request).await;
                    // `HttpError(None, _)` => is_definitive_failure() == false
                    // (network error, mint state unknown). Lane T Fix 1 MUST
                    // keep the quote PENDING here, NOT FAIL+release.
                    Err(cdk::Error::HttpError(
                        None,
                        "injected: post_melt response lost (mint already settled)".into(),
                    ))
                }
                Some(MeltFault::DefinitiveBeforeSettle) => {
                    // Do NOT forward: the mint never sees the request and
                    // provably does not pay. `HttpError(Some(400), _)` =>
                    // is_definitive_failure() == true. Lane T leaves this
                    // unchanged: FAIL + release the reserved proofs.
                    Err(cdk::Error::HttpError(
                        Some(400),
                        "injected: mint definitively rejected (no payment)".into(),
                    ))
                }
            }
        }
        async fn post_swap(&self, request: SwapRequest) -> Result<SwapResponse, cdk::Error> {
            self.inner.post_swap(request).await
        }
        async fn get_mint_info(&self) -> Result<MintInfo, cdk::Error> {
            self.inner.get_mint_info().await
        }
        async fn post_check_state(
            &self,
            request: CheckStateRequest,
        ) -> Result<CheckStateResponse, cdk::Error> {
            self.inner.post_check_state(request).await
        }
        async fn post_restore(
            &self,
            request: RestoreRequest,
        ) -> Result<RestoreResponse, cdk::Error> {
            self.inner.post_restore(request).await
        }
        async fn get_auth_wallet(&self) -> Option<AuthWallet> {
            self.inner.get_auth_wallet().await
        }
        async fn set_auth_wallet(&self, wallet: Option<AuthWallet>) {
            self.inner.set_auth_wallet(wallet).await;
        }
        async fn post_mint_bolt12_quote(
            &self,
            request: MintQuoteBolt12Request,
        ) -> Result<MintQuoteBolt12Response<String>, cdk::Error> {
            self.inner.post_mint_bolt12_quote(request).await
        }
        async fn get_mint_quote_bolt12_status(
            &self,
            quote_id: &str,
        ) -> Result<MintQuoteBolt12Response<String>, cdk::Error> {
            self.inner.get_mint_quote_bolt12_status(quote_id).await
        }
        async fn post_melt_bolt12_quote(
            &self,
            request: MeltQuoteBolt12Request,
        ) -> Result<MeltQuoteBolt12Response<String>, cdk::Error> {
            self.inner.post_melt_bolt12_quote(request).await
        }
        async fn get_melt_bolt12_quote_status(
            &self,
            quote_id: &str,
        ) -> Result<MeltQuoteBolt12Response<String>, cdk::Error> {
            self.inner.get_melt_bolt12_quote_status(quote_id).await
        }
        async fn post_mint_custom_quote(
            &self,
            method: &PaymentMethod,
            request: MintQuoteCustomRequest,
        ) -> Result<MintQuoteCustomResponse<String>, cdk::Error> {
            self.inner.post_mint_custom_quote(method, request).await
        }
        async fn get_mint_quote_custom_status(
            &self,
            method: &str,
            quote_id: &str,
        ) -> Result<MintQuoteCustomResponse<String>, cdk::Error> {
            self.inner
                .get_mint_quote_custom_status(method, quote_id)
                .await
        }
        async fn post_melt_custom_quote(
            &self,
            request: MeltQuoteCustomRequest,
        ) -> Result<MeltQuoteCustomResponse<String>, cdk::Error> {
            self.inner.post_melt_custom_quote(request).await
        }
        async fn get_melt_quote_custom_status(
            &self,
            method: &str,
            quote_id: &str,
        ) -> Result<MeltQuoteCustomResponse<String>, cdk::Error> {
            self.inner
                .get_melt_quote_custom_status(method, quote_id)
                .await
        }
    }

    /// A `CashuProvider` that hands the service a wallet backed by a shared
    /// `FaultConnector` (so `arm()`/counters survive across calls). The mint
    /// URL still comes from `account.details["mint_url"]`, so every
    /// delegated NUT call hits the real local mint.
    struct FaultProvider {
        connector: Arc<FaultConnector>,
    }

    impl std::fmt::Debug for FaultProvider {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FaultProvider").finish_non_exhaustive()
        }
    }

    #[async_trait]
    impl CashuProvider for FaultProvider {
        async fn wallet_for_account(
            &self,
            account: &Account,
        ) -> Result<Arc<agicash_traits::CashuMintWallet>, agicash_traits::CashuProviderError>
        {
            let url = account
                .details
                .get("mint_url")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    agicash_traits::CashuProviderError::InvalidUrl("missing mint_url".into())
                })?;
            let mint_url = MintUrl::from_str(url)
                .map_err(|e| agicash_traits::CashuProviderError::InvalidUrl(e.to_string()))?;
            let connector: Arc<dyn MintConnector + Send + Sync> = self.connector.clone();
            Ok(Arc::new(agicash_traits::CashuMintWallet::new(
                connector, mint_url,
            )))
        }

        async fn mint_info(
            &self,
            _mint_url: &MintUrl,
        ) -> Result<MintInfo, agicash_traits::CashuProviderError> {
            self.connector
                .get_mint_info()
                .await
                .map_err(|e| agicash_traits::CashuProviderError::Network(e.to_string()))
        }
    }

    /// Build the quote row that `create_quote`/`MemStorage::create` would
    /// have produced for a freshly fetched preview, then insert it. Mirrors
    /// the seeding logic in `run_melt`.
    fn seed_row_from_preview(
        storage: &MemStorage,
        account: &Account,
        preview: &agicash_cashu::MeltQuotePreview,
    ) -> Uuid {
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
        storage.insert(quote);
        quote_id
    }

    // ---------------------------------------------------------------------
    // Lane T Fix 2 — payment_hash idempotency (live mint).
    // ---------------------------------------------------------------------

    /// Re-quoting an invoice while a PENDING quote is active must
    /// re-attach to the existing quote (the `find_active_by_payment_hash`
    /// pre-check), NOT issue a 2nd `post_melt`. Then drive the single quote
    /// to PAID and prove at the protocol level the input proofs were spent
    /// exactly once (no double-pay).
    #[tokio::test]
    async fn nut05_fix2_repaste_pending_invoice_reattaches_no_second_post_melt_live() {
        let url = mint_url_str();
        let raw = raw_client(&url);
        let funding = mint_proofs(&raw, 256).await;
        let target = cdk_fake_wallet::create_fake_invoice(64_000, "fix2 pending".into());
        let bolt11 = target.to_string();

        let account = account_for(&url);
        let connector = Arc::new(FaultConnector::new(raw_client(&url)));
        let storage = Arc::new(MemStorage::new());
        let provider: Arc<dyn CashuProvider> = Arc::new(FaultProvider {
            connector: connector.clone(),
        });
        let svc =
            CashuMeltQuoteService::new(storage.clone() as Arc<dyn CashuMeltQuoteStorage>, provider);

        let prepared = prepared_from(&funding);

        // 1st create: get_quote (real post_melt_quote) -> create_quote
        // (no active dup -> real insert).
        let preview1 = svc
            .get_quote(&account, &prepared, &bolt11)
            .await
            .expect("get_quote #1");
        let created1 = svc
            .create_quote(account.user_id, &account, preview1.clone())
            .await
            .expect("create_quote #1");
        assert_eq!(
            storage.create_insert_count(),
            1,
            "first create_quote must insert exactly one row"
        );
        let quote = created1.quote.clone();
        let seed = fresh_seed();
        let melt_inputs: Vec<Proof> = quote
            .proofs
            .iter()
            .map(|tp| {
                funding
                    .iter()
                    .find(|fp| fp.secret.to_string() == tp.secret)
                    .expect("prepared proof traces to a funding proof")
                    .clone()
            })
            .collect();

        // Drive the quote to a GENUINE PENDING state: ambiguous post_melt
        // (mint actually settles the Lightning payment, the response is
        // lost). This is exactly the real in-flight/just-settled window an
        // impatient re-paste hits — the proofs are really spent at the mint
        // and Lane T Fix 1 keeps the quote PENDING (not FAILED).
        connector.arm(MeltFault::AmbiguousAfterSettle);
        let initiated = svc
            .initiate_melt(&account, quote.clone(), &seed)
            .await
            .expect("initiate_melt must not error on ambiguous post_melt");
        assert!(
            matches!(initiated, MeltOutcome::Pending(_)),
            "ambiguous post_melt must leave the quote PENDING: {initiated:?}"
        );
        assert!(
            matches!(
                storage.get(quote.id).await.expect("row").state,
                CashuMeltQuoteState::Pending
            ),
            "quote must be genuinely PENDING (real payment in flight)"
        );
        assert_eq!(
            connector.real_post_melt_count(),
            1,
            "the mint received exactly one post_melt (it settled)"
        );

        // 2nd create for the SAME invoice while the quote is genuinely
        // PENDING (payment already settled at the mint, our row still
        // PENDING). The `find_active_by_payment_hash` pre-check must
        // short-circuit and return the existing quote — NO new row, and
        // critically NO 2nd post_melt (which would be the double-pay).
        let preview2 = svc
            .get_quote(&account, &prepared, &bolt11)
            .await
            .expect("get_quote #2 (re-paste)");
        let created2 = svc
            .create_quote(account.user_id, &account, preview2)
            .await
            .expect("create_quote #2 must re-attach, not error");
        assert_eq!(
            created2.quote.id, quote.id,
            "re-paste must return the EXISTING quote, not a fresh one"
        );
        assert!(
            matches!(created2.quote.state, CashuMeltQuoteState::Pending),
            "re-attached quote must still be PENDING, was {:?}",
            created2.quote.state
        );
        assert_eq!(
            storage.create_insert_count(),
            1,
            "re-paste must NOT insert a 2nd row (a 2nd row drives a 2nd \
         post_melt = double-pay)"
        );
        assert_eq!(
            connector.real_post_melt_count(),
            1,
            "re-paste must NOT issue a 2nd post_melt — still exactly ONE \
         Lightning payment"
        );

        // Disarm and reconcile the single quote via the read-only status
        // poll. The mint already settled -> PAID, with NO 2nd post_melt.
        connector.disarm();
        let pending_quote = storage.get(quote.id).await.expect("row");
        let paid = svc
            .poll_until_complete(
                &account,
                pending_quote,
                &seed,
                std::time::Duration::from_millis(300),
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("poll_until_complete");
        assert!(
            matches!(paid, MeltOutcome::Paid { .. }),
            "single quote must reconcile to PAID: {paid:?}"
        );
        assert_eq!(
            connector.real_post_melt_count(),
            1,
            "exactly ONE Lightning payment for this invoice (no double-pay)"
        );

        // PROTOCOL-level no-double-pay proof: a manual 2nd post_melt with the
        // same inputs is rejected (proofs already spent) and NUT-07 says
        // SPENT.
        let chk = raw_client(&url);
        let replay = chk
            .post_melt(
                &PaymentMethod::BOLT11,
                MeltRequest::new(preview1.melt_quote_id.clone(), melt_inputs.clone(), None),
            )
            .await;
        assert!(
            replay.is_err(),
            "2nd post_melt with same inputs MUST be rejected (would be a \
         double-pay)"
        );
        let st = chk
            .post_check_state(CheckStateRequest {
                ys: melt_inputs.iter().map(|p| p.y().expect("y")).collect(),
            })
            .await
            .expect("check_state");
        assert!(
            st.states.iter().all(|s| s.state == cdk::nuts::State::Spent),
            "input proofs must be SPENT exactly once"
        );
    }

    /// Re-quoting an already-PAID invoice surfaces the existing PAID quote
    /// (the receipt) instead of issuing a second payment.
    #[tokio::test]
    async fn nut05_fix2_repaste_paid_invoice_returns_receipt_no_second_post_melt_live() {
        let url = mint_url_str();
        let raw = raw_client(&url);
        let funding = mint_proofs(&raw, 256).await;
        let target = cdk_fake_wallet::create_fake_invoice(64_000, "fix2 paid".into());
        let bolt11 = target.to_string();

        let account = account_for(&url);
        let connector = Arc::new(FaultConnector::new(raw_client(&url)));
        let storage = Arc::new(MemStorage::new());
        let provider: Arc<dyn CashuProvider> = Arc::new(FaultProvider {
            connector: connector.clone(),
        });
        let svc =
            CashuMeltQuoteService::new(storage.clone() as Arc<dyn CashuMeltQuoteStorage>, provider);

        let prepared = prepared_from(&funding);
        let preview = svc
            .get_quote(&account, &prepared, &bolt11)
            .await
            .expect("get_quote");
        let created = svc
            .create_quote(account.user_id, &account, preview.clone())
            .await
            .expect("create_quote");
        let quote = created.quote.clone();
        assert_eq!(storage.create_insert_count(), 1);

        // Drive the quote to PAID via the real mint.
        let seed = fresh_seed();
        let outcome = svc
            .initiate_melt(&account, quote.clone(), &seed)
            .await
            .expect("initiate_melt");
        let paid_quote = match outcome {
            MeltOutcome::Paid { quote: q, .. } => q,
            MeltOutcome::Pending(q) => {
                match svc
                    .poll_until_complete(
                        &account,
                        q,
                        &seed,
                        std::time::Duration::from_millis(300),
                        std::time::Duration::from_secs(30),
                    )
                    .await
                    .expect("poll")
                {
                    MeltOutcome::Paid { quote: q, .. } => q,
                    other => panic!("did not reach PAID: {other:?}"),
                }
            }
            MeltOutcome::Failed(q) => {
                panic!("unexpected FAILED on the happy path: {:?}", q.state)
            }
        };
        assert!(matches!(paid_quote.state, CashuMeltQuoteState::Paid { .. }));
        let post_melt_after_pay = connector.real_post_melt_count();
        assert_eq!(post_melt_after_pay, 1, "exactly one payment so far");

        // Re-paste the now-PAID invoice. `find_active_by_payment_hash`
        // matches PAID rows too -> return the receipt, no 2nd post_melt.
        let preview_again = svc
            .get_quote(&account, &prepared, &bolt11)
            .await
            .expect("get_quote re-paste");
        let created_again = svc
            .create_quote(account.user_id, &account, preview_again)
            .await
            .expect("create_quote re-paste returns receipt");
        assert_eq!(
            created_again.quote.id, paid_quote.id,
            "re-paste of paid invoice must return the existing PAID quote"
        );
        assert!(
            matches!(created_again.quote.state, CashuMeltQuoteState::Paid { .. }),
            "re-attached quote must be the PAID receipt, was {:?}",
            created_again.quote.state
        );
        assert_eq!(
            storage.create_insert_count(),
            1,
            "re-paste of paid invoice must NOT insert a 2nd row"
        );
        assert_eq!(
            connector.real_post_melt_count(),
            post_melt_after_pay,
            "re-paste of paid invoice must NOT issue another post_melt"
        );
    }

    /// The race-safe DB backstop: if the `find_active_by_payment_hash`
    /// pre-check is bypassed (the TOCTOU window two concurrent
    /// `create_quote` calls hit), the partial unique index still rejects the
    /// 2nd active row as a typed `DuplicatePayment` — modelled in-memory by
    /// `MemStorage::create`. Proves the typed error path, not a raw backend
    /// string.
    #[tokio::test]
    async fn nut05_fix2_db_index_backstop_rejects_concurrent_duplicate_live() {
        let url = mint_url_str();
        let raw = raw_client(&url);
        let funding = mint_proofs(&raw, 256).await;
        let target = cdk_fake_wallet::create_fake_invoice(64_000, "fix2 backstop".into());
        let bolt11 = target.to_string();

        let account = account_for(&url);
        let connector = Arc::new(FaultConnector::new(raw_client(&url)));
        let storage = Arc::new(MemStorage::new());
        let provider: Arc<dyn CashuProvider> = Arc::new(FaultProvider {
            connector: connector.clone(),
        });
        let svc =
            CashuMeltQuoteService::new(storage.clone() as Arc<dyn CashuMeltQuoteStorage>, provider);

        let prepared = prepared_from(&funding);
        // Both racers fetch a real preview for the same invoice.
        let preview_a = svc
            .get_quote(&account, &prepared, &bolt11)
            .await
            .expect("get_quote A");
        let preview_b = svc
            .get_quote(&account, &prepared, &bolt11)
            .await
            .expect("get_quote B");

        // Racer A wins and inserts the row.
        let a = svc
            .create_quote(account.user_id, &account, preview_a)
            .await
            .expect("create_quote A");
        assert_eq!(storage.create_insert_count(), 1);

        // Racer B already passed its (empty-at-the-time) pre-check, so call
        // `storage.create` DIRECTLY — the TOCTOU window the DB index exists
        // to close. It MUST be the typed DuplicatePayment, not a generic
        // backend error and NOT a 2nd inserted row.
        let dup_input = CreateMeltQuote {
            user_id: account.user_id,
            account_id: account.id,
            payment_request: preview_b.bolt11.clone(),
            payment_hash: preview_b.payment_hash.clone(),
            expires_at: preview_b.expires_at,
            quote_id: preview_b.melt_quote_id.clone(),
            amount_requested: preview_b.amount_requested.clone(),
            amount_requested_in_msat: preview_b.amount_requested_in_msat,
            amount_received: preview_b.amount_received.clone(),
            lightning_fee_reserve: preview_b.lightning_fee_reserve.clone(),
            cashu_fee: preview_b.cashu_fee.clone(),
            proofs: preview_b
                .prepared_proofs
                .iter()
                .map(|p| p.proof.clone())
                .collect(),
            proof_ids: preview_b.prepared_proofs.iter().map(|p| p.id).collect(),
            amount_reserved: preview_b.amount_reserved.clone(),
            keyset_id: preview_b.keyset_id.clone(),
            number_of_change_outputs: preview_b.number_of_change_outputs,
        };
        let err = (storage.clone() as Arc<dyn CashuMeltQuoteStorage>)
            .create(dup_input)
            .await
            .expect_err("duplicate active row must be rejected by the index");
        assert!(
            matches!(err, MeltQuoteStorageError::DuplicatePayment),
            "must be the typed DuplicatePayment (23505 partial unique index), \
         was {err:?}"
        );
        assert_eq!(
            storage.create_insert_count(),
            1,
            "the backstop must prevent a 2nd row (the double-pay enabler)"
        );
        let _ = a;
        // No post_melt fired — only get_quote ran on each racer.
        assert_eq!(connector.real_post_melt_count(), 0);
    }

    // ---------------------------------------------------------------------
    // Lane T Fix 1 — ambiguous post_melt stays PENDING (live mint).
    // ---------------------------------------------------------------------

    /// THE Fix-1 PROOF. The mint ACTUALLY settles the invoice but the
    /// `post_melt` response is lost (ambiguous network error). Pre-Lane-T
    /// this marked the quote FAILED and released the RESERVED proofs to
    /// UNSPENT — turning the "failed" UI into a double-pay on retry. Lane T
    /// must instead keep the quote PENDING (proofs RESERVED) and let the
    /// caller reconcile via `get_melt_quote_status` to PAID, with exactly
    /// ONE Lightning payment.
    #[tokio::test]
    async fn nut05_fix1_ambiguous_post_melt_stays_pending_then_reconciles_paid_live() {
        let url = mint_url_str();
        let raw = raw_client(&url);
        let funding = mint_proofs(&raw, 256).await;
        let target = cdk_fake_wallet::create_fake_invoice(64_000, "fix1 ambiguous".into());
        let bolt11 = target.to_string();

        let account = account_for(&url);
        let connector = Arc::new(FaultConnector::new(raw_client(&url)));
        let storage = Arc::new(MemStorage::new());
        let provider: Arc<dyn CashuProvider> = Arc::new(FaultProvider {
            connector: connector.clone(),
        });
        let svc =
            CashuMeltQuoteService::new(storage.clone() as Arc<dyn CashuMeltQuoteStorage>, provider);

        let prepared = prepared_from(&funding);
        let preview = svc
            .get_quote(&account, &prepared, &bolt11)
            .await
            .expect("get_quote");
        let quote_id = seed_row_from_preview(&storage, &account, &preview);
        let quote = storage.get(quote_id).await.expect("seeded row");

        let melt_inputs: Vec<Proof> = preview
            .prepared_proofs
            .iter()
            .map(|pw| {
                funding
                    .iter()
                    .find(|fp| fp.secret.to_string() == pw.proof.secret)
                    .expect("prepared proof traces to a funding proof")
                    .clone()
            })
            .collect();

        // ARM the ambiguous fault: mint settles, response lost.
        connector.arm(MeltFault::AmbiguousAfterSettle);
        let seed = fresh_seed();
        let outcome = svc
            .initiate_melt(&account, quote.clone(), &seed)
            .await
            .expect("initiate_melt must NOT error on an ambiguous post_melt");

        // Lane T Fix 1: ambiguous => Pending (NOT Failed).
        assert!(
            matches!(outcome, MeltOutcome::Pending(_)),
            "ambiguous post_melt MUST yield Pending (pre-Lane-T this was \
         Failed): {outcome:?}"
        );
        let quote_row = storage.get(quote_id).await.expect("row");
        assert!(
            matches!(quote_row.state, CashuMeltQuoteState::Pending),
            "quote MUST stay PENDING (proofs RESERVED) after an ambiguous \
         post_melt — a FAILED here releases proofs while the mint paid \
         = double-pay. Was {:?}",
            quote_row.state
        );
        assert!(
            !matches!(quote_row.state, CashuMeltQuoteState::Failed { .. }),
            "quote MUST NOT be FAILED (that releases the reserved proofs)"
        );
        assert_eq!(
            connector.real_post_melt_count(),
            1,
            "the mint received exactly one post_melt (it settled)"
        );

        // Disarm and reconcile via the read-only status poll. Mint already
        // settled -> reconciles to PAID, NO second post_melt.
        connector.disarm();
        let reconciled = svc
            .poll_until_complete(
                &account,
                quote_row,
                &seed,
                std::time::Duration::from_millis(300),
                std::time::Duration::from_secs(30),
            )
            .await
            .expect("poll_until_complete reconcile");
        assert!(
            matches!(reconciled, MeltOutcome::Paid { .. }),
            "ambiguous melt must reconcile to PAID via status poll: \
         {reconciled:?}"
        );
        let final_row = storage.get(quote_id).await.expect("row");
        assert!(
            matches!(final_row.state, CashuMeltQuoteState::Paid { .. }),
            "final state must be PAID, was {:?}",
            final_row.state
        );
        assert_eq!(
            connector.real_post_melt_count(),
            1,
            "reconcile must NOT issue a 2nd post_melt — exactly ONE \
         Lightning payment for this invoice"
        );

        // PROTOCOL-level proof: input proofs spent exactly once; a manual
        // 2nd post_melt is rejected.
        let chk = raw_client(&url);
        let replay = chk
            .post_melt(
                &PaymentMethod::BOLT11,
                MeltRequest::new(preview.melt_quote_id.clone(), melt_inputs.clone(), None),
            )
            .await;
        assert!(
            replay.is_err(),
            "2nd post_melt with same inputs MUST be rejected (proofs spent)"
        );
        let st = chk
            .post_check_state(CheckStateRequest {
                ys: melt_inputs.iter().map(|p| p.y().expect("y")).collect(),
            })
            .await
            .expect("check_state");
        assert!(
            st.states.iter().all(|s| s.state == cdk::nuts::State::Spent),
            "input proofs must be SPENT exactly once"
        );
    }

    /// The unchanged half of Fix 1: a *definitive* mint rejection (the mint
    /// provably did not pay) still FAILs the quote and releases the reserved
    /// proofs — Lane T must NOT have regressed this safe path.
    #[tokio::test]
    async fn nut05_fix1_definitive_post_melt_rejection_fails_and_releases_live() {
        let url = mint_url_str();
        let raw = raw_client(&url);
        let funding = mint_proofs(&raw, 256).await;
        let target = cdk_fake_wallet::create_fake_invoice(64_000, "fix1 definitive".into());
        let bolt11 = target.to_string();

        let account = account_for(&url);
        let connector = Arc::new(FaultConnector::new(raw_client(&url)));
        let storage = Arc::new(MemStorage::new());
        let provider: Arc<dyn CashuProvider> = Arc::new(FaultProvider {
            connector: connector.clone(),
        });
        let svc =
            CashuMeltQuoteService::new(storage.clone() as Arc<dyn CashuMeltQuoteStorage>, provider);

        let prepared = prepared_from(&funding);
        let preview = svc
            .get_quote(&account, &prepared, &bolt11)
            .await
            .expect("get_quote");
        let quote_id = seed_row_from_preview(&storage, &account, &preview);
        let quote = storage.get(quote_id).await.expect("seeded row");
        let melt_inputs: Vec<Proof> = preview
            .prepared_proofs
            .iter()
            .map(|pw| {
                funding
                    .iter()
                    .find(|fp| fp.secret.to_string() == pw.proof.secret)
                    .expect("prepared proof traces to a funding proof")
                    .clone()
            })
            .collect();

        connector.arm(MeltFault::DefinitiveBeforeSettle);
        let seed = fresh_seed();
        let outcome = svc
            .initiate_melt(&account, quote.clone(), &seed)
            .await
            .expect("initiate_melt returns Failed (not Err) on definitive reject");

        assert!(
            matches!(outcome, MeltOutcome::Failed(_)),
            "definitive mint rejection MUST FAIL the quote (unchanged \
         behaviour): {outcome:?}"
        );
        let quote_row = storage.get(quote_id).await.expect("row");
        assert!(
            matches!(quote_row.state, CashuMeltQuoteState::Failed { .. }),
            "definitive rejection MUST leave the quote FAILED (proofs safe \
         to release — the mint provably did not pay). Was {:?}",
            quote_row.state
        );
        assert_eq!(
            connector.real_post_melt_count(),
            0,
            "definitive-before-settle never forwards to the mint (it \
         provably did not pay)"
        );

        // The mint never saw the request, so the input proofs are UNSPENT
        // and a fresh melt with them would succeed — i.e. releasing the
        // reservation is genuinely safe here (the point of the definitive
        // branch). Prove they are NOT spent.
        let chk = raw_client(&url);
        let st = chk
            .post_check_state(CheckStateRequest {
                ys: melt_inputs.iter().map(|p| p.y().expect("y")).collect(),
            })
            .await
            .expect("check_state");
        assert!(
            st.states
                .iter()
                .all(|s| s.state == cdk::nuts::State::Unspent),
            "after a definitive pre-settle rejection the input proofs must \
         still be UNSPENT (safe to release): {:?}",
            st.states
        );
    }
} // mod e2e
