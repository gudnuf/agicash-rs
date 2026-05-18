import SwiftUI
import UIKit

/// One page of the Send carousel — pay a BOLT-11 invoice over
/// Lightning (NUT-05 melt). Paste/scan an invoice, review the
/// fee-reserve breakdown, confirm, watch the mint settle the payment.
///
/// State machine (mirrors `SendCashuTokenView.Phase` + the Lightning
/// receive flow's poll loop):
///   - `invoiceEntry`        → paste field + Continue.
///   - `quoting`             → spinner while `prepareMeltQuote` runs.
///   - `confirming(preview)` → fee-reserve breakdown card; user taps Pay.
///   - `creating`            → spinner while `createMeltQuote` persists
///     the UNPAID quote + reserves proofs.
///   - `paying(handle)`      → spinner while `executeMeltQuote` fires
///     `post_melt`; on PENDING a long-running `Task` polls
///     `pollMeltQuote` every 2s until a terminal state.
///   - `verifying(handle)`   → the melt was *already initiated* for
///     this quote (executeMeltQuote called) but the call threw, or
///     returned an ambiguous non-terminal state. The mint may have
///     paid the invoice. We MUST NOT re-quote — instead poll
///     `pollMeltQuote` on the held handle (a NUT-05 status check, not
///     a re-pay) until the mint authoritatively reports PAID
///     (→ success) or genuinely-not-paid (→ a non-re-quoting failure).
///   - `paid(snapshot)`      → green check + final amount/fee. Auto-
///     dismisses the carousel after 3s; user can tap Done sooner.
///   - `failure(message, retryable)` → inline error. `retryable` is
///     true ONLY for pre-initiation failures (bad invoice / quote
///     failed before `executeMeltQuote`) where re-quoting the same
///     bolt11 is safe; retry → invoiceEntry. Once a melt has been
///     initiated for a quote, a failure is NEVER `retryable` (a fresh
///     quote for the same invoice would double-pay) — the user can
///     only dismiss; reconciliation already happened via the poll.
///
/// The file name keeps the historical `LightningSendPlaceholderView`
/// identifier so `SendCarouselView`'s `.tag(.lightning)` wiring and
/// the Xcode project membership stay stable across the placeholder →
/// real-view swap (same approach the receive carousel took).
///
/// Mirrors the web `app/features/send/send-confirmation.tsx`
/// (`PayBolt11Confirmation` — "Confirm Payment", amount-to-receive +
/// "Estimated fee" rows + "Estimated total") and
/// `app/features/send/send-input.tsx` (paste destination).
struct LightningSendPlaceholderView: View {
    @Bindable var model: WalletViewModel
    let onDismissCarousel: () -> Void

    /// When set, the view skips `invoiceEntry` and immediately quotes
    /// this invoice. The LN-address page resolves an address to a
    /// bolt11 then hands it down through this so both Lightning send
    /// paths share one melt state machine.
    var presetInvoice: String?

    init(
        model: WalletViewModel,
        onDismissCarousel: @escaping () -> Void,
        presetInvoice: String? = nil
    ) {
        self.model = model
        self.onDismissCarousel = onDismissCarousel
        self.presetInvoice = presetInvoice
    }

    enum Phase: Equatable {
        case invoiceEntry
        case quoting
        case confirming(MeltQuotePreview)
        case creating
        case paying(MeltQuoteHandle)
        /// The melt was initiated for this quote (executeMeltQuote
        /// called) but threw or returned a non-terminal/ambiguous
        /// state. Poll the held handle to reconcile — NEVER re-quote.
        case verifying(MeltQuoteHandle)
        case paid(MeltQuoteSnapshot)
        /// `retryable` is true only for pre-initiation failures where
        /// re-quoting the same invoice is safe. Post-initiation
        /// failures are never retryable (re-quote would double-pay).
        case failure(message: String, retryable: Bool)
    }

    @State private var invoice: String = ""
    @State private var phase: Phase = .invoiceEntry
    @FocusState private var invoiceFocused: Bool
    /// The bolt11 we quoted/created against — held so `confirming` can
    /// hand it to `createMeltQuote` (the FFI preview record doesn't
    /// carry the invoice back across the boundary).
    @State private var quotedInvoice: String = ""
    /// Long-running poll task. Held so we can cancel on disappear /
    /// dismiss / terminal transition.
    @State private var pollTask: Task<Void, Never>?
    /// Auto-dismiss timer on the paid state.
    @State private var autoDismissTask: Task<Void, Never>?

