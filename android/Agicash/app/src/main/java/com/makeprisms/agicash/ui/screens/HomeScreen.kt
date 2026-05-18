package com.makeprisms.agicash.ui.screens

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.LifecycleResumeEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.makeprisms.agicash.ui.theme.BrandButton
import com.makeprisms.agicash.ui.theme.BrandButtonSize
import com.makeprisms.agicash.ui.theme.BrandButtonVariant
import com.makeprisms.agicash.ui.theme.BrandColors
import com.makeprisms.agicash.ui.theme.BrandTypography
import com.makeprisms.agicash.ui.theme.Spacing
import com.makeprisms.agicash.wallet.WalletViewModel
import java.math.BigDecimal
import java.math.RoundingMode
import kotlinx.coroutines.launch
import uniffi.agicash_ffi.AccountFfi

/**
 * Compose analogue of `ios/Agicash/Agicash/HomeView.swift`.
 *
 * Layout mirrors iOS 1:1:
 *   - `BalanceHero` (Teko numeric, currency-symbol prefix, secondary
 *     converted-amount placeholder line in muted text)
 *   - `HomeActionGrid` — Receive (secondary) + Send (primary) stacked,
 *     288dp max width, centered.
 *
 * The Send/Receive primary CTAs surface `onReceive`/`onSend` callbacks
 * the parent shell wires to the sibling Send/Receive workers' carousel
 * sheets. Their lanes (`feat/android-send-cashu`,
 * `feat/android-receive-cashu`) were still on the TLS-init commit at
 * the time of writing; placeholders here mean both buttons render but
 * route to no-op handlers in this branch.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(
    viewModel: WalletViewModel,
    onReceive: () -> Unit = {},
    onSend: () -> Unit = {},
) {
    val accounts by viewModel.accounts.collectAsStateWithLifecycle()
    val isRefreshing by viewModel.isRefreshing.collectAsStateWithLifecycle()
    val refreshError by viewModel.refreshError.collectAsStateWithLifecycle()
    val scope = rememberCoroutineScope()

    // One-shot foreground re-sync. ON_RESUME fires on first entry AND on
    // every return from background / another screen, so this seeds the
    // initial balance and re-syncs on navigation/foregrounding. Mirrors
    // iOS `.task { await model.refreshAccounts() }` and the web
    // `refetchOnWindowFocus:'always'`.
    //
    // The Tier-1 5s `repeatOnLifecycle(STARTED)` poll that used to live
    // here is DELETED: out-of-band receives are now caught by the
    // all-Rust Supabase Realtime subscription wired in `WalletViewModel`
    // (`onConnected` is the no-replay catch-up — it refetches on every
    // (re)connect; `onEvent` refetches on each broadcast). This
    // resume-effect remains only as the initial-paint / on-return seed,
    // never as a recurring poller.
    LifecycleResumeEffect(Unit) {
        viewModel.refreshAccounts()
        onPauseOrDispose { }
    }

    PullToRefreshBox(
        isRefreshing = isRefreshing,
        // Pull-to-refresh: manual recovery matching iOS `.refreshable {
        // await model.refreshAccounts() }`. `refreshAccountsFromPull`
        // suspends and toggles `isRefreshing` so the Material3 indicator
        // shows until the FFI round-trip completes.
        onRefresh = { scope.launch { viewModel.refreshAccountsFromPull() } },
        modifier = Modifier.fillMaxSize(),
    ) {
        Scaffold(
            containerColor = BrandColors.background,
        ) { inner ->
            Column(
                modifier = Modifier
                    .padding(inner)
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(bottom = Spacing.xxl),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.xxxl),
            ) {
                Spacer(Modifier.padding(top = Spacing.hero))
                // Non-fatal transient-refresh banner. An on-resume /
                // realtime-driven `list_accounts()` blip keeps the
                // last-known balance on screen (BalanceHero below still
                // renders the cached `accounts`) and surfaces here
                // instead of routing to the session-destroying
                // ErrorView. Clears itself on the next successful
                // refresh (next resume, realtime onConnected/onEvent, or
                // pull-to-refresh).
                if (refreshError != null) {
                    Text(
                        text = "Showing last known balance — couldn't reach the server.",
                        style = BrandTypography.label,
                        color = BrandColors.mutedForeground,
                        textAlign = TextAlign.Center,
                        modifier = Modifier
                            .fillMaxWidth()
                            .padding(horizontal = Spacing.l),
                    )
                }
                BalanceHero(accounts)
                HomeActionGrid(
                    onReceive = onReceive,
                    onSend = onSend,
                    modifier = Modifier.padding(horizontal = Spacing.l),
                )
            }
        }
    }
}

/**
 * Centered balance display modeled on iOS `BalanceHero`. Teko numeric
 * with a small leading currency symbol (₿ / $), and a smaller muted
 * converted-amount line below.
 *
 * Currency selection mirrors iOS: USD wins when both currencies exist,
 * otherwise BTC, otherwise defaults to USD.
 */
