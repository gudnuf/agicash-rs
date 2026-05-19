package com.makeprisms.agicash.ui.components

import android.view.HapticFeedbackConstants
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.Backspace
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.makeprisms.agicash.ui.theme.BrandColors
import com.makeprisms.agicash.ui.theme.Spacing

/**
 * Custom 3×4 amount numpad. Compose analogue of
 * `ios/Agicash/Agicash/AmountNumpad.swift`.
 *
 * Banking-app-style amount entry: digits 0-9, an optional decimal
 * point, and a backspace key. The platform soft-keyboard is rejected
 * for the same two reasons the iOS view documents — it eats the bottom
 * half of the screen (no room for the live amount + breakdown + CTA)
 * and it doesn't fire per-tap haptics. Banking apps (Cash App, Venmo,
 * Apple Wallet) all use a custom keypad with per-press haptics; that
 * tactile rhythm is what makes amount entry feel native.
 *
 * `value` is the parent's raw buffer string ("0", "1", "1.5",
 * "12345"). This component only manipulates the raw string so the
 * display strip can render in-progress states like "1." that a parsed
 * number would collapse. Parsing is the parent's responsibility (same
 * split as the iOS `parsedAmount`).
 *
 * Accumulator semantics match iOS 1:1: digits append, leading-zero
 * replacement, `.` is a no-op when the buffer already has one or when
 * decimals are disabled, `⌫` pops the last char (long-press clears).
 */
@Composable
fun AmountNumpad(
    value: String,
    onValueChange: (String) -> Unit,
    allowsDecimal: Boolean,
    modifier: Modifier = Modifier,
    maxDigits: Int = 9,
) {
    val view = LocalView.current

    fun haptic(constant: Int) {
        view.performHapticFeedback(constant)
    }

    fun appendDigit(digit: String) {
        // Count only digit chars so a decimal point doesn't eat into
        // the cap ("1.5" is 2 useful digits, not 3).
        val digitCount = value.count { it.isDigit() }
        if (digitCount >= maxDigits) {
            haptic(HapticFeedbackConstants.REJECT)
            return
        }
        haptic(HapticFeedbackConstants.KEYBOARD_TAP)
        if (value == "0") {
            // Replace the leading zero so we don't end up with "01".
            onValueChange(digit)
        } else {
            onValueChange(value + digit)
        }
    }

    fun appendDecimal() {
        if (!allowsDecimal) return
        if (value.contains(".")) {
            haptic(HapticFeedbackConstants.REJECT)
            return
        }
        haptic(HapticFeedbackConstants.KEYBOARD_TAP)
        onValueChange(if (value.isEmpty()) "0." else "$value.")
    }

    fun deleteOne() {
        if (value.isEmpty() || value == "0") {
            haptic(HapticFeedbackConstants.REJECT)
            return
        }
        haptic(HapticFeedbackConstants.CLOCK_TICK)
        val next = value.dropLast(1)
        onValueChange(if (next.isEmpty()) "0" else next)
    }

    fun clearAll() {
        if (value == "0") return
        haptic(HapticFeedbackConstants.LONG_PRESS)
        onValueChange("0")
    }

    Column(
        modifier = modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
        NumpadRow(listOf("1", "2", "3"), ::appendDigit)
        NumpadRow(listOf("4", "5", "6"), ::appendDigit)
        NumpadRow(listOf("7", "8", "9"), ::appendDigit)
        Row(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
            NumpadKey(
                label = if (allowsDecimal) "." else "",
                enabled = allowsDecimal,
                onClick = ::appendDecimal,
                modifier = Modifier.weight(1f),
            )
            NumpadKey(
                label = "0",
                onClick = { appendDigit("0") },
                modifier = Modifier.weight(1f),
            )
            DeleteKey(
                onClick = ::deleteOne,
                onLongClick = ::clearAll,
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@Composable
private fun NumpadRow(digits: List<String>, onDigit: (String) -> Unit) {
    Row(horizontalArrangement = Arrangement.spacedBy(Spacing.s)) {
        digits.forEach { digit ->
            NumpadKey(
                label = digit,
                onClick = { onDigit(digit) },
                modifier = Modifier.weight(1f),
            )
        }
    }
}

@Composable
private fun NumpadKey(
    label: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    enabled: Boolean = true,
) {
    Box(
        modifier = modifier
            .height(56.dp)
            .pointerInput(enabled) {
                if (enabled) {
                    detectTapGestures(onTap = { onClick() })
                }
            },
        contentAlignment = Alignment.Center,
    ) {
        Text(
            text = label,
            color = if (enabled) {
                BrandColors.foreground
            } else {
                BrandColors.mutedForeground.copy(alpha = 0.3f)
            },
            fontFamily = FontFamily.Default,
            fontSize = 28.sp,
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun DeleteKey(
    onClick: () -> Unit,
    onLongClick: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Box(
        modifier = modifier
            .height(56.dp)
            .pointerInput(Unit) {
                detectTapGestures(
                    onTap = { onClick() },
                    onLongPress = { onLongClick() },
                )
            },
        contentAlignment = Alignment.Center,
    ) {
        Icon(
            imageVector = Icons.AutoMirrored.Outlined.Backspace,
            contentDescription = "Delete",
            tint = BrandColors.foreground,
        )
    }
}

/**
 * Format a raw numpad buffer with thousands separators for the display
 * strip. Leaves a mid-decimal state ("12.") intact so the user sees
 * they're mid-entry. Mirrors the iOS `displayAmount` computed property
 * shared across the receive/send amount-entry views.
 */
fun formatAmountDisplay(buffer: String): String {
    val n = buffer.toLongOrNull()
        ?: return buffer.ifEmpty { "0" }
    return "%,d".format(n)
}

/**
 * Parse a raw buffer into the FFI's integer minor unit. Mirrors the
 * iOS `parsedAmount`:
 *   - BTC  → integer sats (buffer is already integer sats; a trailing
 *            dot is dropped).
 *   - USD  → cents ("12" → 1200, "12.5" → 1250, "12.50" → 1250;
 *            >2 fractional digits is rejected — sub-cent isn't
 *            mintable).
 * Returns null for empty / mid-decimal / unparseable buffers so the
 * caller can keep the CTA disabled until the value is well-formed.
 */
fun parseAmountMinorUnits(buffer: String, allowsDecimal: Boolean): ULong? {
    if (!allowsDecimal) {
        val clean = buffer.trim('.')
        return clean.toULongOrNull()
    }
    val parts = buffer.split(".")
    val whole = parts.getOrNull(0)?.ifEmpty { "0" }?.toULongOrNull() ?: return null
    if (parts.size == 1) return whole * 100u
    if (parts.size != 2) return null
    val fracRaw = parts[1]
    if (fracRaw.length > 2) return null
    val fracPadded = fracRaw.padEnd(2, '0')
    val frac = if (fracPadded.isEmpty()) 0uL else fracPadded.toULongOrNull() ?: 0uL
    return whole * 100u + frac
}
