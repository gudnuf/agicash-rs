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

/// Minor-unit label for a currency. `"sat"` for BTC, `"cent"` for
/// USD/USDB — verbatim `ffi::convert::unit_label`.
#[must_use]
fn unit_label(c: Currency) -> &'static str {
    match c {
        Currency::Btc => "sat",
        Currency::Usd | Currency::Usdb => "cent",
    }
}

/// Facade `AccountSummary` → `AccountWasm`. Mirror of
/// `ffi::convert::account_ffi_from_summary`: stringified id,
/// `account_type`/`currency` Display labels, `mint_url` passthrough,
/// decimal balance, currency-derived unit label.
#[must_use]
pub fn account_wasm_from_summary(s: &agicash_wallet::AccountSummary) -> crate::types::AccountWasm {
    crate::types::AccountWasm {
        id: s.id.to_string(),
        name: s.name.clone(),
        account_type: s.account_type.to_string(),
        currency: s.currency.to_string(),
        mint_url: s.mint_url.clone(),
        balance: s.balance.clone(),
        unit: unit_label(s.currency).to_string(),
    }
}

/// Facade `SendTokenQuote` → `SendQuotePreviewWasm`. Mirrors the FFI
/// `SendQuotePreview` Money→String shape for the **field-complete
/// subset** only — `mint_url` is omitted (note ◇: facade
/// `SendTokenQuote` has no `mint_url`; the FFI reconstructs it off the
/// picked account, which is 12b-2/12c facade-surface work). `unit` /
/// `currency` are derived from `amount_requested` (all the quote's
/// Money fields share the account's unit/currency, exactly as the FFI
/// derives them).
#[must_use]
pub fn send_quote_preview_from_facade(
    q: &agicash_wallet::SendTokenQuote,
) -> crate::types::SendQuotePreviewWasm {
    crate::types::SendQuotePreviewWasm {
        amount_requested: q.amount_requested.amount().to_string(),
        amount_to_send: q.amount_to_send.amount().to_string(),
        total_amount: q.total_amount.amount().to_string(),
        total_fee: q.total_fee.amount().to_string(),
        cashu_send_fee: q.cashu_send_fee.amount().to_string(),
        cashu_receive_fee: q.cashu_receive_fee.amount().to_string(),
        unit: q.amount_requested.unit().to_string(),
        currency: q.amount_requested.currency().to_string(),
        account_id: q.account_id.to_string(),
    }
}

/// Facade `Session` → `SessionWasm`. Mirror of
/// `ffi::convert::session_from_facade`.
#[must_use]
pub fn session_from_facade(s: &agicash_wallet::Session) -> crate::types::SessionWasm {
    crate::types::SessionWasm {
        user_id: s.user_id.to_string(),
        refresh_token: s.refresh_token.clone(),
    }
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

    #[wasm_bindgen_test]
    fn account_wasm_from_summary_maps_all_fields_and_derives_unit() {
        let id = Uuid::new_v4();
        let summary = agicash_wallet::AccountSummary {
            id: AccountId::from(id),
            user_id: agicash_domain::UserId::from(Uuid::new_v4()),
            name: "My Mint".into(),
            account_type: agicash_domain::AccountType::Cashu,
            currency: Currency::Btc,
            mint_url: Some("https://mint.example".into()),
            balance: "1234".into(),
        };
        let w = account_wasm_from_summary(&summary);
        assert_eq!(w.id, id.to_string());
        assert_eq!(w.name, "My Mint");
        assert_eq!(w.account_type, "cashu");
        assert_eq!(w.currency, "BTC");
        assert_eq!(w.mint_url, Some("https://mint.example".to_string()));
        assert_eq!(w.balance, "1234");
        assert_eq!(w.unit, "sat");

        let usd = agicash_wallet::AccountSummary {
            currency: Currency::Usd,
            ..summary
        };
        assert_eq!(account_wasm_from_summary(&usd).unit, "cent");
    }

    #[wasm_bindgen_test]
    fn send_quote_preview_maps_field_complete_subset_no_mint_url() {
        let aid = Uuid::new_v4();
        let m = |n: u64| Money::new(Decimal::from(n), Currency::Btc, Unit::Sat);
        let q = agicash_wallet::SendTokenQuote {
            amount_requested: m(100),
            amount_to_send: m(101),
            total_amount: m(103),
            total_fee: m(3),
            cashu_send_fee: m(2),
            cashu_receive_fee: m(1),
            account_id: AccountId::from(aid),
        };
        let w = send_quote_preview_from_facade(&q);
        assert_eq!(w.amount_requested, "100");
        assert_eq!(w.amount_to_send, "101");
        assert_eq!(w.total_amount, "103");
        assert_eq!(w.total_fee, "3");
        assert_eq!(w.cashu_send_fee, "2");
        assert_eq!(w.cashu_receive_fee, "1");
        assert_eq!(w.unit, "sat");
        assert_eq!(w.currency, "BTC");
        assert_eq!(w.account_id, aid.to_string());
    }

    #[wasm_bindgen_test]
    fn session_from_facade_stringifies_user_id() {
        let uid = Uuid::new_v4();
        let fs = agicash_wallet::Session {
            user_id: agicash_domain::UserId::from(uid),
            refresh_token: "rt.x".into(),
        };
        let w = session_from_facade(&fs);
        assert_eq!(w.user_id, uid.to_string());
        assert_eq!(w.refresh_token, "rt.x");
    }
}
