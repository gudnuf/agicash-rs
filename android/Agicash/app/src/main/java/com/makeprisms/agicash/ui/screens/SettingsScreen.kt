package com.makeprisms.agicash.ui.screens

import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.ChevronRight
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.CreditCard
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.People
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.makeprisms.agicash.ui.theme.BrandButton
import com.makeprisms.agicash.ui.theme.BrandButtonVariant
import com.makeprisms.agicash.ui.theme.BrandColors
import com.makeprisms.agicash.ui.theme.BrandTypography
import com.makeprisms.agicash.ui.theme.Spacing
import com.makeprisms.agicash.wallet.WalletViewModel

/**
 * Compose analogue of `ios/Agicash/Agicash/SettingsView.swift`.
 *
 *   - `LnAddressDisplay`  — large user-id row with a copy glyph.
 *   - `SettingsNavStack`  — three rows: Edit profile, Accounts (the
 *     only wired destination), Contacts.
 *   - `SettingsFooter`    — Sign Out CTA in a centered 144dp column,
 *     plus a Terms / & / Privacy footer line.
 *
 * The Accounts row navigates via `onOpenAccounts`. The shell
 * (`AgicashRoot`) wires that into the NavHost so a back tap returns
 * here.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(
    viewModel: WalletViewModel,
    onOpenAccounts: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val accounts by viewModel.accounts.collectAsStateWithLifecycle()
    val isWorking by viewModel.isWorking.collectAsStateWithLifecycle()
    var confirmingSignOut by remember { mutableStateOf(false) }

    LaunchedEffect(Unit) { viewModel.refreshAccounts() }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("") },
                colors = TopAppBarDefaults.topAppBarColors(
                    containerColor = BrandColors.background,
                ),
            )
        },
        containerColor = BrandColors.background,
    ) { inner ->
        Column(
            modifier = Modifier
                .padding(inner)
                .fillMaxSize()
                .verticalScroll(rememberScrollState()),
            verticalArrangement = Arrangement.spacedBy(Spacing.xxl),
        ) {
            LnAddressDisplay(
                state = state,
                modifier = Modifier.padding(
                    horizontal = Spacing.l,
                    vertical = Spacing.l,
                ),
            )
            SettingsNavStack(
                defaultAccountLabel = accounts.firstOrNull()?.name ?: "Accounts",
                onOpenAccounts = onOpenAccounts,
                modifier = Modifier.padding(horizontal = Spacing.l),
            )
            Spacer(Modifier.height(Spacing.xxl))
            SettingsFooter(
                isWorking = isWorking,
                onSignOut = { confirmingSignOut = true },
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(horizontal = Spacing.l, vertical = Spacing.xxl),
            )
        }
    }

    if (confirmingSignOut) {
        AlertDialog(
            onDismissRequest = { confirmingSignOut = false },
            title = { Text("Sign out of Agicash?") },
            text = { Text("Your local session will be cleared. You can sign back in any time.") },
            confirmButton = {
                TextButton(onClick = {
                    confirmingSignOut = false
                    viewModel.signOut()
                }) {
                    Text("Sign out", color = BrandColors.destructive)
                }
            },
            dismissButton = {
                TextButton(onClick = { confirmingSignOut = false }) { Text("Cancel") }
            },
        )
    }
}

/**
 * Visual analogue of iOS `LnAddressDisplay`. We don't have a real
 * lightning address yet so we render the truncated user UUID in
 * `username@agicash` shape — same layout, same copy affordance.
 */
@Composable
private fun LnAddressDisplay(
    state: WalletViewModel.BootState,
    modifier: Modifier = Modifier,
) {
    val display = remember(state) {
        when (val s = state) {
            is WalletViewModel.BootState.Ready -> when (val p = s.phase) {
                is WalletViewModel.Phase.SignedIn -> "${p.userId.take(8)}@agicash"
                else -> "\u2014"
            }
            else -> "\u2014"
        }
    }
    Row(
        modifier = modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            text = display,
            style = BrandTypography.title,
            color = BrandColors.foreground,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        Icon(
            Icons.Outlined.ContentCopy,
            contentDescription = "Copy",
            tint = BrandColors.mutedForeground,
            modifier = Modifier.size(20.dp),
        )
    }
}

/**
 * Mirrors iOS `SettingsNavStack`: three 40dp rows with a leading icon
 * + label + trailing chevron. Accounts is the only wired destination
 * today; Edit profile + Contacts are static rows pending their own
 * lanes.
 */
@Composable
private fun SettingsNavStack(
    defaultAccountLabel: String,
    onOpenAccounts: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxWidth()) {
        SettingsNavRow(icon = Icons.Outlined.Edit, label = "Edit profile", onClick = null)
        SettingsNavRow(
            icon = Icons.Outlined.CreditCard,
            label = defaultAccountLabel,
            onClick = onOpenAccounts,
        )
        SettingsNavRow(icon = Icons.Outlined.People, label = "Contacts", onClick = null)
    }
}

@Composable
private fun SettingsNavRow(
    icon: ImageVector,
    label: String,
    onClick: (() -> Unit)?,
) {
    val interactionSource = remember { MutableInteractionSource() }
    val baseMod = Modifier
        .fillMaxWidth()
        .height(40.dp)
    val rowMod = if (onClick != null) {
        baseMod.clickable(
            interactionSource = interactionSource,
            indication = null,
            onClick = onClick,
        )
    } else baseMod
    Row(
        modifier = rowMod,
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.s),
    ) {
        Icon(
            icon,
            contentDescription = null,
            tint = BrandColors.foreground,
            modifier = Modifier.size(16.dp),
        )
        Text(label, style = BrandTypography.body, color = BrandColors.foreground)
        Box(modifier = Modifier.weight(1f))
        Icon(
            Icons.Outlined.ChevronRight,
            contentDescription = null,
            tint = BrandColors.mutedForeground,
            modifier = Modifier.size(16.dp),
        )
    }
}

/**
 * iOS `SettingsFooter`: Sign Out button in a 144dp centered column,
 * Terms / & / Privacy line below in muted text.
 */
@Composable
private fun SettingsFooter(
    isWorking: Boolean,
    onSignOut: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier,
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Spacing.xxl),
    ) {
        BrandButton(
            label = "Sign Out",
            onClick = onSignOut,
            variant = BrandButtonVariant.Primary,
            isLoading = isWorking,
            modifier = Modifier.widthIn(max = 144.dp),
        )

        Row(
            modifier = Modifier.widthIn(max = 144.dp).fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text("Terms", style = BrandTypography.label, color = BrandColors.mutedForeground)
            Text("&", style = BrandTypography.label, color = BrandColors.mutedForeground)
            Text("Privacy", style = BrandTypography.label, color = BrandColors.mutedForeground)
        }
    }
}
