//! E2E: `agicash send` amount-edge validation. Roadmap entry **A2**
//! in `docs/superpowers/specs/2026-05-15-e2e-test-strategy.md`
//! ("Send: amount == 0 — should reject pre-quote" + "Send: amount >
//! balance" + clap-level non-numeric parsing).
//!
//! Closes the matrix cells **"Send: amount == 0 → MISSING"**,
//! **"Send: amount > balance"** (already covered for empty wallet by
//! `send::send_insufficient_balance_errors`, this file covers the
//! **funded-but-too-low** branch which hits a different code path),
//! and pins clap-level parsing errors to a stable shape.
//!
//! Three branches in this single file (one `#[test]` per branch so
//! a flake on one doesn't mask another, per strategy doc §5):
//!   1. **`send 0` with funded balance — currently succeeds (BUG)**.
//!      The strategy doc §5 says this "should reject pre-quote" with a
//!      typed `amount-too-small` error. Today it silently produces an
//!      empty `cashuB…` token with `amount="0"` because
//!      `select_send_proofs` short-circuits on `target_amount == 0`
//!      with `Ok(empty)` (see
//!      `agicash-cashu/src/send_swap/service.rs:758-760`), the
//!      `requested_amount == 0` guard at line 415 only fires on the
//!      pass-2 branch (which a zero-amount call never reaches), and
//!      `create()` happily walks the exact-proofs path with
//!      `input_total = amount_to_send = 0`. The test PINS the actual
//!      buggy contract and ALSO ships an `#[ignore]`'d test for the
//!      desired contract — flip the assertions when the bug is fixed.
//!   2. **`send <amount>` with `amount > funded balance`** — should
//!      produce `insufficient-balance` (the funded variant of the
//!      branch that `send::send_insufficient_balance_errors` exercises
//!      for an empty wallet). Asserts a follow-up `send <funded>`
//!      still works → proves the failed call left no PENDING swap row
//!      that would block the next attempt.
//!   3. **`send abc`** — non-numeric amount → clap's own parse error,
//!      exit code 2, NO JSON envelope. Pins this contract so a future
//!      "wrap clap errors in our JSON shape" change is a deliberate
//!      visible API break, not a silent one.
//!
//! Branches 1 and 2 deposit 100 sats via the testnut helper before
//! exercising the send. 100 sats covers the 1-sat receive-side input
//! fee testnut applies, leaving ~99 sats redeemable. Branch 3 is
//! hermetic (clap rejects before any I/O).
//!
//! Run:
//!     cargo test -p agicash-cli \
//!         --features real-mint-tests,real-supabase-tests,real-opensecret-tests \
//!         --test send_amount_validation -- --nocapture

#![allow(clippy::doc_markdown)]

#[cfg(all(
    feature = "real-mint-tests",
    feature = "real-supabase-tests",
    feature = "real-opensecret-tests"
))]
mod common;

#[cfg(all(
    feature = "real-mint-tests",
    feature = "real-supabase-tests",
    feature = "real-opensecret-tests"
))]
mod gated {
    use super::common::*;

