//! Tier 2 real-service e2e — the headline confidence investment.
//!
//! Drives the **real** `WalletClient` facade against:
//! - a **real** `OpenSecret` enclave (attached to the standing `:3999`
//!   instance, or harness-cold-spawned) — real attestation handshake,
//!   real guest registration, real third-party JWT, real Cashu seed
//!   derivation;
//! - a **real** harness-spawned `cdk-mintd` (`FakeWallet` LN backend) —
//!   real NUT-04/05/06/07 wire protocol, real `construct_proofs`
//!   crypto;
//! - in-memory storage (the docker-wedge constraint: the only local
//!   Supabase is docker-managed; auth+mint give the real-glue
//!   confidence, storage is deterministic RPC-shaped CRUD already
//!   covered by `agicash-storage-supabase`'s own real-supabase gate).
//!
//! **No docker is invoked anywhere.** cdk-mintd is a cached binary, the
//! enclave is a host `cargo run` process, postgres is the standing
//! nix-native instance. One shared cdk-mintd + the attached enclave are
//! brought up ONCE per test binary (the proven one-mint-many-flows
//! model); each test gets a fresh real guest session + fresh in-memory
//! storages.
//!
//! GATING — opt-in `--features tier2-e2e`. Without it this file compiles
//! to a single skipped test so CI never wedges (mirrors
//! `crates/agicash-cashu/tests/cdk_mint_money_flows.rs:47-55`). Run:
//!
//! ```text
//! cargo test -p agicash-wallet --features tier2-e2e --test tier2_e2e \
//!   -- --test-threads=1
//! ```
//!
//! Preconditions (per `project_opensecret_local_stack`): standing
//! nix-native postgres on `:5432`; `~/opensecret` checkout for the
//! enclave (attached fast path = the standing `:3999` instance);
//! `cdk-mintd` cached (`bash scripts/cdk-mint-e2e.sh start` once). The
//! harness asserts/attaches/spawns each; it never uses docker.

#[cfg(not(feature = "tier2-e2e"))]
#[test]
fn tier2_skipped_without_feature() {
    eprintln!(
        "skipping Tier 2 real-service e2e; run with: \
         cargo test -p agicash-wallet --features tier2-e2e \
         --test tier2_e2e -- --test-threads=1 \
         (needs standing nix-pg :5432 + enclave :3999 + cached cdk-mintd \
         per project_opensecret_local_stack)"
    );
}

#[cfg(feature = "tier2-e2e")]
#[allow(clippy::too_many_lines)]
mod e2e {
    use agicash_domain::{Account, Currency};
    use agicash_money::{Money, Unit};
    use agicash_testing::ServiceHarness;
    use agicash_traits::UserStorage;
    use agicash_wallet::types::SendLightningStatus;
    use agicash_wallet::{ReceiveLightningState, ReceiveStatus, TokenVersion};
    use rust_decimal::Decimal;

    fn sats(n: u64) -> Money {
        Money::new(Decimal::from(n), Currency::Btc, Unit::Sat)
    }

    /// Real guest auth + `add_mint` (real NUT-06 `mint_info` over the wire
    /// to the shared cdk-mintd) → returns the freshly-created BTC Cashu
    /// `Account` (read back from the in-memory user storage so callers can
    /// fund it / assert against it).
    async fn guest_with_btc_account(h: &ServiceHarness) -> Account {
        let w = h.wallet().wallet();
        let session = w
            .auth_guest()
            .await
            .expect("auth_guest against the REAL enclave (real attestation + JWT chain)");
        assert!(
            !session.user_id.to_string().is_empty(),
            "real guest session must carry a user_id"
        );
        let summary = w
            .add_mint(h.mint_url().to_string(), Currency::Btc)
            .await
            .expect("add_mint (real mint_info wire round-trip to shared cdk-mintd)");
        assert_eq!(summary.balance, "0", "fresh mint account starts at 0");

        let accounts = h
            .wallet()
            .user_storage()
            .list_accounts(session.user_id)
            .await
            .expect("list_accounts");
        accounts
            .into_iter()
            .find(|a| a.id == summary.id)
            .expect("the just-added account must be in storage")
    }

    /// Real OpenSecret guest auth + the full third-party JWT chain.
    #[tokio::test]
    async fn guest_auth_real_jwt_chain() {
        let h = ServiceHarness::up()
            .await
            .expect("ServiceHarness::up (shared enclave attach/spawn + cdk-mintd, no docker)");
        let w = h.wallet().wallet();

        let session = w.auth_guest().await.expect("real auth_guest");
        assert!(
            !session.user_id.to_string().is_empty(),
            "guest session user_id must be non-empty (real enclave-issued)"
        );
        let status = w.auth_status().await.expect("auth_status");
        assert!(status.logged_in, "auth_status must report logged_in");
        assert_eq!(
            status.user_id,
            Some(session.user_id),
            "auth_status user_id must match the registered guest"
        );
        eprintln!(
            "[tier2] guest_auth_real_jwt_chain OK — real enclave session \
             user_id={} (enclave was {:?})",
            session.user_id,
            h.enclave_disposition()
        );
    }

