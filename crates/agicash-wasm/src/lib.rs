//! WASM bindings for the agicash Rust SDK — a `wasm-bindgen` shell over
//! the SAME `WalletClient::from_config` composition core the FFI uses
//! (NOT uniffi; uniffi cannot target wasm). Mirrors the FFI shell
//! structure (`crates/agicash-ffi/src/wallet.rs` + `ffi::convert`).
//! Native `rlib` builds compile only the version fn (the shipping
//! artifact is the wasm cdylib).

#[cfg(not(target_arch = "wasm32"))]
mod native_stub {
    //! Native `cargo test --workspace` only sees this — keeps the
    //! crate in the workspace without pulling the wasm graph natively.
}

#[cfg(target_arch = "wasm32")]
mod convert;
#[cfg(target_arch = "wasm32")]
mod types;

#[cfg(target_arch = "wasm32")]
mod wasm_impl {
    use crate::types::AuthStatusWasm;
    use agicash_wallet::{SessionStorageChoice, TokenVersion, WalletClient, WalletConfig};
    use std::sync::Arc;
    use uuid::Uuid;
    use wasm_bindgen::prelude::*;

    /// Opaque handle the JS / Leptos side holds. Mirrors the FFI
    /// `AgicashWallet` object: one `Arc<WalletClient>` built once via
    /// `from_config`. The shell carries zero business/composition logic
    /// (spec architecture) — composition is `from_config`, mapping is
    /// `convert`.
    #[wasm_bindgen]
    #[derive(Debug)]
    pub struct AgicashWasmWallet {
        client: Arc<WalletClient>,
    }

    #[wasm_bindgen]
    impl AgicashWasmWallet {
        /// Construct over the SAME `from_config` core the FFI uses.
        /// Session storage = InMemory on wasm (the browser persists the
        /// refresh token via `agicash-auth-opensecret`'s
        /// `BrowserSessionStorage` / `window.localStorage` — already
        /// shipped; the facade's InMemory choice is the construction
        /// default, exactly as the FFI uses InMemory + a shell-resident
        /// backend). No network I/O at construction.
        #[wasm_bindgen(constructor)]
        pub fn new(
            opensecret_url: String,
            opensecret_client_id: String,
            supabase_url: String,
            supabase_anon_key: String,
        ) -> Result<AgicashWasmWallet, JsValue> {
            let client_id = Uuid::parse_str(opensecret_client_id.trim())
                .map_err(|e| JsValue::from_str(&format!("invalid client id: {e}")))?;
            let (client, _auth) = WalletClient::from_config(WalletConfig {
                opensecret_url,
                opensecret_client_id: client_id,
                supabase_url,
                supabase_anon_key,
                session_storage: SessionStorageChoice::InMemory,
            })
            .map_err(crate::convert::wallet_error_to_js)?;
            Ok(AgicashWasmWallet { client })
        }

        /// Register a fresh guest account. Pure delegate to
        /// `WalletClient::auth_guest` (mirrors FFI 6.1). Browser session
        /// persistence is handled by `agicash-auth-opensecret`'s
        /// already-shipped `BrowserSessionStorage` inside `from_config`'s
        /// `OpenSecretAuthClient` — the wasm shell has no realtime
        /// supervisor (full realtime cutover is #29, out of 12d scope),
        /// so no shell-resident session-slot side-effect (unlike the
        /// FFI's §6 mirror).
        #[wasm_bindgen(js_name = authGuest)]
        pub async fn auth_guest(&self) -> Result<crate::types::SessionWasm, JsValue> {
            let s = self
                .client
                .auth_guest()
                .await
                .map_err(crate::convert::wallet_error_to_js)?;
            Ok(crate::convert::session_from_facade(&s))
        }

        /// Email + password login. Pure delegate to
        /// `WalletClient::auth_login` (mirrors FFI 6.2). Reuses
        /// `session_from_facade`.
        #[wasm_bindgen(js_name = authLogin)]
        pub async fn auth_login(
            &self,
            email: String,
            password: String,
        ) -> Result<crate::types::SessionWasm, JsValue> {
            let s = self
                .client
                .auth_login(&email, &password)
                .await
                .map_err(crate::convert::wallet_error_to_js)?;
            Ok(crate::convert::session_from_facade(&s))
        }

        /// Best-effort server logout (always clears local state). Pure
        /// delegate to `WalletClient::auth_logout` (mirrors FFI 6.4).
        #[wasm_bindgen(js_name = authLogout)]
        pub async fn auth_logout(&self) -> Result<(), JsValue> {
            self.client
                .auth_logout()
                .await
                .map_err(crate::convert::wallet_error_to_js)
        }

        /// List the logged-in user's accounts. Pure delegate to
        /// `WalletClient::list_accounts` (mirrors FFI 7.1). Returns a
        /// JSON array of `AccountWasm` via `serde_wasm_bindgen::to_value`
        /// (wasm-bindgen cannot return `Vec<Struct>` directly — the
        /// standard idiom; the FFI returns `Vec<AccountFfi>`).
        #[wasm_bindgen(js_name = listAccounts)]
        pub async fn list_accounts(&self) -> Result<JsValue, JsValue> {
            let accounts = self
                .client
                .list_accounts()
                .await
                .map_err(crate::convert::wallet_error_to_js)?;
            let mapped: Vec<crate::types::AccountWasm> = accounts
                .iter()
                .map(crate::convert::account_wasm_from_summary)
                .collect();
            serde_wasm_bindgen::to_value(&mapped)
                .map_err(|e| JsValue::from_str(&format!("serialize accounts: {e}")))
        }

