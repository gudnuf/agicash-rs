//! Test support for the two-tier `WalletClient` strategy.
//!
//! - `fakes`: in-memory impls (Tier 1, both targets, no I/O, no docker,
//!   no spawned services). Wasm32-clean.
//! - `harness`: the Tier 2 real-service lifecycle (Tasks 4-7 of
//!   `plans/2026-05-19-fake-harness-mini-slice.md`) — real `cdk-mintd` +
//!   real `OpenSecret` enclave + standing nix-pg, in-memory storage, **no
//!   docker**. Gated `#[cfg(all(feature = "tier2-e2e",
//!   not(target_arch = "wasm32")))]`: native + opt-in only.
//!
//! The Tier 1 surface is wasm32-clean: no real-socket / process / docker
//! deps, so `agicash-testing` and its dependents build for
//! `wasm32-unknown-unknown` (the `harness` module is excluded there).

pub mod fakes;
pub mod wallet;

#[cfg(all(feature = "tier2-e2e", not(target_arch = "wasm32")))]
pub mod harness;

pub use fakes::auth::FakeAuthClient;
pub use fakes::cashu_storage::{
    InMemoryMeltQuoteStorage, InMemoryMintQuoteStorage, InMemoryReceiveSwapStorage,
    InMemorySendSwapStorage,
};
pub use fakes::misc::{FixedExchangeRate, StubCashuProvider};
pub use fakes::user_storage::{cashu_account, InMemoryUserStorage};
pub use wallet::TestWallet;

#[cfg(all(feature = "tier2-e2e", not(target_arch = "wasm32")))]
pub use harness::{EnclaveProcess, MintProcess, RealWallet, ServiceHarness};
