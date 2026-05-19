//! `FakeAuthClient` — a public, hermetic [`AuthClient`] impl.
//!
//! Mirrors the well-formed `FakeAuth` already living in
//! `agicash-wallet/src/auth.rs` (the `#[cfg(test)]` documentation example),
//! but lifted out as a reusable, public test fixture: `parking_lot::Mutex`
//! (house style), a **non-zero** deterministic 64-byte seed (`[7u8; 64]`)
//! so the cashu state machines never see an all-zero master seed, plus
//! `new()` (logged-out) / `logged_in()` (pre-sessioned) constructors and a
//! `user_id()` accessor for ownership-mismatch tests.

use agicash_wallet::{AuthClient, Session, WalletError};
use async_trait::async_trait;
use agicash_domain::UserId;
use parking_lot::Mutex;

/// In-memory auth backend. No network. Hands out a usable [`Session`] and a
/// deterministic non-zero 64-byte seed.
#[derive(Debug)]
pub struct FakeAuthClient {
    session: Mutex<Option<Session>>,
    seed: [u8; 64],
}

impl Default for FakeAuthClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeAuthClient {
    /// Logged-out client. `cashu_seed`/`require_session` callers see
    /// [`WalletError::Unauthenticated`] until a session is established.
    #[must_use]
    pub fn new() -> Self {
        Self {
            session: Mutex::new(None),
            // Non-zero on purpose: an all-zero master seed is a footgun
            // for the cashu blinded-message machinery.
            seed: [7u8; 64],
        }
    }

    /// Pre-sessioned client. Equivalent to `new()` then a successful
    /// `register_guest()`, but the `user_id` is captured up front so a
    /// test can assert ownership against a *different* user.
    #[must_use]
    pub fn logged_in() -> Self {
        let c = Self::new();
        *c.session.lock() = Some(Session {
            user_id: UserId::new(),
            refresh_token: "fake-logged-in-rt".into(),
        });
        c
    }

    /// The currently-loaded session's `user_id`, or `None` if logged out.
    #[must_use]
    pub fn user_id(&self) -> Option<UserId> {
        self.session.lock().as_ref().map(|s| s.user_id)
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl AuthClient for FakeAuthClient {
    async fn register_guest(&self) -> Result<Session, WalletError> {
        let s = Session {
            user_id: UserId::new(),
            refresh_token: "guest-rt".into(),
        };
        *self.session.lock() = Some(s.clone());
        Ok(s)
    }

    async fn login_email(&self, _email: &str, _pw: &str) -> Result<Session, WalletError> {
        let s = Session {
            user_id: UserId::new(),
            refresh_token: "email-rt".into(),
        };
        *self.session.lock() = Some(s.clone());
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
        *self.session.lock() = Some(s.clone());
        Ok(s)
    }

    async fn logout(&self) -> Result<(), WalletError> {
        *self.session.lock() = None;
        Ok(())
    }

    async fn set_session(&self, session: Session) -> Result<(), WalletError> {
        *self.session.lock() = Some(session);
        Ok(())
    }

    async fn get_session(&self) -> Result<Option<Session>, WalletError> {
        Ok(self.session.lock().clone())
    }

    async fn cashu_seed(&self) -> Result<[u8; 64], WalletError> {
        if self.session.lock().is_none() {
            return Err(WalletError::Unauthenticated);
        }
        Ok(self.seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_is_logged_out() {
        let a = FakeAuthClient::new();
        assert!(a.get_session().await.unwrap().is_none());
        assert!(a.user_id().is_none());
        assert!(matches!(
            a.cashu_seed().await,
            Err(WalletError::Unauthenticated)
        ));
    }

    #[tokio::test]
    async fn logged_in_has_session_and_nonzero_seed() {
        let a = FakeAuthClient::logged_in();
        let uid = a.user_id().expect("logged_in must have a user");
        assert_eq!(a.get_session().await.unwrap().unwrap().user_id, uid);
        let seed = a.cashu_seed().await.unwrap();
        assert_ne!(seed, [0u8; 64], "seed must be non-zero");
    }

    #[tokio::test]
    async fn register_guest_then_logout_round_trips() {
        let a = FakeAuthClient::new();
        let s = a.register_guest().await.unwrap();
        assert_eq!(a.get_session().await.unwrap(), Some(s));
        a.logout().await.unwrap();
        assert!(a.get_session().await.unwrap().is_none());
    }
}
