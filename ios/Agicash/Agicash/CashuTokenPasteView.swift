import SwiftUI

/// One page of the Receive carousel — paste a Cashu token and claim it.
///
/// Refactor of the original `ReceiveView`'s `ReceiveFormCard` +
/// `ReceiveSuccessCard` extracted as a standalone view that lives
/// inside `ReceiveCarouselView`. The NavigationStack + toolbar + sheet
/// chrome are owned by the carousel host now.
///
/// State machine:
///   - `entry`        — paste field + Receive button.
///   - `working`      — Receive button shows a spinner; field is locked.
///                      Covers `Parsing` from the `ReceiveFlow` state machine.
///   - `confirmingMint(...)` — token's source mint isn't in the user's
///                      accounts. Renders a confirmation card with "Add
///                      Mint and Claim" / Cancel buttons. Mirrors the
///                      React `<ReceiveToken/>` page's "Add Mint and Claim"
///                      CTA (`app/features/receive/receive-cashu-token.tsx`
///                      lines 333-339).
///   - `addingMint`   — between confirm tap and mint being added. Spinner.
///   - `swapping`     — between mint-added and swap completion. Spinner.
///   - `success(...)` — replaces the form with a success card; auto-dismisses
///                      the WHOLE carousel after 2s OR the user can tap Done.
///   - `error(...)`   — inline destructive message under the field; user
///                      can edit + retry without dismissing.
///
/// The carousel-level dismiss callback is invoked when the user taps
/// "Done" in the success card or when the auto-dismiss timer fires.
/// The user can also swipe to another tab; we keep the local phase so
/// the tab remembers its state on swipe-back.
///
/// **Cross-account fix (2026-05-22):** previously this view called
/// `model.receive(token:)` which short-circuited with the raw FFI error
/// `"no matching account for mint <url> — add the mint first"` when the
/// token came from a mint the user hadn't added. Now drives the
/// `ReceiveFlow` state machine instead — pasting an unknown-mint token
/// transitions through `NeedsMintConfirmation` and lets the user accept
/// adding the mint inline (matching React's behaviour).
struct CashuTokenPasteView: View {
    @Bindable var model: WalletViewModel
    let onDismissCarousel: () -> Void

    enum Phase: Equatable {
        case entry
        case working
        case confirmingMint(MintConfirmationFfi)
        case addingMint
        case swapping
        case success(ReceiveResult)
        case error(String)

        static func == (lhs: Phase, rhs: Phase) -> Bool {
            switch (lhs, rhs) {
            case (.entry, .entry), (.working, .working),
                 (.addingMint, .addingMint), (.swapping, .swapping):
                return true
            case (.confirmingMint(let a), .confirmingMint(let b)): return a == b
            case (.success(let a), .success(let b)): return a == b
            case (.error(let a), .error(let b)): return a == b
            default: return false
            }
        }
    }

    @State private var token: String = ""
    @State private var phase: Phase = .entry
    @FocusState private var tokenFocused: Bool
    /// Live `ReceiveFlow` handle for the current interaction. Constructed
    /// on submit, dropped when the view returns to `.entry` or terminates.
    /// Held as `@State` because each tap of "Receive" gets a fresh flow —
    /// the Rust handle owns a `Mutex<ReceiveFlowService>` that should be
    /// scoped to the interaction, not the lifetime of the view.
    @State private var flow: ReceiveFlow?
    /// Auto-dismiss timer task. Held so we can cancel it if the user
    /// taps Done (or the view disappears) before the 2-second timeout
    /// fires.
    @State private var autoDismissTask: Task<Void, Never>?

