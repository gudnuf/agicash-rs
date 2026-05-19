//! Enforced session-invariant contract.
//!
//! The bare `AuthClient` trait (`crate::auth`) only *documents* the
//! always-Ok-logout / clear-on-stale-refresh / restore semantics the bespoke
//! FFI hand-rolled — a doc comment enforces nothing. `SessionContract`
//! wraps any `Arc<dyn AuthClient>` plus an optional injected
//! `Arc<dyn SessionStorage>` backend and *enforces* those invariants so every
//! consumer (FFI, CLI, Leptos, MCP) inherits them identically instead of
//! re-implementing — and re-regressing — them. The platform-specific storage
//! *backend* stays shell-injected (Android AES-GCM); only the invariant logic
//! is lifted here (spec §6 carve-out: zero business logic in shells, NOT zero
//! logic).

use crate::auth::{AuthClient, Session};
use crate::error::WalletError;
use agicash_traits::{PersistedSession, SessionStorage};
use std::sync::Arc;

/// Enforces the FFI-harvested session invariants over an `AuthClient`.
pub struct SessionContract {
    auth: Arc<dyn AuthClient>,
    storage: Option<Arc<dyn SessionStorage + Send + Sync>>,
}

impl std::fmt::Debug for SessionContract {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionContract")
            .field("storage_installed", &self.storage.is_some())
            .finish_non_exhaustive()
    }
}

impl SessionContract {
    /// Wrap an auth client with no persistent storage backend (iOS keeps its
    /// Swift Keychain; CLI in-memory fallback; tests).
    #[must_use]
    pub fn new(auth: Arc<dyn AuthClient>) -> Self {
        Self { auth, storage: None }
    }

    /// Wrap an auth client with a shell-injected persistent storage backend
    /// (Android `AndroidFileSessionStorage` AES-256-GCM).
    #[must_use]
    pub fn with_storage(
        auth: Arc<dyn AuthClient>,
        storage: Arc<dyn SessionStorage + Send + Sync>,
    ) -> Self {
        Self {
            auth,
            storage: Some(storage),
        }
    }

    /// INV-1 (P1-7): best-effort logout. ALWAYS returns `Ok(())` — a server
    /// or disk failure must still log the user out locally so the UI can
    /// navigate to sign-in. Mirrors FFI `auth_logout`
    /// (`agicash-ffi/src/wallet.rs:207-223` @ `241e8194`): clear in-memory
    /// FIRST (inner `AuthClient::logout` drops its slot), swallow its error,
    /// then clear the persisted blob, swallow that error too.
    pub async fn logout(&self) -> Result<(), WalletError> {
        if let Err(e) = self.auth.logout().await {
            tracing::warn!(
                target: "agicash_wallet::session",
                error = %e,
                "logout: inner auth logout failed (swallowed; clearing local)"
            );
        }
        if let Some(storage) = &self.storage {
            if let Err(e) = storage.clear().await {
                tracing::warn!(
                    target: "agicash_wallet::session",
                    error = %e,
                    "logout: storage.clear() failed (swallowed)"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;
    use agicash_domain::UserId;
    use async_trait::async_trait;
    use std::sync::Mutex;

    /// Auth fake whose `logout` ALWAYS errors — proves INV-1 swallows it.
    #[derive(Debug, Default)]
    struct ErroringLogoutAuth {
        session: Mutex<Option<Session>>,
    }

    #[async_trait]
    impl AuthClient for ErroringLogoutAuth {
        async fn register_guest(&self) -> Result<Session, WalletError> {
            let s = Session { user_id: UserId::new(), refresh_token: "rt".into() };
            *self.session.lock().unwrap() = Some(s.clone());
            Ok(s)
        }
        async fn login_email(&self, _e: &str, _p: &str) -> Result<Session, WalletError> {
            unimplemented!()
        }
        async fn register_email(
            &self,
            _e: &str,
            _p: &str,
            _n: Option<&str>,
        ) -> Result<Session, WalletError> {
            unimplemented!()
        }
        async fn logout(&self) -> Result<(), WalletError> {
            Err(WalletError::Auth("server 500 on logout".into()))
        }
        async fn set_session(&self, _s: Session) -> Result<(), WalletError> {
            unimplemented!()
        }
        async fn get_session(&self) -> Result<Option<Session>, WalletError> {
            Ok(self.session.lock().unwrap().clone())
        }
        async fn cashu_seed(&self) -> Result<[u8; 64], WalletError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn logout_returns_ok_even_when_inner_logout_errors() {
        let contract = SessionContract::new(Arc::new(ErroringLogoutAuth::default()));
        // INV-1: must be Ok despite the inner client erroring.
        assert!(contract.logout().await.is_ok());
    }

    /// Storage fake whose `clear` ALWAYS errors — proves INV-1 swallows the
    /// disk failure too (logout still Ok).
    #[derive(Debug, Default)]
    struct ErroringClearStorage;

    #[async_trait]
    impl SessionStorage for ErroringClearStorage {
        async fn store(&self, _s: &PersistedSession) -> Result<(), agicash_traits::AuthError> {
            Ok(())
        }
        async fn load(&self) -> Result<Option<PersistedSession>, agicash_traits::AuthError> {
            Ok(None)
        }
        async fn clear(&self) -> Result<(), agicash_traits::AuthError> {
            Err(agicash_traits::AuthError::Internal("disk full on clear".into()))
        }
    }

    /// Auth fake whose logout succeeds (isolate the storage-clear path).
    #[derive(Debug, Default)]
    struct OkLogoutAuth;

    #[async_trait]
    impl AuthClient for OkLogoutAuth {
        async fn register_guest(&self) -> Result<Session, WalletError> {
            Ok(Session { user_id: UserId::new(), refresh_token: "rt".into() })
        }
        async fn login_email(&self, _e: &str, _p: &str) -> Result<Session, WalletError> {
            unimplemented!()
        }
        async fn register_email(
            &self,
            _e: &str,
            _p: &str,
            _n: Option<&str>,
        ) -> Result<Session, WalletError> {
            unimplemented!()
        }
        async fn logout(&self) -> Result<(), WalletError> {
            Ok(())
        }
        async fn set_session(&self, _s: Session) -> Result<(), WalletError> {
            unimplemented!()
        }
        async fn get_session(&self) -> Result<Option<Session>, WalletError> {
            Ok(None)
        }
        async fn cashu_seed(&self) -> Result<[u8; 64], WalletError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn logout_returns_ok_even_when_storage_clear_errors() {
        let contract = SessionContract::with_storage(
            Arc::new(OkLogoutAuth),
            Arc::new(ErroringClearStorage),
        );
        // INV-1: a disk-clear failure must NOT fail logout.
        assert!(contract.logout().await.is_ok());
    }
}
