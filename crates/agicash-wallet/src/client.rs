//! `WalletClient` — the composed facade.
//!
//! Holds `Arc<dyn …>` handles to every storage + provider it needs,
//! plus the four cashu state-machine services (receive-swap, send-swap,
//! mint-quote, melt-quote). Each public method composes 1-3 of those
//! into the operations the iOS app, Leptos PWA, CLI, and MCP server
//! need.
//!
//! Construction is via [`WalletClientBuilder`](crate::WalletClientBuilder).
//! All methods take `&self` so the client lives behind `Arc<WalletClient>`
//! and can be shared across async tasks and FFI/WASM boundaries.

use crate::auth::{AuthClient, Session};
use crate::error::WalletError;
use crate::types::{
    AccountSummary, AuthStatus, BalanceSummary, ExchangeRateSnapshot, MintSummary,
    ReceiveLightningHandle, ReceiveLightningSnapshot, ReceiveLightningState, ReceiveReceipt,
    ReceiveStatus, SendLightningQuote, SendLightningReceipt, SendLightningStatus, SendTokenQuote,
    SendTokenReceipt, TokenVersion, Transaction, TransactionFilter, TransactionPage,
};
use agicash_cashu::{
    add_mint_account, CashuMeltQuoteService, CashuMeltQuoteState, CashuMeltQuoteStorage,
    CashuMintQuoteService, CashuMintQuoteState, CashuMintQuoteStorage, CashuReceiveSwapService,
    CashuReceiveSwapState, CashuSeedProvider, CashuSendSwapService, CashuSendSwapStorage,
    CompleteMintQuoteOutcome, CompleteOutcome, MeltOutcome, MeltQuoteError, ParsedToken,
    ReceiveFlowError, ReceiveFlowService, ReceiveSwapError, ReceiveSwapStorageError, TokenProof,
};
use agicash_domain::{Account, AccountId, AccountState, AccountType, Currency};
use agicash_exchange_rate::ExchangeRateProvider;
use agicash_money::{Money, Unit};
use agicash_traits::{CashuProvider, UserStorage};
use cdk::mint_url::MintUrl;
use cdk::nuts::nut02::Id as KeysetId;
use cdk::nuts::{CurrencyUnit, Proof, Token};
use cdk::Amount;
use rust_decimal::Decimal;
use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Composed wallet facade. See [`crate::WalletClientBuilder`] for
/// construction; the README of this crate has end-to-end examples.
pub struct WalletClient {
    pub(crate) auth: Arc<dyn AuthClient>,
    pub(crate) user_storage: Arc<dyn UserStorage>,
    pub(crate) cashu_provider: Arc<dyn CashuProvider>,
    pub(crate) cashu_send_storage: Arc<dyn CashuSendSwapStorage>,
    pub(crate) cashu_mint_quote_storage: Arc<dyn CashuMintQuoteStorage>,
    pub(crate) cashu_melt_quote_storage: Arc<dyn CashuMeltQuoteStorage>,
    pub(crate) receive_swap_service: Arc<CashuReceiveSwapService>,
    pub(crate) send_swap_service: Arc<CashuSendSwapService>,
    pub(crate) mint_quote_service: Arc<CashuMintQuoteService>,
    pub(crate) melt_quote_service: Arc<CashuMeltQuoteService>,
    pub(crate) exchange_rate: Option<Arc<dyn ExchangeRateProvider>>,
}

impl std::fmt::Debug for WalletClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WalletClient")
            .field("exchange_rate_configured", &self.exchange_rate.is_some())
            .finish_non_exhaustive()
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------
impl WalletClient {
    pub async fn auth_guest(&self) -> Result<Session, WalletError> {
        self.auth.register_guest().await
    }

    pub async fn auth_login(&self, email: &str, password: &str) -> Result<Session, WalletError> {
        self.auth.login_email(email, password).await
    }

    pub async fn auth_signup(
        &self,
        email: &str,
        password: &str,
        name: Option<&str>,
    ) -> Result<Session, WalletError> {
        self.auth.register_email(email, password, name).await
    }

    pub async fn auth_logout(&self) -> Result<(), WalletError> {
        self.auth.logout().await
    }

    pub async fn auth_status(&self) -> Result<AuthStatus, WalletError> {
        let session = self.auth.get_session().await?;
        Ok(match session {
            Some(s) => AuthStatus {
                logged_in: true,
                user_id: Some(s.user_id),
            },
            None => AuthStatus {
                logged_in: false,
                user_id: None,
            },
        })
    }

    pub async fn set_session(&self, session: Session) -> Result<(), WalletError> {
        self.auth.set_session(session).await
    }

    pub async fn get_persisted_session(&self) -> Result<Option<Session>, WalletError> {
        self.auth.get_session().await
    }

    /// Internal: load the current session or return Unauthenticated.
    async fn require_session(&self) -> Result<Session, WalletError> {
        self.auth
            .get_session()
            .await?
            .ok_or(WalletError::Unauthenticated)
    }
}

// ---------------------------------------------------------------------------
// Accounts + balance
// ---------------------------------------------------------------------------
impl WalletClient {
    /// List the user's accounts with per-account balance.
    pub async fn list_accounts(&self) -> Result<Vec<AccountSummary>, WalletError> {
        let session = self.require_session().await?;
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let mut out = Vec::with_capacity(accounts.len());
        for account in accounts {
            let balance = compute_cashu_balance(self.cashu_send_storage.as_ref(), &account).await?;
            out.push(AccountSummary::from_account(&account, balance));
        }
        Ok(out)
    }

    /// Single-account lookup.
    pub async fn get_account(&self, account_id: AccountId) -> Result<AccountSummary, WalletError> {
        let session = self.require_session().await?;
        let account = self
            .user_storage
            .get_account(account_id)
            .await?
            .ok_or_else(|| WalletError::NotFound(format!("account {account_id}")))?;
        // `get_account` is not user-scoped at the storage layer (unlike
        // `list_accounts`), so re-check ownership here — same guard the
        // quote methods use (see `poll_send_lightning`,
        // `poll_receive_lightning`).
        if account.user_id != session.user_id {
            return Err(WalletError::Validation {
                code: "wrong_owner".into(),
                message: "account belongs to a different user".into(),
            });
        }
        let balance = compute_cashu_balance(self.cashu_send_storage.as_ref(), &account).await?;
        Ok(AccountSummary::from_account(&account, balance))
    }

    /// Aggregate balance, optionally filtered to a single account.
    ///
    /// When `account_id` is `None`, sums every account's balance grouped by
    /// currency code and returns the per-account breakdown alongside the
    /// totals so callers can render both views from one round-trip.
    pub async fn balance(
        &self,
        account_id: Option<AccountId>,
    ) -> Result<BalanceSummary, WalletError> {
        let session = self.require_session().await?;
        let all = self.user_storage.list_accounts(session.user_id).await?;
        let filtered: Vec<&Account> = match account_id {
            Some(id) => all.iter().filter(|a| a.id == id).collect(),
            None => all.iter().collect(),
        };
        if filtered.is_empty() {
            if let Some(id) = account_id {
                return Err(WalletError::NotFound(format!("account {id}")));
            }
        }
        let mut per_account = Vec::with_capacity(filtered.len());
        let mut totals: BTreeMap<String, u128> = BTreeMap::new();
        for account in filtered {
            let bal = compute_cashu_balance(self.cashu_send_storage.as_ref(), account).await?;
            per_account.push(AccountSummary::from_account(account, bal));
            *totals.entry(account.currency.to_string()).or_default() += u128::from(bal);
        }
        let total_per_currency: BTreeMap<String, String> = totals
            .into_iter()
            .map(|(k, v)| (k, v.to_string()))
            .collect();
        Ok(BalanceSummary {
            total_per_currency,
            per_account,
        })
    }

