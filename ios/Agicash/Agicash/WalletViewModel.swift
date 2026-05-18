import Foundation
import Observation

/// Phase 1 wallet view model. Holds the `AgicashWallet` UniFFI handle, an
/// auth phase, and the cached accounts list. SwiftUI observes the @Observable
/// state and rerenders on every change.
///
/// Phase 1 talks to local OpenSecret + local Supabase (see `Endpoints`).
/// Endpoint overrides land in Phase 2+ as the app gets a settings UI.
@MainActor
@Observable
final class WalletViewModel {
    enum Phase: Equatable {
        /// Bootstrapping: deciding whether to show the sign-in screen or the
        /// accounts list. We start here at app launch while we attempt to
        /// rehydrate a Keychain-stored session.
        case checking
        case signedOut
        case signedIn(userId: String)
        case error(String)
    }

    /// Hard-coded Phase 1 dev endpoints. The iOS simulator inherits the
    /// host's network namespace, so `127.0.0.1` reaches whatever the
    /// developer is running locally — the enclave on port 3999 and the
    /// supabase stack on 54321.
    enum Endpoints {
        static let opensecretURL = "http://127.0.0.1:3999"
        static let opensecretClientID = "ba5a14b5-d915-47b1-b7b1-afda52bc5fc6"
        static let supabaseURL = "https://127.0.0.1:54321"
        // Local supabase publishable anon key (same one used by the JS app +
        // the integration tests; not a secret).
        static let supabaseAnonKey =
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJpc3MiOiJzdXBhYmFzZS1kZW1vIiwicm9sZSI6ImFub24iLCJleHAiOjE5ODM4MTI5OTZ9.CRXP1A7WOeoJeXxjNni43kdQwgnWNReilDMblYTn_I0"
    }

    var phase: Phase = .checking
    var accounts: [AccountFfi] = []
    /// User row, including the per-currency default-account slots
    /// (`defaultBtcAccountId`, `defaultUsdAccountId`) the web exposes via
    /// `useUser` / `useDefaultAccount`. Fetched alongside `accounts` from
    /// `refreshAccounts`; left `nil` until the first refresh, or when the
    /// user row hasn't been created yet (brand-new guest before any mint
    /// add — the row only exists after the first
    /// `upsertUserWithAccounts` call).
    ///
    /// UI consumers should treat `nil` as "no defaults known": no
    /// "Default" badge renders for any row, and the swipe action stays
    /// available on every row.
    var user: UserFfi?
    var isWorking = false
    /// Inline message shown on the login screen when an interactive sign-in
    /// attempt fails. Cleared on the next attempt. Distinct from `phase ==
    /// .error(...)` which represents a fatal/unexpected error blocking the
    /// whole app.
    var loginErrorMessage: String?
    /// Debug-only marker set by the `-AgicashDemoSignedIn` launch flag in
    /// `AgicashApp`. When true, `refreshAccounts` and `signOut` short-circuit
    /// so the seeded mock accounts stay visible (the FFI calls would
    /// otherwise hit Supabase and either replace the mocks with empty data
    /// or surface a network error). Production builds never set this.
    var isDemoMode: Bool = false

    private let wallet: AgicashWallet

    init() throws {
        self.wallet = try AgicashWallet(
            opensecretUrl: Endpoints.opensecretURL,
            opensecretClientIdUuid: Endpoints.opensecretClientID,
            supabaseUrl: Endpoints.supabaseURL,
            supabaseAnonKey: Endpoints.supabaseAnonKey
        )
    }

    /// Attempt to rehydrate a Keychain session. Called on app launch.
    func bootstrap() async {
        do {
            guard let stored = try SessionStore.load() else {
                phase = .signedOut
                return
            }
            try await wallet.setSession(
                userIdUuid: stored.userId,
                refreshToken: stored.refreshToken
            )
            phase = .signedIn(userId: stored.userId)
            await refreshAccounts()
        } catch let err as SessionStoreError {
            phase = .error("session load failed: \(err)")
        } catch let err as FfiError {
            // Rehydration failed (refresh token rejected). Drop the
            // Keychain copy so we don't keep retrying on every launch.
            try? SessionStore.clear()
            phase = .signedOut
            _ = err
        } catch {
            phase = .error("unexpected: \(error)")
        }
    }

