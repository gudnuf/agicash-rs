//! Centralized facade-type ↔ `*Wasm` conversions + arg parsers + the
//! `WalletError` → `JsValue` mapper. Structural mirror of
//! `crates/agicash-ffi/src/convert.rs` (`ffi::convert`, 12b-1 Task 4):
//! same facade source types, same parse/`amount_to_money` semantics,
//! `JsValue` instead of `FfiError` at the error boundary.
//
// The module declaration in `lib.rs` is already `#[cfg(target_arch =
// "wasm32")]`-gated; no inner `#![cfg]` here (it would be a duplicate).

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
///
/// Takes `WalletError` by value to mirror the FFI sibling
/// `ffi::convert::wallet_error_to_ffi` and so it can be used as a bare
/// fn-reference in `.map_err(wallet_error_to_js)` across the shell.
/// Switching to `&WalletError` would force every call site into a
/// closure — out of scope for this lint-cleanup pass.
#[must_use]
#[allow(clippy::needless_pass_by_value)]
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

/// Facade `SendTokenReceipt` → `SendSwapHandleWasm`. Verbatim
/// `ffi::convert::send_swap_handle_from_facade`: stringified
/// `swap_id`/`account_id`, decimal `amount`/`fee`, `Money`-derived
/// `unit`/`currency`, token + `mint_url` passthrough (receipt is
/// field-complete — no gap).
#[must_use]
pub fn send_swap_handle_from_facade(
    r: &agicash_wallet::SendTokenReceipt,
) -> crate::types::SendSwapHandleWasm {
    crate::types::SendSwapHandleWasm {
        swap_id: r.swap_id.to_string(),
        token: r.token.clone(),
        amount: r.amount.amount().to_string(),
        fee: r.fee.amount().to_string(),
        unit: r.amount.unit().to_string(),
        currency: r.amount.currency().to_string(),
        account_id: r.account_id.to_string(),
        mint_url: r.mint_url.clone(),
    }
}

/// Facade `ReceiveStatus` → `ReceiveStatusWasm`. 1:1 variant map,
/// verbatim `ffi::convert::receive_status_from_facade`.
#[must_use]
fn receive_status_from_facade(s: agicash_wallet::ReceiveStatus) -> crate::types::ReceiveStatusWasm {
    match s {
        agicash_wallet::ReceiveStatus::Received => crate::types::ReceiveStatusWasm::Received,
        agicash_wallet::ReceiveStatus::AlreadyClaimed => {
            crate::types::ReceiveStatusWasm::AlreadyClaimed
        }
        agicash_wallet::ReceiveStatus::AlreadyFailed => {
            crate::types::ReceiveStatusWasm::AlreadyFailed
        }
        agicash_wallet::ReceiveStatus::Pending => crate::types::ReceiveStatusWasm::Pending,
    }
}