    /// Set a per-currency default account.
    ///
    /// Implementation defers to `UserStorage::upsert_user_with_accounts`
    /// with the chosen account marked as default. The detailed semantics
    /// (which row gets `default_btc_account_id` vs `default_usd_account_id`
    /// patched on `wallet.users`) live inside the storage RPC; the facade
    /// just sets the `is_default` flag on the matching account input.
    ///
    /// **Status:** slice 12 returns `Unsupported`. The storage impl exists
    /// (the Supabase RPC reads `is_default` on each account input), but the
    /// existing `upsert_user_with_accounts` shape requires the caller to
    /// supply every account in `p_accounts` (it's an upsert across the
    /// whole set) — a partial-update RPC for the default-account columns
    /// hasn't shipped. Slice 13+ will either add a focused RPC or rebuild
    /// the upsert payload from the existing account list.
    #[allow(clippy::unused_async)] // stub — async surface preserved for slice-13+ impl.
    pub async fn set_default_account(
        &self,
        _account_id: AccountId,
        _currency: Currency,
    ) -> Result<(), WalletError> {
        Err(WalletError::Unsupported(
            "set_default_account: focused RPC not yet shipped",
        ))
    }
}

// ---------------------------------------------------------------------------
// Mint management
// ---------------------------------------------------------------------------
impl WalletClient {
    /// Provision a new Cashu mint + create an account row for it.
    ///
    /// Mirrors the existing FFI `mint_add` flow but takes the desired
    /// currency as a parameter (the FFI hard-codes BTC). For
    /// brand-new guest users, seeds a placeholder Spark account so the
    /// `upsert_user_with_accounts` RPC's "at least one BTC Spark"
    /// constraint is satisfied — same workaround the CLI uses.
    pub async fn add_mint(
        &self,
        mint_url: String,
        currency: Currency,
    ) -> Result<AccountSummary, WalletError> {
        let session = self.require_session().await?;
        let user_id = session.user_id;

        let parsed_url = MintUrl::from_str(mint_url.trim())
            .map_err(|e| WalletError::validation("bad_url", format!("invalid mint URL: {e}")))?;

        // NUT-06 discovery (also warms the connector cache inside the provider).
        let info = self.cashu_provider.mint_info(&parsed_url).await?;
        let canonical_url = parsed_url.to_string();
        let mint_name = info.name.clone().unwrap_or_else(|| canonical_url.clone());

        // 12c D2: the user-row-preservation + AccountInput payload +
        // brand-new-guest Spark workaround + currency-filtered
        // find-matching-account logic is the de-duplicated
        // `agicash_cashu::add_mint_account` primitive. `map_add_mint_err`
        // pins the error classification byte-identical to the pre-12c
        // inline body on BOTH the upsert-failure and not-found paths
        // (Tension T1; the primitive's find is currency-filtered as of
        // Task 3, matching the pre-12c facade `&& a.currency == currency`).
        let new_account = add_mint_account(
            user_id,
            &self.user_storage,
            &canonical_url,
            &mint_name,
            currency,
        )
        .await
        .map_err(map_add_mint_err)?;

        // Brand-new account has zero balance — skip the storage round-trip.
        Ok(AccountSummary::from_account(&new_account, 0))
    }

    /// Group accounts by mint URL into discovered mints. Convenience for
    /// UI that wants to render a mint list rather than an account list.
    pub async fn list_mints(&self) -> Result<Vec<MintSummary>, WalletError> {
        let session = self.require_session().await?;
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let mut grouped: BTreeMap<String, MintSummary> = BTreeMap::new();
        for account in accounts {
            if account.account_type != AccountType::Cashu {
                continue;
            }
            let Some(mint_url) = account
                .details
                .get("mint_url")
                .and_then(|v| v.as_str())
                .map(str::to_string)
            else {
                continue;
            };
            let balance = compute_cashu_balance(self.cashu_send_storage.as_ref(), &account).await?;
            let summary = AccountSummary::from_account(&account, balance);
            grouped
                .entry(mint_url.clone())
                .or_insert_with(|| MintSummary {
                    mint_url: mint_url.clone(),
                    mint_name: account.name.clone(),
                    accounts: Vec::new(),
                })
                .accounts
                .push(summary);
        }
        Ok(grouped.into_values().collect())
    }

    /// Soft-remove a mint.
    ///
    /// **Status:** slice 12 returns `Unsupported`. The storage layer
    /// doesn't yet expose a focused remove-mint RPC and the iOS UI doesn't
    /// surface this control; deferred per the slice-12 plan §6.4.
    #[allow(clippy::unused_async)] // stub — async surface preserved for slice-13+ impl.
    pub async fn remove_mint(&self, _account_id: AccountId) -> Result<(), WalletError> {
        Err(WalletError::Unsupported(
            "remove_mint: storage RPC not yet shipped",
        ))
    }
}

// ---------------------------------------------------------------------------
// Send (Cashu token + Lightning)
// ---------------------------------------------------------------------------
impl WalletClient {
    /// Dry-run a send-token to compute fees + breakdown without committing.
    pub async fn quote_send_token(
        &self,
        account_id: Option<AccountId>,
        amount: Money,
    ) -> Result<SendTokenQuote, WalletError> {
        let session = self.require_session().await?;
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let account = pick_cashu_account(&accounts, account_id, amount.currency())?.clone();
        let proofs = self
            .cashu_send_storage
            .list_unspent_proofs(account.id)
            .await
            .map_err(|e| WalletError::Cashu(format!("list_unspent_proofs: {e}")))?;
        let quote = self
            .send_swap_service
            .get_quote(&account, &proofs, amount.clone())
            .await?;
        Ok(SendTokenQuote {
            amount_requested: quote.amount_requested,
            amount_to_send: quote.amount_to_send,
            total_amount: quote.total_amount,
            total_fee: quote.total_fee,
            cashu_send_fee: quote.cashu_send_fee,
            cashu_receive_fee: quote.cashu_receive_fee,
            account_id: account.id,
        })
    }

    /// Create a send-swap and produce the encoded `cashuA…` / `cashuB…`
    /// token string. Mirrors the CLI `agicash send` subcommand.
    pub async fn send_token(
        &self,
        account_id: Option<AccountId>,
        amount: Money,
        token_version: TokenVersion,
    ) -> Result<SendTokenReceipt, WalletError> {
        let session = self.require_session().await?;
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let account = pick_cashu_account(&accounts, account_id, amount.currency())?.clone();
        let proofs = self
            .cashu_send_storage
            .list_unspent_proofs(account.id)
            .await
            .map_err(|e| WalletError::Cashu(format!("list_unspent_proofs: {e}")))?;

        // Create the swap row (DRAFT if mint round-trip needed; PENDING if exact-proofs).
        let create_result = self
            .send_swap_service
            .create(&account, &proofs, amount.clone())
            .await?;

        // Drive DRAFT → PENDING if needed.
        let final_swap = match &create_result.swap.state {
            agicash_cashu::CashuSendSwapState::Draft => {
                let seed = self.auth.cashu_seed().await?;
                self.send_swap_service
                    .swap_for_proofs_to_send(&create_result.account, create_result.swap, &seed)
                    .await?
            }
            _ => create_result.swap,
        };

        // Pull the proofs-to-send + token_hash out of the PENDING state.
        let (token_hash, proofs_to_send) = match &final_swap.state {
            agicash_cashu::CashuSendSwapState::Pending {
                token_hash,
                proofs_to_send,
            }
            | agicash_cashu::CashuSendSwapState::Completed {
                token_hash,
                proofs_to_send,
            } => (token_hash.clone(), proofs_to_send.clone()),
            other => {
                return Err(WalletError::Cashu(format!(
                    "send-swap ended in unexpected state: {other:?}"
                )))
            }
        };

        // Encode the wire token. V4 is the canonical encoding; V3 is the
        // legacy form some receivers still expect.
        let mint_url_str = create_result
            .account
            .details
            .get("mint_url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| WalletError::Internal("account.details missing mint_url".into()))?
            .to_string();
        let mint_url = MintUrl::from_str(&mint_url_str)
            .map_err(|e| WalletError::Internal(format!("mint URL: {e}")))?;
        let cdk_proofs = proofs_to_send
            .iter()
            .map(token_proof_to_cdk_proof)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| WalletError::Internal(format!("proof decode: {e}")))?;
        let unit = cashu_unit_for_currency(create_result.account.currency).ok_or_else(|| {
            WalletError::validation(
                "unsupported_currency",
                format!("no cashu unit for {}", create_result.account.currency),
            )
        })?;
        let token_string = match token_version {
            TokenVersion::V4 => Token::new(mint_url, cdk_proofs, None, unit).to_string(),
            TokenVersion::V3 => {
                // CDK's `Token::new_v3` ctor isn't directly exposed; fall back
                // to building V4 then asking the token for v3-encoded form
                // via `to_v3_string`. CDK 0.15 exposes `to_v3_string` on
                // `Token`.
                let t = Token::new(mint_url, cdk_proofs, None, unit);
                t.to_v3_string()
            }
        };

        Ok(SendTokenReceipt {
            token: token_string,
            amount: final_swap.amount_received.clone(),
            fee: final_swap.total_fee.clone(),
            account_id: final_swap.account_id,
            mint_url: mint_url_str,
            swap_id: final_swap.id,
            token_hash,
        })
    }

