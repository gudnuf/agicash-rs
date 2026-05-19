package com.makeprisms.agicash.wallet

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.agicash_ffi.AccountFfi
import uniffi.agicash_ffi.AgicashWallet
import uniffi.agicash_ffi.FfiException
import uniffi.agicash_ffi.LightningAddressException
import uniffi.agicash_ffi.MeltQuoteFfiState
import uniffi.agicash_ffi.MeltQuoteHandle
import uniffi.agicash_ffi.MeltQuotePreview
import uniffi.agicash_ffi.MeltQuoteSnapshot
import uniffi.agicash_ffi.MintAddResult
import uniffi.agicash_ffi.MintQuoteFfiState
import uniffi.agicash_ffi.MintQuoteHandle
import uniffi.agicash_ffi.RealtimeStatusFfi
import uniffi.agicash_ffi.ReceiveResult
import uniffi.agicash_ffi.SendQuotePreview
import uniffi.agicash_ffi.SendSwapClaimState
import uniffi.agicash_ffi.SendSwapHandle
import uniffi.agicash_ffi.Session
import uniffi.agicash_ffi.UserFfi
import uniffi.agicash_ffi.WalletEventListener
import uniffi.agicash_ffi.requestLightningInvoice
import uniffi.agicash_ffi.resolveLightningAddress

/**
 * Mirrors `ios/Agicash/Agicash/WalletViewModel.swift` in Android idiom.
 *
 * Subclassing [AndroidViewModel] so the constructor has an
 * [Application] handle — needed for the `getFilesDir()` path the Rust
 * FFI installs as the [AgicashWallet]'s session storage root. Without
 * that path, the wallet has only an in-memory session slot and the user
 * re-authenticates on every cold start (the issue this deliverable
 * fixes).
 *
 * Lifecycle:
 *
 *   1. `init` constructs the [AgicashWallet] and immediately installs
 *      `setSessionStorageDir(application.filesDir.absolutePath)`. This
 *      is the JNI hop into `AndroidFileSessionStorage` (an AES-256-GCM
 *      blob in the app's private data dir).
 *   2. `tryRestoreSession()` runs the OpenSecret handshake + refresh
 *      chain against any blob already on disk. On success the app
 *      lands on the home screen; on failure we drop the blob and route
 *      to the sign-in screen.
 *   3. Every `auth_*` call writes the new session through to disk; the
 *      next cold start picks it up via step 2.
 *   4. `signOut` clears both the in-memory slot AND the on-disk blob
 *      (the Rust `auth_logout` runs `storage.clear()` after the
 *      best-effort server logout).
 *
 * Endpoint configuration mirrors iOS but uses the Android emulator's
 * host-loopback alias `10.0.2.2` for OpenSecret and Supabase.
 */
class WalletViewModel(application: Application) : AndroidViewModel(application) {

    /**
     * Hard-coded Phase 1 dev endpoints. On the Android emulator, host
     * loopback is reached via `10.0.2.2`, not `127.0.0.1`. The OpenSecret
     * enclave listens on :3999 and the local Supabase stack on :54321.
     */
    private object Endpoints {
        const val OPENSECRET_URL = "http://10.0.2.2:3999"
        const val OPENSECRET_CLIENT_ID = "ba5a14b5-d915-47b1-b7b1-afda52bc5fc6"
        const val SUPABASE_URL = "https://10.0.2.2:54321"
        // Local supabase publishable anon key (same one used by the JS app +
        // the integration tests; not a secret).
        const val SUPABASE_ANON_KEY =
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZS1kZW1vIiwicm9sZSI6ImFub24iLCJleHAiOjE5ODM4MTI5OTZ9.CRXP1A7WOeoJeXxjNni43kdQwgnWNReilDMblYTn_I0"
    }

    sealed interface BootState {
        data object Pending : BootState
        data class Ready(val phase: Phase) : BootState
        data class Failed(val message: String) : BootState
    }

    sealed interface Phase {
        data object SignedOut : Phase
        data class SignedIn(val userId: String) : Phase
        data class Error(val message: String) : Phase
    }

    private val _state = MutableStateFlow<BootState>(BootState.Pending)
    val state: StateFlow<BootState> = _state.asStateFlow()

    private val _accounts = MutableStateFlow<List<AccountFfi>>(emptyList())
    val accounts: StateFlow<List<AccountFfi>> = _accounts.asStateFlow()

    /**
     * Mirrors `WalletViewModel.user` on iOS. Populated alongside
     * `accounts` from [refreshAccounts]. Stays `null` for brand-new
     * guests (the `wallet.users` row only exists after the first
     * `mint_add` call). UI consumers should treat `null` as "no
     * defaults known" — no `Default` badge anywhere, every account is
     * eligible for the swipe-to-default action.
     */
    private val _user = MutableStateFlow<UserFfi?>(null)
    val user: StateFlow<UserFfi?> = _user.asStateFlow()

    private val _isWorking = MutableStateFlow(false)
    val isWorking: StateFlow<Boolean> = _isWorking.asStateFlow()

