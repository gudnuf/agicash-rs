//! Tier 1 hermetic suite — the always-on CI gate.
//!
//! Strictly hermetic: NO network, NO docker, NO spawned services, NO real
//! cdk-mintd/enclave. Everything runs against the `agicash-testing`
//! in-memory fakes through the **real** `WalletClient` facade composed via
//! the public `WalletClientBuilder`.
//!
//! Closes the three named glue regressions parked in `client.rs`:
//!
//! - **M1-test** — `get_account` for a foreign-owned account returns
//!   `WalletError::Validation { code: "wrong_owner" }` (pure ownership
//!   check; never touches the mint). Parked at `client.rs:1605-1611`.
//! - **12c-3** — `check_send_token_claimed` without a session returns
//!   `WalletError::Unauthenticated` via the real facade's
//!   `require_session` seam (pure auth short-circuit; never touches the
//!   mint). Parked at `client.rs:1682-1697`.
//! - **12c-7 (Tier-1 smoke)** — `receive_flow()` is still reachable
//!   end-to-end after the 12c dead-code sweep (seed→provider→storage glue
//!   composes). The full real-glue proof is the Tier 2 task; this is the
//!   hermetic constructibility smoke.

use agicash_domain::{AccountId, Currency, UserId};
use agicash_testing::{cashu_account, TestWallet};
use agicash_wallet::WalletError;
use uuid::Uuid;

/// 12c-3: the auth-seam short-circuit. `check_send_token_claimed` is
/// `require_session`-guarded; with no session the real facade returns
/// `Unauthenticated` before any storage/provider call.
#[tokio::test]
async fn check_send_token_claimed_without_session_is_unauthenticated() {
    let tw = TestWallet::new(); // logged out
    let err = tw
        .wallet()
        .check_send_token_claimed(Uuid::new_v4())
        .await
        .expect_err("logged-out check must error");
    assert!(
        matches!(err, WalletError::Unauthenticated),
        "expected Unauthenticated, got {err:?}"
    );
}

/// M1-test: the ownership guard. A logged-in user fetching an account
/// owned by a *different* user gets `Validation { code: "wrong_owner" }`
/// (the same guard the quote methods use), not the account.
#[tokio::test]
async fn get_account_for_foreign_owner_is_wrong_owner_validation() {
    let tw = TestWallet::logged_in();
    // An account owned by some OTHER user.
    let other_uid = UserId::new();
    let foreign = cashu_account(other_uid, "https://mint.example", Currency::Btc);
    let foreign_id = foreign.id;
    tw.user_storage().insert_account(foreign);

    let err = tw
        .wallet()
        .get_account(foreign_id)
        .await
        .expect_err("foreign-owned account must not be returned");
    match err {
        WalletError::Validation { code, .. } => {
            assert_eq!(code, "wrong_owner", "got code {code}");
        }
        other => panic!("expected Validation{{wrong_owner}}, got {other:?}"),
    }
}

/// M1 control: the SAME guard lets the owner through (proves the test
/// asserts the ownership branch, not a blanket failure).
#[tokio::test]
async fn get_account_for_own_account_succeeds() {
    let tw = TestWallet::logged_in();
    let uid = tw.session_user_id().expect("logged_in has a session");
    let mine = cashu_account(uid, "https://mint.example", Currency::Btc);
    let mine_id = mine.id;
    tw.user_storage().insert_account(mine);

    let summary = tw
        .wallet()
        .get_account(mine_id)
        .await
        .expect("own account must be returned");
    assert_eq!(summary.id, mine_id);
}

/// 12c-7 Tier-1 smoke: after the dead-code sweep, `receive_flow()` is
/// still constructible end-to-end through the facade (session →
/// seed-provider → cashu-provider → receive-swap-service glue all wires).
/// The real money-path proof is the Tier 2 task; here we only assert the
/// flow is reachable and starts Idle.
#[tokio::test]
async fn receive_flow_constructible_through_facade() {
    let tw = TestWallet::logged_in();
    let flow = tw
        .wallet()
        .receive_flow()
        .await
        .expect("receive_flow must be constructible with a session");
    // Smoke the state surface — a fresh flow is Idle.
    let state = flow.current_state();
    assert_eq!(
        format!("{state:?}"),
        "Idle",
        "a fresh receive flow must start Idle, got {state:?}"
    );
}