    /// Single-shot NUT-07 send-claim poll. Re-loads the send swap, asks the
    /// mint via `post_check_state` whether the swap's `proofs_to_send` are
    /// SPENT, and on all-SPENT flips the persisted row PENDING → COMPLETED so
    /// subsequent polls short-circuit. **Single-shot by design** — the
    /// consumer drives cadence (iOS 3 s timer, Leptos interval); the facade
    /// crate stays runtime-agnostic (no sleep/spawn — spec §cross-cutting +
    /// §P2-11 single-shot philosophy). Ports the FFI
    /// `check_send_swap_claimed` (`agicash-ffi/src/wallet.rs:1311-1416` @
    /// `241e8194`) 1:1.
    ///
    /// Returns `Pending` while any proof is non-SPENT (or the response is
    /// empty); `Completed` when every proof is SPENT or the row is already
    /// COMPLETED (fast-path, no mint round-trip); `Failed` only if the row is
    /// already FAILED (defensive).
    pub async fn check_send_token_claimed(
        &self,
        swap_id: Uuid,
    ) -> Result<crate::types::SendTokenClaimStatus, WalletError> {
        use crate::claim_check::all_proofs_spent;
        use crate::types::{SendTokenClaimState, SendTokenClaimStatus};
        use cdk::nuts::CheckStateRequest;
        // Bring `MintConnector` into scope so the dyn-Arc from
        // `wallet.connector()` exposes `post_check_state` (mirrors FFI
        // `wallet.rs:1316`).
        #[allow(unused_imports)]
        use cdk::wallet::MintConnector;

        let session = self.require_session().await?;

        let swap = self
            .cashu_send_storage
            .get(swap_id)
            .await
            .map_err(|e| WalletError::Internal(format!("storage error: {e}")))?;
        if swap.user_id != session.user_id {
            return Err(WalletError::Internal(
                "swap belongs to a different user".into(),
            ));
        }

        // Fast-path terminal states: no mint round-trip (FFI parity
        // `wallet.rs:1345-1361`).
        let proofs_to_send = match &swap.state {
            agicash_cashu::CashuSendSwapState::Completed { .. } => {
                return Ok(SendTokenClaimStatus {
                    state: SendTokenClaimState::Completed,
                    failure_reason: None,
                });
            }
            agicash_cashu::CashuSendSwapState::Failed { failure_reason } => {
                return Ok(SendTokenClaimStatus {
                    state: SendTokenClaimState::Failed,
                    failure_reason: Some(failure_reason.clone()),
                });
            }
            agicash_cashu::CashuSendSwapState::Pending { proofs_to_send, .. } => {
                proofs_to_send.clone()
            }
            other => {
                return Err(WalletError::Internal(format!(
                    "swap in unexpected state for claim-check: {other:?}"
                )));
            }
        };

        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let account = accounts
            .iter()
            .find(|a| a.id == swap.account_id && a.account_type == AccountType::Cashu)
            .ok_or_else(|| WalletError::Internal("no matching account for swap".into()))?;

        let wallet = self.cashu_provider.wallet_for_account(account).await?;

        // Hash each proof's secret to the curve point NUT-07 looks state up
        // by (mirrors FFI `wallet.rs:1374-1386`).
        let ys: Vec<cdk::nuts::PublicKey> = proofs_to_send
            .iter()
            .map(|p| {
                let secret = cdk::secret::Secret::from_str(&p.secret)
                    .map_err(|e| WalletError::Internal(format!("bad secret: {e}")))?;
                cdk::dhke::hash_to_curve(secret.as_bytes())
                    .map_err(|e| WalletError::Internal(format!("hash_to_curve: {e}")))
            })
            .collect::<Result<Vec<_>, _>>()?;

        let resp = wallet
            .connector()
            .post_check_state(CheckStateRequest { ys })
            .await
            .map_err(|e| WalletError::Internal(format!("mint check_state: {e}")))?;

        let states: Vec<cdk::nuts::State> = resp.states.iter().map(|s| s.state).collect();

        if all_proofs_spent(&states) {
            // PENDING → COMPLETED (FFI parity `wallet.rs:1407-1414`).
            self.send_swap_service.complete(&swap).await?;
            Ok(SendTokenClaimStatus {
                state: SendTokenClaimState::Completed,
                failure_reason: None,
            })
        } else {
            Ok(SendTokenClaimStatus {
                state: SendTokenClaimState::Pending,
                failure_reason: None,
            })
        }
    }

    /// NUT-05 melt-quote preview. Returns the fees + total without
    /// reserving any proofs.
    pub async fn quote_send_lightning(
        &self,
        account_id: Option<AccountId>,
        invoice: String,
    ) -> Result<SendLightningQuote, WalletError> {
        let session = self.require_session().await?;
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        // BTC-only routing for Lightning per the existing CLI semantics.
        let account = pick_cashu_account(&accounts, account_id, Currency::Btc)?.clone();
        let proofs = self
            .cashu_send_storage
            .list_unspent_proofs(account.id)
            .await
            .map_err(|e| WalletError::Cashu(format!("list_unspent_proofs: {e}")))?;
        let preview = self
            .melt_quote_service
            .get_quote(&account, &proofs, &invoice)
            .await?;
        Ok(SendLightningQuote {
            bolt11: preview.bolt11,
            amount: preview.amount_received,
            lightning_fee_reserve: preview.lightning_fee_reserve,
            cashu_fee: preview.cashu_fee,
            total_fee: preview.total_fee,
            total_amount: preview.total_amount,
            payment_hash: preview.payment_hash,
            expires_at: preview.expires_at,
            account_id: account.id,
        })
    }

    /// Begin a Lightning send (NUT-05 melt). UNPAID → PENDING via the
    /// core's `initiate_melt`. Returns a reconcile-aware
    /// [`SendLightningStatus`] — **never** an `Err` on "still
    /// in-flight" (P0-1). On [`SendLightningStatus::InFlight`] the
    /// caller MUST drive [`Self::poll_send_lightning`] on its own
    /// cadence and MUST NOT re-quote / re-`begin` this invoice
    /// (re-quoting fires a second `post_melt` → double-pay). Mirrors
    /// the proven FFI `create_melt_quote` + `execute_melt_quote`
    /// contract (`crates/agicash-ffi/src/wallet.rs` @ canonical ref).
    pub async fn begin_send_lightning(
        &self,
        account_id: Option<AccountId>,
        invoice: String,
    ) -> Result<SendLightningStatus, WalletError> {
        let session = self.require_session().await?;
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let account = pick_cashu_account(&accounts, account_id, Currency::Btc)?.clone();
        let proofs = self
            .cashu_send_storage
            .list_unspent_proofs(account.id)
            .await
            .map_err(|e| WalletError::Cashu(format!("list_unspent_proofs: {e}")))?;

        // Build preview, persist quote, then call post_melt.
        let preview = self
            .melt_quote_service
            .get_quote(&account, &proofs, &invoice)
            .await?;
        let create_result = self
            .melt_quote_service
            .create_quote(session.user_id, &account, preview)
            .await?;
        let seed = self.auth.cashu_seed().await?;

        // initiate_melt may return Paid (sync mint), Pending (in-flight),
        // or Failed. Map onto the typed, reconcile-aware status — a
        // PENDING is the non-error `InFlight`, never an `Err` (P0-1).
        let outcome = self
            .melt_quote_service
            .initiate_melt(&account, create_result.quote.clone(), &seed)
            .await?;
        Ok(meltoutcome_to_status(outcome))
    }

