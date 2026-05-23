import SwiftUI

/// Brand color tokens — semantic names matching the web app's CSS variables
/// in `app/tailwind.css` (`--background`, `--foreground`, `--primary`, …).
///
/// **Resolution model (changed 2026-05-22):** these accessors used to be
/// stored properties reading `Asset Catalog` entries with light + dark
/// appearance variants. The asset-catalog approach couples color resolution
/// to iOS's system appearance trait — it can't represent the web's
/// **two-axis** theme model (currency track × color mode) and can't honor a
/// default of `.btc` while the system is in light mode.
///
/// They are now **computed properties** evaluated at render time against
/// `ThemeStore.shared`, which mirrors the SwiftUI `ThemeEnvironment` injected
/// at the app root. Default `(track: .btc, mode: .light)` matches React's
/// `defaultTheme = 'btc'` boot (`app/features/theme/theme-context.tsx`).
///
/// The legacy asset catalog entries under `Assets.xcassets/Colors/*.colorset`
/// remain on disk as documentation of the old palette but are NO LONGER READ
/// by these accessors. They can be deleted in a follow-up; left for now to
/// keep this lane's diff scoped to the resolver introduction.
///
/// Use these instead of `Color(.systemBackground)` / `.label`. The system
/// equivalents drift from the web palette and (more importantly) ignore the
/// app's BTC/USD currency theme entirely.
///
/// See `ThemeEnvironment.swift`, `ThemeResolver.swift`, `ThemeStore.swift`.
extension Color {
    /// `--background` — root canvas. Deep BTC-blue on default boot.
    static var brandBackground: Color { ThemeStore.shared.color(.background) }
    /// `--foreground` — primary text on background.
    static var brandForeground: Color { ThemeStore.shared.color(.foreground) }

    /// `--card` — card surfaces.
    static var brandCard: Color { ThemeStore.shared.color(.card) }
    /// `--card-foreground` — text on cards.
    static var brandCardForeground: Color { ThemeStore.shared.color(.cardForeground) }

    /// `--primary` — primary CTA fill. Used by the default `<Button>` variant
    /// on web.
    static var brandPrimary: Color { ThemeStore.shared.color(.primary) }
    /// `--primary-foreground` — text on primary fills.
    static var brandPrimaryForeground: Color { ThemeStore.shared.color(.primaryForeground) }

    /// `--secondary` — secondary/ghost surface (light gray in light mode,
    /// near-black in dark). Used by the `secondary` button variant on web.
    static var brandSecondary: Color { ThemeStore.shared.color(.secondary) }
    /// `--secondary-foreground` — text on secondary fills.
    static var brandSecondaryForeground: Color { ThemeStore.shared.color(.secondaryForeground) }

    /// `--muted` — muted surface for inputs and resting controls.
    static var brandMuted: Color { ThemeStore.shared.color(.muted) }
    /// `--muted-foreground` — secondary/helper text. Matches web's
    /// `text-muted-foreground` exactly.
    static var brandMutedForeground: Color { ThemeStore.shared.color(.mutedForeground) }
    /// 50% opacity of muted foreground — web uses `text-muted-foreground/50`
    /// for tertiary text (e.g. the domain in a lightning address).
    static var brandTertiaryForeground: Color {
        ThemeStore.shared.color(.mutedForeground).opacity(0.5)
    }

    /// `--accent` — hover surface (mirrors `--secondary` in shadcn neutral).
    static var brandAccent: Color { ThemeStore.shared.color(.accent) }
    /// `--accent-foreground` — text on accent fills.
    static var brandAccentForeground: Color { ThemeStore.shared.color(.accentForeground) }

    /// `--destructive` — destructive action color (red).
    static var brandDestructive: Color { ThemeStore.shared.color(.destructive) }
    /// `--destructive-foreground` — text on destructive fills.
    static var brandDestructiveForeground: Color { ThemeStore.shared.color(.destructiveForeground) }

    /// `--border` — subtle hairline border on cards / inputs.
    static var brandBorder: Color { ThemeStore.shared.color(.border) }
    /// `--input` — input border (same value as `--border` on web).
    static var brandInput: Color { ThemeStore.shared.color(.input) }
    /// `--ring` — focus ring color.
    static var brandRing: Color { ThemeStore.shared.color(.ring) }
}
