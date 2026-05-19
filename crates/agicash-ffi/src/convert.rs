//! Centralized facade-type ↔ `*Ffi`-record conversions.
//!
//! Spec §2: one boundary module per shell. Every `From`/builder that
//! used to be scattered across the wallet methods + the per-feature
//! type modules lives here. The `*Ffi` record *definitions* stay in
//! their modules (uniffi derives there); this module owns the
//! *mappings* (facade → FFI) and the arg parsers (FFI String/u64 →
//! facade typed inputs).

use crate::error::FfiError;
use agicash_domain::{AccountId, Currency};
use agicash_money::{Money, Unit};
use agicash_wallet::WalletError;
use rust_decimal::Decimal;
use std::str::FromStr;
use uuid::Uuid;

/// FFI `String` (UUID) → domain `AccountId`. Bad UUID → `Internal`
/// (verbatim the message shape the old scattered call-sites used:
/// `"invalid account_id: <e>"`).
pub fn parse_account_id(s: &str) -> Result<AccountId, FfiError> {
    Uuid::parse_str(s.trim())
        .map(AccountId::from)
        .map_err(|e| FfiError::internal(format!("invalid account_id: {e}")))
}

/// Optional FFI account-id string → `Option<AccountId>`.
pub fn parse_opt_account_id(s: Option<&str>) -> Result<Option<AccountId>, FfiError> {
    match s {
        Some(v) => Ok(Some(parse_account_id(v)?)),
        None => Ok(None),
    }
}

/// FFI `Option<String>` currency (defaulting to `"BTC"` when `None`) →
/// domain `Currency`. Unknown code → `Internal` (verbatim message shape
/// `"unsupported currency: <c>"`).
pub fn parse_currency(c: Option<&str>) -> Result<Currency, FfiError> {
    let s = c.unwrap_or("BTC");
    Currency::from_str(s).map_err(|_| FfiError::internal(format!("unsupported currency: {s}")))
}

/// FFI minor-unit `u64` + currency → facade `Money` (minor unit:
/// `sat` for BTC, `cent` for USD/USDB). Mirrors `wallet.rs`'s
/// `Money::new(Decimal::from(amount), c, unit_for_currency(c))`.
#[must_use]
pub fn amount_to_money(amount: u64, currency: Currency) -> Money {
    let unit = match currency {
        Currency::Btc => Unit::Sat,
        Currency::Usd | Currency::Usdb => Unit::Cent,
    };
    Money::new(Decimal::from(amount), currency, unit)
}

/// `WalletError` → `FfiError`. 12b-1 PRESERVES today's behavior: the
/// bespoke FFI funneled cashu/validation/etc through
/// `FfiError::Internal { message }` carrying the discriminator-bearing
/// string; only `Unauthenticated` mapped to the structured `Auth`
/// variant. This keeps that exact shape (structured auth-code
/// granularity is **12b-2**, NOT here). Storage/NotFound keep the
/// structured `Storage` mapping the FFI `From<StorageError>` gave.
pub fn wallet_error_to_ffi(e: WalletError) -> FfiError {
    match e {
        WalletError::Unauthenticated => FfiError::Auth {
            code: crate::error::auth_code::UNAUTHENTICATED,
            message: "not authenticated".into(),
        },
        WalletError::Storage(m) => FfiError::Storage {
            code: crate::error::storage_code::BACKEND,
            message: m,
        },
        WalletError::NotFound(m) => FfiError::Storage {
            code: crate::error::storage_code::NOT_FOUND,
            message: m,
        },
        // Auth/Cashu/Network/Concurrency/Validation/Unsupported/etc —
        // all funnel to Internal with the Display string (verbatim
        // pre-refactor FFI behavior; the message text carries the
        // discriminator the iOS UI parses).
        other => FfiError::internal(other.to_string()),
    }
}

/// Facade `Session` → FFI `Session` record. Verbatim the old
/// `PersistedSession -> ffi::session::Session` mapping shape.
#[must_use]
pub fn session_from_facade(s: agicash_wallet::Session) -> crate::session::Session {
    crate::session::Session {
        user_id: s.user_id.to_string(),
        refresh_token: s.refresh_token,
    }
}