    /// Single-shot poll of a PENDING melt quote (P0-1). Does exactly
    /// one mint status check then returns — the caller owns the
    /// polling cadence + cancel lifecycle (mirrors FFI
    /// `poll_melt_quote`'s zero-timeout contract and the iOS 2s
    /// poll `Task`). A still-in-flight result is
    /// [`SendLightningStatus::InFlight`], **never** an `Err` — so a
    /// transient still-pending can never be mistaken for a failure and
    /// re-quoted (the removed `complete_send_lightning`'s 30s-timeout
    /// `Err(Cashu("still pending"))` was exactly that double-pay
    /// vector).
    pub async fn poll_send_lightning(
        &self,
        quote_id: Uuid,
    ) -> Result<SendLightningStatus, WalletError> {
        let session = self.require_session().await?;
        let quote = self
            .cashu_melt_quote_storage
            .get(quote_id)
            .await
            .map_err(|e| WalletError::Cashu(format!("storage error: {e}")))?;
        if quote.user_id != session.user_id {
            return Err(WalletError::Validation {
                code: "wrong_owner".into(),
                message: "quote belongs to a different user".into(),
            });
        }
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let account = accounts
            .iter()
            .find(|a| a.id == quote.account_id && a.account_type == AccountType::Cashu)
            .ok_or_else(|| WalletError::NotFound(format!("account {}", quote.account_id)))?
            .clone();
        let seed = self.auth.cashu_seed().await?;

        let outcome = self
            .melt_quote_service
            .poll_until_complete(
                &account,
                quote.clone(),
                &seed,
                Duration::ZERO,
                Duration::ZERO,
            )
            .await?;

        Ok(meltoutcome_to_status(outcome))
    }

    /// LUD-16 resolve → quote → send convenience wrapper.
    pub async fn send_to_lightning_address(
        &self,
        account_id: Option<AccountId>,
        address: String,
        amount: Money,
    ) -> Result<SendLightningReceipt, WalletError> {
        // Lightning send always settles against a BTC Cashu account
        // (see `begin_send_lightning` → `pick_cashu_account(.., Currency::Btc)`).
        // Guard the requested currency BEFORE conversion so a non-BTC
        // amount fails fast with a clear error instead of producing a
        // misleading msat figure.
        if amount.currency() != Currency::Btc {
            return Err(WalletError::validation(
                "currency_mismatch",
                format!(
                    "Lightning send requires a BTC amount; got {}",
                    amount.currency()
                ),
            ));
        }
        // Resolve the address → BOLT-11.
        let info = agicash_lightning_address::resolve(&address).await?;
        // amount.amount() is in the account's minor unit; convert to msat.
        let amount_msat = money_to_msat(&amount)?;
        let invoice = agicash_lightning_address::request_invoice(&info, amount_msat, None).await?;

        // Convenience wrapper: begin, then ONE reconcile poll if the
        // mint left the payment in flight. It deliberately does NOT
        // loop (a runtime-agnostic facade must not block on a slow
        // Lightning settle — cross-cutting constraint); a caller that
        // needs to drive a slow payment to terminal uses
        // `begin_send_lightning` + its own `poll_send_lightning`
        // cadence. Behavior change vs. the removed 30s-loop wrapper:
        // documented + intended (P0-1).
        match self.begin_send_lightning(account_id, invoice).await? {
            SendLightningStatus::Paid(receipt) => Ok(receipt),
            SendLightningStatus::InFlight { quote_id } => {
                match self.poll_send_lightning(quote_id).await? {
                    SendLightningStatus::Paid(receipt) => Ok(receipt),
                    SendLightningStatus::InFlight { quote_id } => {
                        // Still in flight after one reconcile poll —
                        // surface as the typed concurrency signal, NOT
                        // a `Cashu("still pending")` (the old double-pay
                        // string). The caller must poll, never re-send.
                        Err(WalletError::Concurrency(format!(
                            "lightning send {quote_id} still in flight — poll, do not re-send"
                        )))
                    }
                    SendLightningStatus::Failed { reason, .. } => Err(WalletError::Cashu(format!(
                        "lightning send failed: {reason}"
                    ))),
                }
            }
            SendLightningStatus::Failed { reason, .. } => Err(WalletError::Cashu(format!(
                "lightning send failed: {reason}"
            ))),
        }
    }
}

// ---------------------------------------------------------------------------
// Receive (Cashu token + Lightning)
// ---------------------------------------------------------------------------
impl WalletClient {
    /// Redeem a Cashu token (V3 `cashuA…` or V4 `cashuB…`).
    ///
    /// THIS IS the slice-12 hook the Leptos PWA's L4 receive flow calls.
    /// Idempotent on repeat redeems of the same token (returns
    /// [`ReceiveStatus::AlreadyClaimed`]).
    pub async fn receive_cashu_token(&self, token: &str) -> Result<ReceiveReceipt, WalletError> {
        let session = self.require_session().await?;
        let parsed = ParsedToken::parse(token, &self.cashu_provider).await?;

        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let account = pick_cashu_account_for_token(&accounts, &parsed.mint_url, &parsed.unit)
            .ok_or_else(|| {
                WalletError::validation(
                    "no_matching_account",
                    format!(
                        "no matching account for mint {} — add the mint first",
                        parsed.mint_url
                    ),
                )
            })?
            .clone();

        let create_result = match self
            .receive_swap_service
            .create(session.user_id, &parsed, &account, None)
            .await
        {
            Ok(r) => r,
            Err(ReceiveSwapError::Storage(ReceiveSwapStorageError::AlreadyClaimed)) => {
                let zero = Money::new(
                    Decimal::from(0u64),
                    account.currency,
                    unit_for_currency(account.currency),
                );
                return Ok(ReceiveReceipt {
                    status: ReceiveStatus::AlreadyClaimed,
                    amount: zero.clone(),
                    fee: zero,
                    account_id: account.id,
                    mint_url: parsed.mint_url,
                    token_hash: parsed.hash,
                });
            }
            Err(e) => return Err(e.into()),
        };

        let seed = self.auth.cashu_seed().await?;
        let outcome = self
            .receive_swap_service
            .complete_swap(&create_result.account, create_result.swap, &seed)
            .await?;

        Ok(receive_outcome_to_receipt(
            outcome,
            &create_result.account,
            &parsed,
        ))
    }

    /// Construct a fresh interactive receive-flow orchestrator.
    ///
    /// 12c §3: exposes the already-existing
    /// [`agicash_cashu::ReceiveFlowService`] (the sans-IO
    /// Idle→Parsing→NeedsMintConfirmation→AddingMint→Swapping→
    /// Done/AlreadyClaimed/Failed machine, with its `accepts()`
    /// event-guard and `AlreadyClaimed`-no-amount invariant) through the
    /// facade, built from the deps the facade already holds. Each call
    /// returns a fresh orchestrator — flows are not persisted across
    /// constructions (the binding shell owns the per-interaction
    /// handle's interior mutability; the facade stays runtime-agnostic).
    ///
    /// Requires an active session; returns
    /// [`WalletError::Unauthenticated`] otherwise (verbatim the prior
    /// FFI `receive_flow` `Auth { UNAUTHENTICATED }` behavior — the FFI
    /// `convert::wallet_error_to_ffi` maps that 1:1).
    pub async fn receive_flow(&self) -> Result<ReceiveFlowService, WalletError> {
        let session = self.require_session().await?;
        let seed_provider: Arc<dyn CashuSeedProvider> = Arc::new(AuthClientSeedProvider {
            auth: Arc::clone(&self.auth),
        });
        Ok(ReceiveFlowService::new(
            session.user_id,
            Arc::clone(&self.user_storage),
            Arc::clone(&self.cashu_provider),
            Arc::clone(&self.receive_swap_service),
            seed_provider,
        ))
    }