    var body: some View {
        ScrollView {
            VStack(spacing: Spacing.xxl) {
                Spacer(minLength: Spacing.l)

                Group {
                    switch phase {
                    case .entry, .working, .error:
                        FormCard(
                            token: $token,
                            tokenFocused: $tokenFocused,
                            isWorking: phase == .working,
                            errorMessage: errorMessageForCurrentPhase,
                            onPaste: pasteFromClipboard,
                            onReceive: { Task { await submit() } }
                        )
                    case .confirmingMint(let confirmation):
                        MintConfirmationCard(
                            confirmation: confirmation,
                            isWorking: false,
                            onConfirm: { Task { await confirmAddMint() } },
                            onCancel: { Task { await cancelAddMint() } }
                        )
                    case .addingMint:
                        ProgressCard(
                            title: "Adding mint",
                            subtitle: "Setting up your new Cashu account…"
                        )
                    case .swapping:
                        ProgressCard(
                            title: "Claiming token",
                            subtitle: "Finalizing the receive swap…"
                        )
                    case .success(let result):
                        SuccessCard(
                            result: result,
                            onDone: dismissNow
                        )
                    }
                }
                .padding(.horizontal, Spacing.l)

                Spacer(minLength: Spacing.xxl)
            }
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
        .onDisappear {
            autoDismissTask?.cancel()
            // Drop any in-flight flow handle so the Rust side can free
            // its inner service. The carousel may re-create the view on
            // swipe-back, but that interaction should get a fresh flow.
            flow = nil
        }
    }

    /// Surfaces the inline error string when (and only when) the
    /// current phase is `.error`. Working/entry states render a clean
    /// form so the previous failure doesn't bleed through after the
    /// user starts editing.
    private var errorMessageForCurrentPhase: String? {
        if case .error(let message) = phase {
            return message
        }
        return nil
    }

    private func pasteFromClipboard() {
        guard let pasted = UIPasteboard.general.string else { return }
        token = pasted
        // Drop any prior error so the form reads clean post-paste.
        if case .error = phase { phase = .entry }
    }

    /// Begin a new receive flow. Step 0 extracts the encoded cashu token;
    /// then constructs a `ReceiveFlow` and dispatches `Start { token }`.
    /// The flow may resolve directly to `.success` (mint is already in
    /// the user's accounts — same-mint receive-swap) or surface a
    /// `NeedsMintConfirmation` state requiring user confirmation.
    private func submit() async {
        let trimmed = token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            phase = .error("Paste a Cashu token first.")
            return
        }
        // Step 0: extract the encoded cashu token from whatever the user
        // pasted (URL with ?token=…/#…, cashu: URI, embedded text, or
        // raw `cashuA…`/`cashuB…`). The downstream FFI receive is strict
        // — passing the raw URL through would re-create the bug this
        // extractor fixes.
        guard let encoded = extractCashuToken(input: trimmed) else {
            phase = .error("No Cashu token found in that text.")
            return
        }
        tokenFocused = false
        phase = .working

        // Construct a fresh flow handle. Failure here is auth/transient
        // (no session, FFI init issue) — surface and bail.
        let flowOutcome = await model.makeReceiveFlow()
        let activeFlow: ReceiveFlow
        switch flowOutcome {
        case .success(let f):
            activeFlow = f
            flow = f
        case .failure(let message):
            phase = .error(message)
            return
        }

        // Kick off the state machine. The dispatch returns the next stable
        // state — terminal (Done/AlreadyClaimed/Failed) or paused on user
        // input (NeedsMintConfirmation).
        do {
            let next = try await activeFlow.dispatch(event: .start(token: encoded))
            await render(state: next)
        } catch let err as FfiError {
            phase = .error(ffiErrorMessage(err))
            flow = nil
        } catch {
            phase = .error("unexpected: \(error)")
            flow = nil
        }
    }

    /// User said yes to "Add Mint and Claim". Dispatches `ConfirmAddMint`
    /// into the live flow — the Rust side runs `add_mint` + `receive_swap`
    /// in sequence and reports back through state transitions.
    private func confirmAddMint() async {
        guard let activeFlow = flow else {
            phase = .error("Receive flow was lost. Please paste the token again.")
            return
        }
        phase = .addingMint
        do {
            let next = try await activeFlow.dispatch(event: .confirmAddMint)
            await render(state: next)
        } catch let err as FfiError {
            phase = .error(ffiErrorMessage(err))
            flow = nil
        } catch {
            phase = .error("unexpected: \(error)")
            flow = nil
        }
    }

    /// User declined the mint-add. Dispatch `CancelAddMint` so the Rust
    /// side closes the flow cleanly, then return to entry so the user
    /// can paste a different token (or close the sheet).
    private func cancelAddMint() async {
        if let activeFlow = flow {
            // Best-effort: the cancel transition is internal bookkeeping;
            // we don't need to surface the resulting state, just drop it.
            _ = try? await activeFlow.dispatch(event: .cancelAddMint)
        }
        flow = nil
        phase = .entry
        // Re-focus the field so the user can paste a fresh token.
        tokenFocused = true
    }