    /**
     * True while an explicit user-initiated refresh (pull-to-refresh) is
     * in flight. Drives the Material3 `PullToRefreshBox` spinner on
     * [com.makeprisms.agicash.ui.screens.HomeScreen]. Distinct from
     * [isWorking] (auth/mint/send mutations) so a realtime-driven or an
     * on-resume refresh never spins the pull indicator. iOS gets this for
     * free from SwiftUI's `.refreshable` awaiting the async closure; on
     * Android we surface it explicitly because the screen needs a
     * `isRefreshing` boolean.
     */
    private val _isRefreshing = MutableStateFlow(false)
    val isRefreshing: StateFlow<Boolean> = _isRefreshing.asStateFlow()

    private val _loginErrorMessage = MutableStateFlow<String?>(null)
    val loginErrorMessage: StateFlow<String?> = _loginErrorMessage.asStateFlow()

    /**
     * Transient, non-fatal refresh failure surfaced to a soft banner on
     * Home while the user stays signed in (last-known balance preserved).
     * Set when a realtime-driven / on-resume / post-mutation
     * `list_accounts()` blip fails *without* being a genuine auth
     * expiry; cleared on the next successful refresh. Distinct from
     * [loginErrorMessage] (login screen) and from the session-destroying
     * [Phase.Error]/`ErrorView` path, which is now reserved for the
     * initial bootstrap load only. Mirrors the web app degrading to a
     * stale-but-usable balance on a fetch hiccup rather than evicting
     * the session.
     */
    private val _refreshError = MutableStateFlow<String?>(null)
    val refreshError: StateFlow<String?> = _refreshError.asStateFlow()

    private var wallet: AgicashWallet? = null

    /**
     * Tracks whether a realtime subscription is currently active so the
     * two session-establishing paths (cold-start restore + interactive
     * sign-in) don't double-start it, and so [signOut] only tears it
     * down when one is running. Touched only from `viewModelScope`
     * coroutines (single-threaded main dispatcher confinement), so a
     * plain `Boolean` is sufficient — no atomics needed.
     */
    private var realtimeStarted = false

    /**
     * Forwards Supabase-Realtime activity (delivered on the Rust
     * realtime supervisor's tokio task — see the `WalletEventListener`
     * UniFFI callback-interface doc) onto the existing
     * [refreshAccounts] path. `onConnected` is the **no-replay catch-up**
     * (spec §5.5): on every (re)connect we refetch wallet+balance, which
     * is exactly why the Tier-1 foreground poll on `HomeScreen` is
     * deleted — a fresh connect already does the catch-up the poll used
     * to do. `onEvent` is a DB-originated broadcast; the wallet layer
     * demuxes by name, so here we just refetch (mirrors the React
     * `useTrackWalletChanges` → React-Query-invalidate behavior).
     *
     * `onStatus`/`onError` are **non-fatal** by construction: they only
     * log. A realtime drop/error must NEVER route to the
     * session-destroying [Phase.Error]/`ErrorView` — the tiered
     * non-destructive model in [refreshAccountsSuspending]
     * (`isBootstrap`/`isSignedIn` discipline) is preserved untouched.
     * Realtime down → last-known balance stays on screen; the next
     * `onConnected` refetches on reconnect.
     *
     * [refreshAccounts] is fire-and-forget on `viewModelScope`, so these
     * callbacks never block the realtime supervisor task and the bridge
     * holds no `WalletViewModel` strong ref beyond the enclosing
     * instance (it's an `inner class`; its lifetime is the ViewModel's,
     * and `stopWalletEvents()` in [signOut]/`onCleared` drops the
     * Rust-side `Arc` to it).
     */
    private inner class WalletEventBridge : WalletEventListener {
        override fun onConnected() {
            refreshAccounts()
        }

        override fun onEvent(event: String, payloadJson: String) {
            refreshAccounts()
        }

        override fun onStatus(status: RealtimeStatusFfi) {
            android.util.Log.d("WalletViewModel", "realtime status: $status")
        }

        override fun onError(message: String) {
            // Observability only — explicitly NOT escalated to Phase.Error.
            android.util.Log.w("WalletViewModel", "realtime error (non-fatal): $message")
        }
    }

    init {
        bootstrap()
    }

    /**
     * Start the realtime subscription once a session is established
     * (the FFI rejects with `FfiException.Auth` if called without one,
     * so this is only invoked from the two post-auth paths). Idempotent
     * via [realtimeStarted]; the suspend FFI call is confined to
     * `Dispatchers.IO` like every other UniFFI hop in this class
     * (RustFuture::poll must not run on the main looper). A failure to
     * start is **non-fatal**: log and continue on last-known balance —
     * it must not destroy or block the session.
     */
    private suspend fun startRealtime() {
        if (realtimeStarted) return
        val w = wallet ?: return
        try {
            withContext(Dispatchers.IO) { w.startWalletEvents(WalletEventBridge()) }
            realtimeStarted = true
        } catch (e: Throwable) {
            android.util.Log.w(
                "WalletViewModel",
                "startWalletEvents failed (continuing without realtime): ${e.message}",
            )
        }
    }

