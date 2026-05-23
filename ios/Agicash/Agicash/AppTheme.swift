import SwiftUI

/// Legacy facade over the brand token layer in `DesignSystem/`. The Phase-1
/// screens reach for `AppTheme.background`, `.muted`, `.cardBackground()`
/// etc.; rather than rename every call site, we forward those names to the
/// new `Color.brand*` tokens (Asset Catalog colors with light + dark variants
/// matching `app/tailwind.css`).
///
/// New code should prefer the canonical token API directly:
///
///   `Color.brandBackground`, `Spacing.l`, `Radius.card`, `Font.brandBody`, ...
///
/// See `DesignSystem/Color+Theme.swift`, `Spacing.swift`, `Radius.swift`,
/// `Typography.swift` for the full surface.
enum AppTheme {
    // Computed properties so each access resolves against the current
    // `ThemeStore.shared` state. The `Color.brand*` accessors are themselves
    // computed; making these `static let` would freeze the first resolved
    // value across later theme changes.
    static var background: Color { Color.brandBackground }
    static var card: Color { Color.brandCard }
    static var muted: Color { Color.brandMuted }
    static var foreground: Color { Color.brandForeground }
    static var mutedForeground: Color { Color.brandMutedForeground }
    static var tertiaryForeground: Color { Color.brandTertiaryForeground }
    static var border: Color { Color.brandBorder }
    static var destructive: Color { Color.brandDestructive }
    static var primary: Color { Color.brandPrimary }
    static var primaryForeground: Color { Color.brandPrimaryForeground }

    /// `Radius.card` (8pt) — kept for backward compatibility.
    static let cardCornerRadius: CGFloat = Radius.card
    /// `Radius.control` (6pt) — kept for backward compatibility.
    static let controlCornerRadius: CGFloat = Radius.control
    /// `Spacing.l` (16pt) — kept for backward compatibility.
    static let horizontalPadding: CGFloat = Spacing.l
}

/// Legacy alias for the brand card modifier. Prefer `.brandCard()`.
extension View {
    func cardBackground() -> some View { brandCard() }
}
