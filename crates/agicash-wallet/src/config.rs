//! Public construction config for `WalletClient::from_config`.
//!
//! The single input contract all binding shells share. The only thing
//! that differs per shell is the session-storage backend choice; every
//! other dep is wired identically inside `from_config`.

use agicash_traits::SessionStorage;
use std::sync::Arc;
use uuid::Uuid;

/// Which session-storage backend `from_config` installs. This is the
/// ONLY axis the three shells differ on — everything else in the
/// composition is identical.
///
/// The `Keyring` variant is feature-gated on `keyring-storage` — passing
/// it without that feature is a compile-time error (operator decision —
/// 2026-05-22 session-loading plan). The `Browser` variant exists on
/// every target; its match arm in [`crate::WalletClient::from_config_async`]
/// resolves to [`agicash_auth_opensecret::BrowserSessionStorage`] on
/// `target_arch = "wasm32"` and falls back to in-memory elsewhere — same
/// pattern as `Android`.
#[derive(Clone)]
pub enum SessionStorageChoice {
    /// Process-lifetime only (CLI default, iOS — iOS persists the
    /// refresh token in Keychain on the Swift side, not here).
    InMemory,
    /// Android AES-256-GCM file backend rooted at the app's private
    /// data dir. `dir` is `Context.getFilesDir().getAbsolutePath()`.
    Android { dir: String },
    /// `window.localStorage`-backed persistence for the Leptos wasm
    /// shell. Wasm32-only at the impl level; on other targets the match
    /// arm falls back to [`Self::InMemory`] (mirrors `Android`).
    Browser,
    /// OS keyring (macOS Keychain / Windows Credential Manager /
    /// Linux secret-service) — used by the CLI shell. Compile-gated:
    /// requires the `keyring-storage` feature on `agicash-wallet`.
    /// Passing this variant without the feature is a compile error
    /// (operator decision — sessions must not silently fall back to
    /// in-memory when the CLI shell explicitly asked for the keyring).
    #[cfg(feature = "keyring-storage")]
    Keyring {
        /// Service identifier the keyring backend stores under
        /// (`com.agicash.cli` by default — see
        /// [`agicash_auth_opensecret::DEFAULT_SERVICE`]).
        service: String,
    },
    /// Caller-supplied `Arc<dyn SessionStorage>` — escape hatch for
    /// harnesses, tests, and bespoke shells (e.g. iOS Keychain bridge).
    /// The auto-load path in `from_config_async` calls `.load()` on
    /// this just like any other variant.
    Custom(Arc<dyn SessionStorage>),
}

impl std::fmt::Debug for SessionStorageChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InMemory => f.write_str("InMemory"),
            Self::Android { dir } => f.debug_struct("Android").field("dir", dir).finish(),
            Self::Browser => f.write_str("Browser"),
            #[cfg(feature = "keyring-storage")]
            Self::Keyring { service } => {
                f.debug_struct("Keyring").field("service", service).finish()
            }
            Self::Custom(_) => f.write_str("Custom(<dyn SessionStorage>)"),
        }
    }
}

/// Public input contract for [`crate::WalletClient::from_config`].
///
/// Mirrors exactly the four args the existing FFI
/// `AgicashWallet::new(opensecret_url, opensecret_client_id_uuid,
/// supabase_url, supabase_anon_key)` ctor takes, plus the
/// session-storage backend choice (the FFI installs that
/// post-construction today via `set_session_storage_dir`; `from_config`
/// folds it into the one root).
#[derive(Debug, Clone)]
pub struct WalletConfig {
    pub opensecret_url: String,
    pub opensecret_client_id: Uuid,
    pub supabase_url: String,
    pub supabase_anon_key: String,
    pub session_storage: SessionStorageChoice,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_fields() {
        let cfg = WalletConfig {
            opensecret_url: "https://os.example".into(),
            opensecret_client_id: uuid::Uuid::nil(),
            supabase_url: "https://sb.example".into(),
            supabase_anon_key: "anon".into(),
            session_storage: SessionStorageChoice::InMemory,
        };
        assert_eq!(cfg.opensecret_url, "https://os.example");
        assert!(matches!(
            cfg.session_storage,
            SessionStorageChoice::InMemory
        ));
    }

    #[test]
    fn android_choice_carries_dir() {
        let c = SessionStorageChoice::Android {
            dir: "/data/data/app/files".into(),
        };
        match c {
            SessionStorageChoice::Android { dir } => assert_eq!(dir, "/data/data/app/files"),
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn browser_choice_constructs() {
        let c = SessionStorageChoice::Browser;
        assert!(matches!(c, SessionStorageChoice::Browser));
    }

    #[cfg(feature = "keyring-storage")]
    #[test]
    fn keyring_choice_carries_service() {
        let c = SessionStorageChoice::Keyring {
            service: "com.agicash.test".into(),
        };
        match c {
            SessionStorageChoice::Keyring { service } => {
                assert_eq!(service, "com.agicash.test");
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn custom_choice_carries_storage() {
        use agicash_auth_opensecret::InMemorySessionStorage;
        let storage: Arc<dyn SessionStorage> = Arc::new(InMemorySessionStorage::new());
        let c = SessionStorageChoice::Custom(storage);
        assert!(matches!(c, SessionStorageChoice::Custom(_)));
    }
}
