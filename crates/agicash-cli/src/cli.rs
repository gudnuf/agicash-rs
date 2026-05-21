use clap::{Parser, Subcommand, ValueEnum};

/// Top-level orientation banner. Stated once — the one piece of contract an
/// agent cannot derive from the command tree itself.
const TOP_BANNER: &str = "\
OUTPUT CONTRACT
  Success: exactly one JSON line on stdout, exit 0.
  Failure: {\"error\":{\"code\":\"…\",\"message\":\"…\"}} on stderr, non-zero exit.

EXIT CODES
  0  success
  1  error (network, mint, storage, …)
  2  bad arguments (rejected by the parser)
  3  auth required (no session — run `agicash auth login`)
  4  not found

GETTING STARTED
  Most commands need a session. Run `agicash auth login` or
  `agicash auth guest` first.

DISCOVERY
  Drill into any command with `<command> --help`. Use `-h` for a terse
  scan, `--help` for the full page.";

#[derive(Parser, Debug)]
#[command(
    name = "agicash",
    version,
    about = "Agicash CLI — self-custody Bitcoin wallet (JSON output)",
    long_about = "Agicash CLI — a self-custody Cashu Bitcoin wallet driven entirely \
through JSON.\n\nEvery command prints exactly one JSON line on success and a \
structured JSON error on failure. The CLI is designed as an agent surface: walk \
the command tree with `--help` to learn each operation without trial and error.",
    after_long_help = TOP_BANNER
)]
pub struct Cli {
    #[command(subcommand)]
    pub cmd: Option<Command>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Print the SDK version.
    #[command(
        long_about = "Print the agicash SDK version. Offline, no session required.",
        after_long_help = "EXAMPLE\n  $ agicash version\n  {\"version\":\"0.1.0\"}\n\n  \
version  the agicash-cli package version (semver).\n\nEXIT CODES\n  0  success\n\n\
SEE ALSO\n  agicash --help"
    )]
    Version,
    /// Authentication and session management.
    #[command(long_about = "Authentication and session management. The session is \
persisted across processes (OS keyring where available), so `auth login` once \
then run other commands freely. Run `agicash auth <sub> --help` for detail.")]
    Auth(AuthArgs),
    /// Accounts (cashu and spark) for the current user.
    #[command(
        long_about = "Inspect and configure the current user's accounts (cashu and \
spark). Requires a session. Run `agicash account <sub> --help` for detail."
    )]
    Account(AccountArgs),
    /// Manage Cashu mints.
    #[command(
        long_about = "Manage Cashu mints. Adding a mint creates an account backed \
by it; an account is required before you can receive or send. Run \
`agicash mint <sub> --help` for detail."
    )]
    Mint(MintArgs),
    /// Show balance for all accounts (or a specific account).
    #[command(
        long_about = "Show the spendable balance of every account, or one account \
when `--account` is given. Requires a session.",
        after_long_help = "EXAMPLE\n  $ agicash balance\n  \
[{\"account_id\":\"…\",\"name\":\"testnut\",\"currency\":\"BTC\",\"balance\":\"1200\",\"unit\":\"sat\"}]\n\n  \
JSON array, one object per account. `balance` is in the smallest unit\n  \
(`sat` for BTC, `cent` for USD). Non-BTC accounts also carry a\n  \
`btc_equivalent` / `rate_btc` pair (or `btc_equivalent_error` if the\n  \
rate provider is down).\n\nEXIT CODES\n  0  success\n  1  storage/network error\n  \
3  not authenticated\n\nSEE ALSO\n  agicash account list, agicash mint add"
    )]
    Balance {
        /// Show balance for a specific account ID only (UUID).
        #[arg(long)]
        account: Option<String>,
    },
    /// Decode a Cashu token, BOLT-11 invoice, or Lightning Address.
    #[command(
        long_about = "Decode and inspect a Cashu token (`cashuA…`/`cashuB…`), a \
BOLT-11 Lightning invoice, or a LUD-16 Lightning Address. Offline — no session, \
no network, no mint round-trip. The input type is auto-detected. Use it to \
inspect an artifact before acting on it.",
        after_long_help = "EXAMPLE\n  $ agicash decode cashuBo2Ftd…\n  \
{\"artifact\":\"cashu-token\",\"version\":4,\"mint_url\":\"…\",\"amount\":\"100\",\
\"unit\":\"sat\",\"proof_count\":3,\"memo\":\"…\"}\n\n  \
artifact  `cashu-token`, `bolt11-invoice`, or `lightning-address`.\n  \
amount    summed offline from the proofs (token) or the invoice amount;\n  \
          omitted for an amountless invoice.\n  \
proof_count / payee / expiry / …  fields vary by artifact type.\n\n\
EXIT CODES\n  0  success\n  1  unrecognized input, or a malformed token /\n  \
   invoice / address\n\nSEE ALSO\n  agicash receive token, agicash send lightning"
    )]
    Decode {
        /// The string to decode: a `cashuA…`/`cashuB…` token, a `lnbc…`
        /// BOLT-11 invoice, or a `user@domain` Lightning Address.
        input: String,
    },
    /// Receive funds into a Cashu account.
    #[command(
        long_about = "Receive funds into a Cashu account — either by claiming a \
Cashu token (NUT-03 swap) or by issuing a Lightning invoice (NUT-04 mint quote). \
Run `agicash receive <sub> --help` for detail."
    )]
    Receive(ReceiveArgs),
    /// Send funds out of a Cashu account.
    #[command(
        long_about = "Send funds out of a Cashu account — produce a Cashu token \
(NUT-03 swap), pay a BOLT-11 invoice (NUT-05 melt), or pay a Lightning Address. \
Run `agicash send <sub> --help` for detail."
    )]
    Send(SendArgs),
}

