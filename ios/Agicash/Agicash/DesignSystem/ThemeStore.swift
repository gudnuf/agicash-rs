import SwiftUI

/// Process-wide mirror of the current `ThemeEnvironment` axes.
///
/// **Why this exists alongside `ThemeEnvironment`:** the brand color tokens
/// (`Color.brandBackground`, `Color.brandPrimary`, …) are consumed at 200+
/// call sites as **static-looking** computed properties. Threading a
/// `ThemeEnvironment` instance through every call site, every modifier, and
/// every helper would be a thousand-line diff for what is effectively a
/// global application-wide setting. Instead the SwiftUI `ThemeEnvironment`
/// (the source of truth that participates in observation) writes through to
/// this singleton on every change, and the `Color.brand*` accessors read
/// from this singleton at render time.
///
/// **Re-render guarantee:** the SwiftUI view tree does NOT observe this
/// singleton directly — observation is on the `ThemeEnvironment` placed in
/// the environment. The root view applies `.id(theme.signature)` so the
/// whole tree is reconstructed when axes flip; child views' subsequent
/// reads of `Color.brand*` see the new values because the store was
/// updated synchronously in `ThemeEnvironment.set(...)` before the next
/// SwiftUI tick.
///
/// **Thread safety:** all writes funnel through `ThemeEnvironment.set(...)`
/// (and its `didSet` chain) which is driven from SwiftUI on the main thread.
/// Reads happen during SwiftUI view-body evaluation, also on the main
/// thread. The class is intentionally NOT `@MainActor` so that
/// `Color.brand*` static-var accessors (which can be touched from any
/// context that builds a `View` body) compile cleanly under Swift 5.9. If
/// we ever introduce off-main reads, switch to atomic storage or formal
/// MainActor isolation.
final class ThemeStore: @unchecked Sendable {
    /// Process-wide singleton.
    static let shared = ThemeStore()

    private(set) var track: ThemeTrack = .btc
    private(set) var mode: ThemeMode = .light

    private init() {}

    func update(track: ThemeTrack, mode: ThemeMode) {
        self.track = track
        self.mode = mode
    }

    /// Convenience for the `Color.brand*` accessors.
    func color(_ token: ThemeToken) -> Color {
        ThemeResolver.resolve(token, track: track, mode: mode)
    }
}