    /**
     * Stop the realtime subscription on logout. The FFI call is an
     * idempotent no-op when nothing is running, but we still gate on
     * [realtimeStarted] to skip the `Dispatchers.IO` hop in the common
     * signed-out-already case. Best-effort: a failure here never blocks
     * the local sign-out state transition.
     */
    private suspend fun stopRealtime() {
        if (!realtimeStarted) return
        val w = wallet ?: return
        try {
            withContext(Dispatchers.IO) { w.stopWalletEvents() }
        } catch (e: Throwable) {
            android.util.Log.w(
                "WalletViewModel",
                "stopWalletEvents failed (ignored): ${e.message}",
            )
        } finally {
            realtimeStarted = false
        }
    }

    private fun bootstrap() {
        try {
            wallet = AgicashWallet(
                opensecretUrl = Endpoints.OPENSECRET_URL,
                opensecretClientIdUuid = Endpoints.OPENSECRET_CLIENT_ID,
                supabaseUrl = Endpoints.SUPABASE_URL,
                supabaseAnonKey = Endpoints.SUPABASE_ANON_KEY,
            )
        } catch (e: Throwable) {
            _state.value = BootState.Failed("init failed: ${e.message}")
            return
        }

        // Install the file-backed session storage + try to rehydrate any
        // existing blob from the previous run. Both calls are best-effort
        // — failures fall through to the sign-in screen rather than
        // wedging the app.
        //
        // FFI calls run on Dispatchers.IO so UniFFI's `RustFuture::poll`
        // never executes on the Android main thread. Without that
        // dispatcher switch, Rust-side `RwLock::lock_contended` waits
        // freeze the main looper for 5s and trigger an ANR (see also
        // the same pattern in refreshAccounts, signOut, addMint, etc.).
        viewModelScope.launch {
            val w = wallet ?: return@launch
            val filesDir = getApplication<Application>().filesDir.absolutePath
            val restored: Session? = withContext(Dispatchers.IO) {
                try {
                    w.setSessionStorageDir(filesDir)
                } catch (e: Throwable) {
                    android.util.Log.w(
                        "WalletViewModel",
                        "setSessionStorageDir failed (continuing in-memory): ${e.message}",
                    )
                }
                try {
                    w.tryRestoreSession()
                } catch (e: Throwable) {
                    android.util.Log.w(
                        "WalletViewModel",
                        "tryRestoreSession failed: ${e.message}",
                    )
                    null
                }
            }

            if (restored != null) {
                _state.value = BootState.Ready(Phase.SignedIn(restored.userId))
                // Bootstrap path: there is no last-known balance to fall
                // back to yet, so a hard failure here legitimately routes
                // to the initial-load error surface.
                refreshAccountsSuspending(isBootstrap = true)
                // Session is established (cold-start restore succeeded):
                // attach the realtime subscription. This replaces the
                // deleted Tier-1 HomeScreen poll — on (re)connect the
                // bridge's onConnected does the no-replay catch-up.
                startRealtime()
            } else {
                _state.value = BootState.Ready(Phase.SignedOut)
            }
        }
    }

    fun signInAsGuest() {
        runSignIn { it.authGuest() }
    }

    fun signInWithEmail(email: String, password: String) {
        val trimmedEmail = email.trim()
        if (trimmedEmail.isEmpty() || password.isEmpty()) {
            _loginErrorMessage.value = "Email and password are required."
            return
        }
        runSignIn { it.authLogin(trimmedEmail, password) }
    }

    private fun runSignIn(call: suspend (AgicashWallet) -> Session) {
        val w = wallet ?: return
        _isWorking.value = true
        _loginErrorMessage.value = null
        viewModelScope.launch {
            try {
                val session = withContext(Dispatchers.IO) { call(w) }
                _state.value = BootState.Ready(Phase.SignedIn(session.userId))
                refreshAccounts()
                // Post-login session established: attach realtime. Same
                // catch-up role as the bootstrap path; replaces the
                // deleted Tier-1 poll.
                startRealtime()
            } catch (e: FfiException) {
                _loginErrorMessage.value = ffiErrorMessage(e)
            } catch (e: Throwable) {
                _loginErrorMessage.value = "unexpected: ${e.message}"
            } finally {
                _isWorking.value = false
            }
        }
    }

    fun signOut() {
        val w = wallet ?: return
        _isWorking.value = true
        viewModelScope.launch {
            // Tear down the realtime subscription BEFORE the server
            // logout so the channel's access_token is still valid for
            // the clean phx_leave + socket close.
            stopRealtime()
            try {
                withContext(Dispatchers.IO) { w.authLogout() }
            } catch (_: Throwable) {
                // Best-effort; clear local state regardless.
            }
            _accounts.value = emptyList()
            _user.value = null
            _loginErrorMessage.value = null
            _refreshError.value = null
            _state.value = BootState.Ready(Phase.SignedOut)
            _isWorking.value = false
        }
    }

