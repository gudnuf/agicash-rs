package com.makeprisms.agicash.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Add
import androidx.compose.material.icons.outlined.ArrowBack
import androidx.compose.material.icons.outlined.Bolt
import androidx.compose.material.icons.outlined.CreditCard
import androidx.compose.material.icons.outlined.Star
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SwipeToDismissBox
import androidx.compose.material3.SwipeToDismissBoxValue
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.material3.rememberSwipeToDismissBoxState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.makeprisms.agicash.ui.theme.BrandButton
import com.makeprisms.agicash.ui.theme.BrandButtonVariant
import com.makeprisms.agicash.ui.theme.BrandColors
import com.makeprisms.agicash.ui.theme.BrandTypography
import com.makeprisms.agicash.ui.theme.Radius
import com.makeprisms.agicash.ui.theme.Spacing
import com.makeprisms.agicash.wallet.WalletViewModel
import kotlinx.coroutines.launch
import uniffi.agicash_ffi.AccountFfi

/**
 * Compose analogue of `ios/Agicash/Agicash/AccountsView.swift`.
 *
 *   - Top app bar with a back arrow and a "+" toolbar action that
 *     opens [AddMintScreen].
 *   - List of accounts (sorted: default-for-its-currency on top).
 *   - Each row shows the account name + balance with a `Default`
 *     badge when applicable.
 *   - Swipe-left on non-default rows reveals a "Set as default"
 *     action (Compose `SwipeToDismissBox` confined to the end edge).
 *   - Empty state mirrors iOS's friendly nudge with an inline
 *     "Add Mint" CTA.
 *
 * Implementation note on swipe-to-default: iOS uses the
 * `SwipeActions` modifier with `allowsFullSwipe: false`. Compose
 * ships `SwipeToDismissBox` which is conceptually identical but
 * triggers the dismiss on a full pull. We treat the
 * `EndToStart` "dismissed" state as the "Set as default" intent;
 * after dispatch we reset the swipe state so the row springs back
 * instead of vanishing. This is the closest Compose primitive and
 * the behaviour reads identically — see the doc on
 * `feedback_accounts_view_swipe_gap` in the report for the gap
 * vs iOS's allowsFullSwipe=false (Compose has no equivalent flag).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AccountsScreen(
    viewModel: WalletViewModel,
    onBack: () -> Unit,
    onAddMint: () -> Unit,
) {
    val accountsState by viewModel.accounts.collectAsStateWithLifecycle()
    var setDefaultError by remember { mutableStateOf<String?>(null) }
    val sortedAccounts = viewModel.sortedAccounts()
    val scope = rememberCoroutineScope()

    LaunchedEffect(Unit) { viewModel.refreshAccounts() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Accounts", style = BrandTypography.titleSmall) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(
                            Icons.Outlined.ArrowBack,
                            contentDescription = "Back",
                            tint = BrandColors.foreground,
                        )
                    }
                },
                actions = {
                    IconButton(onClick = onAddMint) {
                        Icon(
                            Icons.Outlined.Add,
                            contentDescription = "Add mint",
                            tint = BrandColors.foreground,
                        )
                    }
                },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = BrandColors.background,
                    titleContentColor = BrandColors.foreground,
                ),
            )
        },
        containerColor = BrandColors.background,
    ) { inner ->
        if (accountsState.isEmpty()) {
            EmptyAccountsState(
                onAddMint = onAddMint,
                modifier = Modifier
                    .padding(inner)
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = Spacing.l, vertical = Spacing.xxl),
            )
        } else {
            LazyColumn(
                modifier = Modifier
                    .padding(inner)
                    .fillMaxSize()
                    .padding(horizontal = Spacing.l),
                contentPadding = androidx.compose.foundation.layout.PaddingValues(vertical = Spacing.s),
                verticalArrangement = Arrangement.spacedBy(Spacing.s),
            ) {
                items(sortedAccounts, key = { it.id }) { account ->
                    val isDefault = viewModel.isDefault(account)
                    if (isDefault) {
                        // Default rows are pinned — no swipe affordance.
                        AccountRow(account = account, isDefault = true)
                    } else {
                        SwipeToDefaultRow(
                            account = account,
                            onSetDefault = {
                                scope.launch {
                                    val outcome = viewModel.setDefaultAccount(account)
                                    if (outcome is WalletViewModel.SetDefaultOutcome.Failure) {
                                        setDefaultError = outcome.message
                                    }
                                }
                            },
                        )
                    }
                }
            }
        }
    }

    if (setDefaultError != null) {
        AlertDialog(
            onDismissRequest = { setDefaultError = null },
            title = { Text("Could not set default") },
            text = { Text(setDefaultError ?: "") },
            confirmButton = {
                TextButton(onClick = { setDefaultError = null }) { Text("OK") }
            },
        )
    }
}

/** Wraps an [AccountRow] in a SwipeToDismissBox that surfaces the
 * "Set as default" action when the user swipes left. After the action
 * fires we reset the state so the row springs back into place (we
 * never want the row to actually dismiss). */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun SwipeToDefaultRow(
    account: AccountFfi,
    onSetDefault: () -> Unit,
) {
    val dismissState = rememberSwipeToDismissBoxState(
        confirmValueChange = { value ->
            if (value == SwipeToDismissBoxValue.EndToStart) {
                onSetDefault()
                // Returning false here keeps the row in place — the
                // action fires once and the row springs back.
                false
            } else false
        },
    )
    SwipeToDismissBox(
        state = dismissState,
        enableDismissFromStartToEnd = false,
        enableDismissFromEndToStart = true,
        backgroundContent = {
            // Background shown while the user swipes — a primary-colored
            // panel with the star glyph + label, mirroring iOS's
            // `Label("Set as default", systemImage: "star.fill")` chip.
            Box(
                modifier = Modifier
                    .fillMaxSize()
                    .clip(Radius.card)
                    .background(BrandColors.primary)
                    .padding(horizontal = Spacing.l),
                contentAlignment = Alignment.CenterEnd,
            ) {
                Row(
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Spacing.s),
                ) {
                    Icon(
                        Icons.Outlined.Star,
                        contentDescription = null,
                        tint = BrandColors.primaryForeground,
                    )
                    Text(
                        "Set as default",
                        style = BrandTypography.bodyEmphasis,
                        color = BrandColors.primaryForeground,
                    )
                }
            }
        },
        content = {
            AccountRow(account = account, isDefault = false)
        },
    )
}