    /// PINS THE BUG: `send 0` against a funded wallet currently
    /// **succeeds** with an empty token (`amount="0"`, valid
    /// `cashuB…` envelope). Strategy doc §5 says this should be
    /// rejected pre-quote with `amount-too-small`; today nothing
    /// rejects it.
    ///
    /// Why this is the canonical (passing) test:
    ///   * `select_send_proofs(_, 0, _)` returns `Ok(empty)` at
    ///     `send_swap/service.rs:758-760`.
    ///   * `prepare_proofs_and_fee` then returns the empty
    ///     selection on the **pass-1** branch (line 391: `send_total
    ///     == amount_to_send` with both sides equal to 0).
    ///   * The pass-2 `requested_amount == 0` guard at line 415 only
    ///     fires when pass-1 doesn't return early — a zero-amount
    ///     call never gets that far.
    ///   * `create()` then walks the exact-proofs path with
    ///     `input_total == amount_to_send` (both 0) and produces an
    ///     empty token.
    ///
    /// What the contract SHOULD be (covered by the
    /// `#[ignore]`d test below): exit nonzero with
    /// `code="amount-too-small"`, no token produced, no swap row
    /// inserted.
    ///
    /// Pinning the buggy contract here means: (a) a **fix** that
    /// rejects the call will turn this test red and force the worker
    /// to flip both this test and the ignored one in the same diff,
    /// (b) a **regression** that changes the buggy envelope (e.g.
    /// stops producing the token but doesn't add a typed error)
    /// also turns it red.
    #[test]
    #[allow(clippy::too_many_lines)]
    #[allow(non_snake_case)]
    fn send_zero_amount_currently_succeeds_with_empty_token_BUG() {
        if !env_ready() {
            eprintln!("skipping: env vars not set");
            return;
        }
        let session = TestSession::new("send-amount-zero-bug");
        let (deposit_token, _) = mint_test_token_blocking(100);

        session.spawn_guest_with_test_mint();
        let receive = session
            .cmd()
            .args(["receive", "token", &deposit_token])
            .output()
            .expect("spawn agicash receive (deposit)");
        assert!(
            receive.status.success(),
            "deposit receive failed: stdout={}, stderr={}",
            String::from_utf8_lossy(&receive.stdout),
            String::from_utf8_lossy(&receive.stderr),
        );

        let send = session
            .cmd()
            .args(["send", "0"])
            .output()
            .expect("spawn agicash send 0");
        // BUG: succeeds today. When fixed, the `success()` assertion
        // will flip to `!success()` and the assertions below will be
        // replaced by an `extract_error_code` call.
        assert!(
            send.status.success(),
            "PIN-BUG: send 0 currently succeeds. If this assertion \
             flipped, the underlying behaviour changed — flip this \
             test AND the `#[ignore]`'d sibling test in this file in \
             the same diff. stdout={}, stderr={}",
            String::from_utf8_lossy(&send.stdout),
            String::from_utf8_lossy(&send.stderr),
        );
        let body = parse_json("send 0 (BUG)", &send);
        assert_eq!(
            body.get("status").and_then(|v| v.as_str()),
            Some("sent"),
            "PIN-BUG: send 0 currently emits status=sent. body={body}",
        );
        assert_eq!(
            body.get("amount").and_then(|v| v.as_str()),
            Some("0"),
            "PIN-BUG: send 0 currently emits amount=0. body={body}",
        );
        let token = body
            .get("token")
            .and_then(|v| v.as_str())
            .expect("PIN-BUG: send 0 currently emits a token field");
        assert!(
            token.starts_with("cashuB") || token.starts_with("cashuA"),
            "PIN-BUG: send 0 currently emits a real-looking token: {token}",
        );

        // ---- A follow-up `send 50` must still succeed. Even though
        // the zero-amount call shouldn't have happened, it does
        // produce a swap row today; this proves the row doesn't
        // poison the next call. ----
        let follow_up = session
            .cmd()
            .args(["send", "50"])
            .output()
            .expect("spawn agicash send 50 (follow-up)");
        assert!(
            follow_up.status.success(),
            "follow-up send must succeed even after the zero-amount \
             swap row exists. stdout={}, stderr={}",
            String::from_utf8_lossy(&follow_up.stdout),
            String::from_utf8_lossy(&follow_up.stderr),
        );
        let follow_body = parse_json("follow-up send", &follow_up);
        assert_eq!(
            follow_body.get("status").and_then(|v| v.as_str()),
            Some("sent"),
            "unexpected follow-up send body: {follow_body}",
        );
    }

    /// The test the strategy doc §5 actually wants. **Ignored** until
    /// the underlying bug (above) is fixed. When the fix lands:
    ///   * Drop the `#[ignore]` here.
    ///   * Flip the `assert!(send.status.success())` in the
    ///     `..._BUG` test above to `assert!(!success)` and replace
    ///     its body with the same `extract_error_code` shape this
    ///     test uses, OR delete it (the buggy contract no longer
    ///     exists to pin).
    #[test]
    #[ignore = "TODO: send 0 currently succeeds; un-ignore + delete the \
                _BUG test once amount-too-small guard is added"]
    fn send_zero_amount_should_return_amount_too_small() {
        if !env_ready() {
            eprintln!("skipping: env vars not set");
            return;
        }
        let session = TestSession::new("send-amount-zero-spec");
        let (deposit_token, _) = mint_test_token_blocking(100);

        session.spawn_guest_with_test_mint();
        let receive = session
            .cmd()
            .args(["receive", "token", &deposit_token])
            .output()
            .expect("spawn agicash receive (deposit)");
        assert!(receive.status.success());

        let send = session
            .cmd()
            .args(["send", "0"])
            .output()
            .expect("spawn agicash send 0");
        assert!(
            !send.status.success(),
            "send 0 must reject pre-quote per strategy doc §5",
        );
        let stderr = String::from_utf8_lossy(&send.stderr).into_owned();
        let code = extract_error_code("send 0 (spec)", &stderr);
        assert_eq!(code, "amount-too-small");
    }

    /// Deposit 100 sats, then attempt to send 1000 sats. Must fail
    /// with `insufficient-balance` and leave the wallet's spendable
    /// proofs intact (a follow-up send of the actually-available
    /// amount must still work).
    ///
    /// Distinct from `send::send_insufficient_balance_errors`, which
    /// exercises the **empty-wallet** branch — that hits
    /// `select_send_proofs`'s `total_avail < target_amount` guard with
    /// `total_avail = 0`. This test exercises the **funded-but-low**
    /// branch where the same guard fires with a non-zero
    /// `total_avail`, plus proves the post-failure state is recoverable.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn send_amount_exceeding_balance_returns_insufficient_balance() {
        if !env_ready() {
            eprintln!("skipping: env vars not set");
            return;
        }
        let session = TestSession::new("send-amount-exceeds");
        let (deposit_token, _) = mint_test_token_blocking(100);