    /**
     * Fire-and-forget account refresh. Re-reads balance from the FFI
     * (`list_accounts()` → Σ UNSPENT proofs, the correct cross-device
     * source). Used by the existing call sites that don't need to await
     * completion: bootstrap session-restore, post-sign-in, `addMint`,
     * `setDefaultAccount`, and (when the receive lane lands) the future
     * `receive()` success path — same role as iOS `await refreshAccounts()`
     * inside `receive`/`completeLightningQuote`/`createSend`.
     *
     * The lifecycle-aware on-resume refresh and the realtime bridge
     * (`onConnected`/`onEvent`) call this too. Pull-to-refresh uses
     * [refreshAccountsFromPull] instead so it can flip [isRefreshing] for
     * the Material3 spinner.
     */
    fun refreshAccounts() {
        viewModelScope.launch { refreshAccountsSuspending() }
    }

    /**
     * True if a non-auth refresh failure should NOT escalate to the
     * session-destroying [Phase.Error]/`ErrorView`. Holds whenever the
     * user is already established (`SignedIn`) so background-poll /
     * on-resume / post-mutation blips degrade to a stale-but-usable
     * balance instead of evicting the session. The bootstrap restore
     * passes `isBootstrap = true` explicitly because it flips `_state`
     * to `SignedIn` *before* the first refresh — reading `_state` there
     * would misclassify the initial load as recoverable.
     */
    private fun isSignedIn(): Boolean =
        (_state.value as? BootState.Ready)?.phase is Phase.SignedIn

    /**
     * Pull-to-refresh entry point for `HomeScreen`'s `PullToRefreshBox`.
     * Suspends until the FFI round-trip completes and toggles
     * [isRefreshing] around it so the Compose pull indicator shows/hides
     * in lockstep — the Android analogue of SwiftUI `.refreshable`
     * awaiting `model.refreshAccounts()` on iOS. Safe to call from a
     * Compose `rememberCoroutineScope` launch.
     */
    suspend fun refreshAccountsFromPull() {
        _isRefreshing.value = true
        try {
            refreshAccountsSuspending()
        } finally {
            _isRefreshing.value = false
        }
    }

    /**
     * The actual refresh, suspending so callers (realtime bridge,
     * pull-to-refresh, on-resume) can sequence around it.
     *
     * Failure handling is now tiered so a transient blip on the
     * realtime-driven / on-resume / post-mutation refresh can no longer
     * destroy a live session (the old behavior: ANY `list_accounts()`
     * throw → [Phase.Error] → `ErrorView` whose only action is the
     * session-destroying `signOut()`):
     *
     *  - **Genuine auth failure** ([FfiException.Auth] — refresh token
     *    rejected / session expired): the session really is no longer
     *    valid. Drop to [Phase.SignedOut] so the user re-authenticates.
     *    This is non-destructive auth-expiry handling and mirrors the
     *    bootstrap precedent (iOS `bootstrap()` maps a rejected
     *    rehydration to `signedOut`, not `error`).
     *  - **Non-auth failure during bootstrap** (`isBootstrap = true`):
     *    there is no last-known balance to fall back to, so escalate to
     *    [Phase.Error] — the initial-load error surface is appropriate
     *    here (and is the only path that still reaches `ErrorView`).
     *  - **Non-auth failure while already signed in** (realtime
     *    onConnected/onEvent / on-resume / addMint / setDefault):
     *    NON-FATAL. Keep the last-good `_accounts`, surface a transient
     *    [refreshError] banner, and leave `_state` untouched. The next
     *    realtime (re)connect / broadcast or on-resume self-heals it
     *    when the network recovers.
     *
     * A `get_user()` failure remains non-fatal (brand-new guests have no
     * user row yet — expected).
     */
    private suspend fun refreshAccountsSuspending(isBootstrap: Boolean = false) {
        val w = wallet ?: return
        try {
            _accounts.value = withContext(Dispatchers.IO) { w.listAccounts() }
            // Successful refresh clears any stale transient banner.
            _refreshError.value = null
        } catch (e: FfiException.Auth) {
            // Session genuinely invalid — re-auth required. Non-destructive
            // (no on-disk wipe here; the cold-start restore path handles a
            // stale blob), routes to the login screen rather than the
            // dead-end ErrorView.
            _accounts.value = emptyList()
            _user.value = null
            _refreshError.value = null
            _state.value = BootState.Ready(Phase.SignedOut)
            return
        } catch (e: FfiException) {
            if (isBootstrap || !isSignedIn()) {
                _state.value =
                    BootState.Ready(Phase.Error("list accounts failed: ${ffiErrorMessage(e)}"))
            } else {
                // Transient blip while signed in: keep last-known balance,
                // soft banner, self-heal on the next realtime
                // (re)connect / broadcast or on-resume.
                _refreshError.value = "Couldn't refresh balance: ${ffiErrorMessage(e)}"
            }
            return
        } catch (e: Throwable) {
            if (isBootstrap || !isSignedIn()) {
                _state.value = BootState.Ready(Phase.Error("unexpected: ${e.message}"))
            } else {
                _refreshError.value = "Couldn't refresh balance: ${e.message}"
            }
            return
        }

        // Refresh the user row so per-currency default-account ids are
        // current. Failure is non-fatal — see iOS WalletViewModel for
        // rationale (brand-new guests don't have a user row yet, that's
        // expected).
        try {
            _user.value = withContext(Dispatchers.IO) { w.getUser() }
        } catch (e: FfiException) {
            if (e is FfiException.Internal &&
                (e.message ?: "").contains("user row not found")
            ) {
                _user.value = null
            } else {
                // Other failures: leave user as-is, accounts list is
                // still usable.
            }
        } catch (_: Throwable) {
            // Same conservative handling.
        }
    }