    /// Translate a `ReceiveFlowStateFfi` snapshot into our local `Phase`
    /// and run any follow-up actions (refresh accounts on terminal
    /// success, schedule auto-dismiss).
    private func render(state: ReceiveFlowStateFfi) async {
        switch state {
        case .idle:
            // Shouldn't normally surface here (the flow starts in Idle but
            // the very next event drives it forward). Treat as entry.
            phase = .entry
            flow = nil
        case .parsing:
            // Transient — the dispatch loop should eat through this in
            // one round, but if we observe it map to working.
            phase = .working
        case .needsMintConfirmation(let confirmation):
            phase = .confirmingMint(confirmation)
        case .addingMint:
            phase = .addingMint
        case .swapping:
            phase = .swapping
        case .done(let result):
            phase = .success(receiveResult(fromFlow: result))
            // Refresh so Home's balance/accounts list reflects the new
            // proofs without forcing the user to pull-to-refresh. Mirrors
            // what `WalletViewModel.receive(token:)` used to do.
            await model.refreshAccounts()
            flow = nil
            scheduleAutoDismiss()
        case .alreadyClaimed(let info):
            phase = .success(receiveResult(fromAlreadyClaimed: info))
            flow = nil
            scheduleAutoDismiss()
        case .failed(let reason, _):
            phase = .error(reason)
            flow = nil
        }
    }

    /// Convert a `ReceiveFlowResultFfi` to the legacy `ReceiveResult`
    /// shape so the existing `SuccessCard` renders unchanged. The two
    /// types are structurally identical except `ReceiveStatusFfi` has
    /// three variants (the `AlreadyClaimed` case lives on
    /// `ReceiveFlowStateFfi` instead) — map each variant 1:1.
    private func receiveResult(fromFlow result: ReceiveFlowResultFfi) -> ReceiveResult {
        let status: ReceiveStatus = {
            switch result.status {
            case .received: return .received
            case .alreadyFailed: return .alreadyFailed
            case .pending: return .pending
            }
        }()
        return ReceiveResult(
            status: status,
            amount: result.amount,
            fee: result.fee,
            unit: result.unit,
            currency: result.currency,
            accountId: result.accountId,
            mintUrl: result.mintUrl,
            tokenHash: result.tokenHash
        )
    }

    /// Synthesize a `ReceiveResult` for the `AlreadyClaimed` state. The
    /// info doesn't carry amount/fee (per design — re-rendering "0 sats"
    /// would be misleading); pass empty strings and let `SuccessCard`
    /// surface the "already claimed" subhead via the status enum.
    private func receiveResult(fromAlreadyClaimed info: AlreadyClaimedInfoFfi) -> ReceiveResult {
        ReceiveResult(
            status: .alreadyClaimed,
            amount: "",
            fee: "",
            unit: info.unit,
            currency: info.currency,
            accountId: info.accountId,
            mintUrl: info.mintUrl,
            tokenHash: info.tokenHash
        )
    }

    /// Mirrors `WalletViewModel.ffiErrorMessage` — kept local so the view
    /// doesn't need a viewmodel round-trip just to format an FFI error.
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

    private func scheduleAutoDismiss() {
        autoDismissTask?.cancel()
        autoDismissTask = Task {
            try? await Task.sleep(nanoseconds: 2_000_000_000)
            if !Task.isCancelled {
                await MainActor.run { dismissNow() }
            }
        }
    }

    private func dismissNow() {
        autoDismissTask?.cancel()
        tokenFocused = false
        onDismissCarousel()
    }
}

