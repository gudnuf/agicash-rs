#![allow(clippy::doc_markdown)] // doc comments freely reference CLI/FFI/PWA/MCP product names.
#![allow(clippy::clone_on_copy)] // Money is Copy but explicit clones keep field-builder code uniform.

//! `WalletClient` facade — the public surface every consumer (CLI, FFI,
//! Leptos PWA, MCP server) talks to.
//!
//! Slice 12 (2026-05-17). Composes prior slices (auth seam, accounts,
//! money, exchange rate, cashu send/receive, mint/melt quote, lightning
//! address) behind a single `Arc<WalletClient>` with a stable ~25-method
//! async API. Pre-existing per-feature crates stay untouched; the facade
//! reaches into them and composes — no refactor lane this slice.
//!
//! # Construction
//!
//! ```ignore
//! use agicash_wallet::WalletClientBuilder;
//! let wallet = WalletClientBuilder::new()
//!     .auth(my_auth_client)
//!     .user_storage(my_user_storage)
//!     .cashu_provider(my_cashu_provider)
//!     .cashu_receive_storage(my_receive_storage)
//!     .cashu_send_storage(my_send_storage)
//!     .cashu_mint_quote_storage(my_mint_quote_storage)
//!     .cashu_melt_quote_storage(my_melt_quote_storage)
//!     .exchange_rate(my_exchange_rate)
//!     .build()?;
//! ```
//!
//! The concrete wiring (Supabase, OpenSecret, CDK) lives in each consumer's
//! composition root — `agicash-cli/src/composition.rs`,
//! `agicash-ffi/src/wallet.rs`, the Leptos PWA's SSR module. Slice 12
//! ships the facade trait surface; the per-consumer migration is a follow-up.
//!
//! # Slice-12 scope notes
//!
//! - **Spark (slice 9-10) deferred.** `WalletClient::list_accounts`
//!   reports Spark accounts with balance `0`; Spark-targeted send/receive
//!   methods don't exist on the facade yet.
//! - **Event bus (slice 11) stubbed.** `WalletClient::subscribe()` returns
//!   [`WalletError::Unsupported`] until slice 11 ships `agicash-cache`.
//! - **`list_transactions` / `get_transaction` stubbed.** The unified
//!   `wallet.transactions` storage method ships in a follow-up.
//! - **`set_default_account` / `remove_mint` stubbed.** Both depend on
//!   focused Supabase RPCs not yet present.
//! - **DLEQ verification not enabled in this facade.** Carried forward as
//!   the P0 audit per memory `project_agicash_dleq_gap.md`.

pub mod auth;
pub mod builder;
pub mod client;
pub mod config;
pub mod discriminator;
pub mod error;
pub mod opensecret_auth;
pub mod types;

pub use auth::{AuthClient, Session};
pub use builder::WalletClientBuilder;
pub use client::WalletClient;
pub use config::{SessionStorageChoice, WalletConfig};
pub use discriminator::{AuthErrorCode, CashuDiscriminator};
pub use error::WalletError;
pub use opensecret_auth::OpenSecretAuthClient;
pub use types::{
    AccountSummary, AuthStatus, BalanceSummary, ExchangeRateSnapshot, MintSummary,
    ReceiveLightningHandle, ReceiveLightningSnapshot, ReceiveLightningState, ReceiveReceipt,
    ReceiveStatus, SendLightningHandle, SendLightningQuote, SendLightningReceipt, SendTokenQuote,
    SendTokenReceipt, TokenVersion, Transaction, TransactionDirection, TransactionFilter,
    TransactionPage,
};