    /**
     * Outcome shape for the Add Mint sheet. Success carries the FFI
     * `MintAddResult` so the sheet can render mint name/URL inline;
     * failure carries a presentation-ready string.
     */
    sealed interface AddMintOutcome {
        data class Success(val result: MintAddResult) : AddMintOutcome
        data class Failure(val message: String) : AddMintOutcome
    }

    /**
     * Provision a new Cashu mint. Mirrors `addMint` on iOS:
     * trims+validates the URL, calls the FFI, refreshes the accounts
     * list on success so the Accounts screen reflects the new row
     * without a pull-to-refresh.
     */
    suspend fun addMint(url: String): AddMintOutcome {
        val trimmed = url.trim()
        if (trimmed.isEmpty()) return AddMintOutcome.Failure("Enter a mint URL first.")
        val w = wallet ?: return AddMintOutcome.Failure("Wallet not ready.")
        _isWorking.value = true
        try {
            val result = withContext(Dispatchers.IO) { w.mintAdd(trimmed) }
            refreshAccounts()
            return AddMintOutcome.Success(result)
        } catch (e: FfiException) {
            return AddMintOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            return AddMintOutcome.Failure("unexpected: ${e.message}")
        } finally {
            _isWorking.value = false
        }
    }

    /**
     * Outcome shape for the swipe-to-default action on AccountsScreen.
     */
    sealed interface SetDefaultOutcome {
        data object Success : SetDefaultOutcome
        data class Failure(val message: String) : SetDefaultOutcome
    }

    /**
     * Mirror of `UserService.setDefaultAccount` on web. Calls the FFI
     * then refreshes accounts + user so the row reorders and the badge
     * moves without a separate pull-to-refresh. Does NOT flip
     * `default_currency` — see the FFI doc on `set_default_account`.
     */
    suspend fun setDefaultAccount(account: AccountFfi): SetDefaultOutcome {
        val w = wallet ?: return SetDefaultOutcome.Failure("Wallet not ready.")
        _isWorking.value = true
        try {
            val updated = withContext(Dispatchers.IO) { w.setDefaultAccount(account.id) }
            _user.value = updated
            refreshAccounts()
            return SetDefaultOutcome.Success
        } catch (e: FfiException) {
            return SetDefaultOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            return SetDefaultOutcome.Failure("unexpected: ${e.message}")
        } finally {
            _isWorking.value = false
        }
    }

    /**
     * True when the given account is the user's default for its
     * currency. Mirrors iOS `isDefault(_:)`. Returns false (no badge)
     * when the user row hasn't loaded yet, or when the account's
     * currency has no default slot (e.g., USDB).
     */
    fun isDefault(account: AccountFfi): Boolean {
        val u = _user.value ?: return false
        return when (account.currency) {
            "BTC" -> account.id == u.defaultBtcAccountId
            "USD" -> account.id == u.defaultUsdAccountId
            else -> false
        }
    }

    /**
     * `accounts` sorted so the default-for-its-currency row sits on
     * top. Mirrors `sortedAccounts` on iOS.
     */
    fun sortedAccounts(): List<AccountFfi> {
        val list = _accounts.value
        return list.sortedWith(Comparator { lhs, rhs ->
            val l = isDefault(lhs)
            val r = isDefault(rhs)
            when {
                l && !r -> -1
                !l && r -> 1
                else -> 0
            }
        })
    }

    // ---- Receive: Cashu token redeem (NUT-03) ----

    /**
     * Outcome shape returned to the receive screens. Mirrors iOS
     * `WalletViewModel.ReceiveOutcome`: success carries the FFI
     * `ReceiveResult` so the view renders amount/mint/status directly;
     * failure carries a presentation-ready string already mapped
     * through [ffiErrorMessage]. Shared by the Cashu-token paste flow
     * and the Lightning-receive `complete` step (same as iOS).
     */
    sealed interface ReceiveOutcome {
        data class Success(val result: ReceiveResult) : ReceiveOutcome
        data class Failure(val message: String) : ReceiveOutcome
    }

