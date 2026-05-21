mod account;
mod auth;
mod cli;
mod composition;
mod decode;
mod mint;
mod receive;
mod receive_lightning;
mod send;
mod send_lightning;
mod send_lightning_address;

use account::AccountCmdError;
use agicash_lightning_address::LightningAddressError;
use agicash_traits::{AuthError, StorageError};
use auth::AuthCmdError;
use clap::Parser;
use cli::{AccountCommand, AuthCommand, Cli, Command, MintCommand, ReceiveCommand, SendCommand};
use composition::{build_deps, rehydrate_session};
use decode::DecodeCmdError;
use mint::MintCmdError;
use receive::ReceiveCmdError;
use receive_lightning::ReceiveLightningCmdError;
use send::SendCmdError;
use send_lightning::SendLightningCmdError;
use send_lightning_address::SendLightningAddressCmdError;
use serde::Serialize;

#[derive(Serialize)]
struct VersionOutput<'a> {
    version: &'a str,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: String,
}

#[derive(Serialize)]
struct ErrorOutput<'a> {
    error: ErrorBody<'a>,
}

/// Map a boxed CLI error to (error code, exit code).
///
/// Exit codes:
///   - `1` error — network, mint, storage, encoding and other runtime
///     failures with no more specific code.
///   - `2` bad arguments — a value the parser rejected, OR a usage
///     condition the caller can self-correct by re-invoking. clap rejects
///     most bad arguments before `run` is reached; this code is emitted
///     here for post-parse argument validation (e.g. a malformed UUID
///     passed to `account default`) and for `interactive-input-required`
///     (re-invoke `auth login`/`signup` with `--password-stdin`).
///   - `3` auth required — no session present or the session is
///     unauthenticated; the fix is `agicash auth login`.
///   - `4` not found — the addressed resource (e.g. an account id) does
///     not exist.
fn classify_error(e: &(dyn std::error::Error + 'static)) -> (&'static str, i32) {
    if let Some(auth_cmd) = e.downcast_ref::<AuthCmdError>() {
        return match auth_cmd {
            // No terminal and `--password-stdin` not passed. A usage
            // condition the agent can self-correct by re-invoking with
            // the flag, hence exit 2 (bad arguments) — NOT exit 1 generic
            // and NOT `internal-error` (the old crash code).
            AuthCmdError::InteractiveInputRequired => ("interactive-input-required", 2),
            // Verbatim the pre-fix codes for these two: the bare
            // `rpassword` read failure and the signup confirm-mismatch /
            // too-short conditions previously surfaced as
            // `AuthError::Internal` → (`internal-error`, 1). Preserved so
            // the `auth signup` help's "exit 1" claim stays accurate.
            AuthCmdError::ReadPassword(_)
            | AuthCmdError::PasswordMismatch
            | AuthCmdError::PasswordTooShort => ("internal-error", 1),
            AuthCmdError::Auth(inner) => classify_auth(inner),
        };
    }
    if let Some(acc) = e.downcast_ref::<AccountCmdError>() {
        return match acc {
            AccountCmdError::NotLoggedIn => ("not-logged-in", 3),
            AccountCmdError::InvalidId(_) => ("invalid-argument", 2),
            AccountCmdError::NotFound(_) => ("not-found", 4),
            AccountCmdError::UnsupportedCurrency(_) => ("unsupported-currency", 2),
            AccountCmdError::Auth(inner) => classify_auth(inner),
            AccountCmdError::Storage(inner) => (classify_storage(inner), 1),
        };
    }
    if let Some(mint_err) = e.downcast_ref::<MintCmdError>() {
        return match mint_err {
            MintCmdError::NotLoggedIn => ("not-logged-in", 3),
            MintCmdError::InvalidUrl(_) => ("invalid-mint-url", 1),
            MintCmdError::MintUnreachable(_) => ("mint-unreachable", 1),
            MintCmdError::MintError(_) => ("mint-error", 1),
            MintCmdError::UnsupportedCurrency(_) => ("unsupported-currency", 1),
            MintCmdError::Auth(inner) => classify_auth(inner),
            MintCmdError::Storage(inner) => (classify_storage(inner), 1),
        };
    }
    if let Some(rcv_err) = e.downcast_ref::<ReceiveCmdError>() {
        return match rcv_err {
            ReceiveCmdError::NotLoggedIn => ("not-logged-in", 3),
            ReceiveCmdError::InvalidToken(_) => ("invalid-token", 1),
            ReceiveCmdError::NoMatchingAccount(_) => ("no-matching-account", 1),
            ReceiveCmdError::Receive(_) => ("mint-error", 1),
            ReceiveCmdError::Storage(inner) => (classify_storage(inner), 1),
            ReceiveCmdError::Auth(inner) => classify_auth(inner),
        };
    }
    if let Some(rl_err) = e.downcast_ref::<ReceiveLightningCmdError>() {
        return match rl_err {
            ReceiveLightningCmdError::NotLoggedIn => ("not-logged-in", 3),
            ReceiveLightningCmdError::NoMatchingAccount => ("no-matching-account", 1),
            ReceiveLightningCmdError::AccountAmbiguous => ("account-ambiguous", 1),
            ReceiveLightningCmdError::InvalidAccountId(_) => ("invalid-account-id", 1),
            ReceiveLightningCmdError::InvalidQuoteId(_) => ("invalid-quote-id", 1),
            ReceiveLightningCmdError::AmountTooSmall => ("amount-too-small", 1),
            ReceiveLightningCmdError::QuoteNotPaid => ("quote-not-paid", 1),
            ReceiveLightningCmdError::Quote(_) => ("mint-error", 1),
            ReceiveLightningCmdError::Storage(inner) => (classify_storage(inner), 1),
            ReceiveLightningCmdError::Auth(inner) => classify_auth(inner),
        };
    }
    if let Some(sl_err) = e.downcast_ref::<SendLightningCmdError>() {
        return classify_send_lightning(sl_err);
    }
    if let Some(la_err) = e.downcast_ref::<SendLightningAddressCmdError>() {
        return match la_err {
            SendLightningAddressCmdError::Resolve(inner) => (classify_lnurl(inner), 1),
            SendLightningAddressCmdError::Send(inner) => classify_send_lightning(inner),
        };
    }
    if let Some(snd_err) = e.downcast_ref::<SendCmdError>() {
        return match snd_err {
            SendCmdError::NotLoggedIn => ("not-logged-in", 3),
            SendCmdError::NoMatchingAccount => ("no-matching-account", 1),
            SendCmdError::AccountAmbiguous => ("account-ambiguous", 1),
            SendCmdError::InvalidAccountId(_) => ("invalid-account-id", 1),
            SendCmdError::TokenEncode(_) => ("token-encode-error", 1),
            SendCmdError::InsufficientBalance(_) => ("insufficient-balance", 1),
            SendCmdError::Send(_) => ("mint-error", 1),
            SendCmdError::Storage(inner) => (classify_storage(inner), 1),
            SendCmdError::Auth(inner) => classify_auth(inner),
        };
    }
    if let Some(dec_err) = e.downcast_ref::<DecodeCmdError>() {
        // `decode` is offline: every failure is a code-1 input error.
        // It never produces auth-required (3) or not-found (4).
        return match dec_err {
            DecodeCmdError::Unrecognized => ("unrecognized-input", 1),
            DecodeCmdError::InvalidToken(_) => ("invalid-token", 1),
            DecodeCmdError::InvalidInvoice(_) => ("invalid-invoice", 1),
            DecodeCmdError::InvalidLightningAddress(_) => ("invalid-lightning-address", 1),
        };
    }
    if let Some(auth) = e.downcast_ref::<AuthError>() {
        return classify_auth(auth);
    }
    if let Some(st) = e.downcast_ref::<StorageError>() {
        return (classify_storage(st), 1);
    }
    ("unknown", 1)
}

fn classify_send_lightning(sl_err: &SendLightningCmdError) -> (&'static str, i32) {
    match sl_err {
        SendLightningCmdError::NotLoggedIn => ("not-logged-in", 3),
        SendLightningCmdError::NoMatchingAccount => ("no-matching-account", 1),
        SendLightningCmdError::AccountAmbiguous => ("account-ambiguous", 1),
        SendLightningCmdError::InvalidAccountId(_) => ("invalid-account-id", 1),
        SendLightningCmdError::InvalidQuoteId(_) => ("invalid-quote-id", 1),
        SendLightningCmdError::InsufficientBalance(_) => ("insufficient-balance", 1),
        SendLightningCmdError::Quote(_) => ("mint-error", 1),
        SendLightningCmdError::Storage(inner) => (classify_storage(inner), 1),
        SendLightningCmdError::Auth(inner) => classify_auth(inner),
    }
}

fn classify_lnurl(e: &LightningAddressError) -> &'static str {
    match e {
        LightningAddressError::InvalidAddress(_) => "invalid-lightning-address",
        LightningAddressError::Network(_) => "network-error",
        LightningAddressError::InvalidResponse(_) => "invalid-lnurl-response",
        LightningAddressError::AmountOutOfRange { .. } => "amount-out-of-range",
        LightningAddressError::ServerError(_) => "lnurl-server-error",
    }
}

