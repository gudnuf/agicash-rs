import SwiftUI

/// Resolves a semantic theme token (`background`, `primary`, `muted`, …) to a
/// concrete sRGB `Color` given the active theme axes (track + mode).
///
/// **Source of truth:** `~/agicash/design/tokens.json` — `colors.themes.light`,
/// `colors.themes.dark`, `colors.themes.btc`, `colors.themes.usd`. Those values
/// are themselves verbatim from `app/tailwind.css` at React commit `5751be2e`.
///
/// **Composition rules** (mirror `app/tailwind.css` cascade):
///
/// 1. **Light mode**: start from `themes.light`. If `track == .btc`, overlay
///    `themes.btc` overrides on top. If `track == .usd`, overlay `themes.usd`
///    overrides. (Both currency themes only override 8 of the 25 light tokens;
///    the rest fall through to light.)
/// 2. **Dark mode**: start from `themes.dark` and DO NOT layer the currency
///    overrides on top. The React cascade specifies that `.dark` appears
///    later in the CSS, so dark variables win when both `.dark` and a
///    currency class are present (see `tokens.json` `theme_scoping_mechanism`
///    + `app/tailwind.css:47-89`).
///
/// **HSL → sRGB conversion** is done at compile time below (every literal is
/// the result of the standard HSL-to-RGB formula at full s/l precision; the
/// 0..1 floats round-trip exactly through `Color(red:green:blue:)`).
enum ThemeToken: Hashable, CaseIterable {
    case background
    case foreground
    case card
    case cardForeground
    case popover
    case popoverForeground
    case primary
    case primaryForeground
    case secondary
    case secondaryForeground
    case muted
    case mutedForeground
    case accent
    case accentForeground
    case destructive
    case destructiveForeground
    case border
    case input
    case ring
}

enum ThemeResolver {
    /// Resolve a token to a concrete sRGB color under the given axes.
    static func resolve(
        _ token: ThemeToken,
        track: ThemeTrack,
        mode: ThemeMode
    ) -> Color {
        switch mode {
        case .dark:
            // Dark wins over currency track per React cascade.
            return darkValue(token)
        case .light:
            // Currency track overrides selected light tokens.
            if let overridden = trackOverride(token, track: track) {
                return overridden
            }
            return lightValue(token)
        }
    }

    // MARK: - themes.light (all 25 tokens — tokens.json themes.light)

    private static func lightValue(_ token: ThemeToken) -> Color {
        switch token {
        // hsl(0 0% 100%)
        case .background: return Color(red: 1.000, green: 1.000, blue: 1.000)
        // hsl(0 0% 3.9%)
        case .foreground: return Color(red: 0.039, green: 0.039, blue: 0.039)
        // hsl(0 0% 100%)
        case .card: return Color(red: 1.000, green: 1.000, blue: 1.000)
        // hsl(0 0% 98%)
        case .cardForeground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(0 0% 100%)
        case .popover: return Color(red: 1.000, green: 1.000, blue: 1.000)
        // hsl(0 0% 3.9%)
        case .popoverForeground: return Color(red: 0.039, green: 0.039, blue: 0.039)
        // hsl(0 0% 9%)
        case .primary: return Color(red: 0.090, green: 0.090, blue: 0.090)
        // hsl(0 0% 98%)
        case .primaryForeground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(0 0% 96.1%)
        case .secondary: return Color(red: 0.961, green: 0.961, blue: 0.961)
        // hsl(0 0% 9%)
        case .secondaryForeground: return Color(red: 0.090, green: 0.090, blue: 0.090)
        // hsl(0 0% 96.1%)
        case .muted: return Color(red: 0.961, green: 0.961, blue: 0.961)
        // hsl(0 0% 45.1%)
        case .mutedForeground: return Color(red: 0.451, green: 0.451, blue: 0.451)
        // hsl(0 0% 96.1%)
        case .accent: return Color(red: 0.961, green: 0.961, blue: 0.961)
        // hsl(0 0% 9%)
        case .accentForeground: return Color(red: 0.090, green: 0.090, blue: 0.090)
        // hsl(0 84.2% 60.2%) → R=0.937 G=0.267 B=0.267
        case .destructive: return Color(red: 0.937, green: 0.267, blue: 0.267)
        // hsl(0 0% 98%)
        case .destructiveForeground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(0 0% 89.8%)
        case .border: return Color(red: 0.898, green: 0.898, blue: 0.898)
        // hsl(0 0% 89.8%)
        case .input: return Color(red: 0.898, green: 0.898, blue: 0.898)
        // hsl(0 0% 83.1%)
        case .ring: return Color(red: 0.831, green: 0.831, blue: 0.831)
        }
    }

    // MARK: - themes.dark (tokens.json themes.dark)

