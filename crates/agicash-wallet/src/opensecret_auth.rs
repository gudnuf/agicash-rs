//! Production `AuthClient` impl over `agicash-auth-opensecret`.
//!
//! Moves the FFI's existing auth-method behavior (register guest/email,
//! login, logout, set/get session, cashu seed) behind the facade's
//! `AuthClient` trait verbatim — NO behavior change vs. the FFI bodies
//! at `agicash-rs/master` `3eabf076` (Rust source byte-identical to the
//! plan's `241e8194` ref). The FFI keeps its own shell-resident session
//! slot + persistence plumbing (platform-layer carve-out, spec §6);
//! this client owns the slot the facade reads.

use crate::auth::{AuthClient, Session};
use crate::error::WalletError;
use agicash_auth_opensecret::{
    auth_error_from_opensecret, login_email, logout, register_email, register_guest,
    OpenSecretClient,
};
use agicash_traits::{PersistedSession, SessionStorage};
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;

fn random_password() -> String {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf).expect("OS RNG must be available");
    hex::encode(buf)
}

/// Concrete auth backend wrapping `OpenSecretClient`. Owns the in-memory
/// session slot the facade's `auth_*`/`require_session` methods read.
pub struct OpenSecretAuthClient {
    client: OpenSecretClient,
    session: Arc<RwLock<Option<PersistedSession>>>,
    /// Optional persistent backend. `from_config` installs InMemory or
    /// the Android file backend here. Errors on store/clear are logged,
    /// never surfaced (matches FFI `persist_session` semantics).
    storage: Arc<dyn SessionStorage>,
}

// Manual `Debug` — `dyn SessionStorage` is not `Debug` (the trait does
// not require it). Mirrors the FFI's manual `Debug for AgicashWallet`.
// The `AuthClient` trait requires `Debug`, so this is load-bearing.
impl std::fmt::Debug for OpenSecretAuthClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenSecretAuthClient")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl OpenSecretAuthClient {
    #[must_use]
    pub fn new(client: OpenSecretClient, storage: Arc<dyn SessionStorage>) -> Self {
        Self {
            client,
            session: Arc::new(RwLock::new(None)),
            storage,
        }
    }

    /// Shared handle to the session slot so the FFI shell can mirror it
    /// into its own shell-resident slot without changing realtime/session
    /// plumbing (Hard Rule 7).
    #[must_use]
    pub fn session_slot(&self) -> Arc<RwLock<Option<PersistedSession>>> {
        Arc::clone(&self.session)
    }

    async fn persist(&self, s: &PersistedSession) {
        if let Err(e) = self.storage.store(s).await {
            tracing::warn!(
                target: "agicash_wallet::opensecret_auth",
                error = %e,
                "persist session failed (continuing in-memory)"
            );
        }
    }
}

#[async_trait]
impl AuthClient for OpenSecretAuthClient {
    async fn register_guest(&self) -> Result<Session, WalletError> {
        let password = random_password();
        let resp = register_guest(&self.client, password, self.client.client_id())
            .await
            .map_err(WalletError::from)?;
        let persisted = PersistedSession {
            user_id: resp.id,
            refresh_token: resp.refresh_token.clone(),
        };
        *self.session.write().await = Some(persisted.clone());
        self.persist(&persisted).await;
        Ok(Session {
            user_id: agicash_domain::UserId::from(persisted.user_id),
            refresh_token: persisted.refresh_token,
        })
    }

    async fn login_email(&self, email: &str, password: &str) -> Result<Session, WalletError> {
        let resp = login_email(
            &self.client,
            email.to_string(),
            password.to_string(),
            self.client.client_id(),
        )
        .await
        .map_err(WalletError::from)?;
        let persisted = PersistedSession {
            user_id: resp.id,
            refresh_token: resp.refresh_token.clone(),
        };
        *self.session.write().await = Some(persisted.clone());
        self.persist(&persisted).await;
        Ok(Session {
            user_id: agicash_domain::UserId::from(persisted.user_id),
            refresh_token: persisted.refresh_token,
        })
    }

    async fn register_email(
        &self,
        email: &str,
        password: &str,
        name: Option<&str>,
    ) -> Result<Session, WalletError> {
        let resp = register_email(
            &self.client,
            email.to_string(),
            password.to_string(),
            self.client.client_id(),
            name.map(str::to_string),
        )
        .await
        .map_err(WalletError::from)?;
        let persisted = PersistedSession {
            user_id: resp.id,
            refresh_token: resp.refresh_token.clone(),
        };
        *self.session.write().await = Some(persisted.clone());
        self.persist(&persisted).await;
        Ok(Session {
            user_id: agicash_domain::UserId::from(persisted.user_id),
            refresh_token: persisted.refresh_token,
        })
    }

    async fn logout(&self) -> Result<(), WalletError> {
        let was_loaded = self.session.read().await.is_some();
        if was_loaded {
            // Server logout failures are non-fatal — clear local anyway
            // (verbatim FFI `auth_logout` semantics).
            let _ = logout(&self.client).await;
        }
        *self.session.write().await = None;
        if let Err(e) = self.storage.clear().await {
            tracing::warn!(
                target: "agicash_wallet::opensecret_auth",
                error = %e,
                "clear persisted session failed"
            );
        }
        Ok(())
    }

    async fn set_session(&self, session: Session) -> Result<(), WalletError> {
        let refresh_token = session.refresh_token.clone();
        self.client
            .ensure_handshake()
            .await
            .map_err(WalletError::from)?;
        self.client
            .inner()
            .set_tokens(String::new(), Some(refresh_token.clone()))
            .map_err(|e| WalletError::from(auth_error_from_opensecret(e)))?;
        if let Err(e) = self.client.inner().refresh_token().await {
            *self.session.write().await = None;
            return Err(WalletError::from(auth_error_from_opensecret(e)));
        }
        let persisted = PersistedSession {
            user_id: session.user_id.as_uuid(),
            refresh_token,
        };
        *self.session.write().await = Some(persisted.clone());
        self.persist(&persisted).await;
        Ok(())
    }

    async fn get_session(&self) -> Result<Option<Session>, WalletError> {
        Ok(self.session.read().await.clone().map(|p| Session {
            user_id: agicash_domain::UserId::from(p.user_id),
            refresh_token: p.refresh_token,
        }))
    }

    async fn cashu_seed(&self) -> Result<[u8; 64], WalletError> {
        self.client
            .get_cashu_seed()
            .await
            .map_err(WalletError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_object_safe_behind_arc_dyn() {
        // Compile-time proof the impl satisfies the trait object bound
        // the builder needs (`Arc<dyn AuthClient>`).
        fn assert_auth_client<T: AuthClient>() {}
        assert_auth_client::<OpenSecretAuthClient>();
    }
}