    /// BTC-only / sats-only for v0 — same constraint the Cashu send +
    /// Lightning receive views ship under. The invoice's own msat
    /// amount drives what the receiver gets; the account currency
    /// picks the source mint.
    private let currency = "BTC"
    private let unitLabel = "sats"

    var body: some View {
        VStack(spacing: 0) {
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task {
            // LN-address page handed us a resolved invoice — jump
            // straight to the quote step.
            if let preset = presetInvoice, phase == .invoiceEntry,
               invoice.isEmpty {
                invoice = preset
                await startQuote()
            }
        }
        .onDisappear {
            pollTask?.cancel()
            autoDismissTask?.cancel()
        }
    }

    @ViewBuilder
    private var content: some View {
        switch phase {
        case .invoiceEntry:
            invoiceEntryView
        case .quoting:
            ProgressView("Fetching quote from mint…")
                .font(.brandLabel)
                .foregroundStyle(Color.brandMutedForeground)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .confirming(let preview):
            ConfirmCard(
                preview: preview,
                onPay: { Task { await commitAndPay() } },
                onCancel: resetToEntry
            )
            .padding(.horizontal, Spacing.l)
        case .creating:
            ProgressView("Reserving proofs…")
                .font(.brandLabel)
                .foregroundStyle(Color.brandMutedForeground)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .paying:
            ProgressView("Paying invoice…")
                .font(.brandLabel)
                .foregroundStyle(Color.brandMutedForeground)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .verifying:
            // The payment may already have settled — we are
            // reconciling against the mint, NOT retrying. No cancel
            // affordance here: bailing now would lose the only UI
            // tracking of an in-flight/possibly-settled payment, and
            // there is nothing safe to "retry".
            ProgressView("Confirming payment status…")
                .font(.brandLabel)
                .foregroundStyle(Color.brandMutedForeground)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .paid(let snapshot):
            PaidCard(
                snapshot: snapshot,
                unitLabel: unitLabel,
                onDone: dismissNow
            )
            .padding(.horizontal, Spacing.l)
        case .failure(let message, let retryable):
            FailureCard(
                message: message,
                // Only offer "Try again" (→ re-quote) when the failure
                // happened BEFORE the melt was initiated. After
                // initiation a fresh quote for the same invoice would
                // double-pay, so the only affordance is Dismiss.
                onRetry: retryable ? resetToEntry : nil,
                onDismiss: dismissNow
            )
            .padding(.horizontal, Spacing.l)
        }
    }

    private var invoiceEntryView: some View {
        ScrollView {
            VStack(spacing: Spacing.xxl) {
                Spacer(minLength: Spacing.l)

                VStack(alignment: .leading, spacing: Spacing.l) {
                    VStack(alignment: .leading, spacing: Spacing.xs) {
                        Text("Pay Lightning invoice")
                            .font(.brandTitle)
                            .foregroundStyle(Color.brandCardForeground)
                        Text("Paste a BOLT-11 invoice to pay it from your Cashu balance")
                            .font(.brandLabel)
                            .foregroundStyle(Color.brandMutedForeground)
                    }

                    VStack(alignment: .leading, spacing: Spacing.s) {
                        HStack {
                            Text("Invoice")
                                .font(.brandLabelEmphasis)
                                .foregroundStyle(Color.brandCardForeground)
                            Spacer()
                            Button(action: pasteFromClipboard) {
                                Text("Paste")
                                    .font(.brandLabel)
                                    .underline()
                                    .foregroundStyle(Color.brandCardForeground)
                            }
                            .buttonStyle(.plain)
                        }

                        TextEditor(text: $invoice)
                            .font(.brandBody)
                            .foregroundStyle(Color.brandForeground)
                            .scrollContentBackground(.hidden)
                            .focused($invoiceFocused)
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
                                        invoiceFocused
                                            ? Color.brandRing : Color.brandInput,
                                        lineWidth: invoiceFocused ? 1.5 : 0.5
                                    )
                            )
                    }

                    BrandButton(
                        "Continue",
                        variant: .primary,
                        isDisabled: invoice.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                        action: { Task { await startQuote() } }
                    )
                }
                .padding(Spacing.xxl)
                .brandCard()
                .frame(maxWidth: 384)
                .padding(.horizontal, Spacing.l)

                Spacer(minLength: Spacing.xxl)
            }
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
    }

    // MARK: - actions

