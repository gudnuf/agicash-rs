import SwiftUI
import UIKit

/// One page of the Send carousel — send to a Lightning Address
/// (LUD-16). Enter `alice@example.com` + an amount, resolve it to a
/// BOLT-11 invoice via the (wallet-agnostic) LUD-16 FFI, then hand the
/// invoice to the shared melt flow (`LightningSendPlaceholderView`)
/// for the confirm → pay → settle steps.
///
/// State machine:
///   - `entry`            → address field + amount numpad + Continue.
///   - `resolving`        → spinner while `resolveLnAddressInvoice`
///     runs (well-known lookup + LUD-06 callback).
///   - `melt(invoice)`    → embeds the shared bolt11 melt view with
///     the resolved invoice preset, so the address path and the
///     paste-invoice path drive one identical melt state machine.
///   - `failure(message)` → inline error + retry → entry.
///
/// The file name keeps the historical
/// `LightningAddressSendPlaceholderView` identifier so
/// `SendCarouselView`'s `.tag(.lightningAddress)` wiring and the Xcode
/// project membership stay stable across the placeholder → real-view
/// swap.
///
/// Mirrors the web `app/features/send/send-input.tsx`
/// (`resolve-destination.ts` → `LN_ADDRESS` branch → `use-get-invoice-
/// from-lud16.ts`): the address resolves to an invoice, then the same
/// `PayBolt11Confirmation` runs.
struct LightningAddressSendPlaceholderView: View {
    @Bindable var model: WalletViewModel
    let onDismissCarousel: () -> Void

    enum Phase: Equatable {
        case entry
        case resolving
        case melt(String)
        case failure(String)
    }

    @State private var address: String = ""
    @State private var amountBuffer: String = "0"
    @State private var phase: Phase = .entry
    @FocusState private var addressFocused: Bool

    /// BTC / sats only for v0 — same constraint as the other send
    /// views. The resolved invoice carries the amount; the source
    /// account currency picks the mint.
    private let currency = "BTC"
    private let unitLabel = "sats"

    var body: some View {
        VStack(spacing: 0) {
            content
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private var content: some View {
        switch phase {
        case .entry:
            entryView
        case .resolving:
            ProgressView("Resolving address…")
                .font(.brandLabel)
                .foregroundStyle(Color.brandMutedForeground)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .melt(let invoice):
            // Hand the resolved invoice to the shared melt flow. It
            // owns the confirm → pay → settle → dismiss lifecycle and
            // bounces the whole carousel on done/cancel (there's no
            // invoice to re-edit on the address path).
            LightningSendPlaceholderView(
                model: model,
                onDismissCarousel: onDismissCarousel,
                presetInvoice: invoice
            )
        case .failure(let message):
            FailureCard(
                message: message,
                onRetry: resetToEntry,
                onDismiss: onDismissCarousel
            )
            .padding(.horizontal, Spacing.l)
        }
    }

    private var entryView: some View {
        ScrollView {
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
                    Text("Send to a Lightning Address")
                        .font(.brandLabel)
                        .foregroundStyle(Color.brandMutedForeground)
                }
                .frame(maxWidth: .infinity)

                VStack(alignment: .leading, spacing: Spacing.s) {
                    HStack {
                        Text("Lightning Address")
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

                    TextField("alice@example.com", text: $address)
                        .textFieldStyle(BrandTextFieldStyle(isFocused: addressFocused))
                        .focused($addressFocused)
                        .autocorrectionDisabled()
                        .textInputAutocapitalization(.never)
                        .keyboardType(.emailAddress)
                }
                .frame(maxWidth: 384)
                .padding(.horizontal, Spacing.l)

                AmountNumpad(value: $amountBuffer, allowsDecimal: false)
                    .padding(.horizontal, Spacing.l)

                BrandButton(
                    "Continue",
                    variant: .primary,
                    size: .large,
                    isDisabled: !isValid,
                    action: { Task { await resolve() } }
                )
                .padding(.horizontal, Spacing.l)

                Spacer(minLength: Spacing.l)
            }
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
    }

    // MARK: - amount parsing (same shape as SendCashuTokenView)

    private var displayAmount: String {
        guard let n = UInt64(amountBuffer) else {
            return amountBuffer.isEmpty ? "0" : amountBuffer
        }
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        formatter.groupingSeparator = ","
        return formatter.string(from: NSNumber(value: n)) ?? amountBuffer
    }

    private var parsedAmount: UInt64? {
        let clean = amountBuffer.trimmingCharacters(in: CharacterSet(charactersIn: "."))
        return UInt64(clean)
    }

    private var isValid: Bool {
        guard let n = parsedAmount, n > 0 else { return false }
        let addr = address.trimmingCharacters(in: .whitespacesAndNewlines)
        // Minimal client-side shape check (`x@y`); the FFI does the
        // authoritative LUD-16 validation + lowercasing.
        return addr.contains("@") && !addr.hasPrefix("@") && !addr.hasSuffix("@")
    }

    // MARK: - actions

    private func pasteFromClipboard() {
        guard let pasted = UIPasteboard.general.string else { return }
        address = pasted.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func resolve() async {
        guard let amount = parsedAmount, amount > 0 else { return }
        addressFocused = false
        phase = .resolving
        let outcome = await model.resolveLnAddressInvoice(
            address: address,
            amountSats: amount,
            comment: nil
        )
        switch outcome {
        case .success(let invoice, _):
            phase = .melt(invoice)
        case .failure(let message):
            phase = .failure(message)
        }
    }

    private func resetToEntry() {
        phase = .entry
    }
}

// MARK: - Failure card (same shape as the Lightning send view's)

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
                Text("Couldn't resolve")
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
                BrandButton("Try again", variant: .primary, action: onRetry)
                BrandButton("Dismiss", variant: .ghost, action: onDismiss)
            }
            .frame(maxWidth: 384)

            Spacer(minLength: Spacing.xxl)
        }
    }
}