/// Minor-unit label for a currency. `"sat"` for BTC, `"cent"` for
/// USD/USDB — verbatim the old `AccountFfi` `unit_label` mapping.
#[must_use]
fn unit_label(c: Currency) -> &'static str {
    match c {
        Currency::Btc => "sat",
        Currency::Usd | Currency::Usdb => "cent",
    }
}

/// Facade `AccountSummary` → FFI `AccountFfi` record. Verbatim the old
/// `AccountFfi::from_account_with_balance` field shape: stringified id,
/// `account_type`/`currency` Display labels, `mint_url` passthrough,
/// decimal balance, currency-derived unit label.
#[must_use]
pub fn account_ffi_from_summary(s: &agicash_wallet::AccountSummary) -> crate::account::AccountFfi {
    crate::account::AccountFfi {
        id: s.id.to_string(),
        name: s.name.clone(),
        account_type: s.account_type.to_string(),
        currency: s.currency.to_string(),
        mint_url: s.mint_url.clone(),
        balance: s.balance.clone(),
        unit: unit_label(s.currency).to_string(),
    }
}

/// Facade `AccountSummary` (the row `add_mint` returns) → FFI
/// `MintAddResult`. Verbatim the old `mint_add` tail mapping:
/// stringified id, `name` as `mint_name`, `mint_url` (empty when the
/// row carries none — unreachable in practice since the row was just
/// created against a mint URL, but defaulted for total-ness exactly as
/// `unwrap_or_default`), currency Display label.
#[must_use]
pub fn mint_add_result_from_summary(
    s: &agicash_wallet::AccountSummary,
) -> crate::mint::MintAddResult {
    crate::mint::MintAddResult {
        account_id: s.id.to_string(),
        mint_name: s.name.clone(),
        mint_url: s.mint_url.clone().unwrap_or_default(),
        currency: s.currency.to_string(),
    }
}

/// Facade `ReceiveStatus` → FFI `ReceiveStatus`. 1:1 variant map.
#[must_use]
fn receive_status_from_facade(s: agicash_wallet::ReceiveStatus) -> crate::receive::ReceiveStatus {
    match s {
        agicash_wallet::ReceiveStatus::Received => crate::receive::ReceiveStatus::Received,
        agicash_wallet::ReceiveStatus::AlreadyClaimed => {
            crate::receive::ReceiveStatus::AlreadyClaimed
        }
        agicash_wallet::ReceiveStatus::AlreadyFailed => {
            crate::receive::ReceiveStatus::AlreadyFailed
        }
        agicash_wallet::ReceiveStatus::Pending => crate::receive::ReceiveStatus::Pending,
    }
}