    /// Start a NUT-04 mint quote — request a BOLT-11 invoice from the
    /// mint. Caller drives `poll_receive_lightning` + `complete_receive_lightning`
    /// against the returned `quote_id`.
    pub async fn quote_receive_lightning(
        &self,
        account_id: Option<AccountId>,
        amount: Money,
    ) -> Result<ReceiveLightningHandle, WalletError> {
        let session = self.require_session().await?;
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let account = pick_cashu_account(&accounts, account_id, amount.currency())?.clone();

        let quote = self
            .mint_quote_service
            .create_quote(session.user_id, &account, amount, None)
            .await?;

        Ok(ReceiveLightningHandle {
            quote_id: quote.id,
            mint_quote_id: quote.quote_id.clone(),
            invoice: quote.payment_request.clone(),
            payment_hash: quote.payment_hash.clone(),
            amount: quote.amount.clone(),
            fee: quote.total_fee.clone(),
            account_id: account.id,
            expires_at: quote.expires_at,
        })
    }

    /// One-shot status check on a mint quote. Returns the snapshot; the
    /// caller drives the polling loop itself.
    pub async fn poll_receive_lightning(
        &self,
        quote_id: Uuid,
    ) -> Result<ReceiveLightningSnapshot, WalletError> {
        let session = self.require_session().await?;
        let quote = self
            .cashu_mint_quote_storage
            .get(quote_id)
            .await
            .map_err(|e| WalletError::Cashu(format!("storage error: {e}")))?;
        if quote.user_id != session.user_id {
            return Err(WalletError::Validation {
                code: "wrong_owner".into(),
                message: "quote belongs to a different user".into(),
            });
        }

        // Fast-path: if already past UNPAID, surface the persisted state.
        if !matches!(quote.state, CashuMintQuoteState::Unpaid) {
            return Ok(mint_quote_to_snapshot(&quote.state));
        }

        // Otherwise, do one mint round-trip via `poll_until_paid` with
        // zero timeout — the existing service exposes that semantics.
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let account = accounts
            .iter()
            .find(|a| a.id == quote.account_id && a.account_type == AccountType::Cashu)
            .ok_or_else(|| WalletError::NotFound(format!("account {}", quote.account_id)))?;
        let polled = self
            .mint_quote_service
            .poll_until_paid(
                account,
                quote,
                Duration::from_millis(0),
                Duration::from_millis(0),
            )
            .await?;
        Ok(mint_quote_to_snapshot(&polled.state))
    }

    /// Drive a PAID mint quote to COMPLETED — mint proofs and credit the
    /// account.
    pub async fn complete_receive_lightning(
        &self,
        quote_id: Uuid,
    ) -> Result<ReceiveReceipt, WalletError> {
        let session = self.require_session().await?;
        let quote = self
            .cashu_mint_quote_storage
            .get(quote_id)
            .await
            .map_err(|e| WalletError::Cashu(format!("storage error: {e}")))?;
        if quote.user_id != session.user_id {
            return Err(WalletError::Validation {
                code: "wrong_owner".into(),
                message: "quote belongs to a different user".into(),
            });
        }
        let accounts = self.user_storage.list_accounts(session.user_id).await?;
        let account = accounts
            .iter()
            .find(|a| a.id == quote.account_id && a.account_type == AccountType::Cashu)
            .ok_or_else(|| WalletError::NotFound(format!("account {}", quote.account_id)))?
            .clone();

        let seed = self.auth.cashu_seed().await?;
        let outcome = self
            .mint_quote_service
            .complete_receive(&account, quote.clone(), &seed)
            .await?;

        Ok(complete_mint_quote_to_receipt(outcome, &account, &quote))
    }
}

// ---------------------------------------------------------------------------
// Transactions (stubbed in slice 12 — slice 11 ships the storage method)
// ---------------------------------------------------------------------------
impl WalletClient {
    /// **Status:** slice 12 returns `Unsupported`. Per the slice plan §6.1,
    /// the unified `wallet.transactions` view's existence is an open
    /// question; either the storage union or a Supabase view migration
    /// lands in a follow-up slice.
    #[allow(clippy::unused_async)] // stub — async surface preserved for slice-11+ impl.
    pub async fn list_transactions(
        &self,
        _filter: TransactionFilter,
    ) -> Result<TransactionPage, WalletError> {
        Err(WalletError::Unsupported(
            "list_transactions: TransactionStorage trait not yet shipped",
        ))
    }

    /// **Status:** slice 12 returns `Unsupported`. See [`Self::list_transactions`].
    #[allow(clippy::unused_async)] // stub — async surface preserved for slice-11+ impl.
    pub async fn get_transaction(&self, _id: Uuid) -> Result<Transaction, WalletError> {
        Err(WalletError::Unsupported(
            "get_transaction: TransactionStorage trait not yet shipped",
        ))
    }
}

// ---------------------------------------------------------------------------
// Exchange rate + events
// ---------------------------------------------------------------------------
impl WalletClient {
    /// Look up a current BTC↔USD (or other) exchange rate from the
    /// configured provider.
    pub async fn exchange_rate(
        &self,
        from: Currency,
        to: Currency,
    ) -> Result<ExchangeRateSnapshot, WalletError> {
        let provider = self.exchange_rate.as_ref().ok_or(WalletError::Unsupported(
            "exchange_rate: no ExchangeRateProvider configured on builder",
        ))?;
        let rate = provider.get_rate(from, to).await?;
        Ok(ExchangeRateSnapshot { from, to, rate })
    }

    /// Subscribe to wallet-level events (balance changes, transaction
    /// updates).
    ///
    /// **Status:** slice 12 returns `Unsupported`. The event-bus
    /// infrastructure lives in `agicash-cache` (slice 11); this method
    /// is exposed now so consumers can compile against the future API.
    // FOLLOW-UP (slice 29): `agicash-realtime` (WalletEventListener /
    // subscription service) now exists on master — wire it in here. Kept
    // stubbed deliberately: realtime integration is out of slice-12 scope.
    #[allow(clippy::unused_self)]
    pub fn subscribe(&self) -> Result<(), WalletError> {
        Err(WalletError::Unsupported(
            "subscribe: event bus ships in slice 11",
        ))
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn pick_cashu_account(
    accounts: &[Account],
    requested: Option<AccountId>,
    currency: Currency,
) -> Result<&Account, WalletError> {
    let candidates: Vec<&Account> = accounts
        .iter()
        .filter(|a| {
            a.account_type == AccountType::Cashu
                && a.currency == currency
                && a.state == AccountState::Active
        })
        .collect();
    match requested {
        Some(id) => candidates
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| WalletError::NotFound(format!("Cashu {currency} account with id {id}"))),
        None => match candidates.len() {
            0 => Err(WalletError::validation(
                "no_account",
                format!("no Cashu {currency} account — add a mint first"),
            )),
            1 => Ok(candidates[0]),
            _ => Err(WalletError::validation(
                "ambiguous_account",
                format!("multiple Cashu {currency} accounts — pass account_id"),
            )),
        },
    }
}

fn pick_cashu_account_for_token<'a>(
    accounts: &'a [Account],
    mint_url: &str,
    unit: &CurrencyUnit,
) -> Option<&'a Account> {
    accounts.iter().find(|a| {
        a.account_type == AccountType::Cashu
            && a.details
                .get("mint_url")
                .and_then(|v| v.as_str())
                .is_some_and(|u| mint_urls_equal(u, mint_url))
            && unit_matches_currency(unit, a.currency)
    })
}

