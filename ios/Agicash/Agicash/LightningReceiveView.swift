import CoreImage.CIFilterBuiltins
import SwiftUI
import UIKit

/// One page of the Receive carousel — generate a BOLT-11 invoice and
/// watch the mint until it gets paid.
///
/// State machine:
///   - `amountEntry` → user types an amount on the numpad, hits
///     "Create invoice".
///   - `generating`  → spinner while `startMintQuote` runs (one mint
///     round-trip).
///   - `invoice`     → QR code + BOLT-11 + breakdown + countdown timer.
///     A long-running `Task` polls `pollMintQuote` every 2s; on PAID
///     transitions to `completing`.
///   - `completing`  → spinner while `completeMintQuote` mints proofs.
///   - `success`     → green check + "Received N sats". Auto-dismisses
///     the carousel after 4s; user can tap "Receive more" to bounce
///     back to amountEntry, or "Done" to dismiss now.
///   - `failure`     → inline error + "Try again" → amountEntry.
///
/// Mirrors `app/features/receive/receive-cashu.tsx` on the web (which
/// is the cashu-mint side of `/receive/cashu`) — same conceptual flow:
/// create quote → show QR → poll → success.
struct LightningReceiveView: View {
    @Bindable var model: WalletViewModel
    let onDismissCarousel: () -> Void

    enum Phase: Equatable {
        case amountEntry
        case generating
        case invoice(MintQuoteHandle)
        case completing(MintQuoteHandle)
        case success(ReceiveResult)
        case failure(String)
    }

    @State private var amountBuffer: String = "0"
    @State private var phase: Phase = .amountEntry
    /// Currency the numpad enters in. Drives the FFI `currency`
    /// argument directly — the mint is quoted in this currency, no
    /// client-side conversion. Switching it resets the buffer because
    /// "100 sats" and "100 cents" aren't the same magnitude and there
    /// is no exchange rate exposed via FFI to carry the value across
    /// (the web app's "≈ X sats" secondary line needs that rate; see
    /// the file footer note).
    @State private var entryCurrency: EntryCurrency = .btc
    /// Long-running poll task. Held so we can cancel it when the view
    /// disappears, the user hits Cancel, or the polled state moves
    /// past UNPAID.
    @State private var pollTask: Task<Void, Never>?
    /// Auto-dismiss timer on the success state.
    @State private var autoDismissTask: Task<Void, Never>?

    /// Currencies the receive numpad can enter in. Maps 1:1 onto the
    /// `startMintQuote` `currency` argument (`"BTC"` / `"USD"`); the
    /// FFI takes the amount in the account's minor unit (integer sats
    /// for BTC, integer cents for USD).
    enum EntryCurrency: CaseIterable {
        case btc
        case usd

        /// Wallet currency string the FFI expects.
        var ffiCurrency: String {
            switch self {
            case .btc: return "BTC"
            case .usd: return "USD"
            }
        }
        /// Unit label rendered next to the hero amount.
        var unitLabel: String {
            switch self {
            case .btc: return "sats"
            case .usd: return "USD"
            }
        }
        /// Short label for the toggle pill.
        var shortLabel: String {
            switch self {
            case .btc: return "sats"
            case .usd: return "USD"
            }
        }
        /// USD enters dollars-with-cents (two decimals); sats are
        /// integer. Drives both the numpad decimal key and the
        /// minor-unit conversion below.
        var allowsDecimal: Bool {
            switch self {
            case .btc: return false
            case .usd: return true
            }
        }
    }

    private var currency: String { entryCurrency.ffiCurrency }
    private var unitLabel: String { entryCurrency.unitLabel }
    private var allowsDecimal: Bool { entryCurrency.allowsDecimal }

