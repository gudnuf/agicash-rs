//! Tier 2 real-service lifecycle harness (no docker).
//!
//! The whole module is `#[cfg(all(feature = "tier2-e2e",
//! not(target_arch = "wasm32")))]` (set in `lib.rs`): it spawns / probes
//! OS processes (`cdk-mintd`, the `OpenSecret` enclave) and talks real
//! sockets, so it is native + opt-in only. The default Tier 1 build and
//! every wasm32 build never compile it.
//!
//! Encapsulates the already-proven bring-up recipes — it does NOT
//! reinvent them:
//! - `scripts/cdk-mint-e2e.sh` (the `FakeWallet` `cdk-mintd` TOML +
//!   binary-resolution order + `/v1/info` readiness probe).
//! - `project_opensecret_local_stack` + `reference_jwt_chain_recipe`
//!   (the host `cargo run --bin opensecret` enclave on `:3999`, the
//!   standing nix-native postgres on `:5432`, the idempotent
//!   `seed-project-secret` JWT seed).
//!
//! Lifecycle (per the plan §Service-Lifecycle-Harness):
//! `ServiceHarness::up()` → assert nix-pg `:5432` → enclave
//! *probe-first* (attach if healthy, cold-spawn only if down) → seed
//! JWT (idempotent) → spawn `cdk-mintd` into a tempdir → compose a real
//! `Arc<WalletClient>` via `WalletClientBuilder` (real auth + real
//! `CdkCashuProvider` + in-memory storages). `Drop` tears down ONLY
//! processes the harness itself spawned; an *attached* standing enclave
//! is never killed.

pub mod service;
pub mod wallet;

pub use service::{EnclaveProcess, MintProcess, ServiceHarness};
pub use wallet::RealWallet;
