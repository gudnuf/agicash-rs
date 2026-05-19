//! Public construction config for `WalletClient::from_config`.
//!
//! The single input contract all binding shells share. The only thing
//! that differs per shell is the session-storage backend choice; every
//! other dep is wired identically inside `from_config`.

use uuid::Uuid;

/// Which session-storage backend `from_config` installs. This is the
/// ONLY axis the three shells differ on — everything else in the
/// composition is identical.
#[derive(Debug, Clone)]
pub enum SessionStorageChoice {
    /// Process-lifetime only (CLI default, iOS — iOS persists the
    /// refresh token in Keychain on the Swift side, not here).
    InMemory,
    /// Android AES-256-GCM file backend rooted at the app's private
    /// data dir. `dir` is `Context.getFilesDir().getAbsolutePath()`.
    Android { dir: String },
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
            SessionStorageChoice::InMemory => panic!("wrong variant"),
        }
    }
}