    private static func darkValue(_ token: ThemeToken) -> Color {
        switch token {
        // hsl(0 0% 3.9%)
        case .background: return Color(red: 0.039, green: 0.039, blue: 0.039)
        // hsl(0 0% 98%)
        case .foreground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(0 0% 3.9%)
        case .card: return Color(red: 0.039, green: 0.039, blue: 0.039)
        // hsl(0 0% 98%)
        case .cardForeground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(0 0% 3.9%)
        case .popover: return Color(red: 0.039, green: 0.039, blue: 0.039)
        // hsl(0 0% 98%)
        case .popoverForeground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(202 13% 13%) → R=0.115 G=0.130 B=0.145
        case .primary: return Color(red: 0.115, green: 0.130, blue: 0.145)
        // hsl(0 0% 98%)
        case .primaryForeground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(0 0% 14.9%)
        case .secondary: return Color(red: 0.149, green: 0.149, blue: 0.149)
        // hsl(0 0% 98%)
        case .secondaryForeground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(0 0% 12%)
        case .muted: return Color(red: 0.120, green: 0.120, blue: 0.120)
        // hsl(0 0% 63.9%)
        case .mutedForeground: return Color(red: 0.639, green: 0.639, blue: 0.639)
        // hsl(0 0% 14.9%)
        case .accent: return Color(red: 0.149, green: 0.149, blue: 0.149)
        // hsl(0 0% 98%)
        case .accentForeground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(0 62.8% 30.6%) → R=0.498 G=0.114 B=0.114
        case .destructive: return Color(red: 0.498, green: 0.114, blue: 0.114)
        // hsl(0 0% 98%)
        case .destructiveForeground: return Color(red: 0.980, green: 0.980, blue: 0.980)
        // hsl(0 0% 14.9%)
        case .border: return Color(red: 0.149, green: 0.149, blue: 0.149)
        // hsl(0 0% 14.9%)
        case .input: return Color(red: 0.149, green: 0.149, blue: 0.149)
        // hsl(0 0% 83.1%)
        case .ring: return Color(red: 0.831, green: 0.831, blue: 0.831)
        }
    }

    // MARK: - currency-track overrides (light mode only — dark wins per cascade)
    //
    // Returns nil when the track does NOT override the requested token; caller
    // falls through to `lightValue`. Matches the 8-token override set declared
    // in `tokens.json` themes.btc and themes.usd.

    private static func trackOverride(_ token: ThemeToken, track: ThemeTrack) -> Color? {
        switch track {
        case .btc:
            switch token {
            // hsl(217 68% 35%) → R=0.112 G=0.294 B=0.588
            case .background: return Color(red: 0.112, green: 0.294, blue: 0.588)
            // hsl(217 30% 90%) → R=0.870 G=0.900 B=0.930
            case .foreground: return Color(red: 0.870, green: 0.900, blue: 0.930)
            // hsl(219 44% 45%) → R=0.252 G=0.396 B=0.648
            case .primary: return Color(red: 0.252, green: 0.396, blue: 0.648)
            // hsl(217 30% 90%) → R=0.870 G=0.900 B=0.930
            case .primaryForeground: return Color(red: 0.870, green: 0.900, blue: 0.930)
            // hsl(217 68% 38%) → R=0.122 G=0.317 B=0.638
            case .muted: return Color(red: 0.122, green: 0.317, blue: 0.638)
            // hsl(217 30% 85%) → R=0.805 G=0.850 B=0.895
            case .mutedForeground: return Color(red: 0.805, green: 0.850, blue: 0.895)
            // hsl(217 70% 45%) → R=0.135 G=0.378 B=0.765
            case .border: return Color(red: 0.135, green: 0.378, blue: 0.765)
            // hsl(217 68% 38%) → R=0.122 G=0.317 B=0.638
            case .card: return Color(red: 0.122, green: 0.317, blue: 0.638)
            default: return nil
            }
        case .usd:
            switch token {
            // hsl(178 100% 15%) → R=0.0 G=0.300 B=0.290
            case .background: return Color(red: 0.000, green: 0.300, blue: 0.290)
            // hsl(178 30% 90%) → R=0.870 G=0.930 B=0.928
            case .foreground: return Color(red: 0.870, green: 0.930, blue: 0.928)
            // hsl(177 42% 26%) → R=0.151 G=0.369 B=0.366
            case .primary: return Color(red: 0.151, green: 0.369, blue: 0.366)
            // hsl(178 30% 90%) → R=0.870 G=0.930 B=0.928
            case .primaryForeground: return Color(red: 0.870, green: 0.930, blue: 0.928)
            // hsl(178 100% 14%) → R=0.0 G=0.280 B=0.271
            case .muted: return Color(red: 0.000, green: 0.280, blue: 0.271)
            // hsl(178 30% 81%) → R=0.749 G=0.881 B=0.876
            case .mutedForeground: return Color(red: 0.749, green: 0.881, blue: 0.876)
            // hsl(178 100% 21%) → R=0.0 G=0.420 B=0.406
            case .border: return Color(red: 0.000, green: 0.420, blue: 0.406)
            // hsl(178 100% 14%) → R=0.0 G=0.280 B=0.271
            case .card: return Color(red: 0.000, green: 0.280, blue: 0.271)
            default: return nil
            }
        }
    }
}