/// Cashu token serialization format. Maps to `agicash_wallet::TokenVersion`.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenVersion {
    /// V3 — legacy JSON encoding (`cashuA…`).
    #[value(name = "3")]
    V3,
    /// V4 — compact CBOR encoding (`cashuB…`), the default.
    #[value(name = "4")]
    V4,
}

impl From<TokenVersion> for agicash_wallet::TokenVersion {
    fn from(v: TokenVersion) -> Self {
        match v {
            TokenVersion::V3 => agicash_wallet::TokenVersion::V3,
            TokenVersion::V4 => agicash_wallet::TokenVersion::V4,
        }
    }
}

/// Currency selector for CLI arguments. Restricted to the two end-user
/// currencies; maps to `agicash_domain::Currency`.
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Currency {
    /// Bitcoin — amounts in sats.
    #[value(name = "BTC", alias = "btc")]
    Btc,
    /// US dollar — amounts in cents.
    #[value(name = "USD", alias = "usd")]
    Usd,
}

impl From<Currency> for agicash_domain::Currency {
    fn from(c: Currency) -> Self {
        match c {
            Currency::Btc => agicash_domain::Currency::Btc,
            Currency::Usd => agicash_domain::Currency::Usd,
        }
    }
}

#[derive(clap::Args, Debug)]
pub struct SendArgs {
    #[command(subcommand)]
    pub cmd: SendCommand,
}