/// Paste-token form card. Mirrors the original `ReceiveFormCard` but
/// without the surrounding `NavigationStack`/toolbar — the carousel
/// supplies the close affordance via its own toolbar.
private struct FormCard: View {
    @Binding var token: String
    var tokenFocused: FocusState<Bool>.Binding
    let isWorking: Bool
    let errorMessage: String?
    let onPaste: () -> Void
    let onReceive: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.l) {
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text("Receive Cashu")
                    .font(.brandTitle)
                    .foregroundStyle(Color.brandCardForeground)
                Text("Paste a Cashu token to claim it into your wallet")
                    .font(.brandLabel)
                    .foregroundStyle(Color.brandMutedForeground)
            }

            VStack(spacing: Spacing.l) {
                VStack(alignment: .leading, spacing: Spacing.s) {
                    HStack {
                        Text("Token")
                            .font(.brandLabelEmphasis)
                            .foregroundStyle(Color.brandCardForeground)
                        Spacer()
                        Button(action: onPaste) {
                            Text("Paste")
                                .font(.brandLabel)
                                .underline()
                                .foregroundStyle(Color.brandCardForeground)
                        }
                        .buttonStyle(.plain)
                        .disabled(isWorking)
                    }

                    TextEditor(text: $token)
                        .font(.brandBody)
                        .foregroundStyle(Color.brandForeground)
                        .scrollContentBackground(.hidden)
                        .focused(tokenFocused)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .frame(minHeight: 96, maxHeight: 200)
                        .padding(Spacing.s)
                        .background(
                            RoundedRectangle(cornerRadius: Radius.control)
                                .fill(Color.brandBackground)
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: Radius.control)
                                .stroke(
                                    tokenFocused.wrappedValue
                                        ? Color.brandRing : Color.brandInput,
                                    lineWidth: tokenFocused.wrappedValue ? 1.5 : 0.5
                                )
                        )
                        .disabled(isWorking)
                }

                if let errorMessage {
                    Text(errorMessage)
                        .font(.brandCaption)
                        .foregroundStyle(Color.brandDestructive)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }

                BrandButton(
                    "Receive",
                    variant: .primary,
                    isLoading: isWorking,
                    isDisabled: token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                    action: onReceive
                )
            }
        }
        .padding(Spacing.xxl)
        .brandCard()
        .frame(maxWidth: 384)
    }
}

/// Confirmation card shown when the pasted token is from a mint the user
/// hasn't added yet. Mirrors React's `<ReceiveToken/>` "Add Mint and
/// Claim" branch (`app/features/receive/receive-cashu-token.tsx` lines
/// 333-339) where the source-mint placeholder is selected as the
/// destination and the CTA copy switches to `"Add Mint and Claim"`.
///
/// iOS form factor doesn't host React's `<AccountSelector/>` (Slice 2
/// scope is source-mint-only — the Rust `ReceiveFlow` state machine
/// doesn't surface alternative destinations yet, per
/// `2026-05-22-cross-account-audit.md`). So this card collapses the
/// React picker into a single mint preview plus the same CTA pair.
private struct MintConfirmationCard: View {
    let confirmation: MintConfirmationFfi
    let isWorking: Bool
    let onConfirm: () -> Void
    let onCancel: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.l) {
            // Card header — same rhythm as AddMintFormCard / FormCard so
            // the language reads consistently across the receive surface.
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text("Add this mint?")
                    .font(.brandTitle)
                    .foregroundStyle(Color.brandCardForeground)
                Text("This token is from a mint you haven't added yet. Add it to claim the funds.")
                    .font(.brandLabel)
                    .foregroundStyle(Color.brandMutedForeground)
            }

            // Mint identity block — mirrors AddMintSuccessCard's geometry:
            // big name, monospaced URL underneath, currency badge.
            VStack(alignment: .center, spacing: Spacing.s) {
                Text(confirmation.mintName)
                    .font(.brandTitle)
                    .foregroundStyle(Color.brandCardForeground)
                    .lineLimit(1)
                    .truncationMode(.tail)
                Text(confirmation.mintUrl)
                    .font(.brandCaption)
                    .foregroundStyle(Color.brandMutedForeground)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .frame(maxWidth: .infinity)

            // Amount block — mirrors SuccessCard's amount rendering so
            // the pre-claim and post-claim cards feel like the same
            // visual family.
            VStack(alignment: .center, spacing: Spacing.xs) {
                Text("Claiming")
                    .font(.brandCaption)
                    .foregroundStyle(Color.brandMutedForeground)
                HStack(alignment: .lastTextBaseline, spacing: 6) {
                    Text(confirmation.amount)
                        .font(.brandNumericInline)
                        .foregroundStyle(Color.brandCardForeground)
                        .monospacedDigit()
                    Text(confirmation.unit)
                        .font(.brandLabel)
                        .foregroundStyle(Color.brandMutedForeground)
                }
                if !confirmation.fee.isEmpty && confirmation.fee != "0" {
                    Text("Mint fee: \(confirmation.fee) \(confirmation.unit)")
                        .font(.brandCaption)
                        .foregroundStyle(Color.brandMutedForeground)
                }
            }
            .frame(maxWidth: .infinity)

            // CTA stack — primary "Add Mint and Claim" (exact React copy)
            // + ghost "Cancel". Same shape as AddMintFormCard.
            BrandButton(
                "Add Mint and Claim",
                variant: .primary,
                isLoading: isWorking,
                isDisabled: false,
                action: onConfirm
            )

            BrandButton(
                "Cancel",
                variant: .ghost,
                isLoading: false,
                isDisabled: isWorking,
                action: onCancel
            )
        }
        .padding(Spacing.xxl)
        .brandCard()
        .frame(maxWidth: 384)
    }
}

