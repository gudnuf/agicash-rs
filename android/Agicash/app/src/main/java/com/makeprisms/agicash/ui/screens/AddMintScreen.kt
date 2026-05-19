package com.makeprisms.agicash.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.OutlinedTextFieldDefaults
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.TopAppBarDefaults
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.makeprisms.agicash.ui.theme.BrandButton
import com.makeprisms.agicash.ui.theme.BrandButtonVariant
import com.makeprisms.agicash.ui.theme.BrandColors
import com.makeprisms.agicash.ui.theme.BrandTypography
import com.makeprisms.agicash.ui.theme.Radius
import com.makeprisms.agicash.ui.theme.Spacing
import com.makeprisms.agicash.wallet.WalletViewModel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.agicash_ffi.MintAddResult

/**
 * Phase machine for the [AddMintScreen]. Lifted to file scope because
 * Kotlin disallows sealed-hierarchy locals inside a composable.
 */
private sealed interface AddMintPhase {
    data object Entry : AddMintPhase
    data object Working : AddMintPhase
    data class Success(val result: MintAddResult) : AddMintPhase
    data class Error(val message: String) : AddMintPhase
}

/**
 * Compose analogue of `ios/Agicash/Agicash/AddMintView.swift`.
 *
 * State machine matches iOS:
 *   - `Entry`    — URL field + Add + Cancel buttons.
 *   - `Working`  — Add button shows a spinner, field is locked.
 *   - `Success`  — replaces the form with a success card; auto-dismisses
 *                  after 1.5s OR the user taps Done.
 *   - `Error`    — inline destructive message under the field; user can
 *                  edit + retry without dismissing.
 *
 * On a Compose Activity we surface this as a full destination route
 * via the NavHost (iOS uses a `.sheet` on top of the Accounts screen;
 * Compose's modal sheets don't carry their own back stack cleanly,
 * and the navigation feel is closer to iOS push than iOS sheet
 * regardless).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AddMintScreen(
    viewModel: WalletViewModel,
    onClose: () -> Unit,
) {
    var url by remember { mutableStateOf("") }
    var phase by remember { mutableStateOf<AddMintPhase>(AddMintPhase.Entry) }
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    // Auto-dismiss 1.5s after success — same cadence as iOS.
    if (phase is AddMintPhase.Success) {
        androidx.compose.runtime.LaunchedEffect(phase) {
            delay(1500)
            onClose()
        }
    }

    val errorMessage = (phase as? AddMintPhase.Error)?.message
    val isWorking = phase is AddMintPhase.Working

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Add Mint", style = BrandTypography.titleSmall) },
                actions = {
                    IconButton(onClick = onClose, enabled = !isWorking) {
                        Icon(
                            Icons.Outlined.Close,
                            contentDescription = "Close",
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
        Column(
            modifier = Modifier
                .padding(inner)
                .fillMaxSize()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = Spacing.l, vertical = Spacing.xxl),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(Spacing.xxl),
        ) {
            when (val p = phase) {
                is AddMintPhase.Success -> AddMintSuccessCard(result = p.result, onDone = onClose)
                else -> AddMintFormCard(
                    url = url,
                    onUrlChange = { newUrl ->
                        url = newUrl
                        if (phase is AddMintPhase.Error) phase = AddMintPhase.Entry
                    },
                    isWorking = isWorking,
                    errorMessage = errorMessage,
                    onPaste = {
                        val pasted = clipboard.getText()?.text
                        if (!pasted.isNullOrEmpty()) {
                            url = pasted
                            if (phase is AddMintPhase.Error) phase = AddMintPhase.Entry
                        }
                    },
                    onAdd = {
                        val trimmed = url.trim()
                        if (trimmed.isEmpty()) {
                            phase = AddMintPhase.Error("Enter a mint URL first.")
                            return@AddMintFormCard
                        }
                        phase = AddMintPhase.Working
                        scope.launch {
                            val outcome = viewModel.addMint(trimmed)
                            phase = when (outcome) {
                                is WalletViewModel.AddMintOutcome.Success ->
                                    AddMintPhase.Success(outcome.result)
                                is WalletViewModel.AddMintOutcome.Failure ->
                                    AddMintPhase.Error(outcome.message)
                            }
                        }
                    },
                    onCancel = onClose,
                )
            }
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun AddMintFormCard(
    url: String,
    onUrlChange: (String) -> Unit,
    isWorking: Boolean,
    errorMessage: String?,
    onPaste: () -> Unit,
    onAdd: () -> Unit,
    onCancel: () -> Unit,
) {
    Box(
        modifier = Modifier
            .widthIn(max = 384.dp)
            .fillMaxWidth()
            .clip(Radius.card)
            .background(BrandColors.card)
            .border(0.5.dp, BrandColors.border, Radius.card)
            .padding(Spacing.xxl),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
            // Header — mirrors web's `space-y-1.5`.
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                Text(
                    "Add Cashu Mint",
                    style = BrandTypography.title,
                    color = BrandColors.cardForeground,
                )
                Text(
                    "Enter a mint URL to create a Cashu account for it.",
                    style = BrandTypography.label,
                    color = BrandColors.mutedForeground,
                )
            }

            Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "Mint URL",
                        style = BrandTypography.labelEmphasis,
                        color = BrandColors.cardForeground,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        "Paste",
                        style = BrandTypography.label,
                        color = BrandColors.cardForeground,
                        modifier = Modifier
                            .clip(RoundedCornerShape(4.dp))
                            .padding(Spacing.xs)
                            .clickableNoIndication(enabled = !isWorking, onClick = onPaste),
                    )
                }
                OutlinedTextField(
                    value = url,
                    onValueChange = onUrlChange,
                    enabled = !isWorking,
                    placeholder = {
                        Text(
                            "https://mint.example.com",
                            style = BrandTypography.body,
                            color = BrandColors.mutedForeground,
                        )
                    },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(
                        keyboardType = KeyboardType.Uri,
                        imeAction = ImeAction.Go,
                    ),
                    textStyle = BrandTypography.body,
                    modifier = Modifier.fillMaxWidth(),
                    shape = Radius.control,
                    colors = OutlinedTextFieldDefaults.colors(
                        focusedContainerColor = BrandColors.background,
                        unfocusedContainerColor = BrandColors.background,
                        focusedBorderColor = BrandColors.foreground,
                        unfocusedBorderColor = BrandColors.border,
                        focusedTextColor = BrandColors.foreground,
                        unfocusedTextColor = BrandColors.foreground,
                    ),
                )
                Text(
                    "Search trusted mints at bitcoinmints.com",
                    style = BrandTypography.caption,
                    color = BrandColors.mutedForeground,
                )
            }

            if (errorMessage != null) {
                Text(
                    errorMessage,
                    style = BrandTypography.caption,
                    color = BrandColors.destructive,
                )
            }

            BrandButton(
                label = "Add",
                onClick = onAdd,
                variant = BrandButtonVariant.Primary,
                isLoading = isWorking,
                enabled = url.trim().isNotEmpty(),
            )
            BrandButton(
                label = "Cancel",
                onClick = onCancel,
                variant = BrandButtonVariant.Ghost,
                enabled = !isWorking,
            )
        }
    }
}

@Composable
private fun AddMintSuccessCard(result: MintAddResult, onDone: () -> Unit) {
    Box(
        modifier = Modifier
            .widthIn(max = 384.dp)
            .fillMaxWidth()
            .clip(Radius.card)
            .background(BrandColors.card)
            .border(0.5.dp, BrandColors.border, Radius.card)
            .padding(Spacing.xxl),
    ) {
        Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                Text(
                    "Mint added",
                    style = BrandTypography.title,
                    color = BrandColors.cardForeground,
                )
                Text(
                    "Your new account is ready to use.",
                    style = BrandTypography.label,
                    color = BrandColors.mutedForeground,
                )
            }
            Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.s),
            ) {
                Text(
                    result.mintName,
                    style = BrandTypography.title,
                    color = BrandColors.cardForeground,
                    maxLines = 1,
                )
                Text(
                    result.mintUrl,
                    style = BrandTypography.caption,
                    color = BrandColors.mutedForeground,
                    maxLines = 1,
                )
                Text(
                    result.currency,
                    style = BrandTypography.label,
                    color = BrandColors.mutedForeground,
                )
            }
            BrandButton(
                label = "Done",
                onClick = onDone,
                variant = BrandButtonVariant.Primary,
            )
        }
    }
}

// `clickableNoIndication` now lives once (package-`internal`) in
// ReceiveCarouselScreen.kt so the receive/send carousels share it; the
// duplicate `private` copy that used to live here was removed to avoid
// a conflicting-overload error.

