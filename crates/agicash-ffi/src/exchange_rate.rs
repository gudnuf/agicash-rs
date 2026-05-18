//! FFI exchange-rate value type.
//!
//! Wrapper around `agicash_exchange_rate`'s slice-4 provider
//! (`ExchangeRateProvider::get_rate` -> `rust_decimal::Decimal`)
//! flattened into a Swift-codable primitive. The structural cousin of
//! [`crate::melt_quote`] (one record + one error-mapper + their unit
//! tests), mirrored for the read-only exchange-rate surface.
//!
//! The Rust-side provider returns a bare `Decimal` price for a
//! `(from, to)` `Currency` pair. The Swift consumer doesn't thread
//! Rust's `Decimal` through the FFI boundary, so the snapshot
//! decimal-stringifies the rate the same way the
//! [`crate::receive::ReceiveResult`] / [`crate::melt_quote::MeltQuotePreview`]
//! records stringify their `Money` fields. One record covers the
//! surface:
//!
//! - [`ExchangeRateSnapshot`] — returned from
//!   [`crate::wallet::AgicashWallet::get_exchange_rate`]. Carries the
//!   decimal-stringified rate plus the echoed `from` / `to` currency
//!   codes so the iOS converted-display layer can label the figure
//!   without re-deriving the pair it asked for.
//!
//! The core provider exposes no timestamp (the slice-4
//! `mempool.space` impl drops the response's `time` field), so the
//! snapshot carries none — adding one later is a core-side change, not
//! an FFI one.

/// A single exchange-rate reading for one currency pair.
///
/// `rate` is decimal-stringified to match the
/// [`crate::receive::ReceiveResult`] / [`crate::melt_quote::MeltQuotePreview`]
/// convention so Swift consumers don't thread Rust's `Decimal`
/// through the FFI boundary. It is the price of `1` major unit of
/// `from` denominated in major units of `to` — e.g.
/// `get_exchange_rate("BTC", "USD")` yields the BTC->USD price
/// (~50000 today); `get_exchange_rate("USD", "BTC")` yields its
/// 8-dp inverse.
///
/// `from` / `to` echo the requested pair as canonical upper-case
/// currency codes (`BTC`, `USD`, `USDB`) — the iOS converted-display
/// layer labels the figure off these instead of re-deriving the pair
/// it asked for. They are the parsed-and-normalised forms, so a
/// lower-case request comes back upper-cased.
#[derive(Debug, Clone, uniffi::Record)]
pub struct ExchangeRateSnapshot {
    /// Price of `1` major unit of `from` in major units of `to`.
    /// Decimal-stringified (matches the `ReceiveResult.amount`
    /// convention).
    pub rate: String,
    /// Canonical upper-case source currency code (`BTC`, `USD`,
    /// `USDB`).
    pub from: String,
    /// Canonical upper-case target currency code (`BTC`, `USD`,
    /// `USDB`).
    pub to: String,
}
