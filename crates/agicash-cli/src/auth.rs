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
use std::io::{IsTerminal, Read};

/// Error type for the `auth login` / `auth signup` commands.
///
/// `auth` is the one place the CLI must read a secret. A secret cannot
/// travel on argv (`ps`) or env (`/proc`, child env), so it travels on
/// stdin only. Two stdin shapes exist:
///
///   - **interactive** — a human at a TTY; the password is prompted with
///     echo off (`rpassword::prompt_password`).
///   - **non-interactive** — an agent or script; the caller passes
///     `--password-stdin` and pipes the password as one line on fd 0.
///
/// When stdin is *not* a TTY and `--password-stdin` was not passed there
/// is no safe way to obtain the secret. The old code blindly called
/// `rpassword::prompt_password`, which forces `/dev/tty` open and crashes
/// with an `internal-error` ("Device not configured"). `InteractiveInputRequired`
/// replaces that crash with a clean, prescriptive, self-correctable error.
#[derive(Debug, thiserror::Error)]
pub enum AuthCmdError {
    /// No TTY and `--password-stdin` was not passed — the caller must
    /// re-invoke with `--password-stdin` and pipe the password on fd 0.
    #[error(
        "password input unavailable: stdin is not a terminal. \
         Re-run with --password-stdin and pipe the password on stdin, e.g. \
         `printf %s \"$pw\" | agicash auth login <email> --password-stdin`"
    )]
    InteractiveInputRequired,
    /// Failed to read the password from stdin / the terminal.
    #[error("read password: {0}")]
    ReadPassword(String),
    /// `auth signup`: the two interactive prompts did not match.
    #[error("passwords do not match")]
    PasswordMismatch,
    /// `auth signup`: the password is shorter than the 8-char minimum.
    #[error("password must have at least 8 characters")]
    PasswordTooShort,
    /// An error from the auth backend / facade.
    #[error(transparent)]
    Auth(#[from] AuthError),
}

/// Obtain the account password for `auth login` / `auth signup`.
///
/// `--password-stdin` (a boolean flag — the secret is never an argv
/// value) reads the password from fd 0 as a single piped chunk; one read,
/// one trailing `\n` stripped. Without the flag: if stdin is a TTY the
/// password is prompted with echo off; if stdin is NOT a TTY there is no
/// safe input channel, so a clean `InteractiveInputRequired` is returned
/// instead of letting `rpassword` crash on a missing `/dev/tty`.
fn read_password(password_stdin: bool, prompt: &str) -> Result<String, AuthCmdError> {
    if password_stdin {
        // Non-interactive: the secret arrives on fd 0 (real stdin), never
        // on argv or in the environment. Read the whole pipe once, then
        // strip a single trailing newline (`printf %s` writes none; a
        // `read`/`echo` pipeline writes exactly one).
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| AuthCmdError::ReadPassword(e.to_string()))?;
        if buf.ends_with('\n') {
            buf.pop();
            if buf.ends_with('\r') {
                buf.pop();
            }
        }
        return Ok(buf);
    }
    if !std::io::stdin().is_terminal() {
        // No flag, no TTY: refuse cleanly instead of crashing in
        // `rpassword` (which forces `/dev/tty` and fails with
        // `internal-error` when there is no controlling terminal).
        return Err(AuthCmdError::InteractiveInputRequired);
    }
    // A human at a terminal: prompt with echo off, exactly as before.
    rpassword::prompt_password(prompt).map_err(|e| AuthCmdError::ReadPassword(e.to_string()))
}

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

pub async fn cmd_login(
    deps: &CliDeps,
    email: String,
    password_stdin: bool,
) -> Result<(), AuthCmdError> {
    let password = read_password(password_stdin, "Password: ")?;
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

pub async fn cmd_signup(
    deps: &CliDeps,
    email: String,
    password_stdin: bool,
) -> Result<(), AuthCmdError> {
    let password = if password_stdin {
        // Non-interactive: an agent passing `--password-stdin` has already
        // decided the password — read it ONCE and skip the confirm prompt
        // (the double-prompt is a human typo-guard, not a wire contract).
        read_password(true, "Password: ")?
    } else {
        // Interactive: prompt twice to match the web `confirm-password`
        // field, so a typo doesn't silently create an account whose
        // password the operator can't reproduce.
        let password = read_password(false, "Password: ")?;
        let confirm = read_password(false, "Confirm password: ")?;
        if password != confirm {
            return Err(AuthCmdError::PasswordMismatch);
        }
        password
    };
    if password.len() < 8 {
        return Err(AuthCmdError::PasswordTooShort);
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