/// 12c-7 control: `receive_flow()` itself is `require_session`-guarded —
/// logged out, it short-circuits to `Unauthenticated` (no provider/storage
/// touch), the same seam as 12c-3.
#[tokio::test]
async fn receive_flow_without_session_is_unauthenticated() {
    let tw = TestWallet::new();
    let err = tw
        .wallet()
        .receive_flow()
        .await
        .expect_err("logged-out receive_flow must error");
    assert!(
        matches!(err, WalletError::Unauthenticated),
        "expected Unauthenticated, got {err:?}"
    );
    // Silence unused-import lints if the assert form changes.
    let _ = AccountId::new();
}

/// `reverse_send_swap` is `require_session`-guarded — logged out it
/// short-circuits to `Unauthenticated` before any storage/provider call,
/// the same auth seam as `check_send_token_claimed`.
#[tokio::test]
async fn reverse_send_swap_without_session_is_unauthenticated() {
    let tw = TestWallet::new(); // logged out
    let err = tw
        .wallet()
        .reverse_send_swap(Uuid::new_v4())
        .await
        .expect_err("logged-out reverse must error");
    assert!(
        matches!(err, WalletError::Unauthenticated),
        "expected Unauthenticated, got {err:?}"
    );
}

/// `reverse_send_swap` on a swap id that does not exist returns
/// `NotFound` — a logged-in wallet with empty send storage has no row to
/// reverse.
#[tokio::test]
async fn reverse_send_swap_unknown_id_is_not_found() {
    let tw = TestWallet::logged_in();
    let err = tw
        .wallet()
        .reverse_send_swap(Uuid::new_v4())
        .await
        .expect_err("reversing a non-existent swap must error");
    assert!(
        matches!(err, WalletError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

/// F15 Lane 3: `refresh_pending_state` is `require_session`-guarded —
/// logged out it short-circuits to `Unauthenticated` before any of the
/// four storage reads fire (the realtime-reconnect catch-up never runs
/// for a signed-out wallet).
#[tokio::test]
async fn refresh_pending_state_without_session_is_unauthenticated() {
    let tw = TestWallet::new();
    let err = tw
        .wallet()
        .refresh_pending_state()
        .await
        .expect_err("logged-out refresh_pending_state must error");
    assert!(
        matches!(err, WalletError::Unauthenticated),
        "expected Unauthenticated, got {err:?}"
    );
}

/// F15 Lane 3: a logged-in wallet with nothing in flight gets an empty
/// `PendingStateSnapshot` — `Vec::is_empty()` is the canonical "nothing
/// pending" state, not an error. Proves the `tokio::try_join!` aggregator
/// wires all four `list_*` reads through the real facade.
#[tokio::test]
async fn refresh_pending_state_empty_wallet_returns_empty_snapshot() {
    let tw = TestWallet::logged_in();
    let snapshot = tw
        .wallet()
        .refresh_pending_state()
        .await
        .expect("refresh_pending_state must succeed for a logged-in wallet");
    assert!(snapshot.mint_quotes.is_empty(), "no mint quotes expected");
    assert!(
        snapshot.receive_swaps.is_empty(),
        "no receive swaps expected"
    );
    assert!(snapshot.melt_quotes.is_empty(), "no melt quotes expected");
    assert!(snapshot.send_swaps.is_empty(), "no send swaps expected");
}

/// F15 Lane 3: the per-list methods are individually `require_session`-
/// guarded too, and return empty for a fresh logged-in wallet.
#[tokio::test]
async fn list_pending_methods_empty_for_fresh_logged_in_wallet() {
    let tw = TestWallet::logged_in();
    let w = tw.wallet();
    assert!(w.list_pending_mint_quotes().await.unwrap().is_empty());
    assert!(w.list_pending_receive_swaps().await.unwrap().is_empty());
    assert!(w.list_unresolved_melt_quotes().await.unwrap().is_empty());
    assert!(w.list_unresolved_send_swaps().await.unwrap().is_empty());
}
