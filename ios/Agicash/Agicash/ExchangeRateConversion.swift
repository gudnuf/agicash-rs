import Foundation

/// Client-side BTC⇄USD conversion helper, layered on top of the FFI's
/// `getExchangeRate` (`AgicashSDK/agicash_ffi.swift` →
/// `ExchangeRateSnapshot`). Mirrors the web app's converted-amount line
/// (`app/features/shared/converted-money-switcher.tsx` /
/// `MoneyWithConvertedAmount`): the wallet keeps balances and quotes in
/// each account's native currency, and this layer only produces the
/// secondary "≈ X" display figure — it never feeds an FFI argument, so a
/// missing/garbled rate degrades to "no secondary line", never a wrong
/// quote.
///
/// Everything here is minor-unit in / minor-unit out (sat for BTC, cent
/// for USD/USDB) to match `AccountFfi.balance` and the
/// `startMintQuote(amount:)` convention used across the app. The pure
/// value-type shape keeps the math unit-testable without an FFI handle.
struct ExchangeRateConversion {
    /// Price of `1` major unit of `from` in major units of `to`, parsed
    /// from `ExchangeRateSnapshot.rate` (decimal string). Held as a
    /// `Decimal` so the sat-scale (1e8) round-trip doesn't lose precision
    /// the way a `Double` would.
    let rate: Decimal
    /// Canonical upper-case source currency code (`BTC`, `USD`, `USDB`),
    /// echoed back by the FFI.
    let from: String
    /// Canonical upper-case target currency code.
    let to: String

    /// Build from the FFI snapshot. Returns `nil` when the rate string
    /// can't be parsed or is non-positive — the caller then simply omits
    /// the secondary line (the documented rate-unavailable behaviour)
    /// instead of rendering a bogus "≈".
    init?(snapshot: ExchangeRateSnapshot) {
        guard let parsed = Decimal(string: snapshot.rate, locale: Locale(identifier: "en_US_POSIX")),
              parsed > 0
        else { return nil }
        self.rate = parsed
        self.from = snapshot.from.uppercased()
        self.to = snapshot.to.uppercased()
    }

    /// Minor units per `1` major unit, for the currencies the
    /// mempool.space provider prices. `BTC` → 100_000_000 sat,
    /// `USD`/`USDB` → 100 cent. `nil` for anything we don't model so the
    /// conversion bails cleanly rather than guessing a scale.
    private static func minorPerMajor(_ currency: String) -> Decimal? {
        switch currency.uppercased() {
        case "BTC": return 100_000_000
        case "USD", "USDB": return 100
        default: return nil
        }
    }

    /// Convert `minorAmount` (in the smallest unit of `sourceCurrency`)
    /// into the smallest unit of `targetCurrency`, rounding to a whole
    /// minor unit (sub-sat / sub-cent isn't displayable). Returns `nil`
    /// when the snapshot's pair doesn't cover the requested direction or
    /// a currency isn't modelled — the caller omits the ≈ line.
    ///
    /// `rate` is "1 major `from` = rate major `to`", so for the forward
    /// direction:
    ///   minor_to = minorAmount / minorPerMajor(from)   // → major from
    ///            * rate                                 // → major to
    ///            * minorPerMajor(to)                    // → minor to
    /// and the reverse direction divides by `rate` instead.
    func convert(
        minorAmount: Decimal,
        from sourceCurrency: String,
        to targetCurrency: String
    ) -> Decimal? {
        let src = sourceCurrency.uppercased()
        let tgt = targetCurrency.uppercased()
        guard src != tgt else { return minorAmount }

        guard let srcScale = Self.minorPerMajor(src),
              let tgtScale = Self.minorPerMajor(tgt)
        else { return nil }

        let majorFrom = minorAmount / srcScale
        let majorTo: Decimal
        if src == from && tgt == to {
            majorTo = majorFrom * rate
        } else if src == to && tgt == from {
            majorTo = majorFrom / rate
        } else {
            // The snapshot priced a different pair than asked. Don't
            // guess — surface "unavailable" to the caller.
            return nil
        }

        var raw = majorTo * tgtScale
        var rounded = Decimal()
        NSDecimalRound(&rounded, &raw, 0, .plain)
        return rounded
    }

    /// Convenience for the common UInt64 minor-unit call sites
    /// (`AccountFfi.balance` parsed total, parsed numpad value). Returns
    /// the converted whole-minor figure, or `nil` to omit the ≈ line.
    func convert(
        minorAmount: UInt64,
        from sourceCurrency: String,
        to targetCurrency: String
    ) -> Decimal? {
        convert(
            minorAmount: Decimal(minorAmount),
            from: sourceCurrency,
            to: targetCurrency
        )
    }
}

/// Display helpers shared by the balance hero and the LN amount-entry so
/// the "≈ 1,234 sats" / "≈ $12.34" string is formatted the same way in
/// both places (and the same way the web's converted line groups
/// thousands).
enum ConvertedAmountFormatter {
    /// Group a whole-minor figure with thousands separators (sats / cents
    /// are integers). Matches `LightningReceiveView.displayAmount` and
    /// `BalanceHero.formatDecimal`.
    static func groupedMinor(_ value: Decimal) -> String {
        var copy = value
        var rounded = Decimal()
        NSDecimalRound(&rounded, &copy, 0, .plain)
        let formatter = NumberFormatter()
        formatter.numberStyle = .decimal
        formatter.groupingSeparator = ","
        formatter.maximumFractionDigits = 0
        return formatter.string(from: NSDecimalNumber(decimal: rounded))
            ?? NSDecimalNumber(decimal: rounded).stringValue
    }

    /// Render a USD/USDB cent figure as dollars-with-cents ("1234" →
    /// "$12.34"). BTC stays integer sats via `groupedMinor`.
    static func usdFromCents(_ cents: Decimal) -> String {
        let dollars = cents / 100
        let formatter = NumberFormatter()
        formatter.numberStyle = .currency
        formatter.currencyCode = "USD"
        formatter.currencySymbol = "$"
        return formatter.string(from: NSDecimalNumber(decimal: dollars))
            ?? "$\(NSDecimalNumber(decimal: dollars).stringValue)"
    }

    /// Build the full secondary "≈ …" string for a converted whole-minor
    /// figure in `currency`. BTC → "≈ 1,234 sats", USD/USDB → "≈ $12.34".
    /// Returns `nil` for a currency we don't model (caller omits the
    /// line).
    static func approxLine(minor: Decimal, currency: String) -> String? {
        switch currency.uppercased() {
        case "BTC":
            let unit = minor == 1 ? "sat" : "sats"
            return "≈ \(groupedMinor(minor)) \(unit)"
        case "USD", "USDB":
            return "≈ \(usdFromCents(minor))"
        default:
            return nil
        }
    }
}