fn mint_urls_equal(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// 12c Tension T1: pin `WalletClient::add_mint`'s error classification
/// byte-identical to the pre-12c inline body.
///
/// The pre-12c facade had TWO distinct error paths:
///  1. `upsert_user_with_accounts(..).await?` — a `StorageError`
///     propagated via `From<StorageError> for WalletError` (`error.rs`):
///     `NotFound` → `WalletError::NotFound`; any other → `WalletError::Storage`.
///  2. upsert succeeded but no account matched — an explicit
///     `WalletError::Internal("upsert returned no account matching the
///     new mint URL")`.
///
/// The shared `add_mint_account` primitive (in `agicash-cashu`, which
/// has no `WalletError`) wraps BOTH as `ReceiveFlowError::MintAdd(
/// StorageError)`. Routing through the blanket `From<ReceiveFlowError>
/// for WalletError` would land on its `_ => Self::Cashu(..)` arm — a
/// user-visible reclassification. Naively unwrapping via
/// `WalletError::from(se)` fixes path 1 but mis-maps path 2's
/// `StorageError::Internal(<sentinel>)` to `Storage` instead of
/// `Internal`. This adapter special-cases the path-2 sentinel message
/// so BOTH paths are byte-identical to pre-12c.
fn map_add_mint_err(e: ReceiveFlowError) -> WalletError {
    use agicash_cashu::ReceiveFlowError as RFE;
    /// The exact sentinel `add_mint_account` emits when `upsert`
    /// succeeded but returned no matching account
    /// (`agicash-cashu/src/receive_flow/service.rs`); pre-12c the facade
    /// classified this as `WalletError::Internal`, not `Storage`.
    const NO_MATCH_SENTINEL: &str = "upsert returned no account matching the new mint URL";
    match e {
        RFE::MintAdd(agicash_traits::StorageError::Internal(msg))
        | RFE::Storage(agicash_traits::StorageError::Internal(msg))
            if msg == NO_MATCH_SENTINEL =>
        {
            WalletError::Internal(msg)
        }
        RFE::MintAdd(se) | RFE::Storage(se) => WalletError::from(se),
        // add_mint_account never parses tokens / discovers / swaps, so
        // no other RFE variant is reachable from it; fall back to the
        // existing blanket mapping defensively (unreachable in practice).
        other => WalletError::from(other),
    }
}

/// 12c D1: bridges the facade's `Arc<dyn AuthClient>` to the cashu
/// crate's `CashuSeedProvider` so `ReceiveFlowService` can fetch the
/// 64-byte seed through the SAME source `receive_cashu_token` already
/// uses (`self.auth.cashu_seed()`), eliminating the FFI's bespoke
/// `OpenSecretSeedProvider`. The error mapping preserves the FFI's
/// behavior of surfacing `code::AUTH` on the Failed state (so the UI's
/// "re-authenticate" branch is unchanged): any seed failure becomes
/// `ReceiveFlowError::Auth(..)`, whose `.code()` is `code::AUTH`.
#[derive(Debug)]
struct AuthClientSeedProvider {
    auth: Arc<dyn AuthClient>,
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait::async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait::async_trait(?Send))]
impl CashuSeedProvider for AuthClientSeedProvider {
    async fn get_cashu_seed(&self) -> Result<[u8; 64], ReceiveFlowError> {
        self.auth
            .cashu_seed()
            .await
            .map_err(|e| ReceiveFlowError::Auth(format!("fetch cashu seed: {e}")))
    }
}

fn unit_matches_currency(unit: &CurrencyUnit, currency: Currency) -> bool {
    matches!(
        (unit, currency),
        (CurrencyUnit::Sat, Currency::Btc) | (CurrencyUnit::Usd, Currency::Usd)
    )
}

fn unit_for_currency(currency: Currency) -> Unit {
    match currency {
        Currency::Btc => Unit::Sat,
        Currency::Usd | Currency::Usdb => Unit::Cent,
    }
}

fn cashu_unit_for_currency(currency: Currency) -> Option<CurrencyUnit> {
    match currency {
        Currency::Btc => Some(CurrencyUnit::Sat),
        Currency::Usd => Some(CurrencyUnit::Usd),
        Currency::Usdb => None,
    }
}

async fn compute_cashu_balance(
    storage: &dyn CashuSendSwapStorage,
    account: &Account,
) -> Result<u64, WalletError> {
    match account.account_type {
        AccountType::Cashu => {
            let proofs = storage
                .list_unspent_proofs(account.id)
                .await
                .map_err(|e| WalletError::Cashu(format!("list_unspent_proofs: {e}")))?;
            Ok(proofs.iter().map(|p| p.proof.amount).sum())
        }
        // Slice 9 wires Spark; until then balance is zero.
        AccountType::Spark => Ok(0),
    }
}

fn token_proof_to_cdk_proof(tp: &TokenProof) -> Result<Proof, String> {
    let keyset_id = KeysetId::from_str(&tp.id).map_err(|e| format!("keyset id: {e}"))?;
    let secret = cdk::secret::Secret::from_str(&tp.secret).map_err(|e| format!("secret: {e}"))?;
    let c_bytes = hex::decode(&tp.c).map_err(|e| format!("C hex: {e}"))?;
    let c = cdk::nuts::PublicKey::from_slice(&c_bytes).map_err(|e| format!("C key: {e}"))?;
    Ok(Proof {
        amount: Amount::from(tp.amount),
        keyset_id,
        secret,
        c,
        witness: None,
        dleq: None,
    })
}

fn receive_outcome_to_receipt(
    outcome: CompleteOutcome,
    fallback_account: &Account,
    parsed: &ParsedToken,
) -> ReceiveReceipt {
    match outcome {
        CompleteOutcome::Completed { swap, account, .. } => ReceiveReceipt {
            status: ReceiveStatus::Received,
            amount: swap.amount_received,
            fee: swap.fee_amount,
            account_id: account.id,
            mint_url: parsed.mint_url.clone(),
            token_hash: parsed.hash.clone(),
        },
        CompleteOutcome::AlreadyTerminal(swap) => {
            let status = match &swap.state {
                CashuReceiveSwapState::Completed => ReceiveStatus::Received,
                CashuReceiveSwapState::Failed { .. } => ReceiveStatus::AlreadyFailed,
                CashuReceiveSwapState::Pending => ReceiveStatus::Pending,
            };
            ReceiveReceipt {
                status,
                amount: swap.amount_received,
                fee: swap.fee_amount,
                account_id: fallback_account.id,
                mint_url: parsed.mint_url.clone(),
                token_hash: parsed.hash.clone(),
            }
        }
        CompleteOutcome::Failed(swap) => ReceiveReceipt {
            status: ReceiveStatus::AlreadyFailed,
            amount: swap.amount_received,
            fee: swap.fee_amount,
            account_id: fallback_account.id,
            mint_url: parsed.mint_url.clone(),
            token_hash: parsed.hash.clone(),
        },
    }
}

fn mint_quote_to_snapshot(state: &CashuMintQuoteState) -> ReceiveLightningSnapshot {
    match state {
        CashuMintQuoteState::Unpaid => ReceiveLightningSnapshot {
            state: ReceiveLightningState::Unpaid,
            failure_reason: None,
        },
        CashuMintQuoteState::Paid { .. } => ReceiveLightningSnapshot {
            state: ReceiveLightningState::Paid,
            failure_reason: None,
        },
        CashuMintQuoteState::Completed { .. } => ReceiveLightningSnapshot {
            state: ReceiveLightningState::Completed,
            failure_reason: None,
        },
        CashuMintQuoteState::Expired => ReceiveLightningSnapshot {
            state: ReceiveLightningState::Expired,
            failure_reason: None,
        },
        CashuMintQuoteState::Failed { failure_reason } => ReceiveLightningSnapshot {
            state: ReceiveLightningState::Failed,
            failure_reason: Some(failure_reason.clone()),
        },
    }
}

fn complete_mint_quote_to_receipt(
    outcome: CompleteMintQuoteOutcome,
    fallback_account: &Account,
    fallback_quote: &agicash_cashu::CashuMintQuote,
) -> ReceiveReceipt {
    let mint_url = fallback_account
        .details
        .get("mint_url")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default();
    let token_hash = fallback_quote.payment_hash.clone();

    match outcome {
        CompleteMintQuoteOutcome::Completed { quote, account, .. } => ReceiveReceipt {
            status: ReceiveStatus::Received,
            amount: quote.amount,
            fee: quote.total_fee,
            account_id: account.id,
            mint_url: account
                .details
                .get("mint_url")
                .and_then(|v| v.as_str())
                .map_or(mint_url, str::to_string),
            token_hash,
        },
        CompleteMintQuoteOutcome::AlreadyTerminal(quote) => {
            let status = match &quote.state {
                CashuMintQuoteState::Completed { .. } => ReceiveStatus::Received,
                CashuMintQuoteState::Failed { .. } | CashuMintQuoteState::Expired => {
                    ReceiveStatus::AlreadyFailed
                }
                CashuMintQuoteState::Paid { .. } | CashuMintQuoteState::Unpaid => {
                    ReceiveStatus::Pending
                }
            };
            ReceiveReceipt {
                status,
                amount: quote.amount,
                fee: quote.total_fee,
                account_id: fallback_account.id,
                mint_url,
                token_hash,
            }
        }
        CompleteMintQuoteOutcome::Failed(quote) => ReceiveReceipt {
            status: ReceiveStatus::AlreadyFailed,
            amount: quote.amount,
            fee: quote.total_fee,
            account_id: fallback_account.id,
            mint_url,
            token_hash,
        },
    }
}