fn classify_auth(e: &AuthError) -> (&'static str, i32) {
    match e {
        AuthError::Network(_) => ("network-error", 1),
        AuthError::Unauthenticated => ("unauthenticated", 3),
        AuthError::Backend(_) => ("auth-backend-error", 1),
        AuthError::Internal(_) => ("internal-error", 1),
    }
}

fn classify_storage(e: &StorageError) -> &'static str {
    match e {
        StorageError::Network(_) => "network-error",
        StorageError::NotFound => "not-found",
        StorageError::Backend(_) => "storage-backend-error",
        StorageError::Internal(_) => "internal-error",
    }
}

#[tokio::main]
async fn main() {
    let _ = dotenvy::dotenv();

    let args = Cli::parse();
    let exit_code = match run(args).await {
        Ok(()) => 0,
        Err(e) => {
            let (code, exit) = classify_error(e.as_ref());
            let body = ErrorOutput {
                error: ErrorBody {
                    code,
                    message: e.to_string(),
                },
            };
            eprintln!(
                "{}",
                serde_json::to_string(&body).expect("serialize error JSON")
            );
            exit
        }
    };
    std::process::exit(exit_code);
}

#[allow(clippy::too_many_lines)]
async fn run(args: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Version doesn't need auth or env vars — handle it before building deps.
    if let Some(Command::Version) = args.cmd {
        println!(
            "{}",
            serde_json::to_string(&VersionOutput {
                version: env!("CARGO_PKG_VERSION"),
            })
            .expect("serialize version JSON")
        );
        return Ok(());
    }

    // `decode` is offline — no session, no env, no network. Handle it
    // before building deps so it works with zero configuration.
    if let Some(Command::Decode { input }) = &args.cmd {
        decode::cmd_decode(input)?;
        return Ok(());
    }

    // Single composition root: the `WalletClient` facade via
    // `from_config` (+ the CLI-shell-resident keyring & thin
    // UserStorage handle), all wired from one endpoint config.
    let deps = build_deps().await?;
    // Hydrate the facade session from the keyring once at startup so
    // every subcommand inherits a live session when one persists across
    // processes. `set_session` (inside the facade) does the OpenSecret
    // handshake+refresh; a failed refresh clears the keyring inside the
    // helper; we swallow the error so `auth status`/`auth logout` still
    // run and report the resulting logged-out state.
    let _ = rehydrate_session(&deps).await;

    match args.cmd {
        Some(Command::Version | Command::Decode { .. }) => unreachable!("handled above"),
        Some(Command::Auth(a)) => match a.cmd {
            AuthCommand::Guest => auth::cmd_guest(&deps).await?,
            AuthCommand::Login {
                email,
                password_stdin,
            } => auth::cmd_login(&deps, email, password_stdin).await?,
            AuthCommand::Signup {
                email,
                password_stdin,
            } => auth::cmd_signup(&deps, email, password_stdin).await?,
            AuthCommand::Logout => auth::cmd_logout(&deps).await?,
            AuthCommand::Status => auth::cmd_status(&deps).await?,
        },
        Some(Command::Account(a)) => match a.cmd {
            AccountCommand::List => account::cmd_list(&deps).await?,
            AccountCommand::Info { id } => {
                account::cmd_info(&deps, &id).await?;
            }
            AccountCommand::Default { id } => {
                account::cmd_set_default(&deps, &id).await?;
            }
        },
        Some(Command::Mint(m)) => match m.cmd {
            MintCommand::Add { url, currency } => {
                mint::cmd_mint_add(&deps, &url, currency.into()).await?;
            }
            MintCommand::List => {
                mint::cmd_mint_list(&deps).await?;
            }
        },
        Some(Command::Balance { account: _ }) => {
            mint::cmd_balance(&deps).await?;
        }
        Some(Command::Receive(r)) => match r.cmd {
            ReceiveCommand::Token { token } => {
                receive::cmd_receive(&deps, &token).await?;
            }
            ReceiveCommand::Lightning {
                amount,
                account,
                currency,
                description,
                no_wait,
                poll_ms,
                timeout_s,
            } => {
                receive_lightning::cmd_receive_lightning(
                    &deps,
                    amount,
                    account,
                    currency.into(),
                    description,
                    no_wait,
                    poll_ms,
                    timeout_s,
                )
                .await?;
            }
            ReceiveCommand::LightningComplete {
                quote_id,
                poll_ms,
                timeout_s,
            } => {
                receive_lightning::cmd_receive_lightning_complete(
                    &deps, quote_id, poll_ms, timeout_s,
                )
                .await?;
            }
        },
        Some(Command::Send(s)) => match s.cmd {
            SendCommand::Token {
                amount,
                account,
                token_version,
                dry_run,
            } => {
                send::cmd_send(&deps, amount, account, token_version.into(), dry_run).await?;
            }
            SendCommand::Lightning {
                invoice,
                account,
                dry_run,
                no_wait,
                poll_ms,
                timeout_s,
            } => {
                send_lightning::cmd_send_lightning(
                    &deps, invoice, account, dry_run, no_wait, poll_ms, timeout_s,
                )
                .await?;
            }
            SendCommand::LightningComplete {
                quote_id,
                poll_ms,
                timeout_s,
            } => {
                send_lightning::cmd_send_lightning_complete(&deps, quote_id, poll_ms, timeout_s)
                    .await?;
            }
            SendCommand::LightningAddress {
                address,
                amount,
                account,
                comment,
                dry_run,
                no_wait,
                poll_ms,
                timeout_s,
            } => {
                send_lightning_address::cmd_send_lightning_address(
                    &deps, address, amount, account, comment, dry_run, no_wait, poll_ms, timeout_s,
                )
                .await?;
            }
        },
        None => {}
    }
    Ok(())
}