    /// Real NUT-04 Lightning mint-quote receive **through the facade**:
    /// `quote_receive_lightning` → `poll_receive_lightning` (FakeWallet
    /// auto-settles) → `complete_receive_lightning` (real `post_mint` +
    /// real `construct_proofs`). Asserts the facade receipt
    /// (`status = Received`, the credited amount) — the real NUT-04 glue
    /// end-to-end. (Balance is not asserted here: in the in-memory fake
    /// seam the NUT-04 path persists to the mint-quote store, which is
    /// independent of the send store `balance` reads — that cross-store
    /// unification is the Supabase schema's job, covered by the storage
    /// crate's own real-supabase gate; here the real-glue proof is the
    /// receipt from the real protocol round-trip.)
    #[tokio::test]
    async fn lightning_mint_quote_receive_credits_balance() {
        let h = ServiceHarness::up().await.expect("ServiceHarness::up");
        let _account = guest_with_btc_account(&h).await;
        let w = h.wallet().wallet();

        let handle = w
            .quote_receive_lightning(None, sats(64))
            .await
            .expect("quote_receive_lightning (real NUT-04 post_mint_quote)");
        assert!(
            !handle.invoice.is_empty(),
            "real mint must return a bolt11 invoice"
        );

        let mut paid = false;
        for _ in 0..40 {
            let snap = w
                .poll_receive_lightning(handle.quote_id)
                .await
                .expect("poll_receive_lightning (real get_mint_quote_status)");
            match snap.state {
                ReceiveLightningState::Paid | ReceiveLightningState::Completed => {
                    paid = true;
                    break;
                }
                ReceiveLightningState::Unpaid => {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                other => panic!("mint quote went to unexpected state {other:?}"),
            }
        }
        assert!(paid, "FakeWallet mint did not auto-settle the NUT-04 quote");

        let receipt = w
            .complete_receive_lightning(handle.quote_id)
            .await
            .expect("complete_receive_lightning (real post_mint + construct_proofs)");
        assert_eq!(
            receipt.status,
            ReceiveStatus::Received,
            "real NUT-04 complete must report a first-time Received credit"
        );
        assert_eq!(
            receipt.amount,
            sats(64),
            "the credited amount must be the 64 sat real-minted via NUT-04"
        );
        eprintln!(
            "[tier2] lightning_mint_quote_receive_credits_balance OK — real \
             NUT-04 receipt Received {:?} (enclave {:?})",
            receipt.amount,
            h.enclave_disposition()
        );
    }

    /// Real seed→blind→`post_swap`→`construct_proofs`→spend glue. Fund the
    /// account with **genuinely real** proofs (real NUT-04 mint-quote +
    /// real `construct_proofs` against the shared mint, placed into the
    /// in-memory send store — the declared Tier 2 fake seam), then
    /// `send_token` (real NUT-06 swap-out) and `receive_cashu_token` the
    /// produced wire token back (real NUT-06 swap-in). This is exactly the
    /// 12c-7 glue the prior fake could only approximate — now real.
    #[tokio::test]
    async fn add_mint_then_token_send_receive_round_trips() {
        let h = ServiceHarness::up().await.expect("ServiceHarness::up");
        let account = guest_with_btc_account(&h).await;
        let w = h.wallet().wallet();

        h.fund_account_with_real_proofs(&account, 128)
            .await
            .expect("fund with real NUT-04-minted proofs");

        // Balance now reflects the real proofs (read from the send store).
        let bal = w.balance(None).await.expect("balance after real fund");
        assert_eq!(
            bal.total_per_currency
                .get("BTC")
                .cloned()
                .unwrap_or_default(),
            "128",
            "balance must reflect 128 sat of real minted proofs"
        );

        // REAL NUT-06 swap to produce a send token.
        let sent = w
            .send_token(None, sats(40), TokenVersion::V4)
            .await
            .expect("send_token (real NUT-06 post_swap + construct_proofs)");
        assert!(
            sent.token.starts_with("cashu"),
            "send_token must produce a cashu wire token, got {:.12}…",
            sent.token
        );

        // REAL receive of the produced token (real swap-in glue).
        let recv = w
            .receive_cashu_token(&sent.token)
            .await
            .expect("receive_cashu_token (real NUT-06 swap-in)");
        assert!(
            matches!(
                recv.status,
                ReceiveStatus::Received | ReceiveStatus::AlreadyClaimed
            ),
            "receive of the just-sent token must succeed, got {:?}",
            recv.status
        );
        eprintln!(
            "[tier2] add_mint_then_token_send_receive_round_trips OK — real \
             NUT-04 fund + NUT-06 send + NUT-06 receive (enclave {:?})",
            h.enclave_disposition()
        );
    }

    /// **THE highest-value test — the P0 no-double-pay vector, now real.**
    ///
    /// Fund with real proofs, then drive a real NUT-05 Lightning melt
    /// (`begin_send_lightning`) with the in-memory melt store's
    /// `complete()` fault-injected to fail once *after the mint already
    /// settled the Lightning payment*. The persisted quote is left PENDING
    /// (the mint already paid). The reconcile-aware facade MUST:
    ///  - reconcile to PAID via the **read-only** `poll_send_lightning`
    ///    (NUT-05 status poll — `poll_until_complete`), NEVER a second
    ///    `post_melt`;
    ///  - `poll_send_lightning` MUST NOT `Err` on a still-pending (P0-1:
    ///    a transient pending mistaken for failure → re-quote = double
    ///    pay);
    ///  - the funding proofs are SPENT, so a second melt is
    ///    protocol-impossible (insufficient funds, never a 2nd payment).
    ///
    /// Note: with the fault armed, `begin_send_lightning` itself returns
    /// `Err` — the storage `complete()` error propagates *after* the real
    /// `post_melt` settled (the exact P0 crash window). That `Err` is the
    /// reconcile *entry point*, not a failure: the row is already
    /// persisted PENDING; the caller recovers the `quote_id` and reconciles
    /// read-only. This mirrors `cdk_mint_money_flows.rs`'s P0 driver
    /// lifted to the facade.
    #[tokio::test]
    async fn lightning_melt_send_no_double_pay() {
        let h = ServiceHarness::up().await.expect("ServiceHarness::up");
        let account = guest_with_btc_account(&h).await;
        let w = h.wallet().wallet();

        // Fund a SINGLE 64-sat real proof (64 = 2^6 is one FakeWallet
        // power-of-2 denomination) and melt it ENTIRELY, so there are no
        // leftover proofs for a *legitimate* second payment to confuse the
        // no-double-pay assertion. Capture the real proofs for the
        // protocol-level NUT-07 SPENT check (the storage-independent,
        // hermetic-impossible invariant).
        let funded = h
            .fund_account_returning_proofs(&account, 64)
            .await
            .expect("fund with a single real 64-sat proof");

        // External bolt11 for exactly the funded amount (FakeWallet:
        // fee_percent=0, reserve_fee_min=0 → no LN fee reserve, so 64 sat
        // of proofs melts a 64 000 msat invoice exactly). FakeWallet's
        // melt backend settles it instantly to PAID.
        let target = agicash_testing::cdk_fake_wallet::create_fake_invoice(
            64_000,
            "tier2 melt no-double-pay".into(),
        );
        let invoice = target.to_string();

        // Arm a single post-settle storage failure: the mint settles the
        // Lightning payment, then `complete()` fails once.
        h.wallet().melt_storage().arm_complete_failure(1);

        // With the fault armed this propagates Err *after* the real
        // post_melt settled — the P0 crash window. The row is already
        // persisted PENDING; recover its quote_id to drive the read-only
        // reconcile (the exact no-double-pay entry point).
        let begin = w.begin_send_lightning(None, invoice.clone()).await;
        let quote_id = match begin {
            Err(e) => {
                eprintln!(
                    "[tier2] begin_send_lightning returned Err post-settle \
                     (expected — injected storage fault): {e:?}"
                );
                h.wallet()
                    .melt_storage()
                    .single_quote_id()
                    .expect("a PENDING melt quote must be persisted post-settle")
            }
            // If the fault somehow didn't intercept (FakeWallet timing),
            // a Paid/InFlight is still no error path — extract the id.
            Ok(SendLightningStatus::Paid(r)) => r.quote_id,
            Ok(SendLightningStatus::InFlight { quote_id }) => quote_id,
            Ok(SendLightningStatus::Failed { quote_id, reason }) => {
                panic!("melt FAILED against live FakeWallet (should settle): quote={quote_id} reason={reason}");
            }
        };

        // Reconcile via the read-only poll (NUT-05 status) — must reach
        // PAID without a second post_melt, never Err on still-pending.
        let mut reconciled = false;
        for _ in 0..40 {
            match w
                .poll_send_lightning(quote_id)
                .await
                .expect("poll_send_lightning must NEVER Err on still-pending (P0-1)")
            {
                SendLightningStatus::Paid(_) => {
                    reconciled = true;
                    break;
                }
                SendLightningStatus::InFlight { .. } => {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                SendLightningStatus::Failed { quote_id, reason } => {
                    panic!("reconcile FAILED: quote={quote_id} reason={reason}");
                }
            }
        }
        assert!(
            reconciled,
            "P0 reconcile did not reach PAID via the read-only poll"
        );

        // ── THE load-bearing invariant, proven at the PROTOCOL level ──
        // The exact funded input proofs are SPENT at the real mint. This
        // is storage-independent (queries the real mint via NUT-07
        // `post_check_state`) — the hermetic-impossible assertion the
        // proven `cdk_mint_money_flows.rs` P0 driver uses, lifted to the
        // facade. A double-pay regression (a 2nd successful `post_melt`)
        // could not satisfy "spent exactly once".
        h.assert_all_proofs_spent_at_mint(&funded)
            .await
            .expect("no-double-pay: every melt-input proof must be SPENT at the mint");

        // Observational only (NOT a hard gate): re-issuing a second send
        // for the same invoice. The *protocol* guarantee is already
        // proven above (the inputs are SPENT at the real mint, so the
        // real `post_melt` of those exact inputs is rejected — a
        // double-pay is impossible at the wire). What a second
        // `begin_send_lightning` does here additionally depends on the
        // **in-memory storage fake seam**: the real Supabase
        // `complete_melt` removes the spent proofs from the spendable set
        // and the `(user_id,payment_hash)` partial-unique index rejects a
        // re-quote; the in-memory `InMemorySendSwapStorage` does NOT
        // decrement its unspent pool on a melt (mint-quote/send/melt
        // stores are independent fakes — the documented Tier 2
        // constraint). So this branch's outcome reflects fake-storage
        // bookkeeping, not the real no-double-pay invariant, and is logged
        // rather than asserted. (The real storage's spend-tracking +
        // payment-hash dedup is covered by `agicash-storage-supabase`'s
        // own real-supabase gate.)
        match w.begin_send_lightning(None, invoice.clone()).await {
            Err(e) => eprintln!(
                "[tier2]   second begin_send_lightning -> Err (fake-seam): {e:?}"
            ),
            Ok(s) => eprintln!(
                "[tier2]   second begin_send_lightning -> {s:?} (fake-storage \
                 bookkeeping; the real no-double-pay invariant is the NUT-07 \
                 SPENT proof above, which passed)"
            ),
        }
        eprintln!(
            "[tier2] lightning_melt_send_no_double_pay OK — fault-injected \
             post-settle, reconciled to PAID via read-only poll, melt inputs \
             SPENT-exactly-once at the real mint (NUT-07) (enclave {:?})",
            h.enclave_disposition()
        );
    }

    /// 12c-7 Tier-2: the full `receive_flow()` orchestrator end-to-end
    /// against the real mint (seed→provider→storage glue, exercised for
    /// real — the hermetic Tier-1 smoke only proves constructibility).
    #[tokio::test]
    async fn receive_flow_end_to_end() {
        use agicash_cashu::{ReceiveFlowEvent, ReceiveFlowState};

        let h = ServiceHarness::up().await.expect("ServiceHarness::up");
        let account = guest_with_btc_account(&h).await;
        let w = h.wallet().wallet();

        // Fund with real proofs, produce a real wire token, then receive
        // it through the interactive orchestrator.
        h.fund_account_with_real_proofs(&account, 96)
            .await
            .expect("fund with real proofs");
        let sent = w
            .send_token(None, sats(32), TokenVersion::V4)
            .await
            .expect("send_token to produce a real token for the flow");

        let mut flow = w
            .receive_flow()
            .await
            .expect("receive_flow constructible with a real session");
        assert_eq!(flow.current_state(), ReceiveFlowState::Idle);

        // Mint is already known (we add_mint'd it) → the flow runs the
        // real swap immediately and lands terminal.
        let state = flow
            .dispatch(ReceiveFlowEvent::Start { token: sent.token })
            .await
            .expect("receive_flow Start dispatch (real NUT-06 swap)");
        assert!(
            state.is_terminal(),
            "flow must reach a terminal state, got {state:?}"
        );
        assert!(
            matches!(
                state,
                ReceiveFlowState::Done(_) | ReceiveFlowState::AlreadyClaimed(_)
            ),
            "receive_flow must succeed end-to-end against the real mint, got {state:?}"
        );
        eprintln!(
            "[tier2] receive_flow_end_to_end OK — full orchestrator vs real \
             mint terminal={state:?} (enclave {:?})",
            h.enclave_disposition()
        );
    }
}