#[derive(Subcommand, Debug)]
pub enum SendCommand {
    /// Produce a Cashu token from the account (NUT-03 send-swap).
    #[command(
        long_about = "Produce a Cashu token by swapping proofs out of a BTC Cashu \
account (NUT-03). The printed `token` string is bearer value — anyone who has it \
can claim it. Use `--dry-run` to preview the fee before committing.",
        after_long_help = "EXAMPLE\n  $ agicash send token 100\n  \
{\"status\":\"sent\",\"token\":\"cashuB…\",\"amount\":\"100\",\"fee\":\"0\",\"unit\":\"sat\",\
\"currency\":\"BTC\",\"account_id\":\"…\",\"mint_url\":\"…\",\"swap_id\":\"…\",\"token_hash\":\"…\"}\n\n  \
token       the encoded bearer token — hand this to the recipient.\n  \
amount/fee  amount sent and mint fee, in `unit`.\n  \
token_hash  stable id for this token (idempotency / lookup).\n\n\
EXIT CODES\n  0  success\n  1  insufficient balance, mint or storage error\n  \
2  bad arguments (e.g. invalid --token-version)\n  3  not authenticated\n\n\
SEE ALSO\n  agicash receive token, agicash balance"
    )]
    Token {
        /// Amount to send, in sats.
        amount: u64,
        /// Account ID to send from (UUID). Omit to use the only Cashu
        /// account; required when several exist.
        #[arg(long)]
        account: Option<String>,
        /// Token format version: 4 (CBOR, default) or 3 (legacy JSON).
        #[arg(long, value_enum, default_value = "4")]
        token_version: TokenVersion,
        /// Preview the fee without persisting or producing a token.
        #[arg(long)]
        dry_run: bool,
    },
    /// Pay a BOLT-11 invoice via NUT-05 melt.
    #[command(
        long_about = "Pay a BOLT-11 Lightning invoice by melting Cashu proofs \
(NUT-05). The invoice must carry an amount. The command begins the melt then \
polls until it settles; use `--dry-run` to preview the fee first.",
        after_long_help = "EXAMPLE\n  $ agicash send lightning lnbc1u1p…\n  \
{\"status\":\"paid\",\"quote_id\":\"…\",\"amount\":\"100\",\"lightning_fee\":\"1\",\
\"cashu_fee\":\"0\",\"total_fee\":\"1\",\"amount_spent\":\"101\",\"payment_preimage\":\"…\",\
\"account_id\":\"…\",\"payment_hash\":\"…\"}\n\n  \
status            `paid`, `failed`, or `timed-out`.\n  \
payment_preimage  proof of payment (present only on `paid`).\n  \
quote_id          melt-quote id — pass to `send lightning-complete` to\n  \
                  resume a `timed-out` or in-flight payment.\n\n\
EXIT CODES\n  0  success (incl. `failed`/`timed-out` outcomes)\n  \
1  insufficient balance, mint or network error\n  3  not authenticated\n\n\
SEE ALSO\n  agicash send lightning-complete, agicash send lightning-address"
    )]
    Lightning {
        /// BOLT-11 invoice to pay (must include an amount).
        invoice: String,
        /// Account ID to send from (UUID). Omit to use the only Cashu
        /// account; required when several exist.
        #[arg(long)]
        account: Option<String>,
        /// Preview the fee without persisting or paying.
        #[arg(long)]
        dry_run: bool,
        /// Begin the melt and return without waiting; resume later with
        /// `agicash send lightning-complete <quote_id>`.
        #[arg(long)]
        no_wait: bool,
        /// Polling interval in milliseconds while the melt is in flight.
        #[arg(long, default_value_t = 1000)]
        poll_ms: u64,
        /// Overall timeout in seconds before reporting `timed-out`.
        #[arg(long, default_value_t = 300)]
        timeout_s: u64,
    },
    /// Finish a previously-initiated Lightning send.
    #[command(
        long_about = "Resume an in-flight Lightning send by its melt-quote id — \
used after `send lightning --no-wait` or to retry a `timed-out` payment. \
Reconcile-aware: it polls the existing quote, it never re-pays.",
        after_long_help = "EXAMPLE\n  $ agicash send lightning-complete \
11111111-2222-3333-4444-555555555555\n  \
{\"status\":\"paid\",\"quote_id\":\"…\",\"amount\":\"100\",\"total_fee\":\"1\",\
\"payment_preimage\":\"…\",\"account_id\":\"…\",\"payment_hash\":\"…\"}\n\n  \
status  `paid`, `failed`, or `timed-out` — same shape as `send lightning`.\n\n\
EXIT CODES\n  0  success (incl. `failed`/`timed-out` outcomes)\n  \
1  invalid quote id, mint or network error\n  3  not authenticated\n\n\
SEE ALSO\n  agicash send lightning"
    )]
    LightningComplete {
        /// The melt-quote id (UUID) returned by `send lightning --no-wait`.
        quote_id: String,
        /// Polling interval in milliseconds while the melt is in flight.
        #[arg(long, default_value_t = 1000)]
        poll_ms: u64,
        /// Overall timeout in seconds before reporting `timed-out`.
        #[arg(long, default_value_t = 30)]
        timeout_s: u64,
    },
    /// Pay a LUD-16 Lightning Address (`user@domain`).
    #[command(
        long_about = "Pay a LUD-16 Lightning Address (`user@domain`). The CLI \
resolves the address's well-known endpoint, fetches a BOLT-11 invoice for the \
requested amount, then runs the regular NUT-05 melt flow.",
        after_long_help = "EXAMPLE\n  $ agicash send lightning-address \
alice@walletofsatoshi.com 100\n  \
{\"status\":\"paid\",\"quote_id\":\"…\",\"amount\":\"100\",\"total_fee\":\"1\",\
\"payment_preimage\":\"…\",\"account_id\":\"…\",\"payment_hash\":\"…\"}\n\n  \
The command first prints `resolved` and `invoice-fetched` lines, then the\n  \
melt outcome (`paid`/`failed`/`timed-out`) — same shape as `send lightning`.\n\n\
EXIT CODES\n  0  success (incl. `failed`/`timed-out` outcomes)\n  \
1  address resolution, insufficient balance or network error\n  \
3  not authenticated\n\nSEE ALSO\n  agicash send lightning"
    )]
    LightningAddress {
        /// LUD-16 address, e.g. `alice@walletofsatoshi.com`.
        address: String,
        /// Amount to send, in sats.
        amount: u64,
        /// Account ID to send from (UUID). Omit to use the only Cashu
        /// account; required when several exist.
        #[arg(long)]
        account: Option<String>,
        /// Comment for the LUD-12 callback (used only if the remote
        /// advertises `commentAllowed`).
        #[arg(long)]
        comment: Option<String>,
        /// Preview the fee without persisting or paying.
        #[arg(long)]
        dry_run: bool,
        /// Begin the melt and return without waiting; resume later with
        /// `agicash send lightning-complete <quote_id>`.
        #[arg(long)]
        no_wait: bool,
        /// Polling interval in milliseconds while the melt is in flight.
        #[arg(long, default_value_t = 1000)]
        poll_ms: u64,
        /// Overall timeout in seconds before reporting `timed-out`.
        #[arg(long, default_value_t = 300)]
        timeout_s: u64,
    },
}

#[derive(clap::Args, Debug)]
pub struct ReceiveArgs {
    #[command(subcommand)]
    pub cmd: ReceiveCommand,
}

