//! Integration tests for `agicash-driver::run_sweep`.
//!
//! Strategy: compose a `WalletClient` via the public `WalletClientBuilder`
//! using `agicash-testing`'s in-memory fakes (the same set the Tier-1
//! `TestWallet` uses) plus a **programmable `CashuProvider`** defined
//! in this test crate so we can:
//!
//! - leave rows stuck (the provider errs at `wallet_for_account`, so
//!   any service method that would do a mint round-trip fails — but
//!   the storage `create()` calls have already inserted the row in its
//!   stuck state, exactly the situation the driver is built for); and
//! - inject specific error classes (Network = Transient — useful for
//!   "retry exhausted" / "one bad row doesn't break the sweep").
//!
//! These tests do NOT spin up `cdk-mintd` (the `tier2-e2e` real-
//! service harness is gated behind a feature and out of Lane A's
//! scope — it's wired up by Lane B's trigger task tests). Lane A
//! verifies the driver's *dispatch + classification + report*
//! contract; the wire-protocol round-trips are already covered by the
//! services' own real-service suites.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agicash_cashu::{
    CashuMeltQuote, CashuMeltQuoteState, CashuMintQuote, CashuMintQuoteState, CashuReceiveSwap,
    CashuReceiveSwapState, CashuSendSwap, CashuSendSwapState, CashuSendSwapStorage, CreateSendSwap,
    OutputAmounts, TokenProof,
};
use agicash_domain::{Account, Currency, UserId};
use agicash_driver::report::RowStatus;
use agicash_driver::{
    run_sweep_with_config, sweep::sweep_snapshot, RetryConfig, RowKind, SweepReport,
};
use agicash_money::{Money, Unit};
use agicash_testing::{
    FakeAuthClient, FixedExchangeRate, InMemoryMeltQuoteStorage, InMemoryMintQuoteStorage,
    InMemoryReceiveSwapStorage, InMemorySendSwapStorage, InMemoryUserStorage,
};
use agicash_traits::{CashuMintWallet, CashuProvider, CashuProviderError};
use agicash_wallet::{
    AuthClient, PendingStateSnapshot, WalletClient, WalletClientBuilder, WalletError,
};
use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use rust_decimal::Decimal;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Programmable CashuProvider — counts calls; default behavior is
// `CashuProviderError::Network`, which routes through
// `From<CashuProviderError> for WalletError` to `WalletError::Network`
// — i.e. the `ErrorClass::Transient` rail. Useful for the "retry
// budget exhausted" / "one bad row doesn't break the sweep" tests.
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct ProgrammableProvider {
    wallet_for_account_calls: AtomicUsize,
}

impl ProgrammableProvider {
    fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[async_trait]
impl CashuProvider for ProgrammableProvider {
    async fn wallet_for_account(
        &self,
        _account: &Account,
    ) -> Result<Arc<CashuMintWallet>, CashuProviderError> {
        self.wallet_for_account_calls.fetch_add(1, Ordering::SeqCst);
        Err(CashuProviderError::Network(
            "ProgrammableProvider: mint round-trip disabled in driver tests".into(),
        ))
    }