/// Facade `ReceiveReceipt` → FFI `ReceiveResult`. Verbatim the old
/// `receive_result_from_outcome` shape: decimal `amount`/`fee` strings,
/// `Money`-derived `unit`/`currency`, stringified `account_id`.
#[must_use]
pub fn receive_result_from_receipt(
    r: &agicash_wallet::ReceiveReceipt,
) -> crate::receive::ReceiveResult {
    crate::receive::ReceiveResult {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_account_id_ok() {
        let u = Uuid::new_v4();
        assert_eq!(
            parse_account_id(&u.to_string()).unwrap(),
            AccountId::from(u)
        );
    }

    #[test]
    fn parse_account_id_bad_uuid_is_internal() {
        let e = parse_account_id("not-a-uuid").unwrap_err();
        assert!(matches!(e, FfiError::Internal { .. }));
    }

    #[test]
    fn parse_currency_defaults_btc_on_none() {
        assert_eq!(parse_currency(None).unwrap(), Currency::Btc);
    }

    #[test]
    fn parse_currency_unknown_is_internal() {
        let e = parse_currency(Some("XYZ")).unwrap_err();
        assert!(matches!(e, FfiError::Internal { .. }));
    }

    #[test]
    fn amount_to_money_uses_minor_unit() {
        let m = amount_to_money(1000, Currency::Btc);
        assert_eq!(m.currency(), Currency::Btc);
    }

    #[test]
    fn wallet_error_maps_to_internal_message_preserved() {
        let e = wallet_error_to_ffi(WalletError::Cashu("boom".into()));
        match e {
            FfiError::Internal { message } => assert!(message.contains("boom")),
            other => panic!("expected Internal, got {other:?}"),
        }
    }

    #[test]
    fn wallet_error_unauthenticated_maps_to_auth_unauthenticated() {
        let e = wallet_error_to_ffi(WalletError::Unauthenticated);
        assert!(matches!(
            e,
            FfiError::Auth { code, .. } if code == crate::error::auth_code::UNAUTHENTICATED
        ));
    }

    #[test]
    fn session_from_facade_stringifies_user_id() {
        let uid = Uuid::new_v4();
        let fs = agicash_wallet::Session {
            user_id: agicash_domain::UserId::from(uid),
            refresh_token: "rt.x".into(),
        };
        let ffi = session_from_facade(fs);
        assert_eq!(ffi.user_id, uid.to_string());
        assert_eq!(ffi.refresh_token, "rt.x");
    }

    #[test]
    fn account_ffi_from_summary_maps_all_fields_and_derives_unit() {
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
        let ffi = account_ffi_from_summary(&summary);
        assert_eq!(ffi.id, id.to_string());
        assert_eq!(ffi.name, "My Mint");
        assert_eq!(ffi.account_type, "cashu");
        assert_eq!(ffi.currency, "BTC");
        assert_eq!(ffi.mint_url, Some("https://mint.example".to_string()));
        assert_eq!(ffi.balance, "1234");
        assert_eq!(ffi.unit, "sat");

        let usd = agicash_wallet::AccountSummary {
            currency: Currency::Usd,
            ..summary
        };
        assert_eq!(account_ffi_from_summary(&usd).unit, "cent");
    }

    #[test]
    fn mint_add_result_from_summary_maps_fields() {
        let id = Uuid::new_v4();
        let summary = agicash_wallet::AccountSummary {
            id: AccountId::from(id),
            user_id: agicash_domain::UserId::from(Uuid::new_v4()),
            name: "Coinos".into(),
            account_type: agicash_domain::AccountType::Cashu,
            currency: Currency::Btc,
            mint_url: Some("https://mint.coinos.io".into()),
            balance: "0".into(),
        };
        let r = mint_add_result_from_summary(&summary);
        assert_eq!(r.account_id, id.to_string());
        assert_eq!(r.mint_name, "Coinos");
        assert_eq!(r.mint_url, "https://mint.coinos.io");
        assert_eq!(r.currency, "BTC");
    }

    #[test]
    fn mint_add_result_missing_mint_url_defaults_empty() {
        let summary = agicash_wallet::AccountSummary {
            id: AccountId::from(Uuid::new_v4()),
            user_id: agicash_domain::UserId::from(Uuid::new_v4()),
            name: "x".into(),
            account_type: agicash_domain::AccountType::Cashu,
            currency: Currency::Btc,
            mint_url: None,
            balance: "0".into(),
        };
        assert_eq!(mint_add_result_from_summary(&summary).mint_url, "");
    }

    #[test]
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
        assert!(matches!(r.status, crate::receive::ReceiveStatus::Received));
        assert_eq!(r.amount, "900");
        assert_eq!(r.fee, "3");
        assert_eq!(r.unit, "sat");
        assert_eq!(r.currency, "BTC");
        assert_eq!(r.account_id, acct.to_string());
        assert_eq!(r.mint_url, "https://m.example");
        assert_eq!(r.token_hash, "deadbeef");
    }

    #[test]
    fn receive_result_status_variants_map_one_to_one() {
        let mk = |s| agicash_wallet::ReceiveReceipt {
            status: s,
            amount: amount_to_money(0, Currency::Btc),
            fee: amount_to_money(0, Currency::Btc),
            account_id: AccountId::from(Uuid::new_v4()),
            mint_url: String::new(),
            token_hash: String::new(),
        };
        assert!(matches!(
            receive_result_from_receipt(&mk(agicash_wallet::ReceiveStatus::AlreadyClaimed)).status,
            crate::receive::ReceiveStatus::AlreadyClaimed
        ));
        assert!(matches!(
            receive_result_from_receipt(&mk(agicash_wallet::ReceiveStatus::AlreadyFailed)).status,
            crate::receive::ReceiveStatus::AlreadyFailed
        ));
        assert!(matches!(
            receive_result_from_receipt(&mk(agicash_wallet::ReceiveStatus::Pending)).status,
            crate::receive::ReceiveStatus::Pending
        ));
    }
}