#[derive(Subcommand, Debug)]
pub enum ReceiveCommand {
    /// Claim a Cashu token (NUT-03 swap).
    #[command(
        long_about = "Claim a Cashu token into the matching account by swapping \
its proofs (NUT-03). The token's mint must already have an account — run \
`agicash mint add` first if it does not. Idempotent: re-claiming an \
already-spent token reports `already-claimed`.",
        after_long_help = "EXAMPLE\n  $ agicash receive token cashuBo2F0…\n  \
{\"status\":\"received\",\"amount\":\"100\",\"fee\":\"0\",\"unit\":\"sat\",\
\"currency\":\"BTC\",\"account_id\":\"…\",\"mint_url\":\"…\",\"token_hash\":\"…\"}\n\n  \
status      `received`, `already-claimed`, `already-failed`, or `pending`.\n  \
amount/fee  amount credited and mint fee, in `unit`.\n\n\
EXIT CODES\n  0  success\n  1  invalid token, no matching account, mint error\n  \
3  not authenticated\n\nSEE ALSO\n  agicash send token, agicash balance"
    )]
    Token {
        /// Encoded Cashu token (`cashuA…` V3 or `cashuB…` V4).
        token: String,
    },
    /// Receive sats via a Lightning invoice (NUT-04 mint quote).
    #[command(
        long_about = "Receive funds over Lightning: request a NUT-04 mint quote, \
print the invoice for the payer, then mint proofs once it is paid. By default \
the command polls until the invoice settles; pass `--no-wait` to print the \
invoice and exit.",
        after_long_help = "EXAMPLE\n  $ agicash receive lightning 100\n  \
{\"status\":\"quote-issued\",\"quote_id\":\"…\",\"invoice\":\"lnbc1u1p…\",\
\"payment_hash\":\"…\",\"amount\":\"100\",\"unit\":\"sat\",\"currency\":\"BTC\",\
\"expires_at\":\"…\",\"account_id\":\"…\"}\n  …then `{\"status\":\"received\",…}` once paid.\n\n  \
invoice   give this BOLT-11 string to the payer.\n  \
quote_id  pass to `receive lightning-complete` to finish after `--no-wait`.\n\n\
EXIT CODES\n  0  success\n  1  no matching account, amount too small, mint error\n  \
3  not authenticated\n\nSEE ALSO\n  agicash receive lightning-complete"
    )]
    Lightning {
        /// Amount to receive, in the account's unit (sats for BTC,
        /// cents for USD).
        amount: u64,
        /// Account ID to receive into (UUID). Omit to use the only Cashu
        /// account matching `--currency`; required when several exist.
        #[arg(long)]
        account: Option<String>,
        /// Currency of the account to receive into.
        #[arg(long, value_enum, default_value = "BTC")]
        currency: Currency,
        /// Optional memo to attach to the mint quote.
        #[arg(long)]
        description: Option<String>,
        /// Print the invoice + quote id and exit without polling; resume
        /// later with `agicash receive lightning-complete <quote_id>`.
        #[arg(long)]
        no_wait: bool,
        /// Polling interval in milliseconds while the invoice is unpaid.
        #[arg(long, default_value_t = 1000)]
        poll_ms: u64,
        /// Overall timeout in seconds before reporting `timed-out`.
        #[arg(long, default_value_t = 300)]
        timeout_s: u64,
    },
    /// Finish a previously-created Lightning receive.
    #[command(
        long_about = "Finish a Lightning receive by its quote id — used after \
`receive lightning --no-wait`. Polls the quote; once the invoice is paid it \
mints the proofs and credits the account.",
        after_long_help = "EXAMPLE\n  $ agicash receive lightning-complete \
11111111-2222-3333-4444-555555555555\n  \
{\"status\":\"received\",\"amount\":\"100\",\"fee\":\"0\",\"unit\":\"sat\",\
\"currency\":\"BTC\",\"account_id\":\"…\",\"quote_id\":\"…\",\"payment_hash\":\"…\"}\n\n  \
status  `received`, `timed-out`, or `already-failed`.\n\n\
EXIT CODES\n  0  success\n  1  invalid/unpaid quote, mint error\n  \
3  not authenticated\n\nSEE ALSO\n  agicash receive lightning"
    )]
    LightningComplete {
        /// The quote id (UUID) returned by `receive lightning --no-wait`.
        quote_id: String,
        /// Polling interval in milliseconds while the invoice is unpaid.
        #[arg(long, default_value_t = 1000)]
        poll_ms: u64,
        /// Overall timeout in seconds before reporting `timed-out`.
        #[arg(long, default_value_t = 30)]
        timeout_s: u64,
    },
}

