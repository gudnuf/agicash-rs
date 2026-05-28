import SwiftUI

/// Runtime theme state — the iOS equivalent of React's `ThemeProvider`
/// (`app/features/theme/theme-provider.tsx` + the `:root`/`.btc`/`.usd`/`.dark`
/// CSS-variable bundles in `app/tailwind.css`).
///
/// Two independent axes, mirrored from the web's two-class scoping mechanism:
///
/// - **`track`** — the currency theme (`.btc` deep-blue or `.usd` deep-teal).
///   Defaults to `.btc` to match React's `defaultTheme = 'btc'` boot. **Not a
///   user-facing toggle.** On web the track is auto-derived from the user's
///   default currency (`useSyncThemeWithDefaultCurrency` in `wallet.tsx`:
///   `defaultCurrency === 'BTC' ? 'btc' : 'usd'`), NOT a switch in settings.
///   iOS keeps the same model: there is deliberately no UI to flip the track.
/// - **`colorMode`** — the user's *preference*: `.light` / `.dark` / `.system`.
///   Default `.system` matches React's `defaultColorMode = 'system'`
///   (`theme.constants.ts`). This IS the user-facing switch (see
///   `ColorModeToggle` on web → `ColorModePicker` in iOS `SettingsView`).
///
/// The resolver consumes a concrete two-value `ThemeMode` (`.light` / `.dark`);
/// `.system` is collapsed to the live OS color scheme via `systemColorMode`,
/// mirroring React's `effectiveColorMode = colorMode === 'system' ?
/// systemColorMode : colorMode`.
///
/// **Persistence:** `track` + `colorMode` are written to `UserDefaults` on
/// every change and rehydrated at construction, so the selection survives an
/// app restart — the iOS analogue of React's theme cookies. First launch (no
/// stored value) yields `(track: .btc, colorMode: .system)`.
///
/// **Why an `@Observable` class and not a SwiftUI environment value carrying
/// raw enums:** the brand-color tokens (`Color.brandBackground`, etc.) are
/// consumed at 214+ call sites as **static-looking** properties. Threading a
/// theme value through every view + every modifier is invasive. Instead this
/// type is injected at the app root, mirrored into `ThemeStore.shared` on
/// change (see `ThemeStore.swift`), and the existing `Color.brand*` accessors
/// read from that store at render time. Views that participate in the
/// SwiftUI Environment system (via `@Environment(\.theme)`) get
/// re-evaluation for free when the theme changes; the root view also bumps
/// a `.id(theme.signature)` to guarantee a full redraw across the screen.
@Observable
final class ThemeEnvironment {
    /// `UserDefaults` keys for the persisted axes. Names rhyme with the web's
    /// cookie names (`theme`, `color-mode`) under an app-scoped prefix.
    private enum DefaultsKey {
        static let track = "agicash.theme.track"
        static let colorMode = "agicash.theme.colorMode"
    }

    /// Currency theme — the `.btc` / `.usd` axis. Default `.btc` boots into
    /// the React app's `defaultTheme = 'btc'` deep-blue palette. Persisted so a
    /// future default-currency sync survives restart, but not user-switchable.
    var track: ThemeTrack {
        didSet {
            persist()
            syncStore()
        }
    }
    /// User color-mode *preference* — `.light` / `.dark` / `.system`. This is
    /// the value the switcher writes. Default `.system`. Persisted to
    /// `UserDefaults`.
    var colorMode: ColorMode {
        didSet {
            persist()
            syncStore()
        }
    }
    /// The live OS color scheme (`.light` / `.dark`). Fed from the SwiftUI
    /// `\.colorScheme` environment at the app root. Only consulted when
    /// `colorMode == .system`. Mirrors React's `systemColorMode` (read from
    /// `matchMedia('(prefers-color-scheme: dark)')`).
    var systemColorMode: ThemeMode {
        didSet { syncStore() }
    }

    /// Effective two-value mode handed to the resolver. Mirrors React's
    /// `effectiveColorMode`.
    var mode: ThemeMode {
        switch colorMode {
        case .light: return .light
        case .dark: return .dark
        case .system: return systemColorMode
        }
    }

