//! `auth` subcommands — now composed over the `WalletClient` facade.
//!
//! The facade owns the `OpenSecret` handshake/refresh + the in-memory
//! session slot (`OpenSecretAuthClient`, byte-identical to the prior
//! `deps.client` bodies). The CLI shell owns keyring persistence the way
//! the iOS shell owns Keychain: after a successful `auth_*` the returned
//! `Session` is written through to the keyring; `logout` clears it;
//! `status` reads the (already-rehydrated) facade slot. stdout JSON is
//! byte-for-byte the pre-migration contract.

use crate::composition::{persist_session, wallet_err_to_auth, CliDeps};
use agicash_traits::AuthError;
use serde::Serialize;

#[derive(Serialize)]
struct SignedIn<'a> {
    status: &'a str,
    user_id: String,
    guest: bool,
}

#[derive(Serialize)]
#[serde(untagged)]
enum StatusOutput {
    LoggedIn { logged_in: bool, user_id: String },
    LoggedOut { logged_in: bool },
}

#[derive(Serialize)]
struct LogoutOutput<'a> {
    status: &'a str,
}

fn print_json<T: Serialize>(value: &T) {
    // Stdout is the structured result channel; agents parse line-by-line.
    println!("{}", serde_json::to_string(value).expect("serialize JSON"));
}

pub async fn cmd_guest(deps: &CliDeps) -> Result<(), AuthError> {
    let session = deps.wallet.auth_guest().await.map_err(wallet_err_to_auth)?;
    persist_session(deps, &session).await?;
    print_json(&SignedIn {
        status: "signed-in",
        user_id: session.user_id.as_uuid().to_string(),
        guest: true,
    });
    Ok(())
}

pub async fn cmd_login(deps: &CliDeps, email: String) -> Result<(), AuthError> {
    let password = rpassword::prompt_password("Password: ")
        .map_err(|e| AuthError::Internal(format!("read password: {e}")))?;
    let session = deps
        .wallet
        .auth_login(&email, &password)
        .await
        .map_err(wallet_err_to_auth)?;
    persist_session(deps, &session).await?;
    print_json(&SignedIn {
        status: "signed-in",
        user_id: session.user_id.as_uuid().to_string(),
        guest: false,
    });
    Ok(())
}

pub async fn cmd_signup(deps: &CliDeps, email: String) -> Result<(), AuthError> {
    // Prompt twice to match the web `confirm-password` field. We keep the
    // confirmation enforcement here so a typo doesn't silently create an
    // account whose password the operator can't reproduce.
    let password = rpassword::prompt_password("Password: ")
        .map_err(|e| AuthError::Internal(format!("read password: {e}")))?;
    let confirm = rpassword::prompt_password("Confirm password: ")
        .map_err(|e| AuthError::Internal(format!("read password: {e}")))?;
    if password != confirm {
        return Err(AuthError::Internal("passwords do not match".into()));
    }
    if password.len() < 8 {
        return Err(AuthError::Internal(
            "password must have at least 8 characters".into(),
        ));
    }
    let session = deps
        .wallet
        .auth_signup(&email, &password, None)
        .await
        .map_err(wallet_err_to_auth)?;
    persist_session(deps, &session).await?;
    print_json(&SignedIn {
        status: "signed-in",
        user_id: session.user_id.as_uuid().to_string(),
        guest: false,
    });
    Ok(())
}

pub async fn cmd_logout(deps: &CliDeps) -> Result<(), AuthError> {
    if deps.keyring.load().await?.is_none() {
        print_json(&LogoutOutput {
            status: "not-logged-in",
        });
        return Ok(());
    }
    // Best-effort server logout via the facade. Even if it fails (network
    // / expired session) we clear local state so the command is
    // idempotent (verbatim prior semantics).
    if let Err(e) = deps.wallet.auth_logout().await {
        eprintln!("warning: server logout failed: {e}");
    }
    deps.keyring.clear().await?;
    print_json(&LogoutOutput {
        status: "signed-out",
    });
    Ok(())
}

pub async fn cmd_status(deps: &CliDeps) -> Result<(), AuthError> {
    // Read the keyring (the durable source of truth across processes) —
    // verbatim the prior `deps.storage.load()` contract.
    let out = match deps.keyring.load().await? {
        None => StatusOutput::LoggedOut { logged_in: false },
        Some(session) => StatusOutput::LoggedIn {
            logged_in: true,
            user_id: session.user_id.to_string(),
        },
    };
    print_json(&out);
    Ok(())
}