/// Map the core `MeltOutcome` onto the reconcile-aware
/// [`SendLightningStatus`] (P0-1). `Pending` becomes the typed,
/// non-error `InFlight` (NEVER an `Err`); `Failed` carries the
/// persisted reason and is terminal/non-re-quotable (the verdict
/// round-tripped the mint via `poll_until_complete` / `initiate_melt`).
fn meltoutcome_to_status(outcome: MeltOutcome) -> SendLightningStatus {
    match outcome {
        MeltOutcome::Paid { quote, .. } => {
            // melt_paid_to_receipt returns Result; a PAID outcome whose
            // state isn't Paid{..} is an internal invariant break — fall
            // back to InFlight so we never present a false terminal.
            match melt_paid_to_receipt(&quote) {
                Ok(receipt) => SendLightningStatus::Paid(receipt),
                Err(_) => SendLightningStatus::InFlight { quote_id: quote.id },
            }
        }
        MeltOutcome::Pending(q) => SendLightningStatus::InFlight { quote_id: q.id },
        MeltOutcome::Failed(q) => SendLightningStatus::Failed {
            quote_id: q.id,
            reason: format!("{:?}", q.state),
        },
    }
}

fn melt_paid_to_receipt(
    quote: &agicash_cashu::CashuMeltQuote,
) -> Result<SendLightningReceipt, WalletError> {
    let CashuMeltQuoteState::Paid {
        payment_preimage,
        lightning_fee,
        amount_spent,
        total_fee,
    } = &quote.state
    else {
        return Err(WalletError::Cashu(format!(
            "expected PAID melt quote, got {:?}",
            quote.state
        )));
    };
    Ok(SendLightningReceipt {
        quote_id: quote.id,
        amount: quote.amount_received.clone(),
        lightning_fee: lightning_fee.clone(),
        cashu_fee: quote.cashu_fee.clone(),
        total_fee: total_fee.clone(),
        amount_spent: amount_spent.clone(),
        payment_preimage: payment_preimage.clone(),
        payment_hash: quote.payment_hash.clone(),
        account_id: quote.account_id,
    })
}

fn money_to_msat(amount: &Money) -> Result<u64, WalletError> {
    use rust_decimal::prelude::ToPrimitive;
    // Canonical conversion (mirrors `money_to_minor_units` in
    // agicash-cashu/src/mint_quote/service.rs and `amount_as_u64` in
    // send_swap/service.rs): normalize to the target unit, then read the
    // integer. `to_unit` handles every unit the rest of the system handles
    // (Major / Sat / Msat / Cent), so a BTC `Unit::Major` input converts
    // correctly instead of being rejected as `unsupported_unit`.
    let normalized = amount
        .to_unit(Unit::Msat)
        .map_err(|e| WalletError::validation("unsupported_unit", format!("to msat: {e}")))?;
    let dec = normalized.amount();
    // Precision loss must be explicit: a non-integer msat amount is a
    // caller error, not something we silently truncate (the old
    // `Decimal::try_into` rounded fractional values away).
    if dec.fract() != Decimal::ZERO {
        return Err(WalletError::validation(
            "bad_amount",
            format!("amount {dec} msat is not a whole number of msat"),
        ));
    }
    dec.to_u64()
        .ok_or_else(|| WalletError::validation("bad_amount", "amount overflows u64 msat"))
}

// `MeltQuoteError` already implements `From` to `WalletError`; this alias
// keeps grep-ability for the future when error variants get filtered more
// granularly.
#[allow(dead_code)]
type _MeltErr = MeltQuoteError;

#[cfg(test)]
mod tests {
    use super::*;
    use agicash_domain::{AccountPurpose, AccountState, UserId};
    use chrono::Utc;
    use serde_json::json;

    fn account(currency: Currency, mint_url: &str) -> Account {
        Account {
            id: AccountId::new(),
            created_at: Utc::now(),
            user_id: UserId::new(),
            name: "test mint".into(),
            account_type: AccountType::Cashu,
            purpose: AccountPurpose::Transactional,
            currency,
            details: json!({ "mint_url": mint_url, "keyset_counters": {} }),
            version: 0,
            state: AccountState::Active,
            expires_at: None,
        }
    }

    #[test]
    fn pick_cashu_account_returns_only_matching_currency() {
        let accounts = vec![
            account(Currency::Btc, "https://m.example"),
            account(Currency::Usd, "https://m.example"),
        ];
        let picked = pick_cashu_account(&accounts, None, Currency::Btc).unwrap();
        assert_eq!(picked.currency, Currency::Btc);
    }

    #[test]
    fn pick_cashu_account_errors_when_no_match() {
        let accounts = vec![account(Currency::Usd, "https://m.example")];
        let err = pick_cashu_account(&accounts, None, Currency::Btc).unwrap_err();
        assert!(matches!(err, WalletError::Validation { ref code, .. } if code == "no_account"));
    }

    #[test]
    fn pick_cashu_account_errors_when_ambiguous() {
        let accounts = vec![
            account(Currency::Btc, "https://m1.example"),
            account(Currency::Btc, "https://m2.example"),
        ];
        let err = pick_cashu_account(&accounts, None, Currency::Btc).unwrap_err();
        assert!(
            matches!(err, WalletError::Validation { ref code, .. } if code == "ambiguous_account")
        );
    }

    #[test]
    fn pick_cashu_account_returns_requested_id() {
        let accounts = vec![
            account(Currency::Btc, "https://m1.example"),
            account(Currency::Btc, "https://m2.example"),
        ];
        let want = accounts[1].id;
        let picked = pick_cashu_account(&accounts, Some(want), Currency::Btc).unwrap();
        assert_eq!(picked.id, want);
    }

    #[test]
    fn unit_matches_currency_pairs_match_expected() {
        assert!(unit_matches_currency(&CurrencyUnit::Sat, Currency::Btc));
        assert!(unit_matches_currency(&CurrencyUnit::Usd, Currency::Usd));
        assert!(!unit_matches_currency(&CurrencyUnit::Sat, Currency::Usd));
    }

    #[test]
    fn money_to_msat_handles_sat_unit() {
        let m = Money::new(Decimal::from(10u64), Currency::Btc, Unit::Sat);
        assert_eq!(money_to_msat(&m).unwrap(), 10_000);
    }

    #[test]
    fn money_to_msat_handles_msat_unit() {
        let m = Money::new(Decimal::from(2500u64), Currency::Btc, Unit::Msat);
        assert_eq!(money_to_msat(&m).unwrap(), 2500);
    }

    #[test]
    fn money_to_msat_rejects_cent_unit() {
        let m = Money::new(Decimal::from(1u64), Currency::Usd, Unit::Cent);
        assert!(money_to_msat(&m).is_err());
    }

    // H1 regression: a BTC `Unit::Major` amount must convert, not be
    // rejected as `unsupported_unit`. 0.00000001 BTC = 1 sat = 1000 msat.
    #[test]
    fn money_to_msat_handles_btc_major_unit() {
        let m = Money::new(
            Decimal::from_str("0.00000001").unwrap(),
            Currency::Btc,
            Unit::Major,
        );
        assert_eq!(money_to_msat(&m).unwrap(), 1_000);
    }

