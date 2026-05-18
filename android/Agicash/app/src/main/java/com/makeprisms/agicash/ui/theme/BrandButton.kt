package com.makeprisms.agicash.ui.theme

import androidx.compose.foundation.BorderStroke
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.defaultMinSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp

/**
 * Compose analogue of `ios/.../DesignSystem/BrandButton.swift`. Covers the
 * three variants the iOS screens actually use:
 *
 *   - `Primary`     — `bg-primary text-primary-foreground`
 *   - `Secondary`   — bordered card-background button (Receive on home)
 *   - `Destructive` — sign-out
 *   - `Ghost`       — borderless inline action
 *
 * `size = large` matches the iOS `h-11 / py-6 text-lg` chunky rectangle
 * used on the home screen; `size = medium` matches the 40dp default.
 *
 * The loading state hides the label and overlays a spinner so the height
 * never shifts mid-press, same as iOS.
 */
enum class BrandButtonVariant { Primary, Secondary, Destructive, Ghost }
enum class BrandButtonSize { Medium, Large }

@Composable
fun BrandButton(
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    variant: BrandButtonVariant = BrandButtonVariant.Primary,
    size: BrandButtonSize = BrandButtonSize.Medium,
    isLoading: Boolean = false,
    enabled: Boolean = true,
) {
    val heightDp = if (size == BrandButtonSize.Large) 52.dp else 40.dp
    val horizontalPad = if (size == BrandButtonSize.Large) Spacing.xxl else Spacing.l

    val content: @Composable () -> Unit = {
        Box(
            modifier = Modifier.fillMaxWidth().height(heightDp),
            contentAlignment = Alignment.Center,
        ) {
            if (isLoading) {
                CircularProgressIndicator(
                    modifier = Modifier.size(20.dp),
                    strokeWidth = 2.dp,
                    color = when (variant) {
                        BrandButtonVariant.Primary -> BrandColors.primaryForeground
                        BrandButtonVariant.Secondary -> BrandColors.cardForeground
                        BrandButtonVariant.Destructive -> BrandColors.primaryForeground
                        BrandButtonVariant.Ghost -> BrandColors.foreground
                    },
                )
            } else {
                Text(text = label, style = BrandTypography.bodyEmphasis)
            }
        }
    }

    when (variant) {
        BrandButtonVariant.Primary -> Button(
            onClick = onClick,
            enabled = enabled && !isLoading,
            modifier = modifier.fillMaxWidth().defaultMinSize(minHeight = heightDp),
            shape = Radius.control,
            colors = ButtonDefaults.buttonColors(
                containerColor = BrandColors.primary,
                contentColor = BrandColors.primaryForeground,
            ),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = horizontalPad),
            content = { content() },
        )
        BrandButtonVariant.Secondary -> OutlinedButton(
            onClick = onClick,
            enabled = enabled && !isLoading,
            modifier = modifier.fillMaxWidth().defaultMinSize(minHeight = heightDp),
            shape = Radius.control,
            border = BorderStroke(0.5.dp, BrandColors.border),
            colors = ButtonDefaults.outlinedButtonColors(
                containerColor = BrandColors.card,
                contentColor = BrandColors.cardForeground,
            ),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = horizontalPad),
            content = { content() },
        )
        BrandButtonVariant.Destructive -> Button(
            onClick = onClick,
            enabled = enabled && !isLoading,
            modifier = modifier.fillMaxWidth().defaultMinSize(minHeight = heightDp),
            shape = Radius.control,
            colors = ButtonDefaults.buttonColors(
                containerColor = BrandColors.destructive,
                contentColor = BrandColors.primaryForeground,
            ),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = horizontalPad),
            content = { content() },
        )
        BrandButtonVariant.Ghost -> TextButton(
            onClick = onClick,
            enabled = enabled && !isLoading,
            modifier = modifier.fillMaxWidth().defaultMinSize(minHeight = heightDp),
            shape = Radius.control,
            colors = ButtonDefaults.textButtonColors(
                contentColor = BrandColors.foreground,
            ),
            contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = horizontalPad),
            content = { content() },
        )
    }
}
