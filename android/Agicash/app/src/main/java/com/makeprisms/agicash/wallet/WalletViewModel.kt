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
import uniffi.agicash_ffi.MintAddResult
import uniffi.agicash_ffi.Session
import uniffi.agicash_ffi.UserFfi

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
     * [isWorking] (auth/mint/send mutations) so a background poll or an
     * on-resume refresh never spins the pull indicator. iOS gets this for
     * free from SwiftUI's `.refreshable` awaiting the async closure; on
     * Android we surface it explicitly because the screen needs a
     * `isRefreshing` boolean.
     */
    private val _isRefreshing = MutableStateFlow(false)
    val isRefreshing: StateFlow<Boolean> = _isRefreshing.asStateFlow()

    private val _loginErrorMessage = MutableStateFlow<String?>(null)
    val loginErrorMessage: StateFlow<String?> = _loginErrorMessage.asStateFlow()

    private var wallet: AgicashWallet? = null

    init {
        bootstrap()
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
                refreshAccounts()
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
            try {
                withContext(Dispatchers.IO) { w.authLogout() }
            } catch (_: Throwable) {
                // Best-effort; clear local state regardless.
            }
            _accounts.value = emptyList()
            _user.value = null
            _loginErrorMessage.value = null
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
     * The lifecycle-aware on-resume refresh and the foreground poll on
     * HomeScreen call this too. Pull-to-refresh uses
     * [refreshAccountsFromPull] instead so it can flip [isRefreshing] for
     * the Material3 spinner.
     */
    fun refreshAccounts() {
        viewModelScope.launch { refreshAccountsSuspending() }
    }

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
     * The actual refresh, suspending so callers (poll loop, pull-to-
     * refresh, on-resume) can sequence around it. Identical semantics to
     * the previous fire-and-forget body and to iOS `refreshAccounts()`:
     * a hard `list_accounts()` failure escalates to the error phase; a
     * `get_user()` failure is non-fatal (brand-new guests have no user
     * row yet — expected).
     */
    private suspend fun refreshAccountsSuspending() {
        val w = wallet ?: return
        try {
            _accounts.value = withContext(Dispatchers.IO) { w.listAccounts() }
        } catch (e: FfiException) {
            _state.value = BootState.Ready(Phase.Error("list accounts failed: ${ffiErrorMessage(e)}"))
            return
        } catch (e: Throwable) {
            _state.value = BootState.Ready(Phase.Error("unexpected: ${e.message}"))
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

    private fun ffiErrorMessage(e: FfiException): String = when (e) {
        is FfiException.Auth -> "auth/${e.code}: ${e.message}"
        is FfiException.Storage -> "storage/${e.code}: ${e.message}"
        is FfiException.Internal -> e.message ?: "internal error"
    }
}
