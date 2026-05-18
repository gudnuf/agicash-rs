//! Auth-client seam used by the facade.
//!
//! The facade does NOT depend on `agicash-auth-opensecret` directly —
//! that crate pulls in `keyring` and `opensecret` which carry
//! non-wasm-clean transitive deps. Instead, the facade speaks to an
//! `Arc<dyn AuthClient>` trait object that consumers wire concretely.
//!
//! For native (CLI + FFI) targets, a consumer wraps
//! `agicash_auth_opensecret::OpenSecretClient` in an impl of this trait.
//! For WASM (Leptos PWA), a consumer wraps the
//! `/api/auth/*` SSR proxy in an impl of the same trait. Either way the
//! facade code stays identical.
//!
//! The slice-12 facade only needs the seven CRUD operations matching the
//! existing `AgicashWallet` FFI surface — guest/email register, email login,
//! logout, status, session set/get. Token refresh and rich claims are
//! deferred to slice 11+ alongside the event bus.

use crate::error::WalletError;
use agicash_domain::UserId;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Materialized session — what `auth_*` calls return and what `set_session`
/// accepts.
///
/// Matches `agicash_traits::PersistedSession` field-for-field so a thin
/// `From` impl bridges them; we don't import the trait crate's type
/// directly so this facade stays decoupled from auth backend details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub user_id: UserId,
    pub refresh_token: String,
}

/// Abstract auth backend.
///
/// One impl is wrapped behind `Arc<dyn AuthClient>` and handed to
/// `WalletClientBuilder::auth(...)`. The facade calls these from its
/// `auth_*` methods.
///
/// The `[u8; 64]` seed return from `cashu_seed` is the BIP-39-derived 64
/// byte master seed the cashu state machines (mint_quote, receive_swap)
/// need for blinded message generation. Returned freshly each call —
/// consumers cache it themselves if they want.
#[async_trait]
pub trait AuthClient: Send + Sync + std::fmt::Debug {
    /// Register a fresh guest account on the auth backend.
    async fn register_guest(&self) -> Result<Session, WalletError>;

    /// Email + password login.
    async fn login_email(&self, email: &str, password: &str) -> Result<Session, WalletError>;

    /// Register a new email + password user. Auto-signs in on success.
    async fn register_email(
        &self,
        email: &str,
        password: &str,
        name: Option<&str>,
    ) -> Result<Session, WalletError>;

    /// Best-effort server logout. Always clears local state even if the
    /// server call fails.
    async fn logout(&self) -> Result<(), WalletError>;

    /// Rehydrate an existing session from persisted state (e.g. iOS
    /// Keychain, browser cookie). Performs a token refresh under the hood
    /// so the client has a usable access token.
    async fn set_session(&self, session: Session) -> Result<(), WalletError>;

    /// Snapshot the currently-loaded session, or `None` if logged out.
    async fn get_session(&self) -> Result<Option<Session>, WalletError>;

    /// Return the 64-byte BIP-39 cashu seed for the current session.
    /// Required by the cashu state machines for blinded message
    /// generation.
    async fn cashu_seed(&self) -> Result<[u8; 64], WalletError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// In-memory fake. Used by the integration tests + made available to
    /// downstream consumers as a documentation example.
    #[derive(Debug)]
    pub struct FakeAuth {
        session: Mutex<Option<Session>>,
        seed: [u8; 64],
    }

    impl Default for FakeAuth {
        fn default() -> Self {
            Self {
                session: Mutex::new(None),
                seed: [0u8; 64],
            }
        }
    }

    #[async_trait]
    impl AuthClient for FakeAuth {
        async fn register_guest(&self) -> Result<Session, WalletError> {
            let s = Session {
                user_id: UserId::new(),
                refresh_token: "guest-rt".into(),
            };
            *self.session.lock().unwrap() = Some(s.clone());
            Ok(s)
        }
        async fn login_email(&self, _email: &str, _pw: &str) -> Result<Session, WalletError> {
            let s = Session {
                user_id: UserId::new(),
                refresh_token: "email-rt".into(),
            };
            *self.session.lock().unwrap() = Some(s.clone());
            Ok(s)
        }
        async fn register_email(
            &self,
            _email: &str,
            _pw: &str,
            _name: Option<&str>,
        ) -> Result<Session, WalletError> {
            let s = Session {
                user_id: UserId::new(),
                refresh_token: "signup-rt".into(),
            };
            *self.session.lock().unwrap() = Some(s.clone());
            Ok(s)
        }
        async fn logout(&self) -> Result<(), WalletError> {
            *self.session.lock().unwrap() = None;
            Ok(())
        }
        async fn set_session(&self, session: Session) -> Result<(), WalletError> {
            *self.session.lock().unwrap() = Some(session);
            Ok(())
        }
        async fn get_session(&self) -> Result<Option<Session>, WalletError> {
            Ok(self.session.lock().unwrap().clone())
        }
        async fn cashu_seed(&self) -> Result<[u8; 64], WalletError> {
            let session = self.session.lock().unwrap();
            if session.is_none() {
                return Err(WalletError::Unauthenticated);
            }
            Ok(self.seed)
        }
    }

    #[tokio::test]
    async fn fake_auth_register_guest_sets_session() {
        let a = FakeAuth::default();
        assert!(a.get_session().await.unwrap().is_none());
        let s = a.register_guest().await.unwrap();
        assert_eq!(a.get_session().await.unwrap(), Some(s));
    }

    #[tokio::test]
    async fn fake_auth_logout_clears_session() {
        let a = FakeAuth::default();
        a.register_guest().await.unwrap();
        a.logout().await.unwrap();
        assert!(a.get_session().await.unwrap().is_none());
    }
}
