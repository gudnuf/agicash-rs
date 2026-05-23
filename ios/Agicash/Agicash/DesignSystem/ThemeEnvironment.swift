import SwiftUI

/// Runtime theme state — the iOS equivalent of React's `ThemeProvider`
/// (`app/features/theme/theme-context.tsx` + the `:root`/`.btc`/`.usd`/`.dark`
/// CSS-variable bundles in `app/tailwind.css`).
///
/// Two independent axes, mirrored from the web's two-class scoping mechanism:
///
/// - **`track`** — the currency theme (`.btc` deep-blue or `.usd` deep-teal).
///   Defaults to `.btc` to match React's `defaultTheme = 'btc'` boot.
/// - **`mode`** — light vs dark. The cookie/UserDefaults-persisted color mode
///   axis. Default `.light` matches the web's first-paint when no cookie set.
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
///
/// **Not yet wired:** there is no UI to switch theme; this lane lays the
/// infrastructure. The user-facing switcher + persistence are the next lane.
@Observable
final class ThemeEnvironment {
    /// Currency theme — the `.btc` / `.usd` axis. Default `.btc` boots into
    /// the React app's `defaultTheme = 'btc'` deep-blue palette.
    var track: ThemeTrack {
        didSet { syncStore() }
    }
    /// Color mode — the `light` / `dark` axis. Default `.light` matches the
    /// web's first-paint when no `theme-color-mode` cookie is set.
    var mode: ThemeMode {
        didSet { syncStore() }
    }

    init(track: ThemeTrack = .btc, mode: ThemeMode = .light) {
        self.track = track
        self.mode = mode
        // Sync to the store at construction so the very first read of
        // `Color.brandBackground` already sees the new defaults.
        syncStore()
    }

    /// Stable identity that changes iff the resolved palette would change.
    /// Used by the root view as a `.id(...)` value to force a full SwiftUI
    /// re-evaluation when the theme axes change (the `Color.brand*` accessors
    /// read from `ThemeStore.shared` and are otherwise not observed).
    var signature: Int {
        var hasher = Hasher()
        hasher.combine(track)
        hasher.combine(mode)
        return hasher.finalize()
    }

    /// Apply a theme change. Mirrors React's `setTheme` + cookie write — but
    /// without persistence (that lands with the switcher lane).
    func set(track: ThemeTrack? = nil, mode: ThemeMode? = nil) {
        if let track { self.track = track }
        if let mode { self.mode = mode }
    }

    /// Mirror the current axes into `ThemeStore.shared`. Called on
    /// initialization and on every property change via `didSet`. Theme
    /// reads (e.g. `Color.brand*`) all happen on the main thread (SwiftUI
    /// body evaluation), so a same-thread write before the next view-body
    /// pass is enough to ensure consistency.
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

/// Color mode axis. Mirrors `.dark` on `<html>` in `app/tailwind.css:62-89`.
/// "Light" is the unmarked root.
enum ThemeMode: String, Hashable, Sendable, CaseIterable {
    case light
    case dark
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
