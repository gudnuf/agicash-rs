package com.makeprisms.agicash.ui.theme

import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.makeprisms.agicash.R

/**
 * Brand spacing tokens. Mirrors `ios/Agicash/Agicash/DesignSystem/Spacing.swift`
 * (Tailwind scale: 4/8/12/16/20/24/32/48). Use these instead of bare `.dp`
 * literals so the cadence matches the iOS app exactly.
 */
object Spacing {
    /** Tailwind 1 (4dp). */
    val xs = 4.dp
    /** Tailwind 2 (8dp) — default gap between siblings inside a card. */
    val s = 8.dp
    /** Tailwind 3 (12dp) — input padding. */
    val m = 12.dp
    /** Tailwind 4 (16dp) — page padding, gap between form rows. */
    val l = 16.dp
    /** Tailwind 5 (20dp) — card inner padding. */
    val xl = 20.dp
    /** Tailwind 6 (24dp) — section gap. */
    val xxl = 24.dp
    /** Tailwind 8 (32dp) — large section gap. */
    val xxxl = 32.dp
    /** Tailwind 12 (48dp) — hero spacing. */
    val hero = 48.dp
}

/**
 * Brand corner-radius tokens. Mirrors `DesignSystem/Radius.swift`.
 */
object Radius {
    /** rounded-lg — card surfaces (8dp). */
    val card = RoundedCornerShape(8.dp)
    /** rounded-md — buttons, inputs, list rows (6dp). */
    val control = RoundedCornerShape(6.dp)
}

/**
 * Brand font families. Mirrors `DesignSystem/Typography.swift`:
 *
 *   `Kode Mono` — primary (body, labels, titles)
 *   `Teko`      — numeric (balance hero, money displays)
 *
 * Both fonts are bundled under `res/font/` as variable TTFs (lifted from
 * `ios/Agicash/Agicash/Resources/Fonts/`).
 */
object BrandFont {
    val primary: FontFamily = FontFamily(Font(R.font.kode_mono))
    val numeric: FontFamily = FontFamily(Font(R.font.teko))
}

/**
 * Convenience aliases — mirror iOS `Font.brandBody` / `.brandTitle` /
 * `.brandNumericHero` etc. Used directly inline so screens read like their
 * iOS counterparts.
 */
object BrandTypography {
    /** Default body text (text-base / 16sp). */
    val body = TextStyle(fontFamily = BrandFont.primary, fontSize = 16.sp)
    /** Body text with semibold weight (buttons, emphasised rows). */
    val bodyEmphasis = TextStyle(
        fontFamily = BrandFont.primary,
        fontSize = 16.sp,
        fontWeight = FontWeight.SemiBold,
    )
    /** Small label (text-sm / 14sp). */
    val label = TextStyle(fontFamily = BrandFont.primary, fontSize = 14.sp)
    /** Medium-weight label, above inputs. */
    val labelEmphasis = TextStyle(
        fontFamily = BrandFont.primary,
        fontSize = 14.sp,
        fontWeight = FontWeight.Medium,
    )
    /** Caption / footnote (text-xs / 12sp). */
    val caption = TextStyle(fontFamily = BrandFont.primary, fontSize = 12.sp)
    /** Small heading (text-xl / 20sp). */
    val titleSmall = TextStyle(
        fontFamily = BrandFont.primary,
        fontSize = 20.sp,
        fontWeight = FontWeight.SemiBold,
    )
    /** Section heading (text-2xl / 24sp). */
    val title = TextStyle(
        fontFamily = BrandFont.primary,
        fontSize = 24.sp,
        fontWeight = FontWeight.Bold,
    )
    /** Hero heading (text-4xl / 36sp). */
    val titleLarge = TextStyle(
        fontFamily = BrandFont.primary,
        fontSize = 36.sp,
        fontWeight = FontWeight.Bold,
    )
    /** Hero numeric (text-6xl / 60sp Teko bold) — balance hero. */
    val numericHero = TextStyle(
        fontFamily = BrandFont.numeric,
        fontSize = 60.sp,
        fontWeight = FontWeight.Bold,
    )
    /** Inline numeric (text-2xl / 24sp Teko semibold) — row balances. */
    val numericInline = TextStyle(
        fontFamily = BrandFont.numeric,
        fontSize = 24.sp,
        fontWeight = FontWeight.SemiBold,
    )
}

/**
 * Brand color tokens. The Material 3 `colorScheme` already carries the
 * canonical palette (`Theme.kt`); these aliases let screens read closer to
 * iOS code (`Color.brandPrimary`, `Color.brandMutedForeground`).
 */
object BrandColors {
    val background: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.background
    val foreground: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.onBackground
    val card: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.surface
    val cardForeground: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.onSurface
    val muted: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.surfaceVariant
    val mutedForeground: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.onSurfaceVariant
    val border: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.outline
    val primary: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.primary
    val primaryForeground: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.onPrimary
    val destructive: Color @Composable @ReadOnlyComposable get() = MaterialTheme.colorScheme.error
}
