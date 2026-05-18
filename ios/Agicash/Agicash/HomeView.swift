import Foundation
import SwiftUI

/// Home / accounts overview. Mirrors `app/routes/_protected._index.tsx`:
/// a centered balance at the top (no label, no "Total Balance" header on
/// web) and the receive/send action stack.
///
/// Web does NOT render an accounts list on home — accounts live under
/// `/settings/accounts`. Payment flows mostly live in dedicated lanes;
/// Receive opens `ReceiveCarouselView` (cashu paste / Lightning / Buy
/// tabs), Send is still stubbed.
///
/// Buy used to be a standalone secondary button next to Receive; the
/// 2026-05-15 receive-UX redesign folded it into the Receive carousel
/// as a third tab so the Home grid simplifies to Receive + Send.
/// See `docs/superpowers/specs/2026-05-15-ios-receive-ux-redesign.md`.
struct HomeView: View {
    @Bindable var model: WalletViewModel

    /// Drives presentation of `ReceiveCarouselView` as a sheet. The web
    /// routes to `/receive` which is a separate page; on iOS a `.sheet`
    /// is the closer-to-native equivalent and avoids rebuilding the
    /// nav stack. Stays at this level (not on the action grid) so the
    /// sheet's dismissal cleanly returns control to the home scroll
    /// view.
    @State private var showReceive: Bool = false
    /// Drives presentation of `SendCarouselView` as a sheet. Sibling
    /// of `showReceive`; same `.sheet` pattern.
    @State private var showSend: Bool = false

    var body: some View {
        NavigationStack {
            ScrollView {
                VStack(spacing: Spacing.xxxl) {
                    BalanceHero(model: model)
                        .padding(.top, Spacing.hero)

                    HomeActionGrid(
                        onReceive: { showReceive = true },
                        onSend: { showSend = true }
                    )
                    .padding(.horizontal, Spacing.l)
                }
                .padding(.bottom, Spacing.xxl)
                .frame(maxWidth: .infinity)
            }
            .background(Color.brandBackground.ignoresSafeArea())
            .navigationTitle("")
            .navigationBarTitleDisplayMode(.inline)
            .refreshable { await model.refreshAccounts() }
            .task {
                // First-paint load only. The recurring 4s foreground poll
                // and the scenePhase re-sync that used to live here were
                // deleted with slice 10 Tier-2: the Rust realtime channel
                // now pushes balance changes, and its `onConnected`
                // (re)join catch-up refetch — Supabase Realtime has no
                // replay — covers the foreground-return / missed-while-
                // -backgrounded gap the old poll + scenePhase block
                // handled. No recurring timer remains on Home.
                //
                // This single shot stays on the default (non-background)
                // refresh path so a genuine first-load failure keeps its
                // explicit error UX, exactly as before.
                await model.refreshAccounts()
            }
            .sheet(isPresented: $showReceive) {
                ReceiveCarouselView(
                    model: model,
                    onDismiss: { showReceive = false }
                )
            }
            .sheet(isPresented: $showSend) {
                SendCarouselView(
                    model: model,
                    onDismiss: { showSend = false }
                )
            }
        }
    }
}

/// Centered balance display modeled on `MoneyWithConvertedAmount` on the
/// web home: large numeric on top, smaller FX-converted amount below in
/// muted gray. Numeric uses `Font.brandNumericHero` (Teko Bold).
///
/// Web aggregates by currency via `useBalance('BTC') / useBalance('USD')`
/// in `app/routes/_protected._index.tsx` and renders the user's default
/// currency with a converted secondary line via `MoneyWithConvertedAmount`
/// (`app/features/shared/`). Phase 1 iOS has no user-prefs surface yet, so
/// the primary currency is inferred from the accounts list (USD wins when
/// both are present, mirroring the historical default) and only that
/// currency's per-account balances are summed. The secondary line now
/// shows the *FX-converted* primary total in the other currency via the
/// `getExchangeRate` FFI (`ExchangeRateConversion`) — true web parity.
/// When the rate is unavailable (provider/network blip, single-currency
/// wallet with nothing meaningful to convert to) the line is simply
/// omitted; the primary value always renders.
private struct BalanceHero: View {
    @Bindable var model: WalletViewModel

    /// The converted secondary line ("≈ 1,234 sats" / "≈ $12.34"), or
    /// `nil` while loading / when the rate is unavailable. Refreshed by
    /// the `.task` below whenever the accounts list changes.
    @State private var convertedLine: String?

    private var accounts: [AccountFfi] { model.accounts }

