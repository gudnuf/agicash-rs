//! In-memory fakes for the Tier 1 hermetic suite.
//!
//! Every type here is pure-Rust, I/O-free, and wasm32-clean: no network,
//! no docker, no spawned services. They compose an `Arc<WalletClient>`
//! through the public `WalletClientBuilder` so the facade glue (auth-seam
//! short-circuits, ownership checks, `receive_flow` construction) can be
//! exercised without any backend.

pub mod auth;
pub mod cashu_storage;
pub mod misc;
pub mod user_storage;