    /**
     * Redeem a Cashu token. Mirrors iOS `receive(token:)`: trims +
     * validates, calls the FFI on `Dispatchers.IO`, refreshes the
     * accounts list on success so Home's balance reflects the new
     * proofs without a pull-to-refresh. Intentionally does NOT mutate
     * the boot phase on failure — receive failures stay inside the
     * receive sheet and are rendered inline by the caller.
     */
    suspend fun receive(token: String): ReceiveOutcome {
        val trimmed = token.trim()
        if (trimmed.isEmpty()) return ReceiveOutcome.Failure("Paste a Cashu token first.")
        val w = wallet ?: return ReceiveOutcome.Failure("Wallet not ready.")
        _isWorking.value = true
        try {
            val result = withContext(Dispatchers.IO) { w.receiveToken(trimmed) }
            refreshAccounts()
            return ReceiveOutcome.Success(result)
        } catch (e: FfiException) {
            return ReceiveOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            return ReceiveOutcome.Failure("unexpected: ${e.message}")
        } finally {
            _isWorking.value = false
        }
    }

    // ---- Receive: Lightning (NUT-04 mint quote) ----

    /** Mirrors iOS `LightningQuoteOutcome`. */
    sealed interface LightningQuoteOutcome {
        data class Success(val handle: MintQuoteHandle) : LightningQuoteOutcome
        data class Failure(val message: String) : LightningQuoteOutcome
    }

    /** Mirrors iOS `LightningPollOutcome`. */
    sealed interface LightningPollOutcome {
        data class State(
            val state: MintQuoteFfiState,
            val failureReason: String?,
        ) : LightningPollOutcome
        data class Failure(val message: String) : LightningPollOutcome
    }

    /**
     * Request a BOLT-11 invoice from the user's default Cashu mint.
     * Mirrors iOS `startLightningQuote`. Does NOT flip [isWorking] —
     * the receive view owns a richer entry→generating→invoice→done
     * state machine and renders its own localized spinner.
     */
    suspend fun startLightningQuote(
        amount: ULong,
        accountId: String? = null,
        currency: String? = null,
    ): LightningQuoteOutcome {
        val w = wallet ?: return LightningQuoteOutcome.Failure("Wallet not ready.")
        return try {
            val handle = withContext(Dispatchers.IO) {
                w.startMintQuote(amount, accountId, currency)
            }
            LightningQuoteOutcome.Success(handle)
        } catch (e: FfiException) {
            LightningQuoteOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            LightningQuoteOutcome.Failure("unexpected: ${e.message}")
        }
    }

    /**
     * Single-shot poll for the mint-quote state. Mirrors iOS
     * `pollLightningQuote` — the view owns the cadence and cancels the
     * loop on disappear / terminal transition.
     */
    suspend fun pollLightningQuote(quoteId: String): LightningPollOutcome {
        val w = wallet ?: return LightningPollOutcome.Failure("Wallet not ready.")
        return try {
            val snap = withContext(Dispatchers.IO) { w.pollMintQuote(quoteId) }
            LightningPollOutcome.State(snap.state, snap.failureReason)
        } catch (e: FfiException) {
            LightningPollOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            LightningPollOutcome.Failure("unexpected: ${e.message}")
        }
    }

    /**
     * Drive a PAID quote to COMPLETED — mints proofs + credits the
     * account, then refreshes accounts so Home reflects the credit.
     * Mirrors iOS `completeLightningQuote`; returns the shared
     * [ReceiveOutcome] so the success card can reuse the Cashu-token
     * receive primitives.
     */
    suspend fun completeLightningQuote(quoteId: String): ReceiveOutcome {
        val w = wallet ?: return ReceiveOutcome.Failure("Wallet not ready.")
        return try {
            val result = withContext(Dispatchers.IO) { w.completeMintQuote(quoteId) }
            refreshAccounts()
            ReceiveOutcome.Success(result)
        } catch (e: FfiException) {
            ReceiveOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            ReceiveOutcome.Failure("unexpected: ${e.message}")
        }
    }

    // ---- Send: Cashu send-swap (NUT-03) ----

    /** Mirrors iOS `SendQuoteOutcome`. */
    sealed interface SendQuoteOutcome {
        data class Success(val quote: SendQuotePreview) : SendQuoteOutcome
        data class Failure(val message: String) : SendQuoteOutcome
    }

    /** Mirrors iOS `SendOutcome`. */
    sealed interface SendOutcome {
        data class Success(val handle: SendSwapHandle) : SendOutcome
        data class Failure(val message: String) : SendOutcome
    }

    /** Mirrors iOS `SendClaimOutcome`. */
    sealed interface SendClaimOutcome {
        data class State(
            val state: SendSwapClaimState,
            val failureReason: String?,
        ) : SendClaimOutcome
        data class Failure(val message: String) : SendClaimOutcome
    }

    /**
     * Preview the fee + total for a send. Mirrors iOS `prepareSend`;
     * does NOT flip [isWorking] — the send view owns its phase machine
     * and renders a localized spinner for the brief quote round-trip.
     */
    suspend fun prepareSend(
        amount: ULong,
        accountId: String? = null,
        currency: String? = null,
    ): SendQuoteOutcome {
        val w = wallet ?: return SendQuoteOutcome.Failure("Wallet not ready.")
        return try {
            val quote = withContext(Dispatchers.IO) {
                w.prepareSendQuote(amount, accountId, currency)
            }
            SendQuoteOutcome.Success(quote)
        } catch (e: FfiException) {
            SendQuoteOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            SendQuoteOutcome.Failure("unexpected: ${e.message}")
        }
    }

