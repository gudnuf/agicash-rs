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
    use agicash_wallet::{SessionStorageChoice, WalletClient, WalletConfig};
    use std::sync::Arc;
    use uuid::Uuid;
    use wasm_bindgen::prelude::*;

    /// Opaque handle the JS / Leptos side holds. Mirrors the FFI
    /// `AgicashWallet` object: one `Arc<WalletClient>` built once via
    /// `from_config`. The shell carries zero business/composition logic
    /// (spec architecture) — composition is `from_config`, mapping is
    /// `convert`.
    #[wasm_bindgen]
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

use wasm_bindgen::prelude::*;

/// Version string. Available on every target (kept from the scaffold so
/// the build pipeline smoke test still works).
#[wasm_bindgen]
#[must_use]
pub fn agicash_wasm_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}
