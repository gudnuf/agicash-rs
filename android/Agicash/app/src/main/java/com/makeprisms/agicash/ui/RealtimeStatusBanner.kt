package com.makeprisms.agicash.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.expandVertically
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.shrinkVertically
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.WifiOff
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.makeprisms.agicash.ui.theme.BrandColors
import com.makeprisms.agicash.ui.theme.BrandTypography
import com.makeprisms.agicash.ui.theme.Spacing
import com.makeprisms.agicash.wallet.WalletViewModel
import uniffi.agicash_ffi.RealtimeStatusFfi

/**
 * Thin top-of-screen banner that reflects the Rust realtime supervisor's
 * `RealtimeStatusFfi` (forwarded from `WalletEventBridge.onStatus` into
 * [WalletViewModel.realtimeStatus]). Positioned above the main signed-in
 * content in [AgicashRoot] — purely additive: zero height when the
 * channel is healthy, a thin strip otherwise.
 *
 * State map (mirrors `ios/Agicash/Agicash/RealtimeStatusBanner.swift`
 * and the Lane 1 audit `2026-05-19-realtime-parity.md`):
 *
 *   - `Idle` / `Subscribed` / `Closed`               → hidden (no banner)
 *   - `Connecting` / `Reconnecting` / `Error`        → muted
 *     "Reconnecting…" strip (transient; the supervisor resolves on its own)
 *   - `TerminalError`                                → persistent
 *     destructive strip with "Tap to retry" affordance that calls
 *     [WalletViewModel.retryRealtime] — the supervisor's online-edge
 *     handler clears the latched `terminal` flag on the false→true
 *     transition and resumes the reconnect ladder.
 *
 * Typography: brand `BrandTypography.caption` (Kode Mono 12sp) so it
 * sits quietly under the system status bar without competing with the
 * `BalanceHero`. Colors come straight from the Material 3 color scheme
 * aliases in `BrandColors`/Material theme so the banner is
 * dark-mode-aware without bespoke logic.
 */
@Composable
fun RealtimeStatusBanner(viewModel: WalletViewModel) {
    val status by viewModel.realtimeStatus.collectAsStateWithLifecycle()
    val retryInFlight by viewModel.realtimeRetryInFlight.collectAsStateWithLifecycle()

    // Three-way collapse of `RealtimeStatusFfi` into the visual states
    // the banner actually renders. Keeps the `when` exhaustive (a new
    // FFI variant produces a compiler error here) and decouples
    // copy/style from variant identity.
    val kind = when (status) {
        RealtimeStatusFfi.IDLE,
        RealtimeStatusFfi.SUBSCRIBED,
        RealtimeStatusFfi.CLOSED -> BannerKind.HIDDEN
        RealtimeStatusFfi.CONNECTING,
        RealtimeStatusFfi.RECONNECTING,
        RealtimeStatusFfi.ERROR -> BannerKind.TRANSIENT
        RealtimeStatusFfi.TERMINAL_ERROR -> BannerKind.TERMINAL
    }

    // `AnimatedVisibility` keeps the parent layout pristine when the
    // channel is healthy (HIDDEN → composes to empty Box) and softly
    // slides the banner in/out on transitions so the signed-in content
    // doesn't pop down on a flicker.
    AnimatedVisibility(
        visible = kind != BannerKind.HIDDEN,
        enter = expandVertically() + fadeIn(),
        exit = shrinkVertically() + fadeOut(),
    ) {
        when (kind) {
            BannerKind.TRANSIENT -> TransientBanner()
            BannerKind.TERMINAL -> TerminalBanner(
                retryInFlight = retryInFlight,
                onRetry = { viewModel.retryRealtime() },
            )
            BannerKind.HIDDEN -> Unit // unreachable inside AnimatedVisibility
        }
    }
}

private enum class BannerKind { HIDDEN, TRANSIENT, TERMINAL }

/**
 * Muted "Reconnecting…" strip rendered while the Rust supervisor is
 * mid-reconnect ladder. No tap target — the supervisor is already
 * trying.
 */
@Composable
private fun TransientBanner() {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .background(BrandColors.muted),
    ) {
        Row(
            modifier = Modifier
                .fillMaxWidth()
                .padding(horizontal = Spacing.l, vertical = Spacing.s),
            horizontalArrangement = Arrangement.Center,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            CircularProgressIndicator(
                modifier = Modifier.size(12.dp),
                color = BrandColors.mutedForeground,
                strokeWidth = 1.5.dp,
            )
            Text(
                text = "Reconnecting…",
                style = BrandTypography.caption,
                color = BrandColors.mutedForeground,
                modifier = Modifier.padding(start = Spacing.s),
            )
        }
        // Thin hairline below so the banner reads as a distinct strip
        // rather than blending into the home background.
        Box(
            modifier = Modifier
                .fillMaxWidth()
                .height(0.5.dp)
                .background(BrandColors.border)
                .align(Alignment.BottomCenter),
        )
    }
}

/**
 * Persistent destructive strip rendered when the supervisor latched
 * `TerminalError` (9 consecutive `JoinReplyError` attempts — the F13
 * dampening lever, see `MAX_JOIN_REJECT_ATTEMPTS` in
 * `agicash-realtime/src/service.rs`). The whole strip is clickable so a
 * thumb anywhere on the bar triggers retry.
 */
@Composable
private fun TerminalBanner(retryInFlight: Boolean, onRetry: () -> Unit) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(BrandColors.destructive)
            // `enabled = !retryInFlight` so a double-tap is a no-op
            // visually; the ViewModel also guards the FFI call.
            .clickable(enabled = !retryInFlight, onClick = onRetry)
            .padding(horizontal = Spacing.l, vertical = Spacing.s),
        horizontalArrangement = Arrangement.Center,
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (retryInFlight) {
            CircularProgressIndicator(
                modifier = Modifier.size(12.dp),
                // The Material3 onError is white in this theme; matches
                // the iOS destructive-foreground.
                color = BrandColors.primaryForeground,
                strokeWidth = 1.5.dp,
            )
            Text(
                text = "Reconnecting…",
                style = BrandTypography.caption,
                color = BrandColors.primaryForeground,
                modifier = Modifier.padding(start = Spacing.s),
            )
        } else {
            Icon(
                imageVector = Icons.Outlined.WifiOff,
                contentDescription = null,
                tint = BrandColors.primaryForeground,
                modifier = Modifier.size(14.dp),
            )
            Text(
                text = "Connection lost — tap to retry",
                style = BrandTypography.caption,
                color = BrandColors.primaryForeground,
                modifier = Modifier.padding(start = Spacing.s),
            )
        }
    }
}