/// Facade `ReceiveReceipt` → `ReceiveResultWasm`. Verbatim
/// `ffi::convert::receive_result_from_receipt`: decimal `amount`/`fee`
/// strings, `Money`-derived `unit`/`currency`, stringified
/// `account_id`, 1:1 status.
#[must_use]
pub fn receive_result_from_receipt(
    r: &agicash_wallet::ReceiveReceipt,
) -> crate::types::ReceiveResultWasm {
    crate::types::ReceiveResultWasm {
        status: receive_status_from_facade(r.status),
        amount: r.amount.amount().to_string(),
        fee: r.fee.amount().to_string(),
        unit: r.amount.unit().to_string(),
        currency: r.amount.currency().to_string(),
        account_id: r.account_id.to_string(),
        mint_url: r.mint_url.clone(),
        token_hash: r.token_hash.clone(),
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

/// `ReceiveFlowStatus` (i.e. `agicash_cashu::ReceiveStatus` re-exported)
/// → `ReceiveStatusWasm`. Three variants; the dedicated
/// `AlreadyClaimed` state has no status — surfaced via the parent
/// `ReceiveFlowStateWasm::AlreadyClaimed` variant instead. Mirrors
/// `ffi::receive_flow::ReceiveStatusFfi::from(ReceiveFlowStatus)`.
#[must_use]
fn receive_flow_status_from_inner(
    s: &agicash_cashu::ReceiveFlowStatus,
) -> crate::types::ReceiveStatusWasm {
    match s {
        agicash_cashu::ReceiveFlowStatus::Received => crate::types::ReceiveStatusWasm::Received,
        agicash_cashu::ReceiveFlowStatus::AlreadyFailed => {
            crate::types::ReceiveStatusWasm::AlreadyFailed
        }
        agicash_cashu::ReceiveFlowStatus::Pending => crate::types::ReceiveStatusWasm::Pending,
    }
}

/// `agicash_cashu::ReceiveFlowResult` → `ReceiveFlowResultWasm`.
/// Mirrors `ffi::receive_flow::ReceiveFlowResultFfi::from`. The
/// inner result already carries decimal strings (matching the FFI's
/// pre-stringified shape), so this is a field-for-field copy with a
/// status remap.
#[must_use]
pub fn receive_flow_result_from_inner(
    r: agicash_cashu::ReceiveFlowResult,
) -> crate::types::ReceiveFlowResultWasm {
    crate::types::ReceiveFlowResultWasm {
        status: receive_flow_status_from_inner(&r.status),
        amount: r.amount,
        fee: r.fee,
        unit: r.unit,
        currency: r.currency,
        account_id: r.account_id,
        mint_url: r.mint_url,
        token_hash: r.token_hash,
    }
}

/// `agicash_cashu::MintConfirmation` → `MintConfirmationWasm`.
/// Verbatim `ffi::receive_flow::MintConfirmationFfi::from`.
#[must_use]
fn mint_confirmation_from_inner(
    m: agicash_cashu::MintConfirmation,
) -> crate::types::MintConfirmationWasm {
    crate::types::MintConfirmationWasm {
        mint_url: m.mint_url,
        mint_name: m.mint_name,
        unit: m.unit,
        currency: m.currency,
        amount: m.amount,
        fee: m.fee,
    }
}

/// `agicash_cashu::AlreadyClaimedInfo` → `AlreadyClaimedInfoWasm`.
/// Verbatim `ffi::receive_flow::AlreadyClaimedInfoFfi::from`.
#[must_use]
fn already_claimed_info_from_inner(
    i: agicash_cashu::AlreadyClaimedInfo,
) -> crate::types::AlreadyClaimedInfoWasm {
    crate::types::AlreadyClaimedInfoWasm {
        unit: i.unit,
        currency: i.currency,
        account_id: i.account_id,
        mint_url: i.mint_url,
        token_hash: i.token_hash,
    }
}

/// `agicash_cashu::ReceiveFlowState` → `ReceiveFlowStateWasm`.
/// Variant-for-variant 1:1 mirror of
/// `ffi::receive_flow::ReceiveFlowStateFfi::from`. The output
/// serializes to a tagged-discriminator JSON object the Leptos layer
/// pattern-matches on.
#[must_use]
pub fn receive_flow_state_from_inner(
    s: agicash_cashu::ReceiveFlowState,
) -> crate::types::ReceiveFlowStateWasm {
    use agicash_cashu::ReceiveFlowState as S;
    match s {
        S::Idle => crate::types::ReceiveFlowStateWasm::Idle,
        S::Parsing => crate::types::ReceiveFlowStateWasm::Parsing,
        S::NeedsMintConfirmation(c) => crate::types::ReceiveFlowStateWasm::NeedsMintConfirmation {
            confirmation: mint_confirmation_from_inner(c),
        },
        S::AddingMint { mint_url } => crate::types::ReceiveFlowStateWasm::AddingMint { mint_url },
        S::Swapping {
            account_id,
            mint_url,
        } => crate::types::ReceiveFlowStateWasm::Swapping {
            account_id,
            mint_url,
        },
        S::Done(result) => crate::types::ReceiveFlowStateWasm::Done {
            result: receive_flow_result_from_inner(result),
        },
        S::AlreadyClaimed(info) => crate::types::ReceiveFlowStateWasm::AlreadyClaimed {
            info: already_claimed_info_from_inner(info),
        },
        S::Failed { reason, code } => crate::types::ReceiveFlowStateWasm::Failed { reason, code },
    }
}

/// `agicash_cashu::ReceiveFlowError` → `JsValue`. Mirrors
/// `ffi::receive_flow::receive_flow_error_to_ffi` at the wasm
/// boundary: a JS `Error` whose message preserves the Display string.
/// The state-machine itself translates most failure paths to a
/// `Failed` state — `Err(...)` is reserved for invalid-event +
/// auth/storage issues, exactly as the FFI does.
#[must_use]
#[allow(clippy::needless_pass_by_value)]
pub fn receive_flow_error_to_js(e: agicash_cashu::ReceiveFlowError) -> JsValue {
    JsValue::from_str(&e.to_string())
}

/// Facade `SendTokenClaimStatus` → `SendClaimStatusWasm`. Verbatim the
/// FFI's `From<SendTokenClaimStatus> for SendSwapClaimSnapshot`
/// (`agicash-ffi/src/convert.rs:316`) — trivial 1:1; the facade type
/// was shaped to match the shell record so this is a straight map.
#[must_use]
pub fn send_claim_status_from_facade(
    s: &agicash_wallet::SendTokenClaimStatus,
) -> crate::types::SendClaimStatusWasm {
    use crate::types::SendClaimStateWasm as T;
    use agicash_wallet::SendTokenClaimState as F;
    crate::types::SendClaimStatusWasm {
        state: match s.state {
            F::Pending => T::Pending,
            F::Completed => T::Completed,
            F::Failed => T::Failed,
        },
        failure_reason: s.failure_reason.clone(),
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
    fn receive_result_from_receipt_maps_money_and_status() {
        let acct = Uuid::new_v4();
        let receipt = agicash_wallet::ReceiveReceipt {
            status: agicash_wallet::ReceiveStatus::Received,
            amount: amount_to_money(900, Currency::Btc),
            fee: amount_to_money(3, Currency::Btc),
            account_id: AccountId::from(acct),
            mint_url: "https://m.example".into(),
            token_hash: "deadbeef".into(),
        };
        let r = receive_result_from_receipt(&receipt);
        assert_eq!(r.status, crate::types::ReceiveStatusWasm::Received);
        assert_eq!(r.amount, "900");
        assert_eq!(r.fee, "3");
        assert_eq!(r.unit, "sat");
        assert_eq!(r.currency, "BTC");
        assert_eq!(r.account_id, acct.to_string());
        assert_eq!(r.mint_url, "https://m.example");
        assert_eq!(r.token_hash, "deadbeef");
    }

    #[wasm_bindgen_test]
    fn receive_result_status_variants_map_one_to_one() {
        let mk = |s| agicash_wallet::ReceiveReceipt {
            status: s,
            amount: amount_to_money(0, Currency::Btc),
            fee: amount_to_money(0, Currency::Btc),
            account_id: AccountId::from(Uuid::new_v4()),
            mint_url: String::new(),
            token_hash: String::new(),
        };
        assert_eq!(
            receive_result_from_receipt(&mk(agicash_wallet::ReceiveStatus::AlreadyClaimed)).status,
            crate::types::ReceiveStatusWasm::AlreadyClaimed
        );
        assert_eq!(
            receive_result_from_receipt(&mk(agicash_wallet::ReceiveStatus::AlreadyFailed)).status,
            crate::types::ReceiveStatusWasm::AlreadyFailed
        );
        assert_eq!(
            receive_result_from_receipt(&mk(agicash_wallet::ReceiveStatus::Pending)).status,
            crate::types::ReceiveStatusWasm::Pending
        );
    }

    #[wasm_bindgen_test]
    fn send_swap_handle_from_facade_maps_money_and_ids() {
        let swap = Uuid::new_v4();
        let acct = Uuid::new_v4();
        let receipt = agicash_wallet::SendTokenReceipt {
            token: "cashuB...".into(),
            amount: amount_to_money(2500, Currency::Btc),
            fee: amount_to_money(5, Currency::Btc),
            account_id: AccountId::from(acct),
            mint_url: "https://m.example".into(),
            swap_id: swap,
            token_hash: "th".into(),
        };
        let h = send_swap_handle_from_facade(&receipt);
        assert_eq!(h.swap_id, swap.to_string());
        assert_eq!(h.token, "cashuB...");
        assert_eq!(h.amount, "2500");
        assert_eq!(h.fee, "5");
        assert_eq!(h.unit, "sat");
        assert_eq!(h.currency, "BTC");
        assert_eq!(h.account_id, acct.to_string());
        assert_eq!(h.mint_url, "https://m.example");
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
