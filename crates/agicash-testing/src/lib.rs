//! Test support for the two-tier `WalletClient` strategy.
//!
//! - `fakes`: in-memory impls (Tier 1, both targets, no I/O, no docker,
//!   no spawned services). This crate currently ships **Tier 1 only**
//!   (Tasks 1-3 of `plans/2026-05-19-fake-harness-mini-slice.md`); the
//!   Tier 2 `harness` module (real `cdk-mintd` + enclave, Tasks 4-7) is
//!   out of scope for this deliverable and not present.
//!
//! Everything here is wasm32-clean: no real-socket / process / docker
//! deps, so `agicash-testing` and its dependents build for
//! `wasm32-unknown-unknown`.

pub mod fakes;
pub mod wallet;

pub use fakes::auth::FakeAuthClient;
pub use fakes::cashu_storage::{
    InMemoryMeltQuoteStorage, InMemoryMintQuoteStorage, InMemoryReceiveSwapStorage,
    InMemorySendSwapStorage,
};
pub use fakes::misc::{FixedExchangeRate, StubCashuProvider};
pub use fakes::user_storage::{cashu_account, InMemoryUserStorage};
pub use wallet::TestWallet;