    func signInAsGuest() async {
        await runSignIn { try await self.wallet.authGuest() }
    }

    func signInWithEmail(email: String, password: String) async {
        let trimmedEmail = email.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedEmail.isEmpty, !password.isEmpty else {
            loginErrorMessage = "Email and password are required."
            return
        }
        await runSignIn {
            try await self.wallet.authLogin(
                email: trimmedEmail, password: password
            )
        }
    }

    /// Register a new email + password account against OpenSecret. Mirrors
    /// the web `/signup` flow: on success the user is auto-signed-in and
    /// routed into the wallet (the FFI returns the same `Session` shape as
    /// `authLogin`). The web form requires confirm-password and an
    /// 8-character minimum; we enforce both here so the FFI never sees a
    /// mismatched pair. `name` is intentionally not collected — the web
    /// doesn't either, and the FFI accepts `nil`.
    func signUpWithEmail(email: String, password: String, confirmPassword: String) async {
        let trimmedEmail = email.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmedEmail.isEmpty, !password.isEmpty else {
            loginErrorMessage = "Email and password are required."
            return
        }
        guard password.count >= 8 else {
            loginErrorMessage = "Password must have at least 8 characters."
            return
        }
        guard password == confirmPassword else {
            loginErrorMessage = "Passwords do not match."
            return
        }
        await runSignIn {
            try await self.wallet.authSignup(
                email: trimmedEmail, password: password, name: nil
            )
        }
    }

    private func runSignIn(_ call: @escaping () async throws -> Session) async {
        isWorking = true
        loginErrorMessage = nil
        defer { isWorking = false }
        do {
            let session = try await call()
            try SessionStore.save(
                PersistedSession(
                    userId: session.userId,
                    refreshToken: session.refreshToken
                )
            )
            phase = .signedIn(userId: session.userId)
            await refreshAccounts()
        } catch let err as FfiError {
            loginErrorMessage = ffiErrorMessage(err)
        } catch let err as SessionStoreError {
            loginErrorMessage = "session save failed: \(err)"
        } catch {
            loginErrorMessage = "unexpected: \(error)"
        }
    }

    func signOut() async {
        isWorking = true
        defer { isWorking = false }
        if isDemoMode {
            accounts = []
            user = nil
            loginErrorMessage = nil
            isDemoMode = false
            phase = .signedOut
            return
        }
        do {
            try await wallet.authLogout()
        } catch {
            // Best-effort; clear local state regardless.
        }
        try? SessionStore.clear()
        accounts = []
        user = nil
        loginErrorMessage = nil
        phase = .signedOut
    }

    /// Outcome shape returned to `ReceiveView`. Success carries the FFI
    /// `ReceiveResult` so the view can render amount/mint/etc. directly;
    /// failure carries a presentation-ready string already mapped through
    /// `ffiErrorMessage` so the view doesn't need to know about FFI shapes.
    enum ReceiveOutcome {
        case success(ReceiveResult)
        case failure(String)
    }

