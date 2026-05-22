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
 * The web app composes themes on two independent axes:
 *   - currency: `usd` | `btc` (overrides on top of the light root)
 *   - color mode: `light` | `dark` (`.dark` wins over the currency theme)
 *
 * React's default at boot is `btc` + light mode, so this Android scaffold
 * defaults to the same: light mode uses the BTC-light track, dark mode uses
 * the shadcn dark root. Theme switching (currency + dark mode toggle) is a
 * future lane — keep this file color-only.
 *
 * HSL → sRGB conversions computed deterministically from `design/tokens.json`.
 * If you need to update a value, update tokens.json first, recompute, then
 * paste the hex here and cite the source token in the comment.
 */

// ---------------------------------------------------------------------------
// Light theme = BTC-light track (React's default at boot)
// ---------------------------------------------------------------------------

/** btc.primary             — hsl(219 44% 45%) */
private val PrimaryLight = Color(0xFF4064A5)

/** btc.primary_foreground  — hsl(217 30% 90%) */
private val OnPrimaryLight = Color(0xFFDEE4ED)

/** btc.background          — hsl(217 68% 35%) */
private val BackgroundLight = Color(0xFF1D4B96)

/** btc.foreground          — hsl(217 30% 90%) */
private val OnBackgroundLight = Color(0xFFDEE4ED)

/** btc.card                — hsl(217 68% 38%) */
private val SurfaceLight = Color(0xFF1F52A3)

/** light.card_foreground   — hsl(0 0% 98%) */
private val OnSurfaceLight = Color(0xFFFAFAFA)

/** btc.muted               — hsl(217 68% 38%) */
private val SurfaceVariantLight = Color(0xFF1F52A3)

/** btc.muted_foreground    — hsl(217 30% 85%) */
private val OnSurfaceVariantLight = Color(0xFFCDD6E4)

/** light.secondary         — hsl(0 0% 96.1%) */
private val SecondaryLight = Color(0xFFF5F5F5)

/** light.secondary_foreground — hsl(0 0% 9%) */
private val OnSecondaryLight = Color(0xFF171717)

/** btc.border              — hsl(217 70% 45%) */
private val OutlineLight = Color(0xFF2260C3)

/** light.destructive       — hsl(0 84.2% 60.2%) */
private val ErrorLight = Color(0xFFEF4444)

/** light.destructive_foreground — hsl(0 0% 98%) */
private val OnErrorLight = Color(0xFFFAFAFA)

// ---------------------------------------------------------------------------
// Dark theme = shadcn dark root
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

private val LightColors = lightColorScheme(
    primary = PrimaryLight,
    onPrimary = OnPrimaryLight,
    secondary = SecondaryLight,
    onSecondary = OnSecondaryLight,
    background = BackgroundLight,
    onBackground = OnBackgroundLight,
    surface = SurfaceLight,
    onSurface = OnSurfaceLight,
    surfaceVariant = SurfaceVariantLight,
    onSurfaceVariant = OnSurfaceVariantLight,
    outline = OutlineLight,
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

@Composable
fun AgicashTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    // We intentionally do NOT use dynamicColor (the user's Material You
    // accent) so the brand palette stays consistent across devices.
    content: @Composable () -> Unit,
) {
    val colorScheme = if (darkTheme) DarkColors else LightColors
    val view = LocalView.current
    if (!view.isInEditMode) {
        SideEffect {
            val window = (view.context as Activity).window
            WindowCompat.getInsetsController(window, view).isAppearanceLightStatusBars = !darkTheme
        }
    }
    MaterialTheme(
        colorScheme = colorScheme,
        typography = AgicashTypography,
        content = content,
    )
}