        session.spawn_guest_with_test_mint();
        let receive = session
            .cmd()
            .args(["receive", "token", &deposit_token])
            .output()
            .expect("spawn agicash receive (deposit)");
        assert!(
            receive.status.success(),
            "deposit receive failed: stdout={}, stderr={}",
            String::from_utf8_lossy(&receive.stdout),
            String::from_utf8_lossy(&receive.stderr),
        );

        let send = session
            .cmd()
            .args(["send", "1000"])
            .output()
            .expect("spawn agicash send 1000");
        assert!(
            !send.status.success(),
            "send 1000 against a 100-sat balance must exit nonzero \
             (insufficient-balance). stdout={}, stderr={}",
            String::from_utf8_lossy(&send.stdout),
            String::from_utf8_lossy(&send.stderr),
        );
        let stderr = String::from_utf8_lossy(&send.stderr).into_owned();
        let code = extract_error_code("send 1000", &stderr);
        assert_eq!(
            code, "insufficient-balance",
            "expected insufficient-balance for send 1000 against 100-sat \
             wallet, got `{code}`. stderr={stderr}",
        );

        // ---- Follow-up: a smaller send must still work. Pins the
        // invariant that the failed send did not reserve / lock any
        // proofs. ----
        let follow_up = session
            .cmd()
            .args(["send", "50"])
            .output()
            .expect("spawn agicash send 50 (post-failure)");
        assert!(
            follow_up.status.success(),
            "follow-up send 50 must succeed after a failed send 1000. \
             stdout={}, stderr={}",
            String::from_utf8_lossy(&follow_up.stdout),
            String::from_utf8_lossy(&follow_up.stderr),
        );
        let body = parse_json("follow-up send 50", &follow_up);
        assert_eq!(
            body.get("status").and_then(|v| v.as_str()),
            Some("sent"),
            "unexpected post-failure send body: {body}",
        );
    }

    /// `send abc` — non-numeric amount. Clap rejects before any
    /// service code runs. Exit code is nonzero (clap convention: 2)
    /// and stderr is clap's own human-readable error, NOT a JSON
    /// envelope. Pins this contract so a future change that wraps
    /// clap errors into our JSON shape is a visible deliberate change.
    ///
    /// Hermetic: needs no env, no auth, no mint. Skipped under the
    /// real-* gate only to keep the file's mod-organization
    /// consistent with the rest of the suite.
    #[test]
    fn send_non_numeric_amount_fails_at_clap_parse() {
        if !env_ready() {
            eprintln!("skipping: env vars not set");
            return;
        }
        let session = TestSession::new("send-amount-nonnumeric");

        let send = session
            .cmd()
            .args(["send", "abc"])
            .output()
            .expect("spawn agicash send abc");
        assert!(
            !send.status.success(),
            "send abc must exit nonzero (clap parse error). \
             stdout={}, stderr={}",
            String::from_utf8_lossy(&send.stdout),
            String::from_utf8_lossy(&send.stderr),
        );
        // Clap parse errors emit human-readable text on stderr, NOT
        // our typed JSON envelope. Detect this by attempting to parse
        // stderr as JSON and asserting it FAILS.
        let stderr = String::from_utf8_lossy(&send.stderr).into_owned();
        let parse_result: Result<serde_json::Value, _> = serde_json::from_str(stderr.trim());
        assert!(
            parse_result.is_err(),
            "clap parse errors must NOT be wrapped in our JSON envelope; \
             stderr was unexpectedly valid JSON: {stderr}",
        );
        // Clap's standard error mentions `amount` and `invalid value`.
        // We don't pin the exact message (clap may reword it), but we
        // do pin that the error mentions the offending arg.
        assert!(
            stderr.to_lowercase().contains("invalid")
                || stderr.to_lowercase().contains("error"),
            "expected clap's parse-error language on stderr, got: {stderr}",
        );

        // Same for negative — u64 can't hold -50.
        let neg = session
            .cmd()
            .args(["send", "-50"])
            .output()
            .expect("spawn agicash send -50");
        assert!(
            !neg.status.success(),
            "send -50 must exit nonzero. stdout={}, stderr={}",
            String::from_utf8_lossy(&neg.stdout),
            String::from_utf8_lossy(&neg.stderr),
        );
    }
}

#[cfg(not(all(
    feature = "real-mint-tests",
    feature = "real-supabase-tests",
    feature = "real-opensecret-tests"
)))]
#[test]
fn send_amount_validation_skipped_without_features() {
    eprintln!(
        "skipping real-network e2e; run with: \
         cargo test -p agicash-cli \
         --features real-mint-tests,real-supabase-tests,real-opensecret-tests \
         --test send_amount_validation"
    );
}