        /// Pre-commit send quote (fee breakdown for the confirm screen,
        /// no persistence). Delegates to `WalletClient::quote_send_token`
        /// (mirrors FFI 11.1). Returns the **field-complete subset** of
        /// the facade quote — `mint_url` is omitted, NOT faked (note ◇:
        /// facade `SendTokenQuote` has no `mint_url`).
        #[wasm_bindgen(js_name = prepareSendQuote)]
        pub async fn prepare_send_quote(
            &self,
            amount: u64,
            account_id: Option<String>,
            currency: Option<String>,
        ) -> Result<crate::types::SendQuotePreviewWasm, JsValue> {
            let currency_enum = crate::convert::parse_currency(currency)?;
            let account_id = crate::convert::parse_opt_account_id(account_id)?;
            let amount_money = crate::convert::amount_to_money(amount, currency_enum);
            let quote = self
                .client
                .quote_send_token(account_id, amount_money)
                .await
                .map_err(crate::convert::wallet_error_to_js)?;
            Ok(crate::convert::send_quote_preview_from_facade(&quote))
        }

        /// Commit a Cashu send swap (persists PENDING + returns the
        /// wire-form V4 token to share + swap id). Delegates to
        /// `WalletClient::send_token(.., TokenVersion::V4)` — FFI always
        /// V4, verbatim (mirrors FFI 11.2). Receipt is field-complete.
        #[wasm_bindgen(js_name = createSendSwap)]
        pub async fn create_send_swap(
            &self,
            amount: u64,
            account_id: Option<String>,
            currency: Option<String>,
        ) -> Result<crate::types::SendSwapHandleWasm, JsValue> {
            let currency_enum = crate::convert::parse_currency(currency)?;
            let account_id = crate::convert::parse_opt_account_id(account_id)?;
            let amount_money = crate::convert::amount_to_money(amount, currency_enum);
            let receipt = self
                .client
                .send_token(account_id, amount_money, TokenVersion::V4)
                .await
                .map_err(crate::convert::wallet_error_to_js)?;
            Ok(crate::convert::send_swap_handle_from_facade(&receipt))
        }

        /// Redeem a Cashu token. Pure delegate to
        /// `WalletClient::receive_cashu_token` (mirrors FFI 9.1). The
        /// facade handles unknown mints per its one-shot semantics — no
        /// pre-check here (the mocked `KNOWN_MINTS` preview was a UX
        /// stub; interactive add-mint confirmation is 12c receive-flow).
        #[wasm_bindgen(js_name = receiveToken)]
        pub async fn receive_token(
            &self,
            token: String,
        ) -> Result<crate::types::ReceiveResultWasm, JsValue> {
            let receipt = self
                .client
                .receive_cashu_token(&token)
                .await
                .map_err(crate::convert::wallet_error_to_js)?;
            Ok(crate::convert::receive_result_from_receipt(&receipt))
        }

        /// Single-shot NUT-07 send-claim poll. Pure delegate to
        /// `WalletClient::check_send_token_claimed` (mirrors FFI
        /// `check_send_swap_claimed`, `agicash-ffi/src/wallet.rs:1031`).
        /// Single-shot by design — the consumer drives cadence (iOS 3 s
        /// timer, Leptos interval); the shell carries zero loop logic.
        /// `swap_id` is the `SendSwapHandleWasm.swap_id` string from
        /// `create_send_swap`; an invalid UUID is a JS error exactly as
        /// the FFI returns `FfiError::Internal`.
        #[wasm_bindgen(js_name = checkSendSwapClaimed)]
        pub async fn check_send_swap_claimed(
            &self,
            swap_id: String,
        ) -> Result<crate::types::SendClaimStatusWasm, JsValue> {
            let id = Uuid::parse_str(swap_id.trim())
                .map_err(|e| JsValue::from_str(&format!("invalid swap_id: {e}")))?;
            let status = self
                .client
                .check_send_token_claimed(id)
                .await
                .map_err(crate::convert::wallet_error_to_js)?;
            Ok(crate::convert::send_claim_status_from_facade(&status))
        }

        /// Logged-in snapshot, no network. Proves the handle is wired +
        /// the facade delegates (mirrors the FFI `auth_status` smoke).
        #[wasm_bindgen(js_name = authStatus)]
        pub async fn auth_status(&self) -> Result<AuthStatusWasm, JsValue> {
            let s = self
                .client
                .auth_status()
                .await
                .map_err(crate::convert::wallet_error_to_js)?;
            Ok(crate::convert::auth_status_from_facade(&s))
        }
    }
}

#[cfg(target_arch = "wasm32")]
pub use wasm_impl::AgicashWasmWallet;

// Re-export the wasm shell return types at the crate root so consumers
// (the Leptos crate) can name `agicash_wasm::SendQuotePreviewWasm` etc.
// Mirrors how the FFI crate re-exports its `*Ffi` records.
#[cfg(target_arch = "wasm32")]
pub use types::{
    AccountWasm, AuthStatusWasm, ReceiveResultWasm, ReceiveStatusWasm, SendClaimStateWasm,
    SendClaimStatusWasm, SendQuotePreviewWasm, SendSwapHandleWasm, SessionWasm,
};

use wasm_bindgen::prelude::*;

/// Version string. Available on every target (kept from the scaffold so
/// the build pipeline smoke test still works).
#[wasm_bindgen]
#[must_use]
pub fn agicash_wasm_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