    private func pasteFromClipboard() {
        guard let pasted = UIPasteboard.general.string else { return }
        // Strip a `lightning:` URI scheme if the user copied a payment
        // link rather than the bare bolt11 (common from wallets).
        invoice = stripLightningScheme(pasted)
    }

    private func stripLightningScheme(_ s: String) -> String {
        let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
        let lower = trimmed.lowercased()
        if lower.hasPrefix("lightning:") {
            return String(trimmed.dropFirst("lightning:".count))
        }
        return trimmed
    }

    private func startQuote() async {
        let bolt11 = stripLightningScheme(invoice)
        guard !bolt11.isEmpty else { return }
        invoiceFocused = false
        quotedInvoice = bolt11
        phase = .quoting
        let outcome = await model.prepareMeltQuote(
            bolt11: bolt11,
            accountId: nil,
            currency: currency
        )
        switch outcome {
        case .success(let preview):
            phase = .confirming(preview)
        case .failure(let message):
            // Pre-initiation: no quote persisted, no proofs reserved,
            // no melt fired. Re-quoting the same invoice is safe.
            phase = .failure(message: message, retryable: true)
        }
    }

    private func commitAndPay() async {
        phase = .creating
        let createOutcome = await model.createMeltQuote(
            bolt11: quotedInvoice,
            accountId: nil,
            currency: currency
        )
        let handle: MeltQuoteHandle
        switch createOutcome {
        case .success(let h):
            handle = h
        case .failure(let message):
            // Pre-initiation: the quote row failed to persist / proofs
            // weren't reserved and `post_melt` never fired. Safe to
            // re-quote the same invoice.
            phase = .failure(message: message, retryable: true)
            return
        }

        // From here on the melt is being INITIATED for `handle`.
        // `post_melt` may reach the mint and the invoice may be paid
        // even if the call below throws or returns ambiguously. After
        // this point we must NEVER route to a re-quote: every
        // non-success outcome reconciles via the poll on `handle`.
        phase = .paying(handle)
        let execOutcome = await model.executeMeltQuote(quoteId: handle.quoteId)
        switch execOutcome {
        case .state(let state, let snapshot):
            switch state {
            case .paid:
                phase = .paid(snapshot)
                scheduleAutoDismiss()
            case .pending:
                // Lightning payment in flight — poll until terminal.
                startPolling(handle)
            case .failed, .expired, .unpaid:
                // The mint *replied* with a non-paid state on the
                // execute round-trip. We still do NOT trust this to
                // mean "no funds moved": the core `post_melt`
                // network-error branch can mark a quote Failed even
                // when the mint received the request. Reconcile via a
                // mint status check before showing any terminal UI —
                // and never via a fresh quote.
                startReconcilePoll(handle)
            }
        case .failure:
            // `executeMeltQuote` THREW after initiation. The classic
            // double-pay trigger: the mint may have paid the invoice
            // and only `complete_with_change` (storage write / NUT-12
            // DLEQ) failed, leaving the quote PENDING. Resolve by
            // polling the held handle — a NUT-05 status check, not a
            // re-pay — never by re-quoting this invoice.
            startReconcilePoll(handle)
        }
    }

    /// Reconcile a melt that was already INITIATED for `handle` but
    /// whose `executeMeltQuote` threw or returned an
    /// ambiguous/non-terminal state. The invoice may have been paid;
    /// the quote row is left PENDING when only the post-payment
    /// bookkeeping (storage `complete` / NUT-12 DLEQ) failed. We do
    /// NOT trust the thrown error or an execute-time Failed to mean
    /// "no funds moved" — we poll `pollMeltQuote` (a NUT-05 status
    /// check via the core `poll_until_complete`, never a re-pay) until
    /// the mint authoritatively resolves the quote. Crucially this
    /// path can never reach `createMeltQuote` for this invoice.
    private func startReconcilePoll(_ handle: MeltQuoteHandle) {
        phase = .verifying(handle)
        startPolling(handle)
    }

