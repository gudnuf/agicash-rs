package com.makeprisms.agicash.ui.theme

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Brightness6
import androidx.compose.material.icons.outlined.DarkMode
import androidx.compose.material.icons.outlined.LightMode
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp

/**
 * Color-mode switcher. 1:1 mirror of React's `ColorModeToggle`
 * (`app/features/theme/color-mode-toggle.tsx`):
 *
 *   - A single trigger button showing the current mode's icon.
 *   - Tapping opens a dropdown listing all three modes (`light`, `dark`,
 *     `system` — React `colorModes` order), each row an icon + capitalized
 *     label, picking one calls `setColorMode`.
 *   - Icons map to React's lucide icons: light → Sun, dark → Moon,
 *     system → SunMoon (Material's closest equivalents: LightMode, DarkMode,
 *     Brightness6).
 *
 * This is the ONLY user-facing theme control React exposes — the currency
 * track is derived from the default currency, not user-selectable.
 */

/** React `colorModes` constant order: light, dark, system. */
private val colorModeOptions = listOf(ColorMode.Light, ColorMode.Dark, ColorMode.System)

private fun iconFor(mode: ColorMode): ImageVector = when (mode) {
    ColorMode.Light -> Icons.Outlined.LightMode
    ColorMode.Dark -> Icons.Outlined.DarkMode
    ColorMode.System -> Icons.Outlined.Brightness6
}

private fun labelFor(mode: ColorMode): String = when (mode) {
    ColorMode.Light -> "Light"
    ColorMode.Dark -> "Dark"
    ColorMode.System -> "System"
}

@Composable
fun ColorModeToggle(
    colorMode: ColorMode,
    onSelect: (ColorMode) -> Unit,
    modifier: Modifier = Modifier,
) {
    var expanded by remember { mutableStateOf(false) }

    IconButton(
        onClick = { expanded = true },
        modifier = modifier,
    ) {
        Icon(
            imageVector = iconFor(colorMode),
            contentDescription = "Current color mode: ${labelFor(colorMode)}. Tap to switch.",
            tint = BrandColors.foreground,
            modifier = Modifier.size(20.dp),
        )
    }

    DropdownMenu(
        expanded = expanded,
        onDismissRequest = { expanded = false },
    ) {
        for (mode in colorModeOptions) {
            DropdownMenuItem(
                text = {
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(8.dp),
                    ) {
                        Icon(
                            imageVector = iconFor(mode),
                            contentDescription = null,
                            modifier = Modifier.size(20.dp),
                        )
                        Text(text = labelFor(mode), style = BrandTypography.body)
                    }
                },
                onClick = {
                    expanded = false
                    onSelect(mode)
                },
            )
        }
    }
}