@Composable
private fun AccountRow(account: AccountFfi, isDefault: Boolean) {
    Box(
        modifier = Modifier
            .fillMaxWidth()
            .clip(Radius.card)
            .background(BrandColors.card)
            .border(0.5.dp, BrandColors.border, Radius.card)
            .padding(Spacing.l),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                val icon = when (account.accountType) {
                    "cashu" -> Icons.Outlined.CreditCard
                    "spark" -> Icons.Outlined.Bolt
                    else -> Icons.Outlined.CreditCard
                }
                Icon(
                    icon,
                    contentDescription = null,
                    tint = BrandColors.mutedForeground,
                    modifier = Modifier.size(20.dp),
                )
                Box(modifier = Modifier.padding(end = Spacing.m))
                Text(
                    account.name,
                    style = BrandTypography.body,
                    color = BrandColors.cardForeground,
                    modifier = Modifier.weight(1f),
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (isDefault) {
                    DefaultBadge()
                    Box(modifier = Modifier.padding(end = Spacing.s))
                }
                Text(
                    displayBalance(account),
                    style = BrandTypography.numericInline,
                    color = BrandColors.cardForeground,
                )
            }
            val url = account.mintUrl
            if (!url.isNullOrEmpty()) {
                Text(
                    url,
                    style = BrandTypography.caption,
                    color = BrandColors.mutedForeground,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
    }
}

@Composable
private fun DefaultBadge() {
    Box(
        modifier = Modifier
            .clip(RoundedCornerShape(9999.dp))
            .background(BrandColors.muted)
            .padding(horizontal = Spacing.s, vertical = 2.dp),
    ) {
        Text(
            "Default",
            style = BrandTypography.caption,
            color = BrandColors.mutedForeground,
        )
    }
}

@Composable
private fun EmptyAccountsState(
    onAddMint: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Box(
        modifier = modifier
            .fillMaxWidth()
            .clip(Radius.card)
            .background(BrandColors.card)
            .border(0.5.dp, BrandColors.border, Radius.card)
            .padding(Spacing.xxl),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
            Text(
                "No accounts yet",
                style = BrandTypography.title,
                color = BrandColors.cardForeground,
            )
            Text(
                "Add a Cashu mint to start using your wallet.",
                style = BrandTypography.label,
                color = BrandColors.mutedForeground,
            )
            BrandButton(
                label = "Add Mint",
                onClick = onAddMint,
                variant = BrandButtonVariant.Primary,
            )
        }
    }
}

private fun displayBalance(account: AccountFfi): String {
    return if (account.unit.isEmpty()) {
        "${account.balance} ${account.currency}"
    } else {
        account.balance
    }
}