    var body: some View {
        VStack(spacing: Spacing.s) {
            HStack(alignment: .lastTextBaseline, spacing: 4) {
                // Currency symbol — small, like Teko's prefix on web.
                Text(primarySymbol)
                    .font(.system(size: 28, weight: .semibold, design: .rounded))
                    .foregroundStyle(Color.brandForeground)
                    .baselineOffset(8)
                Text(primaryAmountText)
                    .font(.brandNumericHero)
                    .foregroundStyle(Color.brandForeground)
                    .monospacedDigit()
            }
            // Rate-unavailable: omit the line entirely rather than render
            // a stale/placeholder "≈" — the hero stays valid on one line.
            if let convertedLine {
                Text(convertedLine)
                    .font(.brandLabel)
                    .foregroundStyle(Color.brandMutedForeground)
            }
        }
        .frame(maxWidth: .infinity)
        // Re-derive the converted line whenever the balances change (a
        // receive/send/poll mutates `model.accounts`). `accounts` is
        // Equatable so `.task(id:)` only re-runs on a real change, not
        // every SwiftUI re-render.
        .task(id: accounts) {
            await refreshConvertedLine()
        }
    }

    /// Fetch the rate for primary→secondary and format the ≈ line.
    /// Clears the line (no FX shown) when there's no meaningful
    /// conversion target or the rate is unavailable.
    private func refreshConvertedLine() async {
        guard let secondaryCurrency else {
            convertedLine = nil
            return
        }
        let primaryMinor = totalForCurrency(primaryCurrency)
        guard let conversion = await model.exchangeRate(
            from: primaryCurrency,
            to: secondaryCurrency
        ) else {
            convertedLine = nil
            return
        }
        guard let convertedMinor = conversion.convert(
            minorAmount: primaryMinor,
            from: primaryCurrency,
            to: secondaryCurrency
        ) else {
            convertedLine = nil
            return
        }
        convertedLine = ConvertedAmountFormatter.approxLine(
            minor: convertedMinor,
            currency: secondaryCurrency
        )
    }

    /// Currency the secondary line converts *into*. We always convert the
    /// primary total across the BTC⇄USD pair the provider prices: if the
    /// primary is USD show its sats equivalent, and vice-versa. (USDB has
    /// no provider pair, so a USDB-only wallet yields `nil` → no line.)
    private var secondaryCurrency: String? {
        switch primaryCurrency {
        case "USD": return "BTC"
        case "BTC": return "USD"
        default: return nil
        }
    }

    /// Pick the most prominent currency symbol from the accounts we know
    /// about. Defaults to "$" since most users land in USD.
    private var primarySymbol: String {
        let currencies = Set(accounts.map(\.currency))
        if currencies.contains("USD") { return "$" }
        if currencies.contains("BTC") { return "₿" }
        return "$"
    }

    /// The currency we're summing for the primary line. Matches
    /// `primarySymbol` so the prefix and numeric agree.
    private var primaryCurrency: String {
        let currencies = Set(accounts.map(\.currency))
        if currencies.contains("USD") { return "USD" }
        if currencies.contains("BTC") { return "BTC" }
        return "USD"
    }

    /// Sum of `balance` (in smallest units) for accounts matching
    /// `primaryCurrency`. The FFI emits balances as decimal strings of the
    /// minor unit (`sat` for BTC, `cent` for USD/USDB); we display the raw
    /// minor-unit total so we never lose precision in the parse round-trip.
    /// Major-unit formatting (BTC, dollars) lands when the iOS app grows a
    /// currency formatter pass.
    private var primaryAmountText: String {
        let total = totalForCurrency(primaryCurrency)
        return formatDecimal(total)
    }

    /// Walk the accounts list, summing balances for the matching currency.
    /// Decimal parsing tolerates the FFI's string shape (`"0"`, `"64"`)
    /// and silently skips non-numeric entries so a malformed row from a
    /// future FFI surface can't crash the hero.
    private func totalForCurrency(_ currency: String) -> Decimal {
        accounts
            .filter { $0.currency == currency }
            .reduce(Decimal.zero) { acc, account in
                acc + (Decimal(string: account.balance) ?? .zero)
            }
    }

    private func formatDecimal(_ value: Decimal) -> String {
        // Default decimal description renders integer values without a
        // trailing decimal point; minor units are always integers so this
        // is fine. Localisation pass lands with the rest of the
        // currency-formatter work.
        var copy = value
        var rounded = Decimal()
        NSDecimalRound(&rounded, &copy, 0, .plain)
        return NSDecimalNumber(decimal: rounded).stringValue
    }
}

/// Receive / Send button stack. Buy used to live next to Receive as a
/// peer secondary button; the 2026-05-15 receive-UX redesign folded
/// it into the Receive carousel as a third tab so the Home surface
/// stays focused on the two primary intents (someone-pay-me /
/// I-pay-someone). See
/// `docs/superpowers/specs/2026-05-15-ios-receive-ux-redesign.md`.
///
/// Receive opens `ReceiveCarouselView`. Send remains a stub — separate
/// lane (slice 8 / Lightning send).
private struct HomeActionGrid: View {
    let onReceive: () -> Void
    let onSend: () -> Void

    var body: some View {
        VStack(spacing: Spacing.l) {
            BrandButton(
                "Receive",
                variant: .secondary,
                size: .large,
                action: onReceive
            )
            BrandButton(
                "Send",
                variant: .primary,
                size: .large,
                action: onSend
            )
        }
        .frame(maxWidth: 288)
        .frame(maxWidth: .infinity) // center the 288pt column.
    }
}
