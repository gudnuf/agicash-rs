//! Centralized facade-type ↔ `*Wasm` conversions + arg parsers + the
//! `WalletError` → `JsValue` mapper. Structural mirror of
//! `crates/agicash-ffi/src/convert.rs` (`ffi::convert`, 12b-1 Task 4):
//! same facade source types, same parse/`amount_to_money` semantics,
//! `JsValue` instead of `FfiError` at the error boundary.
#![cfg(target_arch = "wasm32")]

use crate::types::AuthStatusWasm;
use agicash_domain::{AccountId, Currency};
use agicash_money::{Money, Unit};
use agicash_wallet::WalletError;
use rust_decimal::Decimal;
use std::str::FromStr;
use uuid::Uuid;
use wasm_bindgen::JsValue;

/// JS-side error string UUID → `AccountId`. Bad UUID → a JS `Error`
/// carrying the same message shape `ffi::convert::parse_account_id`
/// used (`"invalid account_id: <e>"`).
pub fn parse_account_id(s: &str) -> Result<AccountId, JsValue> {
    Uuid::parse_str(s.trim())
        .map(AccountId::from)
        .map_err(|e| JsValue::from_str(&format!("invalid account_id: {e}")))
}

pub fn parse_opt_account_id(s: Option<String>) -> Result<Option<AccountId>, JsValue> {
    match s {
        Some(v) => Ok(Some(parse_account_id(&v)?)),
        None => Ok(None),
    }
}

/// Optional currency code (default `"BTC"` on `None`) → `Currency`.
/// Mirrors `ffi::convert::parse_currency`.
pub fn parse_currency(c: Option<String>) -> Result<Currency, JsValue> {
    let s = c.unwrap_or_else(|| "BTC".to_string());
    Currency::from_str(&s).map_err(|_| JsValue::from_str(&format!("unsupported currency: {s}")))
}

/// FFI minor-unit `u64` + currency → `Money`. Byte-identical to
/// `ffi::convert::amount_to_money` (sat for BTC, cent for USD/USDB).
#[must_use]
pub fn amount_to_money(amount: u64, currency: Currency) -> Money {
    let unit = match currency {
        Currency::Btc => Unit::Sat,
        Currency::Usd | Currency::Usdb => Unit::Cent,
    };
    Money::new(Decimal::from(amount), currency, unit)
}

/// `WalletError` → `JsValue`. Mirrors `ffi::convert::wallet_error_to_ffi`'s
/// *intent* (preserve the discriminator-bearing Display string the UI
/// branches on) at the wasm boundary: a JS `Error` whose message is the
/// `WalletError` Display string. The Leptos layer pattern-matches the
/// message exactly as iOS parses the FFI string today (no behavior
/// change vs. the mocked path it replaces — the mock returned a string,
/// this returns the real one). Structured-code granularity is NOT a
/// 12d concern (it is 12b-2 for the FFI shell; the wasm shell mirrors
/// today's string-funnel behavior).
#[must_use]
pub fn wallet_error_to_js(e: WalletError) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// Facade `AuthStatus` → `AuthStatusWasm`. Mirror of the FFI
/// `auth_status` inline mapping (`agicash-ffi/src/wallet.rs`): `user_id:
/// Option<UserId>` stringified.
#[must_use]
pub fn auth_status_from_facade(s: &agicash_wallet::AuthStatus) -> AuthStatusWasm {
    AuthStatusWasm {
        logged_in: s.logged_in,
        user_id: s.user_id.map(|u| u.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn parse_account_id_ok() {
        let u = Uuid::new_v4();
        assert_eq!(
            parse_account_id(&u.to_string()).unwrap(),
            AccountId::from(u)
        );
    }

    #[wasm_bindgen_test]
    fn parse_account_id_bad_is_err() {
        assert!(parse_account_id("nope").is_err());
    }

    #[wasm_bindgen_test]
    fn parse_currency_defaults_btc() {
        assert_eq!(parse_currency(None).unwrap(), Currency::Btc);
    }

    #[wasm_bindgen_test]
    fn amount_to_money_btc_is_sat() {
        let m = amount_to_money(1000, Currency::Btc);
        assert_eq!(m.currency(), Currency::Btc);
    }

    #[wasm_bindgen_test]
    fn wallet_error_message_preserved() {
        let js = wallet_error_to_js(WalletError::Cashu("boom".into()));
        assert!(js.as_string().unwrap().contains("boom"));
    }
}