    init(
        track: ThemeTrack = .btc,
        colorMode: ColorMode = .system,
        systemColorMode: ThemeMode = .light,
        defaults: UserDefaults = .standard
    ) {
        self.defaults = defaults
        // Rehydrate persisted selection; fall back to the passed defaults
        // (which themselves default to the React boot values) on first launch.
        let storedTrack = defaults.string(forKey: DefaultsKey.track)
            .flatMap(ThemeTrack.init(rawValue:))
        let storedMode = defaults.string(forKey: DefaultsKey.colorMode)
            .flatMap(ColorMode.init(rawValue:))
        self.track = storedTrack ?? track
        self.colorMode = storedMode ?? colorMode
        self.systemColorMode = systemColorMode
        // Sync to the store at construction so the very first read of
        // `Color.brandBackground` already sees the resolved palette.
        syncStore()
    }

    private let defaults: UserDefaults

    /// Stable identity that changes iff the resolved palette would change.
    /// Used by the root view as a `.id(...)` value to force a full SwiftUI
    /// re-evaluation when the theme axes change (the `Color.brand*` accessors
    /// read from `ThemeStore.shared` and are otherwise not observed). Keyed on
    /// the *effective* axes (`track` + `mode`) — switching `colorMode` between
    /// `.light` and an equal `.system`-resolved value is a visual no-op and
    /// correctly does not bump the signature.
    var signature: Int {
        var hasher = Hasher()
        hasher.combine(track)
        hasher.combine(mode)
        return hasher.finalize()
    }

    /// Apply a theme change. Mirrors React's `setTheme` / `setColorMode` +
    /// cookie write — now WITH persistence (handled by the property `didSet`).
    func set(track: ThemeTrack? = nil, colorMode: ColorMode? = nil) {
        if let track { self.track = track }
        if let colorMode { self.colorMode = colorMode }
    }

    /// Write the persisted axes to `UserDefaults`. Called on every change to
    /// `track` / `colorMode` (system scheme is OS-derived, never stored).
    private func persist() {
        defaults.set(track.rawValue, forKey: DefaultsKey.track)
        defaults.set(colorMode.rawValue, forKey: DefaultsKey.colorMode)
    }

    /// Mirror the current *effective* axes into `ThemeStore.shared`. Called on
    /// initialization and on every property change via `didSet`. Theme reads
    /// (e.g. `Color.brand*`) all happen on the main thread (SwiftUI body
    /// evaluation), so a same-thread write before the next view-body pass is
    /// enough to ensure consistency.
    private func syncStore() {
        ThemeStore.shared.update(track: track, mode: mode)
    }
}

/// Currency theme axis. Mirrors the `.btc` / `.usd` CSS classes on `<html>`
/// in `app/tailwind.css:48-59` and `app/tailwind.css:34-45`.
enum ThemeTrack: String, Hashable, Sendable, CaseIterable {
    case btc
    case usd
}

/// Effective color mode handed to the resolver. Two values only — `.system`
/// is collapsed to one of these by `ThemeEnvironment.mode`. Mirrors `.dark`
/// on `<html>` in `app/tailwind.css:62-89`; "light" is the unmarked root.
enum ThemeMode: String, Hashable, Sendable, CaseIterable {
    case light
    case dark
}

/// User color-mode *preference*. Mirrors React's `ColorMode`
/// (`app/features/theme/theme.types.ts`): `'light' | 'dark' | 'system'`.
/// `.system` follows the OS appearance. This is the value the user picks in
/// the appearance switcher and the value persisted to `UserDefaults`.
enum ColorMode: String, Hashable, Sendable, CaseIterable {
    case light
    case dark
    case system
}

// MARK: - SwiftUI Environment plumbing

private struct ThemeEnvironmentKey: EnvironmentKey {
    static let defaultValue: ThemeEnvironment = ThemeEnvironment()
}

extension EnvironmentValues {
    /// The current theme. Inject at the app root with
    /// `.environment(\.theme, ThemeEnvironment(...))`. Subviews read via
    /// `@Environment(\.theme) private var theme`.
    var theme: ThemeEnvironment {
        get { self[ThemeEnvironmentKey.self] }
        set { self[ThemeEnvironmentKey.self] = newValue }
    }
}