@Composable
private fun BalanceHero(accounts: List<AccountFfi>) {
    val currencies = remember(accounts) { accounts.map { it.currency }.toSet() }
    val primaryCurrency = remember(currencies) {
        when {
            currencies.contains("USD") -> "USD"
            currencies.contains("BTC") -> "BTC"
            else -> "USD"
        }
    }
    val primarySymbol = remember(primaryCurrency) {
        when (primaryCurrency) {
            "USD" -> "$"
            "BTC" -> "\u20BF"
            else -> "$"
        }
    }
    val primaryAmount = remember(accounts, primaryCurrency) {
        totalForCurrency(accounts, primaryCurrency).toPlainString()
    }
    val secondaryLine = remember(accounts, primaryCurrency, currencies) {
        secondaryLineFor(accounts, primaryCurrency, currencies)
    }

    Column(
        modifier = Modifier.fillMaxWidth(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
        Row(
            verticalAlignment = Alignment.Bottom,
            horizontalArrangement = Arrangement.spacedBy(4.dp),
        ) {
            // Currency symbol — small, semibold rounded — baseline-aligned
            // to the bottom of the Teko hero.
            Text(
                text = primarySymbol,
                style = BrandTypography.titleSmall,
                color = BrandColors.foreground,
                modifier = Modifier.padding(bottom = 14.dp),
            )
            Text(
                text = primaryAmount,
                style = BrandTypography.numericHero,
                color = BrandColors.foreground,
            )
        }
        Text(
            text = secondaryLine,
            style = BrandTypography.label,
            color = BrandColors.mutedForeground,
            textAlign = TextAlign.Center,
        )
    }
}

/**
 * Receive / Send button stack. Mirrors iOS `HomeActionGrid`: 288dp max
 * width, centered in the parent, secondary Receive on top of primary
 * Send so the visual hierarchy matches the web design.
 */
@Composable
private fun HomeActionGrid(
    onReceive: () -> Unit,
    onSend: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier.fillMaxWidth(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Spacing.l),
    ) {
        Column(
            modifier = Modifier.widthIn(max = 288.dp).fillMaxWidth(),
            verticalArrangement = Arrangement.spacedBy(Spacing.l),
        ) {
            BrandButton(
                label = "Receive",
                onClick = onReceive,
                variant = BrandButtonVariant.Secondary,
                size = BrandButtonSize.Large,
            )
            BrandButton(
                label = "Send",
                onClick = onSend,
                variant = BrandButtonVariant.Primary,
                size = BrandButtonSize.Large,
            )
        }
    }
}

/**
 * Sum balances (decimal-string minor units) for the accounts matching
 * `currency`. Skips non-numeric balances so a future malformed row
 * can't crash the hero. Mirrors iOS `totalForCurrency`.
 */
private fun totalForCurrency(accounts: List<AccountFfi>, currency: String): BigDecimal {
    return accounts
        .filter { it.currency == currency }
        .fold(BigDecimal.ZERO) { acc, account ->
            val parsed = runCatching { BigDecimal(account.balance) }.getOrDefault(BigDecimal.ZERO)
            acc + parsed
        }
        .setScale(0, RoundingMode.DOWN)
}

/**
 * Secondary converted-amount line. When the user holds BOTH BTC and
 * USD accounts, render the *other* currency's per-unit total. With
 * only one currency present, render a sats placeholder so the hero
 * doesn't collapse to a single line. Matches iOS `secondaryLine`.
 */
private fun secondaryLineFor(
    accounts: List<AccountFfi>,
    primaryCurrency: String,
    currencies: Set<String>,
): String {
    val secondaryCurrency: String? = when {
        primaryCurrency == "USD" && currencies.contains("BTC") -> "BTC"
        primaryCurrency == "BTC" && currencies.contains("USD") -> "USD"
        else -> null
    }
    if (secondaryCurrency == null) {
        return "\u2248 0 sats"
    }
    val total = totalForCurrency(accounts, secondaryCurrency)
    val unit = when (secondaryCurrency) {
        "BTC" -> if (total.compareTo(BigDecimal.ONE) == 0) "sat" else "sats"
        "USD", "USDB" -> if (total.compareTo(BigDecimal.ONE) == 0) "cent" else "cents"
        else -> ""
    }
    return "\u2248 ${total.toPlainString()} $unit"
}