/// Indeterminate-progress card shown during the `AddingMint` and
/// `Swapping` phases of the receive flow. Keeps the visual rhythm of
/// the other cards (same card chrome, same header geometry) but
/// replaces the form/buttons with a centered spinner + status copy so
/// the user has something to look at during the (typically 1-3s)
/// round-trips.
private struct ProgressCard: View {
    let title: String
    let subtitle: String

    var body: some View {
        VStack(alignment: .center, spacing: Spacing.l) {
            VStack(alignment: .center, spacing: Spacing.xs) {
                Text(title)
                    .font(.brandTitle)
                    .foregroundStyle(Color.brandCardForeground)
                Text(subtitle)
                    .font(.brandLabel)
                    .foregroundStyle(Color.brandMutedForeground)
                    .multilineTextAlignment(.center)
            }

            ProgressView()
                .progressViewStyle(.circular)
                .controlSize(.large)
                .tint(Color.brandForeground)
                .padding(.vertical, Spacing.l)
        }
        .frame(maxWidth: .infinity)
        .padding(Spacing.xxl)
        .brandCard()
        .frame(maxWidth: 384)
    }
}

/// Success state shown after a token claims successfully. Same shape as
/// the original `ReceiveSuccessCard`.
private struct SuccessCard: View {
    let result: ReceiveResult
    let onDone: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: Spacing.l) {
            VStack(alignment: .leading, spacing: Spacing.xs) {
                Text(headline)
                    .font(.brandTitle)
                    .foregroundStyle(Color.brandCardForeground)
                Text(subhead)
                    .font(.brandLabel)
                    .foregroundStyle(Color.brandMutedForeground)
            }

            VStack(alignment: .center, spacing: Spacing.s) {
                // Only render the amount line when we have one. The
                // `alreadyClaimed` state synthesizes an empty `amount`
                // because the FFI deliberately omits it (re-rendering
                // "0 sats" would be misleading).
                if !result.amount.isEmpty {
                    HStack(alignment: .lastTextBaseline, spacing: 6) {
                        Text(result.amount)
                            .font(.brandNumericInline)
                            .foregroundStyle(Color.brandCardForeground)
                            .monospacedDigit()
                        Text(result.unit)
                            .font(.brandLabel)
                            .foregroundStyle(Color.brandMutedForeground)
                    }
                }
                Text(result.mintUrl)
                    .font(.brandCaption)
                    .foregroundStyle(Color.brandMutedForeground)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            .frame(maxWidth: .infinity)

            BrandButton(
                "Done",
                variant: .primary,
                action: onDone
            )
        }
        .padding(Spacing.xxl)
        .brandCard()
        .frame(maxWidth: 384)
    }

    private var headline: String {
        switch result.status {
        case .received:       return "Token received"
        case .alreadyClaimed: return "Already claimed"
        case .pending:        return "Pending"
        case .alreadyFailed:  return "Token unavailable"
        }
    }

    private var subhead: String {
        switch result.status {
        case .received:
            return "Proofs added to your wallet."
        case .alreadyClaimed:
            return "You've already redeemed this token — wallet is unchanged."
        case .pending:
            return "Swap is in progress. It will finish in the background."
        case .alreadyFailed:
            return "Someone else redeemed this token first."
        }
    }
}
