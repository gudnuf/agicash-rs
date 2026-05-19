//! `#[wasm_bindgen]`-exportable plain-data return structs.
//!
//! Mirror of the FFI `*Ffi` records (`crates/agicash-ffi/src/*.rs`) —
//! same field names + JS-exportable field types (String / u64 / bool).
//! wasm-bindgen derives are confined to THIS module (spec §2: the
//! shell-type definitions carry the binding derive; the *mappings* live
//! in `convert.rs`). NOT uniffi — a parallel wasm-bindgen shell.
#![cfg(target_arch = "wasm32")]

use serde::Serialize;
use wasm_bindgen::prelude::*;

/// Mirror of FFI `session::Session`. Facade `Session.user_id` is
/// `UserId`; stringified at the boundary exactly as `ffi::convert`'s
/// `session_from_facade` does.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct SessionWasm {
    #[wasm_bindgen(getter_with_clone)]
    pub user_id: String,
    #[wasm_bindgen(getter_with_clone)]
    pub refresh_token: String,
}

/// Mirror of FFI `session::AuthStatus`.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct AuthStatusWasm {
    pub logged_in: bool,
    #[wasm_bindgen(getter_with_clone)]
    pub user_id: Option<String>,
}

/// Mirror of FFI `account::AccountFfi`. Returned as a JSON array
/// element via `serde_wasm_bindgen::to_value` (wasm-bindgen cannot
/// return `Vec<Struct>` directly — the standard idiom, not an
/// invention). Derives `Serialize` *in addition to* `#[wasm_bindgen]`
/// for the Vec case. Field set + types verbatim the FFI record.
#[wasm_bindgen]
#[derive(Clone, Debug, Serialize)]
pub struct AccountWasm {
    #[wasm_bindgen(getter_with_clone)]
    pub id: String,
    #[wasm_bindgen(getter_with_clone)]
    pub name: String,
    #[wasm_bindgen(getter_with_clone)]
    pub account_type: String,
    #[wasm_bindgen(getter_with_clone)]
    pub currency: String,
    #[wasm_bindgen(getter_with_clone)]
    pub mint_url: Option<String>,
    #[wasm_bindgen(getter_with_clone)]
    pub balance: String,
    #[wasm_bindgen(getter_with_clone)]
    pub unit: String,
}

/// Mirror of FFI `send::SendQuotePreview` — the **field-complete
/// subset** of facade `SendTokenQuote`. Per the 12b-1 facade-field-gap
/// (note ◇): facade `SendTokenQuote` carries NO `mint_url` (the FFI
/// reconstructs it off the picked account; that is a behavior/shape
/// change → 12b-2/12c facade-surface work). `mint_url` is therefore
/// **omitted here (NOT faked, NOT defaulted)** — a strict
/// no-behavior-invention shell. All `Money` fields are
/// decimal-stringified; `unit`/`currency` derived from the Money.
#[wasm_bindgen]
#[derive(Clone, Debug, Serialize)]
pub struct SendQuotePreviewWasm {
    #[wasm_bindgen(getter_with_clone)]
    pub amount_requested: String,
    #[wasm_bindgen(getter_with_clone)]
    pub amount_to_send: String,
    #[wasm_bindgen(getter_with_clone)]
    pub total_amount: String,
    #[wasm_bindgen(getter_with_clone)]
    pub total_fee: String,
    #[wasm_bindgen(getter_with_clone)]
    pub cashu_send_fee: String,
    #[wasm_bindgen(getter_with_clone)]
    pub cashu_receive_fee: String,
    #[wasm_bindgen(getter_with_clone)]
    pub unit: String,
    #[wasm_bindgen(getter_with_clone)]
    pub currency: String,
    #[wasm_bindgen(getter_with_clone)]
    pub account_id: String,
}

/// Mirror of FFI `send::SendSwapHandle`. Facade `SendTokenReceipt` IS
/// field-complete (`mint_url` is on the receipt) — full mapping, no
/// gap. `Money` fields decimal-stringified; ids stringified.
#[wasm_bindgen]
#[derive(Clone, Debug, Serialize)]
pub struct SendSwapHandleWasm {
    #[wasm_bindgen(getter_with_clone)]
    pub swap_id: String,
    #[wasm_bindgen(getter_with_clone)]
    pub token: String,
    #[wasm_bindgen(getter_with_clone)]
    pub amount: String,
    #[wasm_bindgen(getter_with_clone)]
    pub fee: String,
    #[wasm_bindgen(getter_with_clone)]
    pub unit: String,
    #[wasm_bindgen(getter_with_clone)]
    pub currency: String,
    #[wasm_bindgen(getter_with_clone)]
    pub account_id: String,
    #[wasm_bindgen(getter_with_clone)]
    pub mint_url: String,
}

/// Mirror of FFI `receive::ReceiveStatus`. 1:1 variant set.
/// `#[wasm_bindgen]` C-like enum (JS sees integer discriminants);
/// `Serialize` for the `serde_wasm_bindgen` path / JSON parity with the
/// FFI's snake_case wire value the UI branches on.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ReceiveStatusWasm {
    Received,
    AlreadyClaimed,
    AlreadyFailed,
    Pending,
}

/// Mirror of FFI `receive::ReceiveResult`. Field set + types verbatim
/// the FFI record. `Money` fields decimal-stringified.
#[wasm_bindgen]
#[derive(Clone, Debug, Serialize)]
pub struct ReceiveResultWasm {
    pub status: ReceiveStatusWasm,
    #[wasm_bindgen(getter_with_clone)]
    pub amount: String,
    #[wasm_bindgen(getter_with_clone)]
    pub fee: String,
    #[wasm_bindgen(getter_with_clone)]
    pub unit: String,
    #[wasm_bindgen(getter_with_clone)]
    pub currency: String,
    #[wasm_bindgen(getter_with_clone)]
    pub account_id: String,
    #[wasm_bindgen(getter_with_clone)]
    pub mint_url: String,
    #[wasm_bindgen(getter_with_clone)]
    pub token_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn session_wasm_constructs() {
        let s = SessionWasm {
            user_id: "u".into(),
            refresh_token: "r".into(),
        };
        assert_eq!(s.user_id, "u");
        assert_eq!(s.refresh_token, "r");
    }

    #[wasm_bindgen_test]
    fn auth_status_wasm_logged_out_shape() {
        let a = AuthStatusWasm {
            logged_in: false,
            user_id: None,
        };
        assert!(!a.logged_in);
        assert!(a.user_id.is_none());
    }
}
