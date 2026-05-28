package com.makeprisms.agicash.ui.theme

import android.app.Activity
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.SideEffect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalView
import androidx.core.view.WindowCompat

/**
 * Material 3 palette mirroring the web app's canonical Tailwind tokens.
 *
 * Source of truth: `design/tokens.json` (extracted from
 * MakePrisms/agicash @ 5751be2e, Tailwind 4.1.18).
 *
 * The web app composes themes on two independent axes (see React
 * `app/features/theme/theme-provider.tsx` + `app/tailwind.css`):
 *   - **track**: `btc` | `usd` — currency-theme overrides applied on top of
 *     the light root (`.btc` / `.usd` class on `<html>`).
 *   - **color mode**: `light` | `dark` — `.dark` wins over the currency track
 *     when both classes are present (dark appears later in the CSS cascade).
 *
 * React's default at boot is `btc` track + `system` color mode, but the very
 * first paint with no cookie set resolves to light (`defaultSystemColorMode`).
 * This Android file is now axis-driven: [AgicashTheme] takes the resolved
 * `(track, mode)` and selects the matching palette. The track switch is NOT a
 * user control on any client — React derives it from the default currency
 * (`useSyncThemeWithDefaultCurrency`). The user-facing control is the color
 * mode (light/dark/system); track stays persisted + ready for currency sync.
 *
 * HSL → sRGB conversions computed deterministically from `design/tokens.json`.
 * If you need to update a value, update tokens.json first, recompute, then
 * paste the hex here and cite the source token in the comment.
 */

// ---------------------------------------------------------------------------
// Light root (themes.light) — the unmarked base the currency tracks override.
// Only the tokens a currency track overrides are pulled out per-track below;
// the remaining slots (secondary, error, …) come from this shared light root.
// ---------------------------------------------------------------------------

/** light.secondary         — hsl(0 0% 96.1%) */
private val SecondaryLight = Color(0xFFF5F5F5)

/** light.secondary_foreground — hsl(0 0% 9%) */
private val OnSecondaryLight = Color(0xFF171717)

/** light.card_foreground   — hsl(0 0% 98%) */
private val OnSurfaceLight = Color(0xFFFAFAFA)

/** light.destructive       — hsl(0 84.2% 60.2%) */
private val ErrorLight = Color(0xFFEF4444)

/** light.destructive_foreground — hsl(0 0% 98%) */
private val OnErrorLight = Color(0xFFFAFAFA)

// ---------------------------------------------------------------------------
// BTC-light track — themes.btc overrides (deep blue). React `defaultTheme`.
// ---------------------------------------------------------------------------

/** btc.primary             — hsl(219 44% 45%) */
private val PrimaryBtc = Color(0xFF4064A5)

/** btc.primary_foreground  — hsl(217 30% 90%) */
private val OnPrimaryBtc = Color(0xFFDEE4ED)

/** btc.background          — hsl(217 68% 35%) */
private val BackgroundBtc = Color(0xFF1D4B96)

/** btc.foreground          — hsl(217 30% 90%) */
private val OnBackgroundBtc = Color(0xFFDEE4ED)

/** btc.card                — hsl(217 68% 38%) */
private val SurfaceBtc = Color(0xFF1F52A3)

/** btc.muted               — hsl(217 68% 38%) */
private val SurfaceVariantBtc = Color(0xFF1F52A3)

/** btc.muted_foreground    — hsl(217 30% 85%) */
private val OnSurfaceVariantBtc = Color(0xFFCDD6E4)

/** btc.border              — hsl(217 70% 45%) */
private val OutlineBtc = Color(0xFF2260C3)

// ---------------------------------------------------------------------------
// USD-light track — themes.usd overrides (deep teal).
// ---------------------------------------------------------------------------

/** usd.primary             — hsl(177 42% 26%) */
private val PrimaryUsd = Color(0xFF265E5B)

/** usd.primary_foreground  — hsl(178 30% 90%) */
private val OnPrimaryUsd = Color(0xFFDEEDED)

/** usd.background          — hsl(178 100% 15%) */
private val BackgroundUsd = Color(0xFF004C4A)

/** usd.foreground          — hsl(178 30% 90%) */
private val OnBackgroundUsd = Color(0xFFDEEDED)

/** usd.card                — hsl(178 100% 14%) */
private val SurfaceUsd = Color(0xFF004745)

/** usd.muted               — hsl(178 100% 14%) */
private val SurfaceVariantUsd = Color(0xFF004745)

/** usd.muted_foreground    — hsl(178 30% 81%) */
private val OnSurfaceVariantUsd = Color(0xFFC0DDDC)

/** usd.border              — hsl(178 100% 21%) */
private val OutlineUsd = Color(0xFF006B68)

// ---------------------------------------------------------------------------
// Dark root (themes.dark = shadcn dark). Per the React cascade, dark wins over
// the currency track, so there is one dark palette regardless of track.
// ---------------------------------------------------------------------------