    var body: some View {
        VStack(spacing: 0) {
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onDisappear {
            pollTask?.cancel()
            autoDismissTask?.cancel()
        }
    }

    @ViewBuilder
    private var content: some View {
        switch phase {
        case .amountEntry:
            amountEntryView
        case .generating:
            ProgressView("Requesting invoice from mint…")
                .font(.brandLabel)
                .foregroundStyle(Color.brandMutedForeground)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .invoice(let handle):
            InvoiceView(
                handle: handle,
                onCancel: cancelInvoice,
                onCopy: copyInvoice
            )
        case .completing:
            ProgressView("Minting proofs…")
                .font(.brandLabel)
                .foregroundStyle(Color.brandMutedForeground)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .success(let result):
            SuccessCard(
                result: result,
                onReceiveMore: resetToAmountEntry,
                onDone: dismissNow
            )
            .padding(.horizontal, Spacing.l)
        case .failure(let message):
            FailureCard(
                message: message,
                onRetry: resetToAmountEntry,
                onDismiss: dismissNow
            )
            .padding(.horizontal, Spacing.l)
        }
    }

    private var amountEntryView: some View {
        VStack(spacing: Spacing.xxl) {
            Spacer(minLength: Spacing.l)

            VStack(spacing: Spacing.xs) {
                HStack(alignment: .lastTextBaseline, spacing: 4) {
                    Text(displayAmount)
                        .font(.brandNumericHero)
                        .foregroundStyle(Color.brandForeground)
                        .monospacedDigit()
                        .minimumScaleFactor(0.6)
                        .lineLimit(1)
                    Text(unitLabel)
                        .font(.brandTitleSmall)
                        .foregroundStyle(Color.brandMutedForeground)
                        .baselineOffset(8)
                }
                Text("Receive over Lightning")
                    .font(.brandLabel)
                    .foregroundStyle(Color.brandMutedForeground)

                currencyToggle
                    .padding(.top, Spacing.xs)
            }
            .frame(maxWidth: .infinity)

            AmountNumpad(value: $amountBuffer, allowsDecimal: allowsDecimal)
                .padding(.horizontal, Spacing.l)

            BrandButton(
                "Create invoice",
                variant: .primary,
                size: .large,
                isDisabled: !isAmountValid,
                action: { Task { await createInvoice() } }
            )
            .padding(.horizontal, Spacing.l)

            Spacer(minLength: Spacing.l)
        }
    }

    /// sats ⇄ USD switcher. Mirrors the web's `ConvertedMoneySwitcher`
    /// (`app/features/shared/converted-money-switcher.tsx`) — an
    /// up/down arrow glyph that flips the entry currency. The web
    /// renders the *converted* amount next to the arrow ("≈ 1,234
    /// sats"); we can't here because no exchange-rate symbol is
    /// exported by the FFI (see footer note), so the pill shows the
    /// currency you'd switch *to* instead. Tapping flips and clears
    /// the buffer (the magnitudes aren't comparable without a rate).
    private var currencyToggle: some View {
        Button(action: switchCurrency) {
            HStack(spacing: Spacing.xs) {
                Image(systemName: "arrow.up.arrow.down")
                    .font(.system(size: 12, weight: .semibold))
                Text(otherCurrency.shortLabel)
                    .font(.brandLabel)
            }
            .foregroundStyle(Color.brandMutedForeground)
            .padding(.horizontal, Spacing.m)
            .padding(.vertical, Spacing.s)
            .background(
                Capsule(style: .continuous)
                    .fill(Color.brandMuted)
            )
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Switch to entering the amount in \(otherCurrency.shortLabel)")
    }

    /// The currency the toggle would switch *to*.
    private var otherCurrency: EntryCurrency {
        entryCurrency == .btc ? .usd : .btc
    }

    private func switchCurrency() {
        UISelectionFeedbackGenerator().selectionChanged()
        withAnimation(.easeInOut(duration: 0.18)) {
            entryCurrency = otherCurrency
            // Reset — "100 sats" and "100 USD" aren't equivalent and
            // there's no FFI rate to carry the value across.
            amountBuffer = "0"
        }
    }

    // MARK: - amount parsing

    /// Format the raw buffer with thousands separators for the display
    /// strip. Leaves trailing decimal-in-progress states intact
    /// (`"12."` renders as `"12."` not `"12"`) so the user can see
    /// they're mid-decimal.
    private var displayAmount: String {
        guard let n = UInt64(amountBuffer) else {
            // Mid-decimal or empty — show the raw buffer.
            return amountBuffer.isEmpty ? "0" : amountBuffer
        }
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        formatter.groupingSeparator = ","
        return formatter.string(from: NSNumber(value: n)) ?? amountBuffer
    }

    /// The numpad value converted into the FFI's minor unit:
    ///   - BTC  → integer sats (buffer is already integer sats).
    ///   - USD  → cents (buffer is dollars-with-decimals; "12.5" →
    ///     1250, "12" → 1200). The FFI takes the amount "in the
    ///     account's minor unit (sats for BTC, cents for USD)" per
    ///     `startMintQuote`'s doc-comment, so we scale here rather
    ///     than passing dollars.
    ///
    /// Returns `nil` for empty / mid-decimal / unparseable buffers so
    /// the CTA stays disabled until the value is well-formed.
    private var parsedAmount: UInt64? {
        switch entryCurrency {
        case .btc:
            // Drop a trailing dot ("12." → "12") and parse.
            let clean = amountBuffer.trimmingCharacters(in: CharacterSet(charactersIn: "."))
            return UInt64(clean)
        case .usd:
            // "12" → 1200, "12.5" → 1250, "12.50" → 1250. Reject more
            // than two fractional digits (sub-cent isn't mintable).
            let parts = amountBuffer.split(separator: ".", omittingEmptySubsequences: false)
            guard let whole = UInt64(parts.first ?? "0") else { return nil }
            if parts.count == 1 {
                return whole * 100
            }
            guard parts.count == 2 else { return nil }
            let fracRaw = String(parts[1])
            guard fracRaw.count <= 2 else { return nil }
            // Pad "5" → "50" so the scale is always /100.
            let fracPadded = fracRaw.padding(toLength: 2, withPad: "0", startingAt: 0)
            // Empty fraction ("12.") is treated as ".00".
            let frac = fracPadded.isEmpty ? 0 : UInt64(fracPadded) ?? 0
            return whole * 100 + frac
        }
    }

    private var isAmountValid: Bool {
        guard let n = parsedAmount else { return false }
        return n > 0
    }

    // MARK: - actions

    private func createInvoice() async {
        guard let amount = parsedAmount, amount > 0 else { return }
        phase = .generating
        let outcome = await model.startLightningQuote(
            amount: amount,
            accountId: nil,
            currency: currency
        )
        switch outcome {
        case .success(let handle):
            phase = .invoice(handle)
            startPolling(handle)
        case .failure(let message):
            phase = .failure(message)
        }
    }

    private func startPolling(_ handle: MintQuoteHandle) {
        pollTask?.cancel()
        pollTask = Task {
            // Poll every 2s. Keep going while the view is alive and
            // we're still on the .invoice phase for this handle.
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                if Task.isCancelled { return }
                let outcome = await model.pollLightningQuote(quoteId: handle.quoteId)
                if Task.isCancelled { return }
                switch outcome {
                case .state(let state, let reason):
                    switch state {
                    case .unpaid:
                        // Keep polling.
                        continue
                    case .paid:
                        await MainActor.run { phase = .completing(handle) }
                        await completeQuote(handle)
                        return
                    case .completed:
                        // Server already minted — treat as success.
                        await MainActor.run { phase = .completing(handle) }
                        await completeQuote(handle)
                        return
                    case .expired:
                        await MainActor.run {
                            phase = .failure("Invoice expired before payment. Try again.")
                        }
                        return
                    case .failed:
                        await MainActor.run {
                            phase = .failure(reason ?? "Quote failed.")
                        }
                        return
                    }
                case .failure(let message):
                    // Network blip — log via UI but keep polling so a
                    // transient failure doesn't kick the user out of
                    // the invoice screen. The user can hit Cancel.
                    _ = message
                    continue
                }
            }
        }
    }

    private func completeQuote(_ handle: MintQuoteHandle) async {
        let outcome = await model.completeLightningQuote(quoteId: handle.quoteId)
        switch outcome {
        case .success(let result):
            phase = .success(result)
            scheduleAutoDismiss()
        case .failure(let message):
            phase = .failure(message)
        }
    }

    private func cancelInvoice() {
        pollTask?.cancel()
        phase = .amountEntry
    }

    private func copyInvoice(_ invoice: String) {
        UIPasteboard.general.string = invoice
        UINotificationFeedbackGenerator().notificationOccurred(.success)
    }

    private func resetToAmountEntry() {
        pollTask?.cancel()
        autoDismissTask?.cancel()
        amountBuffer = "0"
        phase = .amountEntry
    }

    private func scheduleAutoDismiss() {
        autoDismissTask?.cancel()
        autoDismissTask = Task {
            try? await Task.sleep(nanoseconds: 4_000_000_000)
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

// MARK: - Invoice card

/// Invoice presentation: QR code + truncated BOLT-11 + amount + fee.
/// The QR is generated synchronously via CoreImage — for a 256pt code
/// it takes <5ms on modern iPhones, no async needed.
private struct InvoiceView: View {
    let handle: MintQuoteHandle
    let onCancel: () -> Void
    let onCopy: (String) -> Void

    var body: some View {
        ScrollView {
            VStack(spacing: Spacing.l) {
                Spacer(minLength: Spacing.l)

                VStack(spacing: Spacing.xs) {
                    HStack(alignment: .lastTextBaseline, spacing: 4) {
                        Text(handle.amount)
                            .font(.brandNumericInline)
                            .foregroundStyle(Color.brandForeground)
                            .monospacedDigit()
                        Text(handle.unit)
                            .font(.brandLabel)
                            .foregroundStyle(Color.brandMutedForeground)
                    }
                    Text("Waiting for payment…")
                        .font(.brandLabel)
                        .foregroundStyle(Color.brandMutedForeground)
                }

                if let qr = qrCode(for: handle.invoice) {
                    Image(uiImage: qr)
                        .interpolation(.none)
                        .resizable()
                        .frame(width: 240, height: 240)
                        .padding(Spacing.s)
                        .background(Color.white)
                        .cornerRadius(Radius.control)
                } else {
                    RoundedRectangle(cornerRadius: Radius.control)
                        .fill(Color.brandMuted)
                        .frame(width: 240, height: 240)
                        .overlay(Text("QR unavailable").font(.brandCaption))
                }

                Button(action: { onCopy(handle.invoice) }) {
                    HStack(spacing: Spacing.xs) {
                        Text(truncated(handle.invoice))
                            .font(.brandCaption)
                            .foregroundStyle(Color.brandMutedForeground)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Image(systemName: "doc.on.doc")
                            .font(.system(size: 12))
                            .foregroundStyle(Color.brandMutedForeground)
                    }
                    .padding(.horizontal, Spacing.m)
                    .padding(.vertical, Spacing.s)
                    .background(
                        RoundedRectangle(cornerRadius: Radius.control)
                            .fill(Color.brandMuted)
                    )
                }
                .buttonStyle(.plain)
                .frame(maxWidth: 280)

                if !handle.fee.isEmpty, handle.fee != "0" {
                    HStack {
                        Text("Mint fee")
                            .font(.brandLabel)
                            .foregroundStyle(Color.brandMutedForeground)
                        Spacer()
                        Text("\(handle.fee) \(handle.unit)")
                            .font(.brandLabel)
                            .foregroundStyle(Color.brandForeground)
                    }
                    .padding(.horizontal, Spacing.m)
                    .frame(maxWidth: 280)
                }

                BrandButton(
                    "Cancel",
                    variant: .ghost,
                    action: onCancel
                )
                .frame(maxWidth: 280)

                Spacer(minLength: Spacing.l)
            }
            .frame(maxWidth: .infinity)
        }
    }

    private func truncated(_ s: String) -> String {
        guard s.count > 24 else { return s }
        let head = s.prefix(12)
        let tail = s.suffix(8)
        return "\(head)…\(tail)"
    }

    /// Synchronously render a BOLT-11 string into a `UIImage` QR code.
    /// CoreImage's `CIQRCodeGenerator` produces a tiny raw bitmap; we
    /// scale it up via a sample affine transform so the image is
    /// crisp at 240pt. `interpolation(.none)` on the SwiftUI side
    /// preserves the hard edges.
    private func qrCode(for string: String) -> UIImage? {
        let data = string.data(using: .utf8) ?? Data()
        let filter = CIFilter.qrCodeGenerator()
        filter.setValue(data, forKey: "inputMessage")
        filter.setValue("M", forKey: "inputCorrectionLevel")
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: 10, y: 10))
        let context = CIContext()
        guard let cg = context.createCGImage(scaled, from: scaled.extent) else { return nil }
        return UIImage(cgImage: cg)
    }
}

// MARK: - Success + failure

private struct SuccessCard: View {
    let result: ReceiveResult
    let onReceiveMore: () -> Void
    let onDone: () -> Void

    var body: some View {
        VStack(spacing: Spacing.xxl) {
            Spacer(minLength: Spacing.xxl)
            VStack(spacing: Spacing.m) {
                Image(systemName: "checkmark.circle.fill")
                    .font(.system(size: 56))
                    .foregroundStyle(Color.green)
                Text("Received")
                    .font(.brandTitle)
                    .foregroundStyle(Color.brandCardForeground)
                HStack(alignment: .lastTextBaseline, spacing: 4) {
                    Text(result.amount)
                        .font(.brandNumericInline)
                        .foregroundStyle(Color.brandCardForeground)
                        .monospacedDigit()
                    Text(result.unit)
                        .font(.brandLabel)
                        .foregroundStyle(Color.brandMutedForeground)
                }
            }
            .frame(maxWidth: .infinity)
            .padding(Spacing.xxl)
            .brandCard()
            .frame(maxWidth: 384)

            VStack(spacing: Spacing.m) {
                BrandButton(
                    "Receive more",
                    variant: .secondary,
                    action: onReceiveMore
                )
                BrandButton(
                    "Done",
                    variant: .primary,
                    action: onDone
                )
            }
            .frame(maxWidth: 384)

            Spacer(minLength: Spacing.xxl)
        }
    }
}

private struct FailureCard: View {
    let message: String
    let onRetry: () -> Void
    let onDismiss: () -> Void

    var body: some View {
        VStack(spacing: Spacing.xxl) {
            Spacer(minLength: Spacing.xxl)
            VStack(spacing: Spacing.m) {
                Image(systemName: "xmark.octagon.fill")
                    .font(.system(size: 48))
                    .foregroundStyle(Color.brandDestructive)
                Text("Couldn't receive")
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
                BrandButton(
                    "Try again",
                    variant: .primary,
                    action: onRetry
                )
                BrandButton(
                    "Dismiss",
                    variant: .ghost,
                    action: onDismiss
                )
            }
            .frame(maxWidth: 384)

            Spacer(minLength: Spacing.xxl)
        }
    }
}

// MARK: - Known gap: secondary "converted amount" line
//
// The web amount-entry screen (`app/features/receive/receive-input.tsx`
// + `app/features/shared/converted-money-switcher.tsx`) renders a
// secondary muted line under the hero amount showing the *converted*
// value ("≈ 1,234 sats" while entering USD, and vice-versa). That
// requires a sat⇄USD exchange rate. The web app gets it client-side
// from `app/lib/exchange-rate/` (mempool.space / coinbase / coingecko
// providers, slice-4 added a mempool.space rate to core).
//
// That rate is NOT exported through the UniFFI binding: there is no
// `exchangeRate` / `btcPrice` / `fiatRate` symbol anywhere in
// `AgicashSDK/agicash_ffi.swift` (only `startMintQuote`, the
// poll/complete trio, and the Lightning-address helpers). So this
// screen ships the toggle WITHOUT the converted secondary line — the
// quote is requested directly in the chosen currency via
// `startMintQuote(currency:)` (the FFI quotes the mint in BTC or USD
// natively, no client conversion needed for correctness). The
// secondary display is a follow-up that needs an exchange-rate FFI
// export, not a hardcoded rate.