#[derive(clap::Args, Debug)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub cmd: AuthCommand,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    /// Sign in with an email and password.
    #[command(
        long_about = "Sign in with an existing email + password account. The \
session is persisted so subsequent commands inherit it.\n\nThe password is \
never an argument or env var (those leak via `ps` / `/proc`). Two ways to \
supply it:\n  - interactive: omit `--password-stdin`; run with a terminal and \
you are prompted (echo off).\n  - non-interactive: pass `--password-stdin` and \
pipe the password as one line on stdin — required for agents and scripts (no \
terminal). Without `--password-stdin` and without a terminal the command fails \
fast with `interactive-input-required` (exit 2).",
        after_long_help = "EXAMPLE\n  # interactive (human at a terminal)\n  \
$ agicash auth login alice@example.com\n  Password: ********\n  \
{\"status\":\"signed-in\",\"user_id\":\"…\",\"guest\":false}\n\n  \
# non-interactive (agent / script)\n  \
$ printf %s \"$pw\" | agicash auth login alice@example.com --password-stdin\n  \
{\"status\":\"signed-in\",\"user_id\":\"…\",\"guest\":false}\n\n\
EXIT CODES\n  0  success\n  1  network or backend error\n  \
2  no terminal and --password-stdin not passed (interactive-input-required)\n  \
3  bad credentials / unauthenticated\n\n\
SEE ALSO\n  agicash auth signup, agicash auth guest"
    )]
    Login {
        /// Email address of the account.
        email: String,
        /// Read the password from stdin (one line on fd 0) instead of
        /// prompting on the terminal. Required for non-interactive use
        /// (agents, scripts) — there is no terminal to prompt on. The
        /// password is never accepted as an argument or env var.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Register a new email + password user and sign in.
    #[command(
        long_about = "Register a new email + password user and sign in. The \
password must be at least 8 characters.\n\nThe password is never an argument \
or env var (those leak via `ps` / `/proc`). Two ways to supply it:\n  - \
interactive: omit `--password-stdin`; run with a terminal and you are prompted \
twice (password + confirm, echo off).\n  - non-interactive: pass \
`--password-stdin` and pipe the password as one line on stdin — read once, the \
confirm prompt is skipped. Required for agents and scripts (no terminal). \
Without `--password-stdin` and without a terminal the command fails fast with \
`interactive-input-required` (exit 2).",
        after_long_help = "EXAMPLE\n  # interactive (human at a terminal)\n  \
$ agicash auth signup alice@example.com\n  Password: ********\n  \
Confirm password: ********\n  {\"status\":\"signed-in\",\"user_id\":\"…\",\"guest\":false}\n\n  \
# non-interactive (agent / script)\n  \
$ printf %s \"$pw\" | agicash auth signup alice@example.com --password-stdin\n  \
{\"status\":\"signed-in\",\"user_id\":\"…\",\"guest\":false}\n\n\
EXIT CODES\n  0  success\n  1  passwords mismatch, too short, or backend error\n  \
2  no terminal and --password-stdin not passed (interactive-input-required)\n  \
3  unauthenticated\n\nSEE ALSO\n  agicash auth login, agicash auth guest"
    )]
    Signup {
        /// Email address for the new account.
        email: String,
        /// Read the password from stdin (one line on fd 0) instead of
        /// prompting on the terminal. Read once — the interactive confirm
        /// prompt is skipped. Required for non-interactive use (agents,
        /// scripts). The password is never accepted as an argument or env
        /// var.
        #[arg(long)]
        password_stdin: bool,
    },
    /// Register and sign in as an anonymous guest user.
    #[command(
        long_about = "Register and sign in as an anonymous guest user — no email, \
no password. The fastest way to get a usable session for testing.",
        after_long_help = "EXAMPLE\n  $ agicash auth guest\n  \
{\"status\":\"signed-in\",\"user_id\":\"…\",\"guest\":true}\n\n\
EXIT CODES\n  0  success\n  1  network or backend error\n\n\
SEE ALSO\n  agicash auth login, agicash auth status"
    )]
    Guest,
    /// Clear the local session.
    #[command(
        long_about = "Clear the local session and best-effort sign out on the \
server. Idempotent — running it without an active session succeeds.",
        after_long_help = "EXAMPLE\n  $ agicash auth logout\n  \
{\"status\":\"signed-out\"}\n\n  status  `signed-out`, or `not-logged-in` if no \
session was present.\n\nEXIT CODES\n  0  success\n\n\
SEE ALSO\n  agicash auth login, agicash auth status"
    )]
    Logout,
    /// Report whether a session is active.
    #[command(
        long_about = "Report whether a session is currently active and, if so, \
the signed-in user id. Never fails on a missing session.",
        after_long_help = "EXAMPLE\n  $ agicash auth status\n  \
{\"logged_in\":true,\"user_id\":\"…\"}\n\n  When logged out: \
{\"logged_in\":false}\n\nEXIT CODES\n  0  success\n\n\
SEE ALSO\n  agicash auth login, agicash auth logout"
    )]
    Status,
}

#[derive(clap::Args, Debug)]
pub struct AccountArgs {
    #[command(subcommand)]
    pub cmd: AccountCommand,
}

#[derive(Subcommand, Debug)]
pub enum AccountCommand {
    /// List active accounts for the current user.
    #[command(
        long_about = "List every active account for the signed-in user, with its \
id, currency, mint and default flags. Requires a session.",
        after_long_help = "EXAMPLE\n  $ agicash account list\n  \
[{\"id\":\"…\",\"name\":\"testnut\",\"currency\":\"BTC\",\"mint_url\":\"…\"}]\n\n  \
JSON array, one object per account. Use an `id` from here as the\n  \
`--account` argument elsewhere.\n\nEXIT CODES\n  0  success\n  \
1  storage/network error\n  3  not authenticated\n\n\
SEE ALSO\n  agicash account default, agicash balance"
    )]
    List,
    /// Set the per-currency default account.
    #[command(
        long_about = "Set the default account for its currency. The currency is \
inferred from the account row (a BTC account sets the BTC default, a USD account \
the USD default). Requires a session.",
        after_long_help = "EXAMPLE\n  $ agicash account default \
11111111-2222-3333-4444-555555555555\n  {\"id\":\"…\",\"default_btc_account_id\":\"…\"}\n\n\
EXIT CODES\n  0  success\n  2  malformed account id (not a UUID)\n  \
3  not authenticated\n  4  account not found\n\n\
SEE ALSO\n  agicash account list"
    )]
    Default {
        /// Account ID (UUID) to make the default for its currency.
        id: String,
    },
    /// Show detail for a single account.
    #[command(
        long_about = "Show the full record for one account by its id — currency, \
type, mint URL and the raw account row. Requires a session; the account must \
belong to the signed-in user.",
        after_long_help = "EXAMPLE\n  $ agicash account info \
11111111-2222-3333-4444-555555555555\n  \
{\"id\":\"…\",\"user_id\":\"…\",\"name\":\"testnut\",\"type\":\"cashu\",\
\"currency\":\"BTC\",\"details\":{\"mint_url\":\"…\"}}\n\n  \
The raw `wallet.accounts` row — same shape as one entry of\n  \
`account list`. `details` carries account-type-specific fields.\n\n\
EXIT CODES\n  0  success\n  1  storage/network error\n  \
2  malformed account id (not a UUID)\n  3  not authenticated\n  \
4  account not found\n\nSEE ALSO\n  agicash account list, agicash balance"
    )]
    Info {
        /// Account ID (UUID) to inspect.
        id: String,
    },
}