    async fn mint_info(
        &self,
        _mint_url: &cdk::mint_url::MintUrl,
    ) -> Result<cdk::nuts::MintInfo, CashuProviderError> {
        Err(CashuProviderError::Network(
            "ProgrammableProvider: mint_info disabled".into(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Test harness
// ---------------------------------------------------------------------------

struct Harness {
    wallet: Arc<WalletClient>,
    auth: Arc<FakeAuthClient>,
    user_storage: Arc<InMemoryUserStorage>,
    send_storage: Arc<InMemorySendSwapStorage>,
    receive_storage: Arc<InMemoryReceiveSwapStorage>,
    mint_quote_storage: Arc<InMemoryMintQuoteStorage>,
    melt_storage: Arc<InMemoryMeltQuoteStorage>,
    provider: Arc<ProgrammableProvider>,
}

impl Harness {
    fn new() -> Self {
        let auth = Arc::new(FakeAuthClient::logged_in());
        let user_storage = Arc::new(InMemoryUserStorage::new());
        let send_storage = Arc::new(InMemorySendSwapStorage::new());
        let receive_storage = Arc::new(InMemoryReceiveSwapStorage::new());
        let mint_quote_storage = Arc::new(InMemoryMintQuoteStorage::new());
        let melt_storage = Arc::new(InMemoryMeltQuoteStorage::new());
        let provider = ProgrammableProvider::new();

        let wallet = WalletClientBuilder::new()
            .auth(Arc::clone(&auth) as Arc<dyn AuthClient>)
            .user_storage(Arc::clone(&user_storage) as Arc<_>)
            .cashu_provider(Arc::clone(&provider) as Arc<_>)
            .cashu_receive_storage(Arc::clone(&receive_storage) as Arc<_>)
            .cashu_send_storage(Arc::clone(&send_storage) as Arc<_>)
            .cashu_mint_quote_storage(Arc::clone(&mint_quote_storage) as Arc<_>)
            .cashu_melt_quote_storage(Arc::clone(&melt_storage) as Arc<_>)
            .exchange_rate(Arc::new(FixedExchangeRate) as Arc<_>)
            .build()
            .expect("compose wallet");

        Self {
            wallet,
            auth,
            user_storage,
            send_storage,
            receive_storage,
            mint_quote_storage,
            melt_storage,
            provider,
        }
    }

    fn user_id(&self) -> UserId {
        self.auth.user_id().expect("FakeAuthClient::logged_in")
    }

    fn account(&self) -> Account {
        let user_id = self.user_id();
        let account =
            agicash_testing::cashu_account(user_id, "https://mint.example", Currency::Btc);
        self.user_storage.insert_account(account.clone());
        account
    }
}

// ---------------------------------------------------------------------------
// Tiny helpers — short retry config + money builder.
// ---------------------------------------------------------------------------

fn fast_retry() -> RetryConfig {
    RetryConfig {
        max_attempts: 3,
        base_backoff: Duration::from_millis(1),
    }
}

fn one_attempt() -> RetryConfig {
    RetryConfig {
        max_attempts: 1,
        base_backoff: Duration::from_millis(1),
    }
}

fn sats(n: u64) -> Money {
    Money::new(Decimal::from(n), Currency::Btc, Unit::Sat)
}

// ---------------------------------------------------------------------------
// Seed helpers — drop rows directly into the in-memory storages so we
// don't have to drive a full facade flow (which would talk to the
// stubbed provider and Network-error before persisting).
// ---------------------------------------------------------------------------

async fn seed_draft_send_swap(h: &Harness, account: &Account) -> CashuSendSwap {
    let proof = TokenProof {
        id: "00000000000000ab".into(),
        amount: 100,
        secret: "test-secret".into(),
        c: "02".repeat(33),
        dleq: None,
        witness: None,
    };
    let input = CreateSendSwap {
        account_id: account.id,
        user_id: h.user_id(),
        token_mint_url: "https://mint.example".into(),
        amount_requested: sats(40),
        amount_to_send: sats(40),
        total_amount: sats(42),
        cashu_send_fee: sats(2),
        cashu_receive_fee: sats(0),
        input_proofs: vec![proof.clone()],
        input_amount: sats(100),
        input_proof_ids: vec![Uuid::new_v4()],
        token_hash: None,
        // Setting keyset_id + output_amounts forces the storage layer
        // to write a DRAFT row (input swap path).
        keyset_id: Some("00000000000000ab".into()),
        output_amounts: Some(OutputAmounts {
            send: vec![40],
            change: vec![58],
        }),
    };
    let result = (h.send_storage.as_ref() as &dyn CashuSendSwapStorage)
        .create(input)
        .await
        .expect("seed draft send swap");
    assert!(matches!(result.swap.state, CashuSendSwapState::Draft));
    result.swap
}

async fn seed_pending_receive_swap(h: &Harness, account: &Account) -> CashuReceiveSwap {
    use agicash_cashu::receive_swap::storage::CreateReceiveSwap;
    let token_hash = format!("{:x}", Uuid::new_v4().as_u128());
    let input = CreateReceiveSwap {
        token_hash: token_hash.clone(),
        token_proofs: vec![],
        token_mint_url: "https://mint.example".into(),
        token_description: None,
        user_id: h.user_id(),
        account_id: account.id,
        keyset_id: "00000000000000ab".into(),
        input_amount: sats(64),
        fee_amount: sats(0),
        amount_received: sats(64),
        output_amounts: vec![32, 32],
        reversed_transaction_id: None,
    };
    let storage: &dyn agicash_cashu::CashuReceiveSwapStorage = h.receive_storage.as_ref();
    storage.create(input).await.expect("seed receive swap").swap
}

async fn seed_unpaid_mint_quote_expired(h: &Harness, account: &Account) -> CashuMintQuote {
    use agicash_cashu::mint_quote::storage::CreateMintQuote;
    let input = CreateMintQuote {
        user_id: h.user_id(),
        account_id: account.id,
        amount: sats(64),
        description: None,
        quote_id: format!("expired-{}", Uuid::new_v4()),
        payment_request: "lnbc-fake-invoice".into(),
        payment_hash: format!("{:x}", Uuid::new_v4().as_u128()),
        // 1 hour in the past — past expiry.
        expires_at: Utc::now() - ChronoDuration::hours(1),
        locking_derivation_path: String::new(),
        minting_fee: None,
        total_fee: sats(0),
    };
    let storage: &dyn agicash_cashu::CashuMintQuoteStorage = h.mint_quote_storage.as_ref();
    storage
        .create(input)
        .await
        .expect("seed expired mint quote")
}

fn seed_unpaid_melt_quote_expired(h: &Harness, account: &Account) -> CashuMeltQuote {
    let quote = CashuMeltQuote {
        id: Uuid::new_v4(),
        quote_id: format!("melt-expired-{}", Uuid::new_v4()),
        user_id: h.user_id(),
        account_id: account.id,
        payment_request: "lnbc-fake-melt-invoice".into(),
        payment_hash: format!("{:x}", Uuid::new_v4().as_u128()),
        amount_requested: sats(50),
        amount_requested_in_msat: 50_000,
        amount_received: sats(50),
        lightning_fee_reserve: sats(0),
        cashu_fee: sats(0),
        proofs: vec![],
        amount_reserved: sats(0),
        keyset_id: "00000000000000ab".into(),
        keyset_counter: 0,
        number_of_change_outputs: 0,
        transaction_id: Uuid::new_v4(),
        created_at: Utc::now() - ChronoDuration::hours(2),
        expires_at: Utc::now() - ChronoDuration::hours(1),
        version: 0,
        state: CashuMeltQuoteState::Unpaid,
    };
    h.melt_storage.insert(quote.clone());
    quote
}

fn seed_live_unpaid_melt_quote(h: &Harness, account: &Account) -> CashuMeltQuote {
    let quote = CashuMeltQuote {
        id: Uuid::new_v4(),
        quote_id: format!("melt-live-{}", Uuid::new_v4()),
        user_id: h.user_id(),
        account_id: account.id,
        payment_request: "lnbc-fake-melt-invoice".into(),
        payment_hash: format!("{:x}", Uuid::new_v4().as_u128()),
        amount_requested: sats(50),
        amount_requested_in_msat: 50_000,
        amount_received: sats(50),
        lightning_fee_reserve: sats(1),
        cashu_fee: sats(0),
        proofs: vec![],
        amount_reserved: sats(51),
        keyset_id: "00000000000000ab".into(),
        keyset_counter: 0,
        number_of_change_outputs: 0,
        transaction_id: Uuid::new_v4(),
        created_at: Utc::now(),
        expires_at: Utc::now() + ChronoDuration::minutes(15),
        version: 0,
        state: CashuMeltQuoteState::Unpaid,
    };
    h.melt_storage.insert(quote.clone());
    quote
}

// ===========================================================================
// Tests
// ===========================================================================

/// **Happy advance**: a stuck UNPAID mint quote past expiry must be
/// flipped to EXPIRED by the driver. `expire_mint_quote` is purely
/// storage-side (no mint round-trip), so it succeeds even against our
/// stubbed provider — the cleanest "row genuinely advanced" assertion
/// in this test crate.
#[tokio::test]
async fn expired_mint_quote_advances() {
    let h = Harness::new();
    let account = h.account();
    let quote = seed_unpaid_mint_quote_expired(&h, &account).await;
    assert!(matches!(quote.state, CashuMintQuoteState::Unpaid));

    let report: SweepReport = run_sweep_with_config(&h.wallet, fast_retry())
        .await
        .expect("sweep");

    assert_eq!(report.rows_seen, 1, "exactly one stuck row seeded");
    assert_eq!(report.advanced, 1, "expired-mint-quote row advanced");
    assert_eq!(report.no_op, 0);
    assert_eq!(report.failed, 0);
    assert!(report.all_clean());

    // Confirm the state really changed (vs. driver merely reporting it).
    let storage: &dyn agicash_cashu::CashuMintQuoteStorage = h.mint_quote_storage.as_ref();
    let row = storage.get(quote.id).await.expect("re-read mint quote");
    assert!(
        matches!(row.state, CashuMintQuoteState::Expired),
        "mint quote should now be EXPIRED, was {:?}",
        row.state
    );

    let outcome = &report.outcomes[0];
    assert_eq!(outcome.row_id, quote.id);
    assert_eq!(outcome.kind, RowKind::MintQuoteExpire);
    assert!(matches!(outcome.status, RowStatus::Advanced));
}

/// **Happy advance (melt)**: an expired UNPAID melt quote advances to
/// EXPIRED. `expire_melt_quote` is storage-only — no mint call.
#[tokio::test]
async fn expired_melt_quote_advances() {
    let h = Harness::new();
    let account = h.account();
    let quote = seed_unpaid_melt_quote_expired(&h, &account);
    assert!(matches!(quote.state, CashuMeltQuoteState::Unpaid));

    let report = run_sweep_with_config(&h.wallet, fast_retry())
        .await
        .expect("sweep");

    assert_eq!(report.rows_seen, 1);
    assert_eq!(report.advanced, 1);
    assert_eq!(report.failed, 0);

    let storage: &dyn agicash_cashu::CashuMeltQuoteStorage = h.melt_storage.as_ref();
    let row = storage.get(quote.id).await.expect("re-read melt quote");
    assert!(
        matches!(row.state, CashuMeltQuoteState::Expired),
        "melt quote should now be EXPIRED, was {:?}",
        row.state
    );
}

/// **`InvalidTransition` is benign**: the driver classifies the
/// service-level `InvalidTransition` (machine-guarded "wrong state for
/// this event" error — plan §6.1) as a benign no-op, not a Failure.
///
/// We trigger one by:
/// 1. Seeding a send-swap row in storage, then transitioning it to
///    FAILED (via storage's `fail()`).
/// 2. Constructing a snapshot whose copy of the row claims state
///    DRAFT — feeding it to `sweep_snapshot` directly.
/// 3. The dispatcher routes via the DRAFT branch → calls
///    `resume_send_swap_draft(id)` → the facade re-reads the row
///    (it's actually FAILED in storage) → the service
///    `swap_for_proofs_to_send` sees the FAILED state → returns
///    `Err(SendSwapError::InvalidTransition)` (FAILED is not in the
///    "idempotent terminal" set, only Pending/Completed/Reversed are).
///    That flattens to `WalletError::Cashu("invalid state transition
///    ...")` and the driver classifies it as `Benign`.
#[tokio::test]
async fn invalid_transition_is_benign() {
    let h = Harness::new();
    let account = h.account();

    // Seed a DRAFT, transition it to FAILED.
    let drafted = seed_draft_send_swap(&h, &account).await;
    let failed = (h.send_storage.as_ref() as &dyn CashuSendSwapStorage)
        .fail(drafted.id, "test-injected failure")
        .await
        .expect("transition DRAFT→FAILED");
    assert!(matches!(failed.state, CashuSendSwapState::Failed { .. }));

    // Hand-build a snapshot whose row says DRAFT (lying about state so
    // the dispatcher takes the DRAFT branch).
    let snapshot = PendingStateSnapshot {
        send_swaps: vec![CashuSendSwap {
            id: failed.id,
            account_id: failed.account_id,
            user_id: failed.user_id,
            input_proofs: failed.input_proofs.clone(),
            input_amount: failed.input_amount,
            amount_received: failed.amount_received,
            cashu_receive_fee: failed.cashu_receive_fee,
            amount_to_send: failed.amount_to_send,
            cashu_send_fee: failed.cashu_send_fee,
            amount_spent: failed.amount_spent,
            total_fee: failed.total_fee,
            keyset_id: failed.keyset_id.clone(),
            keyset_counter: failed.keyset_counter,
            output_amounts: failed.output_amounts.clone(),
            transaction_id: failed.transaction_id,
            created_at: failed.created_at,
            version: failed.version,
            // *** Lie about state to force the DRAFT dispatch branch ***
            state: CashuSendSwapState::Draft,
        }],
        receive_swaps: vec![],
        mint_quotes: vec![],
        melt_quotes: vec![],
    };

    let report = sweep_snapshot(&h.wallet, snapshot, fast_retry()).await;

    assert_eq!(report.rows_seen, 1);
    assert_eq!(
        report.advanced, 0,
        "InvalidTransition must NOT show as Advanced"
    );
    assert_eq!(report.no_op, 1, "InvalidTransition must show as NoOp");
    assert_eq!(report.failed, 0);
    let outcome = &report.outcomes[0];
    assert_eq!(outcome.kind, RowKind::SendSwapDraft);
    match &outcome.status {
        RowStatus::NoOp { reason } => assert!(
            reason.contains("invalid state transition"),
            "expected benign no-op reason to mention InvalidTransition, got {reason:?}"
        ),
        other => panic!("expected NoOp, got {other:?}"),
    }
}

/// **One bad row does NOT break the sweep**: mix a stuck DRAFT
/// send_swap (which our stubbed provider Network-errors on every
/// attempt — Transient, exhausts the retry budget → Failed) with a
/// stuck expired mint quote (which advances cleanly). The report
/// must show 2 rows seen, 1 advanced, 1 failed.
#[tokio::test]
async fn one_bad_row_does_not_break_sweep() {
    let h = Harness::new();
    let account = h.account();

    // Row 1: DRAFT send_swap — provider Network-errors on every retry.
    let draft = seed_draft_send_swap(&h, &account).await;
    assert!(matches!(draft.state, CashuSendSwapState::Draft));

    // Row 2: expired UNPAID mint quote — storage only, advances clean.
    let expired = seed_unpaid_mint_quote_expired(&h, &account).await;

    let cfg = RetryConfig {
        max_attempts: 2,
        base_backoff: Duration::from_millis(1),
    };
    let report = run_sweep_with_config(&h.wallet, cfg).await.expect("sweep");

    assert_eq!(report.rows_seen, 2, "both seeded rows surfaced");
    assert_eq!(report.advanced, 1, "expired-mint advanced");
    assert_eq!(report.no_op, 0);
    assert_eq!(report.failed, 1, "DRAFT send_swap failed after retries");
    assert!(!report.all_clean());

    let provider_calls = h.provider.wallet_for_account_calls.load(Ordering::SeqCst);
    assert!(
        provider_calls >= 2,
        "expected the DRAFT row to exhaust its retry budget against the provider (≥2 calls); got {provider_calls}"
    );

    // The expired-mint row really is now EXPIRED.
    let storage: &dyn agicash_cashu::CashuMintQuoteStorage = h.mint_quote_storage.as_ref();
    let row = storage.get(expired.id).await.expect("re-read mint quote");
    assert!(matches!(row.state, CashuMintQuoteState::Expired));

    // The DRAFT row is still DRAFT — the driver did not corrupt it.
    let storage: &dyn agicash_cashu::CashuSendSwapStorage = h.send_storage.as_ref();
    let row = storage.get(draft.id).await.expect("re-read send swap");
    assert!(matches!(row.state, CashuSendSwapState::Draft));
}

/// **Live (not-yet-expired) UNPAID melt quote → NoOp** (the §6.2 +
/// §2-note guard: the driver MUST NOT initiate a payment for a not-
/// yet-fired melt quote). Provider must see zero calls — the driver
/// takes no action.
#[tokio::test]
async fn live_unpaid_melt_quote_is_noop_not_initiated() {
    let h = Harness::new();
    let account = h.account();
    let live = seed_live_unpaid_melt_quote(&h, &account);

    let report = run_sweep_with_config(&h.wallet, fast_retry())
        .await
        .expect("sweep");
    assert_eq!(report.rows_seen, 1);
    assert_eq!(report.no_op, 1, "live UNPAID melt quote must be NoOp");
    assert_eq!(report.advanced, 0, "driver must NEVER initiate a melt");
    assert_eq!(report.failed, 0);

    let outcome = &report.outcomes[0];
    assert_eq!(outcome.kind, RowKind::MeltQuoteUnpaidExpire);
    match &outcome.status {
        RowStatus::NoOp { reason } => assert!(
            reason.contains("not yet expired"),
            "reason should explain why no action was taken, got {reason:?}"
        ),
        other => panic!("expected NoOp, got {other:?}"),
    }

    assert_eq!(
        h.provider.wallet_for_account_calls.load(Ordering::SeqCst),
        0,
        "live UNPAID melt quote must NOT touch the mint (initiate_melt is the §6.2 double-pay bug)"
    );
    let storage: &dyn agicash_cashu::CashuMeltQuoteStorage = h.melt_storage.as_ref();
    let row = storage.get(live.id).await.expect("re-read");
    assert!(matches!(row.state, CashuMeltQuoteState::Unpaid));
}

/// **Empty pending state → empty report (clean no-op sweep)**. The
/// steady-state assertion every trigger fires when there's nothing
/// to do.
#[tokio::test]
async fn empty_snapshot_is_clean_noop() {
    let h = Harness::new();
    let _account = h.account();

    let report = run_sweep_with_config(&h.wallet, fast_retry())
        .await
        .expect("sweep");
    assert_eq!(report.rows_seen, 0);
    assert_eq!(report.advanced, 0);
    assert_eq!(report.no_op, 0);
    assert_eq!(report.failed, 0);
    assert!(report.all_clean());
    assert!(report.outcomes.is_empty());
}

/// **Unauthenticated → propagated as the sweep's single Err**: the
/// `refresh_pending_state` call is the gate. A logged-out client
/// can't see any rows; `run_sweep` propagates the
/// `WalletError::Unauthenticated` (distinct from per-row failures).
#[tokio::test]
async fn logged_out_propagates_unauthenticated_err() {
    let auth = Arc::new(FakeAuthClient::new()); // logged OUT
    let user_storage = Arc::new(InMemoryUserStorage::new());
    let send_storage = Arc::new(InMemorySendSwapStorage::new());
    let receive_storage = Arc::new(InMemoryReceiveSwapStorage::new());
    let mint_quote_storage = Arc::new(InMemoryMintQuoteStorage::new());
    let melt_storage = Arc::new(InMemoryMeltQuoteStorage::new());
    let provider = ProgrammableProvider::new();

    let wallet = WalletClientBuilder::new()
        .auth(auth as Arc<dyn AuthClient>)
        .user_storage(user_storage as Arc<_>)
        .cashu_provider(provider as Arc<_>)
        .cashu_receive_storage(receive_storage as Arc<_>)
        .cashu_send_storage(send_storage as Arc<_>)
        .cashu_mint_quote_storage(mint_quote_storage as Arc<_>)
        .cashu_melt_quote_storage(melt_storage as Arc<_>)
        .exchange_rate(Arc::new(FixedExchangeRate) as Arc<_>)
        .build()
        .expect("compose wallet");

    let result = run_sweep_with_config(&wallet, one_attempt()).await;
    assert!(
        matches!(result, Err(WalletError::Unauthenticated)),
        "expected Err(Unauthenticated), got {result:?}"
    );
}

/// **Receive-swap PENDING row is dispatched**: the driver routes a
/// PENDING receive swap to `resume_receive_swap` (which reaches the
/// service, which calls `wallet_for_account`). We assert the provider
/// was touched — confirming the dispatch routing is correct — and
/// that the failure is reported per-row (Failed), not raised.
#[tokio::test]
async fn pending_receive_swap_is_dispatched() {
    let h = Harness::new();
    let account = h.account();
    let swap = seed_pending_receive_swap(&h, &account).await;
    assert!(matches!(swap.state, CashuReceiveSwapState::Pending));

    let calls_before = h.provider.wallet_for_account_calls.load(Ordering::SeqCst);
    let report = run_sweep_with_config(&h.wallet, one_attempt())
        .await
        .expect("sweep");
    let calls_after = h.provider.wallet_for_account_calls.load(Ordering::SeqCst);

    assert_eq!(report.rows_seen, 1);
    assert_eq!(report.failed, 1, "stubbed provider Network-errors");
    assert!(
        calls_after > calls_before,
        "dispatcher must reach the receive-swap service (which calls wallet_for_account)"
    );
    let outcome = &report.outcomes[0];
    assert_eq!(outcome.kind, RowKind::ReceiveSwapPending);
}
