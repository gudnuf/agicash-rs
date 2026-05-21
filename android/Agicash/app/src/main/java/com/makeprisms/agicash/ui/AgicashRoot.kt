package com.makeprisms.agicash.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.outlined.Warning
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import com.makeprisms.agicash.ui.screens.AccountsScreen
import com.makeprisms.agicash.ui.screens.AddMintScreen
import com.makeprisms.agicash.ui.screens.HomeScreen
import com.makeprisms.agicash.ui.screens.LoginScreen
import com.makeprisms.agicash.ui.screens.ReceiveCarouselScreen
import com.makeprisms.agicash.ui.screens.SendCarouselScreen
import com.makeprisms.agicash.ui.screens.SettingsScreen
import com.makeprisms.agicash.wallet.WalletViewModel

/**
 * Top-level shell. Mirrors `ContentView.swift` on iOS — reads
 * [WalletViewModel] state and routes to login or the bottom-tab
 * signed-in surface.
 */
@Composable
fun AgicashRoot(viewModel: WalletViewModel) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    when (val s = state) {
        is WalletViewModel.BootState.Pending -> CenteredSpinner("Starting Agicash...")
        is WalletViewModel.BootState.Failed -> FatalErrorView(s.message)
        is WalletViewModel.BootState.Ready -> AuthGate(viewModel, s.phase)
    }
}

@Composable
private fun AuthGate(viewModel: WalletViewModel, phase: WalletViewModel.Phase) {
    when (phase) {
        is WalletViewModel.Phase.SignedOut -> LoginScreen(viewModel)
        is WalletViewModel.Phase.SignedIn -> {
            // Stack the realtime status banner above the signed-in tab
            // shell. The banner uses `AnimatedVisibility` to occupy
            // zero height when the channel is healthy, so SignedInShell
            // measures identically to today on the happy path.
            Column(Modifier.fillMaxSize()) {
                RealtimeStatusBanner(viewModel)
                Box(Modifier.weight(1f)) { SignedInShell(viewModel) }
            }
        }
        is WalletViewModel.Phase.Error -> ErrorView(viewModel, phase.message)
    }
}

/**
 * Signed-in shell. Bottom nav between Home and Settings; Settings is
 * itself a NavHost so the user can drill into Accounts → AddMint and
 * back without leaving the Settings tab.
 *
 * Mirrors the iOS shell where Settings + AccountsView + AddMintView
 * live on a single NavigationStack; on iOS that stack is per-tab,
 * here it's a nested NavHost. Switching back to Home pops to the
 * top-level Home screen — same iOS feel.
 */
@Composable
private fun SignedInShell(viewModel: WalletViewModel) {
    // Receive / Send are presented as full-screen overlays over the
    // bottom-tab shell. iOS presents them as `.sheet`s from HomeView;
    // this app already established full-screen-route-over-sheet as the
    // closer-to-iOS-push feel (see AddMintScreen's doc note), so the
    // carousels follow the same convention. A nullable overlay state
    // is simpler than a top-level NavHost here and keeps the existing
    // per-tab Settings NavHost untouched.
    var overlay by remember { mutableStateOf(Overlay.NONE) }
    var selected by remember { mutableStateOf(Tab.HOME) }

    // Intercept system back while an overlay is showing — return to the
    // bottom-tab home shell instead of letting the Activity finish.
    BackHandler(enabled = overlay != Overlay.NONE) {
        overlay = Overlay.NONE
    }

    when (overlay) {
        Overlay.RECEIVE -> {
            ReceiveCarouselScreen(viewModel = viewModel, onClose = { overlay = Overlay.NONE })
            return
        }
        Overlay.SEND -> {
            SendCarouselScreen(viewModel = viewModel, onClose = { overlay = Overlay.NONE })
            return
        }
        Overlay.NONE -> Unit
    }

    Scaffold(
        bottomBar = {
            NavigationBar {
                NavigationBarItem(
                    selected = selected == Tab.HOME,
                    onClick = { selected = Tab.HOME },
                    icon = { Icon(Icons.Filled.Home, contentDescription = null) },
                    label = { Text("Home") },
                )
                NavigationBarItem(
                    selected = selected == Tab.SETTINGS,
                    onClick = { selected = Tab.SETTINGS },
                    icon = { Icon(Icons.Filled.Settings, contentDescription = null) },
                    label = { Text("Settings") },
                )
            }
        },
    ) { inner ->
        Box(Modifier.padding(inner)) {
            when (selected) {
                Tab.HOME -> HomeScreen(
                    viewModel = viewModel,
                    onReceive = { overlay = Overlay.RECEIVE },
                    onSend = { overlay = Overlay.SEND },
                )
                Tab.SETTINGS -> SettingsTabHost(viewModel)
            }
        }
    }
}

private enum class Overlay { NONE, RECEIVE, SEND }

/**
 * Per-tab NavHost for the Settings flow. Routes:
 *   - settings  — top-level Settings screen
 *   - accounts  — Settings → Accounts (list + swipe-to-default)
 *   - addMint   — Accounts → Add Mint (full-screen here vs iOS sheet)
 */
@Composable
private fun SettingsTabHost(viewModel: WalletViewModel) {
    val nav = rememberNavController()
    NavHost(navController = nav, startDestination = "settings") {
        composable("settings") {
            SettingsScreen(
                viewModel = viewModel,
                onOpenAccounts = { nav.navigate("accounts") },
            )
        }
        composable("accounts") {
            AccountsScreen(
                viewModel = viewModel,
                onBack = { nav.popBackStack() },
                onAddMint = { nav.navigate("addMint") },
            )
        }
        composable("addMint") {
            AddMintScreen(
                viewModel = viewModel,
                onClose = { nav.popBackStack() },
            )
        }
    }
}

private enum class Tab { HOME, SETTINGS }

@Composable
private fun CenteredSpinner(label: String) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            CircularProgressIndicator()
            Text(label, style = MaterialTheme.typography.bodyMedium)
        }
    }
}

@Composable
private fun FatalErrorView(message: String) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Icon(
                Icons.Outlined.Warning,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.error,
            )
            Text("Failed to start", style = MaterialTheme.typography.titleMedium)
            Text(
                message,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun ErrorView(viewModel: WalletViewModel, message: String) {
    Box(
        modifier = Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .padding(24.dp),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Icon(
                Icons.Outlined.Warning,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.error,
            )
            Text("Something went wrong", style = MaterialTheme.typography.titleMedium)
            Text(
                message,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Button(onClick = { viewModel.signOut() }) {
                Text("Sign out and retry")
            }
        }
    }
}