#[derive(clap::Args, Debug)]
pub struct MintArgs {
    #[command(subcommand)]
    pub cmd: MintCommand,
}

#[derive(Subcommand, Debug)]
pub enum MintCommand {
    /// Add a Cashu mint and create an account for it.
    #[command(
        long_about = "Add a Cashu mint and create an account backed by it. The \
mint is reached for NUT-06 discovery, so it must be online. An account is a \
prerequisite for receiving or sending — add a mint before anything else.",
        after_long_help = "EXAMPLE\n  $ agicash mint add https://testnut.cashu.space\n  \
{\"status\":\"added\",\"account_id\":\"…\",\"mint_name\":\"testnut\",\
\"mint_url\":\"https://testnut.cashu.space\"}\n\n  \
account_id  the new account — use as `--account` elsewhere.\n\n\
EXIT CODES\n  0  success\n  1  invalid URL, mint unreachable, mint error\n  \
2  invalid --currency\n  3  not authenticated\n\n\
SEE ALSO\n  agicash account list, agicash balance"
    )]
    Add {
        /// Mint URL, e.g. <https://testnut.cashu.space>
        url: String,
        /// Currency of the account to create for this mint.
        #[arg(long, value_enum, default_value = "BTC")]
        currency: Currency,
    },
    /// List configured Cashu mints.
    #[command(
        long_about = "List every Cashu mint the current user has an account with. \
Mints are grouped by URL; each carries the accounts provisioned against it. \
Requires a session.",
        after_long_help = "EXAMPLE\n  $ agicash mint list\n  \
[{\"mint_url\":\"https://testnut.cashu.space\",\"mint_name\":\"testnut\",\
\"accounts\":[{\"id\":\"…\",\"currency\":\"BTC\",\"balance\":\"1200\"}]}]\n\n  \
JSON array, one object per mint. `accounts` lists every account (one\n  \
per currency) backed by that mint — empty array if none.\n\n\
EXIT CODES\n  0  success\n  1  storage/network error\n  \
3  not authenticated\n\nSEE ALSO\n  agicash mint add, agicash balance"
    )]
    List,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_auth_guest() {
        let cli = Cli::try_parse_from(["agicash", "auth", "guest"]).unwrap();
        match cli.cmd {
            Some(Command::Auth(a)) => assert!(matches!(a.cmd, AuthCommand::Guest)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_auth_login_with_email() {
        let cli = Cli::try_parse_from(["agicash", "auth", "login", "alice@example.com"]).unwrap();
        match cli.cmd {
            Some(Command::Auth(a)) => match a.cmd {
                AuthCommand::Login {
                    email,
                    password_stdin,
                } => {
                    assert_eq!(email, "alice@example.com");
                    // Default: prompt interactively, do not read stdin.
                    assert!(!password_stdin);
                }
                other => panic!("unexpected auth subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_auth_login_with_password_stdin() {
        // The parser must accept `--password-stdin` on `auth login`. This
        // is the non-interactive entry point for agents — if the flag is
        // not recognized, the no-TTY auth path has no escape hatch.
        let cli = Cli::try_parse_from([
            "agicash",
            "auth",
            "login",
            "alice@example.com",
            "--password-stdin",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Auth(a)) => match a.cmd {
                AuthCommand::Login {
                    email,
                    password_stdin,
                } => {
                    assert_eq!(email, "alice@example.com");
                    assert!(password_stdin, "--password-stdin should set the flag");
                }
                other => panic!("unexpected auth subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_auth_signup_with_password_stdin() {
        // `auth signup` must accept `--password-stdin` too — same
        // non-interactive contract as `auth login`.
        let cli = Cli::try_parse_from([
            "agicash",
            "auth",
            "signup",
            "bob@example.com",
            "--password-stdin",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Auth(a)) => match a.cmd {
                AuthCommand::Signup {
                    email,
                    password_stdin,
                } => {
                    assert_eq!(email, "bob@example.com");
                    assert!(password_stdin, "--password-stdin should set the flag");
                }
                other => panic!("unexpected auth subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn auth_login_rejects_password_as_argument() {
        // A password must NEVER be an argv value (it leaks in `ps`).
        // `--password-stdin` is a boolean flag; clap must reject anyone
        // who tries `--password-stdin <value>` or a `--password <value>`.
        assert!(
            Cli::try_parse_from([
                "agicash",
                "auth",
                "login",
                "alice@example.com",
                "--password",
                "hunter2",
            ])
            .is_err(),
            "`--password <value>` must not be a recognized argument",
        );
    }

    #[test]
    fn parses_auth_logout() {
        let cli = Cli::try_parse_from(["agicash", "auth", "logout"]).unwrap();
        match cli.cmd {
            Some(Command::Auth(a)) => assert!(matches!(a.cmd, AuthCommand::Logout)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_auth_status() {
        let cli = Cli::try_parse_from(["agicash", "auth", "status"]).unwrap();
        match cli.cmd {
            Some(Command::Auth(a)) => assert!(matches!(a.cmd, AuthCommand::Status)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn does_not_recognize_whoami() {
        let res = Cli::try_parse_from(["agicash", "whoami"]);
        assert!(res.is_err(), "whoami should NOT be a recognized subcommand");
    }

    #[test]
    fn parses_account_list() {
        let cli = Cli::try_parse_from(["agicash", "account", "list"]).unwrap();
        match cli.cmd {
            Some(Command::Account(a)) => assert!(matches!(a.cmd, AccountCommand::List)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_account_default_with_id() {
        let cli = Cli::try_parse_from([
            "agicash",
            "account",
            "default",
            "00000000-0000-0000-0000-000000000000",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Account(a)) => match a.cmd {
                AccountCommand::Default { id } => {
                    assert_eq!(id, "00000000-0000-0000-0000-000000000000");
                }
                other => panic!("unexpected: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_mint_add_with_url() {
        let cli =
            Cli::try_parse_from(["agicash", "mint", "add", "https://testnut.cashu.space"]).unwrap();
        match cli.cmd {
            Some(Command::Mint(m)) => match m.cmd {
                MintCommand::Add { url, currency } => {
                    assert_eq!(url, "https://testnut.cashu.space");
                    assert_eq!(currency, Currency::Btc);
                }
                other @ MintCommand::List => panic!("unexpected: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_mint_add_with_currency_flag() {
        let cli = Cli::try_parse_from([
            "agicash",
            "mint",
            "add",
            "https://example.com",
            "--currency",
            "USD",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Mint(m)) => match m.cmd {
                MintCommand::Add { currency, .. } => assert_eq!(currency, Currency::Usd),
                other @ MintCommand::List => panic!("unexpected: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn mint_add_rejects_unknown_currency() {
        // ValueEnum now rejects bad currency values at parse time (exit 2).
        let res = Cli::try_parse_from([
            "agicash",
            "mint",
            "add",
            "https://example.com",
            "--currency",
            "EUR",
        ]);
        assert!(res.is_err(), "EUR is not a valid currency");
    }

    #[test]
    fn parses_balance_without_args() {
        let cli = Cli::try_parse_from(["agicash", "balance"]).unwrap();
        assert!(matches!(cli.cmd, Some(Command::Balance { account: None })));
    }

    #[test]
    fn parses_balance_with_account_filter() {
        let cli = Cli::try_parse_from(["agicash", "balance", "--account", "abc-123"]).unwrap();
        match cli.cmd {
            Some(Command::Balance { account: Some(id) }) => assert_eq!(id, "abc-123"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_receive_token_with_v3() {
        let cli = Cli::try_parse_from(["agicash", "receive", "token", "cashuAabc"]).unwrap();
        match cli.cmd {
            Some(Command::Receive(r)) => match r.cmd {
                ReceiveCommand::Token { token } => assert_eq!(token, "cashuAabc"),
                other => panic!("unexpected receive subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_receive_token_with_v4() {
        let cli = Cli::try_parse_from(["agicash", "receive", "token", "cashuBxyz"]).unwrap();
        match cli.cmd {
            Some(Command::Receive(r)) => match r.cmd {
                ReceiveCommand::Token { token } => assert_eq!(token, "cashuBxyz"),
                other => panic!("unexpected receive subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_receive_lightning_with_amount() {
        let cli = Cli::try_parse_from(["agicash", "receive", "lightning", "100"]).unwrap();
        match cli.cmd {
            Some(Command::Receive(r)) => match r.cmd {
                ReceiveCommand::Lightning {
                    amount,
                    account,
                    currency,
                    no_wait,
                    ..
                } => {
                    assert_eq!(amount, 100);
                    assert!(account.is_none());
                    assert_eq!(currency, Currency::Btc);
                    assert!(!no_wait);
                }
                other => panic!("unexpected receive subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_receive_lightning_with_no_wait() {
        let cli = Cli::try_parse_from([
            "agicash",
            "receive",
            "lightning",
            "100",
            "--no-wait",
            "--currency",
            "USD",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Receive(r)) => match r.cmd {
                ReceiveCommand::Lightning {
                    amount,
                    no_wait,
                    currency,
                    ..
                } => {
                    assert_eq!(amount, 100);
                    assert!(no_wait);
                    assert_eq!(currency, Currency::Usd);
                }
                other => panic!("unexpected receive subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_receive_lightning_complete() {
        let cli = Cli::try_parse_from([
            "agicash",
            "receive",
            "lightning-complete",
            "11111111-2222-3333-4444-555555555555",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Receive(r)) => match r.cmd {
                ReceiveCommand::LightningComplete { quote_id, .. } => {
                    assert_eq!(quote_id, "11111111-2222-3333-4444-555555555555");
                }
                other => panic!("unexpected receive subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_send_token_with_amount() {
        let cli = Cli::try_parse_from(["agicash", "send", "token", "100"]).unwrap();
        match cli.cmd {
            Some(Command::Send(s)) => match s.cmd {
                SendCommand::Token {
                    amount,
                    account,
                    token_version,
                    dry_run,
                } => {
                    assert_eq!(amount, 100);
                    assert!(account.is_none());
                    assert_eq!(token_version, TokenVersion::V4);
                    assert!(!dry_run);
                }
                other => panic!("unexpected send subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_send_token_with_account_and_dry_run() {
        let cli = Cli::try_parse_from([
            "agicash",
            "send",
            "token",
            "50",
            "--account",
            "abc-123",
            "--dry-run",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Send(s)) => match s.cmd {
                SendCommand::Token {
                    amount,
                    account,
                    dry_run,
                    ..
                } => {
                    assert_eq!(amount, 50);
                    assert_eq!(account.as_deref(), Some("abc-123"));
                    assert!(dry_run);
                }
                other => panic!("unexpected send subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_send_token_with_token_version_3() {
        let cli = Cli::try_parse_from(["agicash", "send", "token", "100", "--token-version", "3"])
            .unwrap();
        match cli.cmd {
            Some(Command::Send(s)) => match s.cmd {
                SendCommand::Token { token_version, .. } => {
                    assert_eq!(token_version, TokenVersion::V3);
                }
                other => panic!("unexpected send subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn send_token_rejects_bad_token_version() {
        // ValueEnum now rejects bad token-version values at parse time (exit 2).
        let res = Cli::try_parse_from(["agicash", "send", "token", "100", "--token-version", "5"]);
        assert!(res.is_err(), "5 is not a valid token version");
    }

    #[test]
    fn parses_send_lightning_with_invoice() {
        let cli = Cli::try_parse_from(["agicash", "send", "lightning", "lnbc100n1..."]).unwrap();
        match cli.cmd {
            Some(Command::Send(s)) => match s.cmd {
                SendCommand::Lightning {
                    invoice,
                    account,
                    dry_run,
                    no_wait,
                    ..
                } => {
                    assert_eq!(invoice, "lnbc100n1...");
                    assert!(account.is_none());
                    assert!(!dry_run);
                    assert!(!no_wait);
                }
                other => panic!("unexpected send subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_send_lightning_with_no_wait() {
        let cli =
            Cli::try_parse_from(["agicash", "send", "lightning", "lnbc...", "--no-wait"]).unwrap();
        match cli.cmd {
            Some(Command::Send(s)) => match s.cmd {
                SendCommand::Lightning { no_wait, .. } => assert!(no_wait),
                other => panic!("unexpected send subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_send_lightning_address() {
        let cli = Cli::try_parse_from([
            "agicash",
            "send",
            "lightning-address",
            "alice@walletofsatoshi.com",
            "100",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Send(s)) => match s.cmd {
                SendCommand::LightningAddress {
                    address,
                    amount,
                    account,
                    comment,
                    dry_run,
                    no_wait,
                    ..
                } => {
                    assert_eq!(address, "alice@walletofsatoshi.com");
                    assert_eq!(amount, 100);
                    assert!(account.is_none());
                    assert!(comment.is_none());
                    assert!(!dry_run);
                    assert!(!no_wait);
                }
                other => panic!("unexpected send subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_send_lightning_address_with_comment_and_flags() {
        let cli = Cli::try_parse_from([
            "agicash",
            "send",
            "lightning-address",
            "alice@example.com",
            "250",
            "--comment",
            "thanks!",
            "--dry-run",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Send(s)) => match s.cmd {
                SendCommand::LightningAddress {
                    address,
                    amount,
                    comment,
                    dry_run,
                    ..
                } => {
                    assert_eq!(address, "alice@example.com");
                    assert_eq!(amount, 250);
                    assert_eq!(comment.as_deref(), Some("thanks!"));
                    assert!(dry_run);
                }
                other => panic!("unexpected send subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_decode_with_input() {
        let cli = Cli::try_parse_from(["agicash", "decode", "cashuBabc"]).unwrap();
        match cli.cmd {
            Some(Command::Decode { input }) => assert_eq!(input, "cashuBabc"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn decode_requires_an_input_arg() {
        let res = Cli::try_parse_from(["agicash", "decode"]);
        assert!(res.is_err(), "decode without an input should be rejected");
    }

    #[test]
    fn parses_mint_list() {
        let cli = Cli::try_parse_from(["agicash", "mint", "list"]).unwrap();
        match cli.cmd {
            Some(Command::Mint(m)) => assert!(matches!(m.cmd, MintCommand::List)),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn parses_account_info_with_id() {
        let cli = Cli::try_parse_from([
            "agicash",
            "account",
            "info",
            "11111111-2222-3333-4444-555555555555",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Account(a)) => match a.cmd {
                AccountCommand::Info { id } => {
                    assert_eq!(id, "11111111-2222-3333-4444-555555555555");
                }
                other => panic!("unexpected account subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn account_info_requires_an_id_arg() {
        let res = Cli::try_parse_from(["agicash", "account", "info"]);
        assert!(
            res.is_err(),
            "account info without an id should be rejected"
        );
    }

    #[test]
    fn parses_send_lightning_complete() {
        let cli = Cli::try_parse_from([
            "agicash",
            "send",
            "lightning-complete",
            "11111111-2222-3333-4444-555555555555",
        ])
        .unwrap();
        match cli.cmd {
            Some(Command::Send(s)) => match s.cmd {
                SendCommand::LightningComplete { quote_id, .. } => {
                    assert_eq!(quote_id, "11111111-2222-3333-4444-555555555555");
                }
                other => panic!("unexpected send subcommand: {other:?}"),
            },
            other => panic!("unexpected: {other:?}"),
        }
    }
}