    /**
     * Commit a send — runs the input swap and produces a wire-form V4
     * token. Refreshes accounts on success so Home reflects the debit.
     * Mirrors iOS `createSend`.
     */
    suspend fun createSend(
        amount: ULong,
        accountId: String? = null,
        currency: String? = null,
    ): SendOutcome {
        val w = wallet ?: return SendOutcome.Failure("Wallet not ready.")
        return try {
            val handle = withContext(Dispatchers.IO) {
                w.createSendSwap(amount, accountId, currency)
            }
            refreshAccounts()
            SendOutcome.Success(handle)
        } catch (e: FfiException) {
            SendOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            SendOutcome.Failure("unexpected: ${e.message}")
        }
    }

    /**
     * Single-shot "has the receiver claimed?" poll. Mirrors iOS
     * `pollSendClaim` — the share view owns the ~3s cadence and the
     * cancel-on-disappear lifecycle.
     */
    suspend fun pollSendClaim(swapId: String): SendClaimOutcome {
        val w = wallet ?: return SendClaimOutcome.Failure("Wallet not ready.")
        return try {
            val snap = withContext(Dispatchers.IO) { w.checkSendSwapClaimed(swapId) }
            SendClaimOutcome.State(snap.state, snap.failureReason)
        } catch (e: FfiException) {
            SendClaimOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            SendClaimOutcome.Failure("unexpected: ${e.message}")
        }
    }

    // ---- Send: Lightning (NUT-05 melt quote) ----

    /** Mirrors iOS `MeltQuoteOutcome`. */
    sealed interface MeltQuoteOutcome {
        data class Success(val preview: MeltQuotePreview) : MeltQuoteOutcome
        data class Failure(val message: String) : MeltQuoteOutcome
    }

    /** Mirrors iOS `MeltCreateOutcome`. */
    sealed interface MeltCreateOutcome {
        data class Success(val handle: MeltQuoteHandle) : MeltCreateOutcome
        data class Failure(val message: String) : MeltCreateOutcome
    }

    /** Mirrors iOS `MeltStatusOutcome`. */
    sealed interface MeltStatusOutcome {
        data class State(
            val state: MeltQuoteFfiState,
            val snapshot: MeltQuoteSnapshot,
        ) : MeltStatusOutcome
        data class Failure(val message: String) : MeltStatusOutcome
    }

    /**
     * Preview the fee + total for a Lightning send. Mirrors iOS
     * `prepareMeltQuote`; does NOT flip [isWorking].
     */
    suspend fun prepareMeltQuote(
        bolt11: String,
        accountId: String? = null,
        currency: String? = null,
    ): MeltQuoteOutcome {
        val w = wallet ?: return MeltQuoteOutcome.Failure("Wallet not ready.")
        return try {
            val preview = withContext(Dispatchers.IO) {
                w.prepareMeltQuote(bolt11, accountId, currency)
            }
            MeltQuoteOutcome.Success(preview)
        } catch (e: FfiException) {
            MeltQuoteOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            MeltQuoteOutcome.Failure("unexpected: ${e.message}")
        }
    }

    /**
     * Persist the UNPAID melt quote + reserve proofs. Mirrors iOS
     * `createMeltQuote`. Does NOT refresh accounts — the debit isn't
     * final until the melt settles (PAID); the poll refreshes on the
     * terminal transition.
     */
    suspend fun createMeltQuote(
        bolt11: String,
        accountId: String? = null,
        currency: String? = null,
    ): MeltCreateOutcome {
        val w = wallet ?: return MeltCreateOutcome.Failure("Wallet not ready.")
        return try {
            val handle = withContext(Dispatchers.IO) {
                w.createMeltQuote(bolt11, accountId, currency)
            }
            MeltCreateOutcome.Success(handle)
        } catch (e: FfiException) {
            MeltCreateOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            MeltCreateOutcome.Failure("unexpected: ${e.message}")
        }
    }

    /**
     * Fire NUT-05 `post_melt` (UNPAID → PENDING/terminal). Refreshes
     * accounts on the PAID transition so Home reflects the debit.
     * Mirrors iOS `executeMeltQuote`.
     */
    suspend fun executeMeltQuote(quoteId: String): MeltStatusOutcome {
        val w = wallet ?: return MeltStatusOutcome.Failure("Wallet not ready.")
        return try {
            val snap = withContext(Dispatchers.IO) { w.executeMeltQuote(quoteId) }
            if (snap.state == MeltQuoteFfiState.PAID) refreshAccounts()
            MeltStatusOutcome.State(snap.state, snap)
        } catch (e: FfiException) {
            MeltStatusOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            MeltStatusOutcome.Failure("unexpected: ${e.message}")
        }
    }