    /// Drive `pollMeltQuote` on the held handle until a TRUSTWORTHY
    /// terminal state. Used both for the normal PENDING path (after a
    /// clean `executeMeltQuote` → Pending) and for reconciliation
    /// after a post-initiation throw — the loop is identical because
    /// in both cases the only safe move is to ask the mint for the
    /// quote's status, never to issue a new melt.
    ///
    /// Every terminal failure here is reported as **non-retryable**:
    /// the melt was already initiated for this quote, so a "Try again"
    /// that re-quotes the same invoice could double-pay. The
    /// PAID/FAILED/UNPAID/EXPIRED verdicts come from a mint status
    /// check (`poll_until_complete`), so they are authoritative —
    /// unlike a thrown error or the `post_melt` network-error branch.
    private func startPolling(_ handle: MeltQuoteHandle) {
        pollTask?.cancel()
        pollTask = Task {
            // Poll every 2s — same cadence as the Lightning receive
            // poll loop. The melt may stay PENDING for tens of seconds
            // while the mint's Lightning payment settles.
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                if Task.isCancelled { return }
                let outcome = await model.pollMeltQuote(quoteId: handle.quoteId)
                if Task.isCancelled { return }
                switch outcome {
                case .state(let state, let snapshot):
                    switch state {
                    case .pending:
                        // Still in flight per the mint. Keep polling —
                        // do NOT surface a retry affordance.
                        continue
                    case .paid:
                        // Authoritative PAID. The original throw (if
                        // any) was post-payment bookkeeping; the money
                        // moved exactly once. Show success.
                        await MainActor.run { phase = .paid(snapshot) }
                        scheduleAutoDismiss()
                        return
                    case .failed:
                        // Mint status check resolved the quote FAILED
                        // — trustworthy (it round-tripped the mint via
                        // `poll_until_complete`, not the unreliable
                        // post_melt network-error branch). Still NOT
                        // retryable: never re-quote this invoice.
                        await MainActor.run {
                            phase = .failure(
                                message: snapshot.failureReason ?? "Payment failed.",
                                retryable: false
                            )
                        }
                        return
                    case .expired:
                        await MainActor.run {
                            phase = .failure(
                                message: "The invoice expired before payment.",
                                retryable: false
                            )
                        }
                        return
                    case .unpaid:
                        // A poll that sees UNPAID means the mint
                        // reported the melt back as unpaid and the
                        // core flipped the row FAILED — authoritative.
                        // Still not retryable from here.
                        await MainActor.run {
                            phase = .failure(
                                message: "The mint could not pay this invoice.",
                                retryable: false
                            )
                        }
                        return
                    }
                case .failure(let message):
                    // Transient network blip — keep polling so a
                    // single failure doesn't kick the user out, and so
                    // we never fall through to a re-quote. The payment
                    // is in flight on the mint regardless.
                    _ = message
                    continue
                }
            }
        }
    }

    private func resetToEntry() {
        pollTask?.cancel()
        autoDismissTask?.cancel()
        // If we arrived from the LN-address page there's no invoice to
        // re-edit here — bounce the whole carousel instead of showing
        // an empty paste field the user can't meaningfully use.
        if presetInvoice != nil {
            dismissNow()
            return
        }
        phase = .invoiceEntry
    }

    private func scheduleAutoDismiss() {
        autoDismissTask?.cancel()
        autoDismissTask = Task {
            try? await Task.sleep(nanoseconds: 3_000_000_000)
            if !Task.isCancelled {
                await MainActor.run { dismissNow() }
            }
        }
    }

    private func dismissNow() {
        pollTask?.cancel()
        autoDismissTask?.cancel()
        onDismissCarousel()
    }
}

// MARK: - Confirm card

/// Fee-reserve breakdown shown before the user commits. Mirrors the
/// web `PayBolt11Confirmation` (`send-confirmation.tsx`): the headline
/// amount is what the receiver gets, the rows show the worst-case fee
/// reserve, and the prominent total is the most that can leave the
/// account (the real debit may be lower after the NUT-08 reserve
/// refund — surfaced on the success card).
private struct ConfirmCard: View {
    let preview: MeltQuotePreview
    let onPay: () -> Void
    let onCancel: () -> Void

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Spacing.l) {
                VStack(alignment: .leading, spacing: Spacing.xs) {
                    Text("Confirm payment")
                        .font(.brandTitle)
                        .foregroundStyle(Color.brandCardForeground)
                    Text("Paying a Lightning invoice from your Cashu balance")
                        .font(.brandLabel)
                        .foregroundStyle(Color.brandMutedForeground)
                }

                VStack(spacing: Spacing.s) {
                    amountRow(
                        "They receive",
                        value: preview.amount,
                        unit: preview.unit,
                        prominent: true
                    )
                    amountRow(
                        "Lightning fee reserve",
                        value: preview.lightningFeeReserve,
                        unit: preview.unit
                    )
                    if preview.cashuFee != "0" {
                        amountRow(
                            "Cashu fee",
                            value: preview.cashuFee,
                            unit: preview.unit
                        )
                    }
                    Divider()
                    amountRow(
                        "Estimated total",
                        value: preview.totalAmount,
                        unit: preview.unit,
                        prominent: true
                    )
                }

                Text("Unused fee reserve is refunded after the payment settles.")
                    .font(.brandCaption)
                    .foregroundStyle(Color.brandMutedForeground)

                VStack(spacing: Spacing.s) {
                    BrandButton("Pay", variant: .primary, action: onPay)
                    BrandButton("Cancel", variant: .ghost, action: onCancel)
                }
            }
            .padding(Spacing.xxl)
            .brandCard()
            .frame(maxWidth: 384)
        }
    }

    private func amountRow(
        _ label: String,
        value: String,
        unit: String,
        prominent: Bool = false
    ) -> some View {
        HStack {
            Text(label)
                .font(prominent ? .brandLabelEmphasis : .brandLabel)
                .foregroundStyle(prominent ? Color.brandCardForeground : Color.brandMutedForeground)
            Spacer()
            HStack(alignment: .lastTextBaseline, spacing: 4) {
                Text(value)
                    .font(prominent ? .brandLabelEmphasis : .brandLabel)
                    .foregroundStyle(Color.brandCardForeground)
                    .monospacedDigit()
                Text(unit)
                    .font(.brandCaption)
                    .foregroundStyle(Color.brandMutedForeground)
            }
        }
    }
}

