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
}