/** dark.primary            — hsl(202 13% 13%) */
private val PrimaryDark = Color(0xFF1D2225)

/** dark.primary_foreground — hsl(0 0% 98%) */
private val OnPrimaryDark = Color(0xFFFAFAFA)

/** dark.background         — hsl(0 0% 3.9%) */
private val BackgroundDark = Color(0xFF0A0A0A)

/** dark.foreground         — hsl(0 0% 98%) */
private val OnBackgroundDark = Color(0xFFFAFAFA)

/** dark.card               — hsl(0 0% 3.9%) */
private val SurfaceDark = Color(0xFF0A0A0A)

/** dark.card_foreground    — hsl(0 0% 98%) */
private val OnSurfaceDark = Color(0xFFFAFAFA)

/** dark.muted              — hsl(0 0% 12%) */
private val SurfaceVariantDark = Color(0xFF1F1F1F)

/** dark.muted_foreground   — hsl(0 0% 63.9%) */
private val OnSurfaceVariantDark = Color(0xFFA3A3A3)

/** dark.secondary          — hsl(0 0% 14.9%) */
private val SecondaryDark = Color(0xFF262626)

/** dark.secondary_foreground — hsl(0 0% 98%) */
private val OnSecondaryDark = Color(0xFFFAFAFA)

/** dark.border             — hsl(0 0% 14.9%) */
private val OutlineDark = Color(0xFF262626)

/** dark.destructive        — hsl(0 62.8% 30.6%) */
private val ErrorDark = Color(0xFF7F1D1D)

/** dark.destructive_foreground — hsl(0 0% 98%) */
private val OnErrorDark = Color(0xFFFAFAFA)

private val BtcLightColors = lightColorScheme(
    primary = PrimaryBtc,
    onPrimary = OnPrimaryBtc,
    secondary = SecondaryLight,
    onSecondary = OnSecondaryLight,
    background = BackgroundBtc,
    onBackground = OnBackgroundBtc,
    surface = SurfaceBtc,
    onSurface = OnSurfaceLight,
    surfaceVariant = SurfaceVariantBtc,
    onSurfaceVariant = OnSurfaceVariantBtc,
    outline = OutlineBtc,
    error = ErrorLight,
    onError = OnErrorLight,
)

private val UsdLightColors = lightColorScheme(
    primary = PrimaryUsd,
    onPrimary = OnPrimaryUsd,
    secondary = SecondaryLight,
    onSecondary = OnSecondaryLight,
    background = BackgroundUsd,
    onBackground = OnBackgroundUsd,
    surface = SurfaceUsd,
    onSurface = OnSurfaceLight,
    surfaceVariant = SurfaceVariantUsd,
    onSurfaceVariant = OnSurfaceVariantUsd,
    outline = OutlineUsd,
    error = ErrorLight,
    onError = OnErrorLight,
)

private val DarkColors = darkColorScheme(
    primary = PrimaryDark,
    onPrimary = OnPrimaryDark,
    secondary = SecondaryDark,
    onSecondary = OnSecondaryDark,
    background = BackgroundDark,
    onBackground = OnBackgroundDark,
    surface = SurfaceDark,
    onSurface = OnSurfaceDark,
    surfaceVariant = SurfaceVariantDark,
    onSurfaceVariant = OnSurfaceVariantDark,
    outline = OutlineDark,
    error = ErrorDark,
    onError = OnErrorDark,
)

/**
 * Root theme. Resolves the active palette from the two axes:
 *
 *   - [track]: which currency palette to overlay in light mode.
 *   - [mode]: light / dark / system. `system` falls back to the platform
 *     dark-mode setting via [isSystemInDarkTheme] — the Compose analogue of
 *     React's `prefers-color-scheme` / `matchMedia` resolution.
 *
 * Per the React cascade, dark mode wins over the currency track, so when the
 * effective mode is dark we use the single shadcn-dark palette regardless of
 * track.
 */
@Composable
fun AgicashTheme(
    track: ThemeTrack = ThemeTrack.Btc,
    mode: ColorMode = ColorMode.Light,
    // We intentionally do NOT use dynamicColor (the user's Material You
    // accent) so the brand palette stays consistent across devices.
    content: @Composable () -> Unit,
) {
    val isDark = when (mode) {
        ColorMode.Light -> false
        ColorMode.Dark -> true
        ColorMode.System -> isSystemInDarkTheme()
    }
    val colorScheme = when {
        isDark -> DarkColors
        track == ThemeTrack.Usd -> UsdLightColors
        else -> BtcLightColors
    }
    val view = LocalView.current
    if (!view.isInEditMode) {
        SideEffect {
            val window = (view.context as Activity).window
            WindowCompat.getInsetsController(window, view).isAppearanceLightStatusBars = !isDark
        }
    }
    MaterialTheme(
        colorScheme = colorScheme,
        typography = AgicashTypography,
        content = content,
    )
}