// MARK: - Paid card

/// Settled-payment receipt. Shows the actual amount spent (proofs
/// reserved minus the refunded NUT-08 change) when the PAID snapshot
/// carries it, falling back to the receiver amount otherwise. Mirrors
/// `SendCashuTokenView.ClaimedCard`'s shape.
private struct PaidCard: View {
    let snapshot: MeltQuoteSnapshot
    let unitLabel: String
    let onDone: () -> Void

    var body: some View {
        VStack(spacing: Spacing.xxl) {
            Spacer(minLength: Spacing.xxl)
            VStack(spacing: Spacing.m) {
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 56))
                    .foregroundStyle(Color.green)
                Text("Paid")
                    .font(.brandTitle)
                    .foregroundStyle(Color.brandCardForeground)
                HStack(alignment: .lastTextBaseline, spacing: 4) {
                    Text(snapshot.amountSpent ?? "")
                        .font(.brandNumericInline)
                        .foregroundStyle(Color.brandCardForeground)
                        .monospacedDigit()
                    Text(unitLabel)
                        .font(.brandLabel)
                        .foregroundStyle(Color.brandMutedForeground)
                }
                if let fee = snapshot.lightningFee {
                    Text("Lightning fee: \(fee) \(unitLabel)")
                        .font(.brandCaption)
                        .foregroundStyle(Color.brandMutedForeground)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(Spacing.xxl)
            .brandCard()
            .frame(maxWidth: 384)

            BrandButton("Done", variant: .primary, action: onDone)
                .frame(maxWidth: 384)

            Spacer(minLength: Spacing.xxl)
        }
    }
}

// MARK: - Failure card (same shape as SendCashuTokenView's)

private struct FailureCard: View {
    let message: String
    /// `nil` when re-quoting would be unsafe (the melt was already
    /// initiated for this invoice — a fresh quote could double-pay).
    /// Only then is the "Try again" button hidden entirely.
    let onRetry: (() -> Void)?
    let onDismiss: () -> Void

    var body: some View {
        VStack(spacing: Spacing.xxl) {
            Spacer(minLength: Spacing.xxl)
            VStack(spacing: Spacing.m) {
                Image(systemName: "xmark.octagon.fill")
                    .font(.system(size: 48))
                    .foregroundStyle(Color.brandDestructive)
                Text("Couldn't pay")
                    .font(.brandTitle)
                    .foregroundStyle(Color.brandCardForeground)
                Text(message)
                    .font(.brandLabel)
                    .foregroundStyle(Color.brandMutedForeground)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity)
            .padding(Spacing.xxl)
            .brandCard()
            .frame(maxWidth: 384)

            VStack(spacing: Spacing.m) {
                if let onRetry {
                    BrandButton("Try again", variant: .primary, action: onRetry)
                    BrandButton("Dismiss", variant: .ghost, action: onDismiss)
                } else {
                    // Post-initiation failure: re-quoting the same
                    // invoice could double-pay, so no retry is offered.
                    BrandButton("Dismiss", variant: .primary, action: onDismiss)
                }
            }
            .frame(maxWidth: 384)

            Spacer(minLength: Spacing.xxl)
        }
    }
}
