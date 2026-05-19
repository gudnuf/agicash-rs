//! Pure NUT-07 send-claim decision helper.
//!
//! Extracted as a free function (no storage / provider / auth) so the
//! load-bearing correctness — "the receiver has claimed iff EVERY
//! proofs_to_send entry is SPENT on the mint" — is unit-testable without the
//! (absent) full `WalletClient` test harness. Mirrors the FFI's inline
//! all-SPENT predicate (`agicash-ffi/src/wallet.rs:1399-1403` @ `241e8194`:
//! `!resp.states.is_empty() && resp.states.iter().all(|s| matches!(s.state,
//! State::Spent))`).

use cdk::nuts::State;

/// `true` iff the mint reports EVERY checked proof as `Spent` AND at least
/// one proof was checked. An empty response (`states.is_empty()`) is treated
/// as NOT-claimed (Pending) — exactly the FFI semantics: an empty result is
/// never interpreted as "all spent".
#[must_use]
pub fn all_proofs_spent(states: &[State]) -> bool {
    !states.is_empty() && states.iter().all(|s| matches!(s, State::Spent))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdk::nuts::State;

    #[test]
    fn empty_states_is_not_claimed() {
        // FFI parity: an empty post_check_state response is Pending, never
        // mis-read as "all spent".
        assert!(!all_proofs_spent(&[]));
    }

    #[test]
    fn all_spent_is_claimed() {
        assert!(all_proofs_spent(&[State::Spent, State::Spent]));
    }

    #[test]
    fn any_unspent_is_not_claimed() {
        assert!(!all_proofs_spent(&[State::Spent, State::Unspent]));
    }

    #[test]
    fn any_pending_is_not_claimed() {
        assert!(!all_proofs_spent(&[State::Spent, State::Pending]));
    }

    #[test]
    fn single_spent_is_claimed() {
        assert!(all_proofs_spent(&[State::Spent]));
    }
}