    /**
     * Single-shot poll for a PENDING melt quote. Refreshes accounts on
     * the PAID transition. Mirrors iOS `pollMeltQuote` — the view owns
     * the ~2s cadence + cancel-on-disappear.
     */
    suspend fun pollMeltQuote(quoteId: String): MeltStatusOutcome {
        val w = wallet ?: return MeltStatusOutcome.Failure("Wallet not ready.")
        return try {
            val snap = withContext(Dispatchers.IO) { w.pollMeltQuote(quoteId) }
            if (snap.state == MeltQuoteFfiState.PAID) refreshAccounts()
            MeltStatusOutcome.State(snap.state, snap)
        } catch (e: FfiException) {
            MeltStatusOutcome.Failure(ffiErrorMessage(e))
        } catch (e: Throwable) {
            MeltStatusOutcome.Failure("unexpected: ${e.message}")
        }
    }

    // ---- Send: Lightning Address (LUD-16) resolution ----

    /** Mirrors iOS `LnAddressInvoiceOutcome`. */
    sealed interface LnAddressInvoiceOutcome {
        data class Success(
            val invoice: String,
            val amountSats: ULong,
        ) : LnAddressInvoiceOutcome
        data class Failure(val message: String) : LnAddressInvoiceOutcome
    }

    /**
     * Resolve a Lightning Address and request a BOLT-11 invoice for
     * `amountSats`. Two network round-trips (well-known lookup + the
     * LUD-06 callback) wrapped behind one call so the view's state
     * machine stays simple. Mirrors iOS `resolveLnAddressInvoice`. The
     * LUD-16 FFI is wallet-agnostic (module-level free functions); this
     * adapts its `LightningAddressException` shape to the same string
     * convention the rest of the VM uses (per-variant guidance mirrors
     * the iOS `lnAddressErrorMessage`).
     */
    suspend fun resolveLnAddressInvoice(
        address: String,
        amountSats: ULong,
        comment: String? = null,
    ): LnAddressInvoiceOutcome {
        val trimmed = address.trim().lowercase()
        if (trimmed.isEmpty()) {
            return LnAddressInvoiceOutcome.Failure("Enter a Lightning Address first.")
        }
        return try {
            val info = withContext(Dispatchers.IO) { resolveLightningAddress(trimmed) }
            val invoice = withContext(Dispatchers.IO) {
                requestLightningInvoice(info, amountSats * 1000u, comment)
            }
            LnAddressInvoiceOutcome.Success(invoice, amountSats)
        } catch (e: LightningAddressException) {
            LnAddressInvoiceOutcome.Failure(lnAddressErrorMessage(e))
        } catch (e: Throwable) {
            LnAddressInvoiceOutcome.Failure("unexpected: ${e.message}")
        }
    }

    /**
     * Map the LUD-16 FFI error to a user-readable string. Mirrors the
     * per-variant guidance documented on the Rust `LightningAddressError`
     * (kept consistent with iOS `lnAddressErrorMessage`).
     * `AmountOutOfRange` renders the sat bounds (msat / 1000) inline.
     */
    private fun lnAddressErrorMessage(e: LightningAddressException): String = when (e) {
        is LightningAddressException.InvalidAddress ->
            "That doesn't look like a Lightning Address."
        is LightningAddressException.Network ->
            "Couldn't reach the recipient's server. Try again."
        is LightningAddressException.InvalidResponse ->
            "The recipient's server returned an unexpected response."
        is LightningAddressException.AmountOutOfRange ->
            "Amount must be between ${e.min / 1000u} and ${e.max / 1000u} sats."
        is LightningAddressException.ServerException -> e.message ?: "Server error."
        else -> e.message ?: "Lightning Address error."
    }

    private fun ffiErrorMessage(e: FfiException): String = when (e) {
        is FfiException.Auth -> "auth/${e.code}: ${e.message}"
        is FfiException.Storage -> "storage/${e.code}: ${e.message}"
        is FfiException.Internal -> e.message ?: "internal error"
    }

    /**
     * The Rust realtime supervisor runs on a detached tokio task owned
     * by the [AgicashWallet]; a `tokio::spawn` handle does NOT abort on
     * drop, so if this ViewModel is cleared while still signed in
     * (`viewModelScope` is already cancelled by the time `onCleared`
     * runs), that task would outlive the UI it feeds. Drain it on a
     * process-lifetime scope so the supervisor + its listener `Arc` are
     * released. Best-effort and fire-and-forget — process teardown
     * doesn't wait on it, and the FFI stop is an idempotent no-op when
     * nothing is running.
     */
    @OptIn(kotlinx.coroutines.DelicateCoroutinesApi::class)
    override fun onCleared() {
        super.onCleared()
        if (!realtimeStarted) return
        val w = wallet ?: return
        realtimeStarted = false
        kotlinx.coroutines.GlobalScope.launch(Dispatchers.IO) {
            try {
                w.stopWalletEvents()
            } catch (e: Throwable) {
                android.util.Log.w(
                    "WalletViewModel",
                    "stopWalletEvents on onCleared failed (ignored): ${e.message}",
                )
            }
        }
    }
}