    /// Redeem a Cashu token. Mirrors the auth methods' `runSignIn` shape:
    /// flips `isWorking` for the duration so the calling view can render a
    /// spinner, refreshes the accounts list on success so the home
    /// balance updates without an extra round-trip, and translates FFI
    /// errors into user-readable strings via the existing helper.
    ///
    /// Note: this method intentionally does NOT mutate `phase` on
    /// failure (that's what `runSignIn` does because failed sign-ins
    /// stay on the login screen). Receive failures stay inside the
    /// receive sheet — the caller renders them inline.
    func receive(token: String) async -> ReceiveOutcome {
        let trimmed = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return .failure("Paste a Cashu token first.")
        }
        isWorking = true
        defer { isWorking = false }
        do {
            let result = try await wallet.receiveToken(token: trimmed)
            // Refresh so Home's balance/accounts list reflects the new
            // proofs without forcing the user to pull-to-refresh.
            await refreshAccounts()
            return .success(result)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// Outcome shape for `startLightningQuote`. Success carries the FFI
    /// handle (BOLT-11, quote_id, amount, fee, expires_at) so the
    /// LightningReceiveView can render the QR + breakdown directly;
    /// failure carries a presentation-ready string already mapped
    /// through `ffiErrorMessage` so the view doesn't need to know FFI
    /// shapes.
    enum LightningQuoteOutcome {
        case success(MintQuoteHandle)
        case failure(String)
    }

    /// Outcome shape for `pollLightningQuote`. Mirrors `MintQuoteSnapshot`
    /// plus the failure branch. The view loops on this until the state
    /// transitions out of `.unpaid`.
    enum LightningPollOutcome {
        case state(MintQuoteFfiState, failureReason: String?)
        case failure(String)
    }

    /// Outcome shape returned to `AddMintView`. Mirrors `ReceiveOutcome`:
    /// success carries the FFI `MintAddResult` so the sheet can show the
    /// new mint's name/URL inline; failure carries a presentation-ready
    /// string already mapped through `ffiErrorMessage`.
    enum AddMintOutcome {
        case success(MintAddResult)
        case failure(String)
    }

    /// Provision a new Cashu mint and refresh the accounts list. Mirrors
    /// the web `AddMintForm` (`app/features/settings/accounts/add-mint-form.tsx`):
    /// the form trims and submits the URL, the wallet talks NUT-06 to the
    /// mint, the new `wallet.accounts` row gets inserted, and the local
    /// cache refreshes so the Accounts screen reflects the new row without
    /// a pull-to-refresh.
    ///
    /// Empty / whitespace-only URLs short-circuit with a friendly inline
    /// error so the FFI never sees a blank string. All other errors funnel
    /// through `FfiError` and surface as a single user-readable message.
    func addMint(url: String) async -> AddMintOutcome {
        let trimmed = url.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            return .failure("Enter a mint URL first.")
        }
        isWorking = true
        defer { isWorking = false }
        do {
            let result = try await wallet.mintAdd(url: trimmed)
            await refreshAccounts()
            return .success(result)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    // MARK: - Cashu send (NUT-03 send swap)

    /// Outcome shape for `prepareSend`. Success carries the FFI quote
    /// (amount/fee breakdown) so the confirm card can render it
    /// directly; failure carries a presentation-ready string already
    /// mapped through `ffiErrorMessage`.
    enum SendQuoteOutcome {
        case success(SendQuotePreview)
        case failure(String)
    }

    /// Outcome shape for `createSend`. Success carries the FFI handle
    /// (token + swap_id + amount); failure carries a presentation-ready
    /// error string.
    enum SendOutcome {
        case success(SendSwapHandle)
        case failure(String)
    }

    /// Outcome shape for `pollSendClaim`. Mirrors `SendSwapClaimSnapshot`
    /// plus a failure branch. The view loops on this until the state
    /// flips to `.completed` (or the user dismisses).
    enum SendClaimOutcome {
        case state(SendSwapClaimState, failureReason: String?)
        case failure(String)
    }

    /// Preview the fee + total for a send. Mirrors `startLightningQuote`
    /// in shape; does NOT flip `isWorking` for the same reason — the
    /// `SendCashuTokenView` owns its own phase machine and renders a
    /// localised spinner during the brief quote round-trip.
    func prepareSend(
        amount: UInt64,
        accountId: String? = nil,
        currency: String? = nil
    ) async -> SendQuoteOutcome {
        do {
            let quote = try await wallet.prepareSendQuote(
                amount: amount,
                accountId: accountId,
                currency: currency
            )
            return .success(quote)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// Commit a send — runs the input swap (if needed) and produces a
    /// wire-form V4 token. Refreshes the accounts list on success so
    /// Home's balance reflects the debit without a pull-to-refresh.
    /// Failure leaves the wallet untouched (the swap service rolls
    /// back the row on error).
    func createSend(
        amount: UInt64,
        accountId: String? = nil,
        currency: String? = nil
    ) async -> SendOutcome {
        do {
            let handle = try await wallet.createSendSwap(
                amount: amount,
                accountId: accountId,
                currency: currency
            )
            await refreshAccounts()
            return .success(handle)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// Single-shot poll for "has the receiver claimed?". Called from a
    /// long-running `Task` in `SendCashuTokenView.share` every ~3s
    /// while the share screen is on screen. The view owns cadence + the
    /// cancel-on-disappear lifecycle so this method stays a pure shot.
    func pollSendClaim(swapId: String) async -> SendClaimOutcome {
        do {
            let snapshot = try await wallet.checkSendSwapClaimed(swapId: swapId)
            return .state(snapshot.state, failureReason: snapshot.failureReason)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    // MARK: - Lightning receive (NUT-04 mint quote)

    /// Request a BOLT-11 invoice from the user's default Cashu BTC mint.
    /// Wraps `wallet.startMintQuote` — the FFI returns a handle carrying
    /// the invoice + wallet-side quote_id the view uses to drive the
    /// poll/complete cycle.
    ///
    /// `amount` is in the account's minor unit (sats for BTC). The view
    /// passes the parsed numpad value here; validation (>0, not too
    /// large) happens client-side before this is called, but the FFI
    /// also rejects 0 with a friendly error.
    ///
    /// Returns the handle on success or a presentation-ready error
    /// string on failure. Does NOT flip `isWorking` — the
    /// LightningReceiveView owns its own loading state because it has
    /// a richer state machine (entry → generating → invoice → done) and
    /// doesn't want to fight a global spinner.
    func startLightningQuote(
        amount: UInt64,
        accountId: String? = nil,
        currency: String? = nil
    ) async -> LightningQuoteOutcome {
        do {
            let handle = try await wallet.startMintQuote(
                amount: amount,
                accountId: accountId,
                currency: currency
            )
            return .success(handle)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// Poll the mint for the current state of a quote. Called from a
    /// long-running `Task` in the LightningReceiveView every ~2s while
    /// the quote is still UNPAID. Single-shot — no internal loop — so
    /// the view owns cadence and can cancel without holding a service
    /// reference.
    func pollLightningQuote(quoteId: String) async -> LightningPollOutcome {
        do {
            let snapshot = try await wallet.pollMintQuote(quoteId: quoteId)
            return .state(snapshot.state, failureReason: snapshot.failureReason)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// Drive a PAID quote to COMPLETED — mints proofs and credits the
    /// account. Refreshes the accounts list on success so Home's
    /// balance updates without an extra round-trip. Returns the same
    /// `ReceiveOutcome` shape as the Cashu-token receive so the
    /// LightningReceiveView's success card can be rendered with the
    /// shared `ReceiveResult` primitives.
    func completeLightningQuote(quoteId: String) async -> ReceiveOutcome {
        do {
            let result = try await wallet.completeMintQuote(quoteId: quoteId)
            await refreshAccounts()
            return .success(result)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    // MARK: - Lightning send (NUT-05 melt quote)

    /// Outcome shape for `prepareMeltQuote`. Success carries the FFI
    /// preview (amount + fee-reserve breakdown) so the confirm card can
    /// render it directly; failure carries a presentation-ready string
    /// already mapped through `ffiErrorMessage`. Mirrors
    /// `SendQuoteOutcome` on the Cashu side.
    enum MeltQuoteOutcome {
        case success(MeltQuotePreview)
        case failure(String)
    }

    /// Outcome shape for `createMeltQuote`. Success carries the FFI
    /// handle (quote_id + invoice + fee breakdown) the in-flight card
    /// drives the poll/execute cycle from; failure carries a
    /// presentation-ready error string.
    enum MeltCreateOutcome {
        case success(MeltQuoteHandle)
        case failure(String)
    }

    /// Outcome shape for `executeMeltQuote` / `pollMeltQuote`. Mirrors
    /// `MeltQuoteSnapshot` plus a failure branch. The view dispatches
    /// on the state (unpaid/pending/paid/expired/failed) to drive its
    /// own phase machine. Mirrors `LightningPollOutcome` on the
    /// receive side.
    enum MeltStatusOutcome {
        case state(MeltQuoteFfiState, snapshot: MeltQuoteSnapshot)
        case failure(String)
    }

    /// Preview the fee + total for a Lightning send. Mirrors
    /// `prepareSend` in shape; does NOT flip `isWorking` for the same
    /// reason — the Lightning-send view owns its own phase machine and
    /// renders a localised spinner during the brief quote round-trip.
    func prepareMeltQuote(
        bolt11: String,
        accountId: String? = nil,
        currency: String? = nil
    ) async -> MeltQuoteOutcome {
        do {
            let preview = try await wallet.prepareMeltQuote(
                bolt11: bolt11,
                accountId: accountId,
                currency: currency
            )
            return .success(preview)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// Persist the UNPAID melt quote + reserve proofs. Returns the
    /// handle the view uses to drive `executeMeltQuote` then the poll
    /// loop. Does NOT refresh accounts here — the debit isn't final
    /// until the melt settles (PAID); `pollMeltQuote` refreshes on the
    /// terminal transition.
    func createMeltQuote(
        bolt11: String,
        accountId: String? = nil,
        currency: String? = nil
    ) async -> MeltCreateOutcome {
        do {
            let handle = try await wallet.createMeltQuote(
                bolt11: bolt11,
                accountId: accountId,
                currency: currency
            )
            return .success(handle)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// Fire NUT-05 `post_melt` for a created quote (UNPAID → PENDING).
    /// One mint round-trip — returns a terminal snapshot (PAID/FAILED)
    /// or PENDING (the view then drives `pollMeltQuote`). Refreshes
    /// accounts on the PAID transition so Home reflects the debit
    /// without a pull-to-refresh.
    func executeMeltQuote(quoteId: String) async -> MeltStatusOutcome {
        do {
            let snapshot = try await wallet.executeMeltQuote(quoteId: quoteId)
            if snapshot.state == .paid {
                await refreshAccounts()
            }
            return .state(snapshot.state, snapshot: snapshot)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// Single-shot poll for a PENDING melt quote. Called from a
    /// long-running `Task` in the Lightning-send view every ~2s while
    /// the payment is in flight. The view owns cadence + the
    /// cancel-on-disappear lifecycle so this stays a pure shot.
    /// Refreshes accounts on the PAID transition.
    func pollMeltQuote(quoteId: String) async -> MeltStatusOutcome {
        do {
            let snapshot = try await wallet.pollMeltQuote(quoteId: quoteId)
            if snapshot.state == .paid {
                await refreshAccounts()
            }
            return .state(snapshot.state, snapshot: snapshot)
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    // MARK: - Lightning Address (LUD-16) resolution

    /// Outcome for the LN-address → bolt11 resolve step. Success
    /// carries the resolved invoice string the view feeds straight
    /// into the melt flow (`prepareMeltQuote`). Failure carries a
    /// presentation-ready string. The LUD-16 FFI is wallet-agnostic
    /// (module-level free functions) so this just adapts its error
    /// shape to the same string convention the rest of the VM uses.
    enum LnAddressInvoiceOutcome {
        case success(invoice: String, amountSats: UInt64)
        case failure(String)
    }

    /// Resolve a Lightning Address and request a BOLT-11 invoice for
    /// `amountSats`. Two network round-trips (well-known lookup, then
    /// the LUD-06 callback) wrapped behind one call so the view's
    /// state machine stays simple. `amountSats` is bounds-checked
    /// against the server's advertised min/max on the Rust side; the
    /// `AmountOutOfRange` case is surfaced with its sat bounds so the
    /// view can render "minimum N sats".
    func resolveLnAddressInvoice(
        address: String,
        amountSats: UInt64,
        comment: String? = nil
    ) async -> LnAddressInvoiceOutcome {
        let trimmed = address.trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
        guard !trimmed.isEmpty else {
            return .failure("Enter a Lightning Address first.")
        }
        do {
            let info = try await resolveLightningAddress(address: trimmed)
            let invoice = try await requestLightningInvoice(
                info: info,
                amountMsat: amountSats * 1000,
                comment: comment
            )
            return .success(invoice: invoice, amountSats: amountSats)
        } catch let err as LightningAddressError {
            return .failure(lnAddressErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// Map the LUD-16 FFI error enum to a user-readable string. Mirrors
    /// the per-variant UI guidance documented on the Rust
    /// `LightningAddressError` (so the copy stays consistent with the
    /// FFI's own doc contract). `amountOutOfRange` renders the sat
    /// bounds (msat / 1000) inline.
    private func lnAddressErrorMessage(_ err: LightningAddressError) -> String {
        switch err {
        case .InvalidAddress:
            return "That doesn't look like a Lightning Address."
        case .Network:
            return "Couldn't reach the recipient's server. Try again."
        case .InvalidResponse:
            return "The recipient's server returned an unexpected response."
        case .AmountOutOfRange(let amountMsat, let min, let max):
            _ = amountMsat
            return "Amount must be between \(min / 1000) and \(max / 1000) sats."
        case .ServerError(let message):
            return message
        }
    }

    /// Stable code for `FfiError.Auth` meaning "session genuinely dead, the
    /// user must re-authenticate" — the Rust `auth_code::UNAUTHENTICATED`
    /// source of truth in `crates/agicash-ffi/src/error.rs`. UniFFI only
    /// exports the `Auth(code:message:)` shape, not the `auth_code` module,
    /// so we mirror the integer here (same magic-value style as the
    /// `"user row not found"` match below). Every other `Auth` code
    /// (`NETWORK` = 1, `BACKEND` = 3, `INTERNAL` = 4) is a transient blip,
    /// not an expired session.
    private static let authCodeUnauthenticated: UInt32 = 2

    /// - Parameter background: `true` when called from the unattended Home
    ///   foreground poll loop (and the scene-foreground re-sync), where a
    ///   send/receive sheet and an in-flight payment may be presented. On
    ///   that route a transient `listAccounts` failure must NOT escalate to
    ///   `phase = .error`, because `AuthGateView` reacts to `.error` by
    ///   tearing down the entire signed-in UI — sheet and in-flight payment
    ///   included — over what is usually one dropped packet. Only a genuine
    ///   auth-expiry (`FfiError.Auth` / `UNAUTHENTICATED`) still escalates
    ///   on this route; everything else is swallowed (stale `accounts` stay
    ///   on screen, the next poll re-syncs). Defaults to `false` so the
    ///   bootstrap / interactive sign-in / explicit pull-to-refresh /
    ///   post-payment callers keep their existing first-class error UX.
    func refreshAccounts(background: Bool = false) async {
        if isDemoMode { return }
        do {
            let list = try await wallet.listAccounts()
            accounts = list
        } catch let err as FfiError {
            if background && !isGenuineAuthExpiry(err) {
                // Transient network / backend blip on the unattended poll
                // route. Mirror the non-fatal `getUser` handling below:
                // keep the last-known `accounts`, do NOT touch `phase`, let
                // the next poll (or an explicit pull-to-refresh) recover.
                // This is what keeps a network hiccup during a Lightning
                // send from ejecting the user mid-payment.
                _ = err
                return
            }
            phase = .error("list accounts failed: \(ffiErrorMessage(err))")
            return
        } catch {
            if background {
                // Unexpected throw shape on the poll route: still non-fatal
                // — an unrecognised error is even less likely to be a real
                // session expiry, so never tear the UI down for it here.
                _ = error
                return
            }
            phase = .error("unexpected: \(error)")
            return
        }

        // Refresh the user row so per-currency default-account ids are
        // current. Failure here is non-fatal — leave `user` as-is (likely
        // nil for a brand-new guest before the first mint_add creates the
        // row). The accounts list is still useful without it; the UI just
        // omits the "Default" badge.
        do {
            user = try await wallet.getUser()
        } catch let err as FfiError {
            // The "user row not found" Internal error is expected on the
            // fresh-guest path. Treat it as "no defaults yet" rather than
            // a hard failure.
            if case .Internal(let msg) = err, msg.contains("user row not found") {
                user = nil
            } else {
                // Other failures (Auth / Storage / unexpected Internal):
                // leave `user` unchanged but don't escalate to `.error`
                // phase — the accounts list view is still usable.
                _ = err
            }
        } catch {
            // Unexpected throw shape; same conservative handling.
            _ = error
        }
    }

    /// Outcome shape returned to the swipe handler in `AccountsView`.
    /// Success carries no payload (the view re-reads `model.user` via
    /// `@Bindable`); failure carries a presentation-ready string.
    enum SetDefaultOutcome {
        case success
        case failure(String)
    }

    /// Mirror of the web `UserService.setDefaultAccount` for the iOS
    /// swipe action. Calls the FFI, then refreshes accounts + user so
    /// the row reorders and the badge moves without a separate
    /// pull-to-refresh.
    ///
    /// Does NOT touch `default_currency` — see the FFI doc on
    /// `set_default_account` for why (the web only flips it from
    /// account-creation paths, not from "set this existing account as
    /// default").
    func setDefaultAccount(_ account: AccountFfi) async -> SetDefaultOutcome {
        if isDemoMode {
            // Demo mode has no FFI plumbing — pretend it worked so the
            // SwiftUI Previews don't error.
            return .success
        }
        isWorking = true
        defer { isWorking = false }
        do {
            let updated = try await wallet.setDefaultAccount(accountId: account.id)
            user = updated
            // Re-list accounts so the row order reflects the new default
            // even if the FFI hasn't changed any underlying account row.
            // Cheap (one Supabase select).
            await refreshAccounts()
            return .success
        } catch let err as FfiError {
            return .failure(ffiErrorMessage(err))
        } catch {
            return .failure("unexpected: \(error)")
        }
    }

    /// True when the given account is the user's default for its currency.
    /// Mirrors `AccountService.isDefaultAccount` on web. Returns false
    /// (no badge) when the user row hasn't loaded yet, or when the
    /// account's currency has no default slot (e.g., USDB).
    func isDefault(_ account: AccountFfi) -> Bool {
        guard let user else { return false }
        switch account.currency {
        case "BTC": return account.id == user.defaultBtcAccountId
        case "USD": return account.id == user.defaultUsdAccountId
        default: return false
        }
    }

    /// `accounts` with the default-for-its-currency rows sorted to the
    /// top. Mirrors `AccountService.getExtendedAccounts.sort` on web.
    /// Among non-default rows the original FFI order is preserved (the
    /// FFI returns Supabase's natural order, which is creation-time).
    var sortedAccounts: [AccountFfi] {
        accounts.sorted { lhs, rhs in
            let l = isDefault(lhs)
            let r = isDefault(rhs)
            if l != r { return l && !r }
            // Stable: keep original FFI order for ties. SwiftUI's sort is
            // stable as long as the comparator returns false for equal
            // elements, which it does here.
            return false
        }
    }

    /// True only for a genuine "session is dead, re-authenticate" failure:
    /// `FfiError.Auth` with the `UNAUTHENTICATED` code. A `.Auth` with
    /// `NETWORK` / `BACKEND` / `INTERNAL`, or any `.Storage` / `.Internal`,
    /// is a transient or non-auth fault and is explicitly NOT treated as an
    /// expiry — those must not eject the user on the background poll route.
    private func isGenuineAuthExpiry(_ err: FfiError) -> Bool {
        if case .Auth(let code, _) = err {
            return code == Self.authCodeUnauthenticated
        }
        return false
    }

    private func ffiErrorMessage(_ err: FfiError) -> String {
        switch err {
        case .Auth(let code, let message):
            return "auth/\(code): \(message)"
        case .Storage(let code, let message):
            return "storage/\(code): \(message)"
        case .Internal(let message):
            return message
        }
    }
}