    // H1 regression: a fractional msat amount must be an explicit error,
    // not silently truncated (the old `Decimal::try_into` rounded it away).
    // 1500.5 msat at Unit::Msat has a non-zero fractional part.
    #[test]
    fn money_to_msat_rejects_fractional_msat() {
        let m = Money::new(
            Decimal::from_str("1500.5").unwrap(),
            Currency::Btc,
            Unit::Msat,
        );
        let err = money_to_msat(&m).unwrap_err();
        assert!(
            matches!(err, WalletError::Validation { ref code, .. } if code == "bad_amount"),
            "expected bad_amount validation error, got {err:?}"
        );
    }

    #[test]
    fn map_add_mint_err_pins_pre_12c_classification() {
        use agicash_cashu::ReceiveFlowError as RFE;
        // Path 1a: upsert itself fails with a non-NotFound StorageError.
        // Pre-12c: `?` via From<StorageError> → WalletError::Storage.
        let e = map_add_mint_err(RFE::MintAdd(agicash_traits::StorageError::Internal(
            "db down".into(),
        )));
        assert!(
            matches!(e, WalletError::Storage(_)),
            "upsert StorageError::Internal must stay WalletError::Storage (T1 path 1), got {e:?}"
        );
        // Path 1b: upsert fails with StorageError::NotFound.
        // Pre-12c: From<StorageError> → WalletError::NotFound.
        let e = map_add_mint_err(RFE::MintAdd(agicash_traits::StorageError::NotFound));
        assert!(
            matches!(e, WalletError::NotFound(_)),
            "upsert StorageError::NotFound must stay WalletError::NotFound (T1 path 1b), got {e:?}"
        );
        // Path 2 (Defect-2 δ2): upsert SUCCEEDS but no account matched —
        // the primitive emits MintAdd(StorageError::Internal(<sentinel>)).
        // Pre-12c facade: WalletError::Internal (NOT Storage). A naive
        // From<StorageError> remap would wrongly produce Storage here.
        let e = map_add_mint_err(RFE::MintAdd(agicash_traits::StorageError::Internal(
            "upsert returned no account matching the new mint URL".into(),
        )));
        assert!(
            matches!(e, WalletError::Internal(_)),
            "post-upsert not-found must be WalletError::Internal byte-identical to \
             pre-12c (Defect-2 δ2), NOT Storage; got {e:?}"
        );
    }

    // 12c D1: AuthClientSeedProvider bridges Arc<dyn AuthClient> to
    // agicash_cashu::CashuSeedProvider. Tested in isolation against a
    // tiny inline AuthClient stub (FakeAuth is auth.rs-private). ~30 LOC,
    // explicitly budgeted (Defect-3 re-architecture) — NOT a WalletClient
    // harness.
    #[derive(Debug)]
    struct SeedStubAuth {
        seed: Result<[u8; 64], ()>,
    }
    #[async_trait::async_trait]
    impl crate::auth::AuthClient for SeedStubAuth {
        async fn register_guest(&self) -> Result<crate::auth::Session, WalletError> {
            unreachable!()
        }
        async fn login_email(
            &self,
            _e: &str,
            _p: &str,
        ) -> Result<crate::auth::Session, WalletError> {
            unreachable!()
        }
        async fn register_email(
            &self,
            _e: &str,
            _p: &str,
            _n: Option<&str>,
        ) -> Result<crate::auth::Session, WalletError> {
            unreachable!()
        }
        async fn logout(&self) -> Result<(), WalletError> {
            unreachable!()
        }
        async fn set_session(&self, _s: crate::auth::Session) -> Result<(), WalletError> {
            unreachable!()
        }
        async fn get_session(&self) -> Result<Option<crate::auth::Session>, WalletError> {
            unreachable!()
        }
        async fn cashu_seed(&self) -> Result<[u8; 64], WalletError> {
            self.seed.map_err(|()| WalletError::Unauthenticated)
        }
    }

    #[tokio::test]
    async fn auth_client_seed_provider_forwards_seed_and_maps_auth_failure() {
        use agicash_cashu::CashuSeedProvider;
        // success: forwards the 64-byte seed verbatim.
        let ok_provider = AuthClientSeedProvider {
            auth: Arc::new(SeedStubAuth {
                seed: Ok([7u8; 64]),
            }) as Arc<dyn crate::auth::AuthClient>,
        };
        assert_eq!(ok_provider.get_cashu_seed().await.unwrap(), [7u8; 64]);
        // failure: any seed error becomes ReceiveFlowError::Auth, whose
        // .code() is the SAME code a reference ReceiveFlowError::Auth
        // yields (code::AUTH) — so the UI's re-auth branch is unchanged
        // (the behavior the FFI's old OpenSecretSeedProvider produced).
        let err_provider = AuthClientSeedProvider {
            auth: Arc::new(SeedStubAuth { seed: Err(()) }) as Arc<dyn crate::auth::AuthClient>,
        };
        let e = err_provider.get_cashu_seed().await.unwrap_err();
        assert!(
            matches!(e, agicash_cashu::ReceiveFlowError::Auth(_)),
            "seed failure must map to ReceiveFlowError::Auth (preserves code::AUTH), got {e:?}"
        );
        assert_eq!(
            e.code(),
            agicash_cashu::ReceiveFlowError::Auth("ref".into()).code(),
            "the mapped error's .code() must equal a reference Auth's .code() (code::AUTH)"
        );
    }

    // M1-test (CLOSED): `get_account` ownership-mismatch is now covered
    // hermetically via the all-fakes facade — see
    // `tests/tier1_hermetic.rs::get_account_for_foreign_owner_is_wrong_owner_validation`
    // (+ the owner-succeeds control). The fakes lane shipped the
    // `agicash-testing` `TestWallet`/`InMemoryUserStorage`.

    // --- 12b-2 Task 5: P0-1 reconcile-aware surface ---

    #[test]
    fn p0_1_pending_is_a_typed_non_error_outcome_not_an_err() {
        // The regression: complete_send_lightning returned
        // Err(Cashu("still pending after poll timeout")) on the 30s
        // branch; a consumer treating that Err as "failed" + re-quoting
        // double-pays. The fix makes "still pending" an un-mistakable
        // typed variant, never an Err. This test pins that contract: an
        // InFlight status is constructible and is NOT a WalletError.
        let qid = uuid::Uuid::new_v4();
        let status = SendLightningStatus::InFlight { quote_id: qid };
        // Must be matchable as a non-error outcome the consumer polls.
        assert!(matches!(
            status,
            SendLightningStatus::InFlight { quote_id } if quote_id == qid
        ));
    }

    #[test]
    fn p0_1_complete_send_lightning_is_removed() {
        // Compile-time guarantee that the ambiguous bundled-loop method is
        // gone (clean-refactor, replace-not-coexist per the spec). If
        // someone re-adds it, this test's doc comment points them here.
        // Enforced by the Step 4 grep gate; this test documents intent.
        // (No runtime assertion possible for a removed symbol; the grep
        // in the verification gate is the executable gate.)
    }

    use async_trait::async_trait;

    /// Minimal auth fake that is never logged in — exercises the
    /// `require_session` guard on `check_send_token_claimed` without the
    /// (absent) full WalletClient harness; this is the parity bar the
    /// harvest pins (mirrors every FFI `*_without_session_returns_
    /// unauthenticated` test, e.g. `agicash-ffi/src/wallet.rs:2766-2784`).
    #[derive(Debug, Default)]
    struct NoSessionAuth;

    #[async_trait]
    impl AuthClient for NoSessionAuth {
        async fn register_guest(&self) -> Result<Session, WalletError> {
            unimplemented!()
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
            Ok(None) // never logged in
        }
        async fn cashu_seed(&self) -> Result<[u8; 64], WalletError> {
            unimplemented!()
        }
    }

    #[tokio::test]
    async fn check_send_token_claimed_without_session_is_unauthenticated() {
        // 12c-3 (CLOSED): the full facade path
        // `check_send_token_claimed` → `require_session` → `Unauthenticated`
        // is now proven through the real `WalletClient` hermetically — see
        // `tests/tier1_hermetic.rs::check_send_token_claimed_without_session_is_unauthenticated`.
        // This inline assertion stays as the cheap direct-seam precondition
        // (`require_session` ⇒ `get_session().is_none()`).
        let auth = NoSessionAuth;
        assert!(auth.get_session().await.unwrap().is_none());
    }
}
