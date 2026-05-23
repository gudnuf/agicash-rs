package com.makeprisms.agicash.ui.screens

import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.rememberPagerState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.outlined.AccountBalanceWallet
import androidx.compose.material.icons.outlined.AttachMoney
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material.icons.outlined.SwapVert
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
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.graphicsLayer
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.makeprisms.agicash.ui.components.AmountNumpad
import com.makeprisms.agicash.ui.components.QrCode
import com.makeprisms.agicash.ui.components.formatAmountDisplay
import com.makeprisms.agicash.ui.components.parseAmountMinorUnits
import com.makeprisms.agicash.ui.theme.BrandButton
import com.makeprisms.agicash.ui.theme.BrandButtonSize
import com.makeprisms.agicash.ui.theme.BrandButtonVariant
import com.makeprisms.agicash.ui.theme.BrandColors
import com.makeprisms.agicash.ui.theme.BrandTypography
import com.makeprisms.agicash.ui.theme.Radius
import com.makeprisms.agicash.ui.theme.Spacing
import com.makeprisms.agicash.wallet.WalletViewModel
import kotlinx.coroutines.delay
import uniffi.agicash_ffi.AlreadyClaimedInfoFfi
import uniffi.agicash_ffi.FfiException
import uniffi.agicash_ffi.MintConfirmationFfi
import uniffi.agicash_ffi.MintQuoteFfiState
import uniffi.agicash_ffi.MintQuoteHandle
import uniffi.agicash_ffi.ReceiveFlow
import uniffi.agicash_ffi.ReceiveFlowEventFfi
import uniffi.agicash_ffi.ReceiveFlowResultFfi
import uniffi.agicash_ffi.ReceiveFlowStateFfi
import uniffi.agicash_ffi.ReceiveResult
import uniffi.agicash_ffi.ReceiveStatus
import uniffi.agicash_ffi.ReceiveStatusFfi
import uniffi.agicash_ffi.extractCashuToken
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Top-level Receive surface. Compose analogue of
 * `ios/Agicash/Agicash/ReceiveCarouselView.swift`.
 *
 * A three-tab swipeable carousel: Cashu paste / Lightning / Buy. iOS
 * uses `TabView(.page)` + a custom bottom indicator bar; the Compose
 * equivalent is `HorizontalPager` + the same custom navbar that
 * surfaces a semantic icon per tab (wallet / bolt / dollar). Tapping
 * an icon drives the same pager state the swipe gesture does.
 *
 * Presented as a full-screen route from the signed-in shell (iOS uses
 * a `.sheet`; the existing AddMint flow on Android already established
 * full-screen-route-over-sheet as the closer-to-iOS-push feel for this
 * app — same call here). Opens on the Lightning tab (the most common
 * "please pay me" intent and the most visually compelling first
 * impression), matching iOS's default `initialTab`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ReceiveCarouselScreen(
    viewModel: WalletViewModel,
    onClose: () -> Unit,
) {
    val tabs = ReceiveTab.entries
    val pager = rememberPagerState(initialPage = ReceiveTab.LIGHTNING.ordinal) { tabs.size }
    val scope = rememberCoroutineScope()
    val selected = tabs[pager.currentPage]

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(selected.title, style = BrandTypography.titleSmall) },
                actions = {
                    IconButton(onClick = onClose) {
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
        Column(Modifier.padding(inner).fillMaxSize()) {
            HorizontalPager(
                state = pager,
                modifier = Modifier.weight(1f).fillMaxWidth(),
            ) { page ->
                when (tabs[page]) {
                    ReceiveTab.CASHU -> CashuTokenPastePage(viewModel, onClose)
                    ReceiveTab.LIGHTNING -> LightningReceivePage(viewModel, onClose)
                    ReceiveTab.BUY -> BuyPlaceholderPage()
                }
            }
            TabIndicatorBar(
                tabs = tabs,
                selected = selected,
                onSelect = { tab -> scope.launch { pager.animateScrollToPage(tab.ordinal) } },
            )
        }
    }
}

private enum class ReceiveTab(val title: String, val icon: ImageVector) {
    CASHU("Receive Cashu", Icons.Outlined.AccountBalanceWallet),
    LIGHTNING("Receive Lightning", Icons.Filled.Bolt),
    BUY("Buy sats", Icons.Outlined.AttachMoney),
}

/**
 * Bottom indicator bar — tappable icons that double as page
 * indicators. Mirrors the iOS `TabIndicatorBar`.
 */
@Composable
private fun TabIndicatorBar(
    tabs: List<ReceiveTab>,
    selected: ReceiveTab,
    onSelect: (ReceiveTab) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(BrandColors.background)
            .border(0.5.dp, BrandColors.border)
            .padding(horizontal = Spacing.l, vertical = Spacing.s),
    ) {
        tabs.forEach { tab ->
            Box(
                modifier = Modifier.weight(1f).height(40.dp),
                contentAlignment = Alignment.Center,
            ) {
                IconButton(onClick = { onSelect(tab) }) {
                    Icon(
                        tab.icon,
                        contentDescription = tab.title,
                        tint = if (tab == selected) {
                            BrandColors.foreground
                        } else {
                            BrandColors.mutedForeground.copy(alpha = 0.4f)
                        },
                    )
                }
            }
        }
    }
}

// ---- Cashu token paste page ----

/**
 * View-level phase for the Cashu paste page. Mirrors iOS
 * `CashuTokenPasteView.Phase` (`ios/Agicash/Agicash/CashuTokenPasteView.swift`):
 * the FFI `ReceiveFlowStateFfi` states map 1:1 here, with two extras
 * ([Entry], [Working]) that exist only on the UI side of the seam.
 */
private sealed interface CashuPastePhase {
    /** Form is editable; user can paste + tap Receive. */
    data object Entry : CashuPastePhase

    /**
     * Receive was tapped — extracting the encoded token, constructing
     * the `ReceiveFlow` handle, and dispatching `Start`. Covers `Idle`
     * and `Parsing` from the FFI state machine.
     */
    data object Working : CashuPastePhase

    /**
     * Pasted token is from a mint the user hasn't added. The
     * `MintConfirmationCard` is shown; user can accept ("Add Mint and
     * Claim") or cancel.
     */
    data class ConfirmingMint(val confirmation: MintConfirmationFfi) : CashuPastePhase

    /** Mint-add side effect is running. Spinner shown. */
    data object AddingMint : CashuPastePhase

    /** Receive-swap side effect is running. Spinner shown. */
    data object Swapping : CashuPastePhase

    /** Terminal success. Replaces the form with a success card. */
    data class Success(val result: ReceiveResult) : CashuPastePhase

    /** Recoverable failure. Surfaces inline under the form. */
    data class Error(val message: String) : CashuPastePhase
}

/**
 * Paste a Cashu token and claim it. Drives the
 * [uniffi.agicash_ffi.ReceiveFlow] state machine — pasting a token from
 * an unknown mint surfaces a confirmation card ("Add Mint and Claim") so
 * the user can accept the new mint inline. Mirrors iOS
 * `CashuTokenPasteView` (cross-account-ios @ `8b630a5`) and React's
 * `<ReceiveToken/>` page
 * (`app/features/receive/receive-cashu-token.tsx` lines 333-339, the
 * `isReceiveAccountKnown=false` branch where the CTA copy switches to
 * "Add Mint and Claim").
 *
 * **Cross-account fix (2026-05-22):** previously this page called
 * `WalletViewModel.receive(token)`, which short-circuited with the raw
 * FFI error `"no matching account for mint <url> — add the mint first"`
 * when the token came from a mint the user hadn't added. Now drives
 * `ReceiveFlow` instead.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CashuTokenPastePage(
    viewModel: WalletViewModel,
    onDismissCarousel: () -> Unit,
) {
    var token by rememberSaveable { mutableStateOf("") }
    var phase by remember { mutableStateOf<CashuPastePhase>(CashuPastePhase.Entry) }
    // Live `ReceiveFlow` handle for the current interaction. Constructed
    // on submit, dropped when the view returns to Entry or terminates.
    // Held as `remember` (not `rememberSaveable`) because the Rust handle
    // is process-local; saved state restoration would carry a stale
    // reference. Mirrors iOS `@State private var flow: ReceiveFlow?`.
    var flow by remember { mutableStateOf<ReceiveFlow?>(null) }
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    if (phase is CashuPastePhase.Success) {
        LaunchedEffect(phase) {
            delay(2000)
            onDismissCarousel()
        }
    }

    // Translate a `ReceiveFlowStateFfi` snapshot into our local `Phase`
    // and run follow-up actions (refresh accounts on terminal success).
    suspend fun render(state: ReceiveFlowStateFfi) {
        when (state) {
            is ReceiveFlowStateFfi.Idle -> {
                // The flow starts in Idle but the very next event drives
                // it forward; if we observe it map to entry so the user
                // can try again.
                phase = CashuPastePhase.Entry
                flow = null
            }
            is ReceiveFlowStateFfi.Parsing -> phase = CashuPastePhase.Working
            is ReceiveFlowStateFfi.NeedsMintConfirmation ->
                phase = CashuPastePhase.ConfirmingMint(state.confirmation)
            is ReceiveFlowStateFfi.AddingMint -> phase = CashuPastePhase.AddingMint
            is ReceiveFlowStateFfi.Swapping -> phase = CashuPastePhase.Swapping
            is ReceiveFlowStateFfi.Done -> {
                phase = CashuPastePhase.Success(receiveResultFromFlow(state.result))
                // Refresh so Home's balance/accounts list reflects the
                // new proofs without forcing a pull-to-refresh. Mirrors
                // what `receive(token:)` used to do.
                viewModel.refreshAccounts()
                flow = null
            }
            is ReceiveFlowStateFfi.AlreadyClaimed -> {
                phase = CashuPastePhase.Success(receiveResultFromAlreadyClaimed(state.info))
                flow = null
            }
            is ReceiveFlowStateFfi.Failed -> {
                phase = CashuPastePhase.Error(state.reason)
                flow = null
            }
        }
    }

    fun handleError(e: Throwable) {
        flow = null
        phase = when (e) {
            is FfiException.Auth -> CashuPastePhase.Error("auth/${e.code}: ${e.message}")
            is FfiException.Storage -> CashuPastePhase.Error("storage/${e.code}: ${e.message}")
            is FfiException.Internal -> CashuPastePhase.Error(e.message ?: "internal error")
            else -> CashuPastePhase.Error("unexpected: ${e.message}")
        }
    }

    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = Spacing.l, vertical = Spacing.xxl),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        when (val p = phase) {
            is CashuPastePhase.Success -> ReceiveSuccessCard(
                amount = p.result.amount,
                unit = p.result.unit,
                subline = p.result.mintUrl,
                headline = when (p.result.status) {
                    ReceiveStatus.RECEIVED -> "Token received"
                    ReceiveStatus.ALREADY_CLAIMED -> "Already claimed"
                    ReceiveStatus.PENDING -> "Pending"
                    ReceiveStatus.ALREADY_FAILED -> "Token unavailable"
                },
                onDone = onDismissCarousel,
            )
            is CashuPastePhase.ConfirmingMint -> MintConfirmationCard(
                confirmation = p.confirmation,
                onConfirm = {
                    val activeFlow = flow
                    if (activeFlow == null) {
                        phase = CashuPastePhase.Error(
                            "Receive flow was lost. Please paste the token again.",
                        )
                        return@MintConfirmationCard
                    }
                    phase = CashuPastePhase.AddingMint
                    scope.launch {
                        try {
                            val next = activeFlow.dispatch(ReceiveFlowEventFfi.ConfirmAddMint)
                            render(next)
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: Throwable) {
                            handleError(e)
                        }
                    }
                },
                onCancel = {
                    val activeFlow = flow
                    scope.launch {
                        // Best-effort: the cancel transition is internal
                        // bookkeeping; we don't need to surface the
                        // resulting state, just drop the handle.
                        if (activeFlow != null) {
                            runCatching {
                                activeFlow.dispatch(ReceiveFlowEventFfi.CancelAddMint)
                            }
                        }
                        flow = null
                        phase = CashuPastePhase.Entry
                    }
                },
            )
            is CashuPastePhase.AddingMint -> CenteredProgress("Adding mint…")
            is CashuPastePhase.Swapping -> CenteredProgress("Claiming token…")
            else -> CashuPasteFormCard(
                token = token,
                onTokenChange = {
                    token = it
                    if (phase is CashuPastePhase.Error) phase = CashuPastePhase.Entry
                },
                isWorking = p is CashuPastePhase.Working,
                errorMessage = (p as? CashuPastePhase.Error)?.message,
                onPaste = {
                    clipboard.getText()?.text?.let {
                        if (it.isNotEmpty()) {
                            token = it
                            if (phase is CashuPastePhase.Error) phase = CashuPastePhase.Entry
                        }
                    }
                },
                onReceive = {
                    val trimmed = token.trim()
                    if (trimmed.isEmpty()) {
                        phase = CashuPastePhase.Error("Paste a Cashu token first.")
                        return@CashuPasteFormCard
                    }
                    // Step 0: extract the encoded cashu token from
                    // whatever the user pasted (URL with ?token=…/#…,
                    // cashu: URI, embedded prose, or raw
                    // cashuA…/cashuB…). The downstream FFI receive is
                    // strict — passing the raw URL through re-creates
                    // the wrap-paste bug.
                    val encoded = extractCashuToken(trimmed)
                    if (encoded == null) {
                        phase = CashuPastePhase.Error("No Cashu token found in that text.")
                        return@CashuPasteFormCard
                    }
                    phase = CashuPastePhase.Working
                    scope.launch {
                        // Construct a fresh flow handle. Failure here is
                        // auth/transient (no session, FFI init issue) —
                        // surface and bail.
                        val activeFlow = when (
                            val o = viewModel.makeReceiveFlow()
                        ) {
                            is WalletViewModel.ReceiveFlowOutcome.Success -> {
                                flow = o.flow
                                o.flow
                            }
                            is WalletViewModel.ReceiveFlowOutcome.Failure -> {
                                phase = CashuPastePhase.Error(o.message)
                                return@launch
                            }
                        }
                        try {
                            val next = activeFlow.dispatch(
                                ReceiveFlowEventFfi.Start(encoded),
                            )
                            render(next)
                        } catch (e: CancellationException) {
                            throw e
                        } catch (e: Throwable) {
                            handleError(e)
                        }
                    }
                },
            )
        }
    }
}

/**
 * Convert a [ReceiveFlowResultFfi] to the legacy [ReceiveResult] shape
 * so the existing [ReceiveSuccessCard] renders unchanged. The two types
 * are structurally identical except [ReceiveStatusFfi] has three
 * variants (the `AlreadyClaimed` case lives on [ReceiveFlowStateFfi]
 * instead) — map each variant 1:1. Mirrors iOS
 * `receiveResult(fromFlow:)`.
 */
private fun receiveResultFromFlow(result: ReceiveFlowResultFfi): ReceiveResult {
    val status = when (result.status) {
        ReceiveStatusFfi.RECEIVED -> ReceiveStatus.RECEIVED
        ReceiveStatusFfi.ALREADY_FAILED -> ReceiveStatus.ALREADY_FAILED
        ReceiveStatusFfi.PENDING -> ReceiveStatus.PENDING
    }
    return ReceiveResult(
        status = status,
        amount = result.amount,
        fee = result.fee,
        unit = result.unit,
        currency = result.currency,
        accountId = result.accountId,
        mintUrl = result.mintUrl,
        tokenHash = result.tokenHash,
    )
}

/**
 * Synthesize a [ReceiveResult] for the [ReceiveFlowStateFfi.AlreadyClaimed]
 * state. The info doesn't carry amount/fee (per design — re-rendering
 * "0 sats" would be misleading); pass empty strings and let
 * [ReceiveSuccessCard] surface the "already claimed" headline via the
 * status enum. Mirrors iOS `receiveResult(fromAlreadyClaimed:)`.
 */
private fun receiveResultFromAlreadyClaimed(info: AlreadyClaimedInfoFfi): ReceiveResult =
    ReceiveResult(
        status = ReceiveStatus.ALREADY_CLAIMED,
        amount = "",
        fee = "",
        unit = info.unit,
        currency = info.currency,
        accountId = info.accountId,
        mintUrl = info.mintUrl,
        tokenHash = info.tokenHash,
    )

/**
 * Confirmation card shown when the pasted token is from a mint the user
 * hasn't added yet. Mirrors React's `<ReceiveToken/>` "Add Mint and
 * Claim" branch (`app/features/receive/receive-cashu-token.tsx` lines
 * 333-339) and the iOS sibling `MintConfirmationCard`
 * (`ios/Agicash/Agicash/CashuTokenPasteView.swift` @ `cross-account-ios`).
 *
 * Mobile form factor doesn't host React's `<AccountSelector/>` — Slice 2
 * scope is source-mint-only (the Rust `ReceiveFlow` state machine
 * doesn't surface alternative destinations yet, per
 * `2026-05-22-cross-account-audit.md`). So this card collapses the React
 * picker into a single mint preview plus the same CTA pair.
 *
 * Visual rhythm matches [CashuPasteFormCard] / [ReceiveSuccessCard]:
 * card chrome via [BrandCard], header + body block, primary
 * "Add Mint and Claim" + ghost "Cancel" — the same shape the receive
 * surface uses across all phases.
 */
@Composable
private fun MintConfirmationCard(
    confirmation: MintConfirmationFfi,
    onConfirm: () -> Unit,
    onCancel: () -> Unit,
) {
    BrandCard {
        Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                Text(
                    "Add this mint?",
                    style = BrandTypography.title,
                    color = BrandColors.cardForeground,
                )
                Text(
                    "This token is from a mint you haven't added yet. Add it to claim the funds.",
                    style = BrandTypography.label,
                    color = BrandColors.mutedForeground,
                )
            }

            // Mint identity block — mirrors AddMintSuccessCard's geometry:
            // big name, monospaced URL underneath.
            Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.s),
            ) {
                Text(
                    confirmation.mintName,
                    style = BrandTypography.title,
                    color = BrandColors.cardForeground,
                    maxLines = 1,
                )
                Text(
                    confirmation.mintUrl,
                    style = BrandTypography.caption,
                    color = BrandColors.mutedForeground,
                    maxLines = 1,
                )
            }

            // Amount block — mirrors ReceiveSuccessCard so pre-claim and
            // post-claim cards feel like the same visual family.
            Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.xs),
            ) {
                Text(
                    "Claiming",
                    style = BrandTypography.caption,
                    color = BrandColors.mutedForeground,
                )
                Row(verticalAlignment = Alignment.Bottom) {
                    Text(
                        confirmation.amount,
                        style = BrandTypography.numericInline,
                        color = BrandColors.cardForeground,
                    )
                    Spacer(Modifier.size(6.dp))
                    Text(
                        confirmation.unit,
                        style = BrandTypography.label,
                        color = BrandColors.mutedForeground,
                    )
                }
                if (confirmation.fee.isNotEmpty() && confirmation.fee != "0") {
                    Text(
                        "Mint fee: ${confirmation.fee} ${confirmation.unit}",
                        style = BrandTypography.caption,
                        color = BrandColors.mutedForeground,
                    )
                }
            }

            // CTA stack — primary "Add Mint and Claim" (exact React +
            // iOS copy) + ghost "Cancel". Same shape as
            // CashuPasteFormCard's Receive button.
            BrandButton(
                label = "Add Mint and Claim",
                onClick = onConfirm,
                variant = BrandButtonVariant.Primary,
            )
            BrandButton(
                label = "Cancel",
                onClick = onCancel,
                variant = BrandButtonVariant.Ghost,
            )
        }
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CashuPasteFormCard(
    token: String,
    onTokenChange: (String) -> Unit,
    isWorking: Boolean,
    errorMessage: String?,
    onPaste: () -> Unit,
    onReceive: () -> Unit,
) {
    BrandCard {
        Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                Text("Receive Cashu", style = BrandTypography.title, color = BrandColors.cardForeground)
                Text(
                    "Paste a Cashu token to claim it into your wallet",
                    style = BrandTypography.label,
                    color = BrandColors.mutedForeground,
                )
            }
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Text(
                        "Token",
                        style = BrandTypography.labelEmphasis,
                        color = BrandColors.cardForeground,
                        modifier = Modifier.weight(1f),
                    )
                    Text(
                        "Paste",
                        style = BrandTypography.label,
                        color = BrandColors.cardForeground,
                        modifier = Modifier
                            .clip(Radius.control)
                            .padding(Spacing.xs)
                            .clickableNoIndication(enabled = !isWorking, onClick = onPaste),
                    )
                }
                OutlinedTextField(
                    value = token,
                    onValueChange = onTokenChange,
                    enabled = !isWorking,
                    placeholder = {
                        Text("cashuB...", style = BrandTypography.body, color = BrandColors.mutedForeground)
                    },
                    textStyle = BrandTypography.body,
                    modifier = Modifier.fillMaxWidth().heightIn(min = 96.dp),
                    shape = Radius.control,
                    colors = brandFieldColors(),
                )
            }
            if (errorMessage != null) {
                Text(errorMessage, style = BrandTypography.caption, color = BrandColors.destructive)
            }
            BrandButton(
                label = "Receive",
                onClick = onReceive,
                variant = BrandButtonVariant.Primary,
                isLoading = isWorking,
                enabled = token.trim().isNotEmpty(),
            )
        }
    }
}

// ---- Lightning receive page ----

private sealed interface LnReceivePhase {
    data object AmountEntry : LnReceivePhase
    data object Generating : LnReceivePhase
    /**
     * Invoice phase covers both "show invoice, poll for payment" and the
     * subsequent "minting proofs" handoff. [completing] flips true once
     * the mint reports PAID/COMPLETED and we're awaiting the FFI
     * `completeMintQuote` call. Critically, the polling
     * [LaunchedEffect] is keyed on `handle.quoteId` (NOT on the phase
     * identity or [completing]), so the in-flight suspend is NOT
     * cancelled mid-call by the visual transition — that was the bug
     * where success surfaced as "unexpected: The coroutine scope left
     * the composition".
     */
    data class Invoice(
        val handle: MintQuoteHandle,
        val completing: Boolean = false,
    ) : LnReceivePhase
    data class Success(val result: ReceiveResult) : LnReceivePhase
    data class Failure(val message: String) : LnReceivePhase
}

private enum class LnEntryCurrency(val ffi: String, val unit: String, val allowsDecimal: Boolean) {
    BTC("BTC", "sats", false),
    USD("USD", "USD", true),
}

/**
 * Generate a BOLT-11 invoice and watch the mint until paid. Mirrors
 * iOS `LightningReceiveView`'s state machine: amountEntry →
 * generating → invoice (poll every 2s) → completing → success
 * (auto-dismiss 4s; Receive more / Done) / failure (Try again /
 * Dismiss). The sats⇄USD toggle drives the FFI `currency` argument
 * directly — the mint is quoted natively, no client-side conversion
 * (correctness never depends on a rate; the converted-amount line iOS
 * shows is cosmetic and omitted here since it's a non-FFI nicety).
 */
@Composable
private fun LightningReceivePage(
    viewModel: WalletViewModel,
    onDismissCarousel: () -> Unit,
) {
    var phase by remember { mutableStateOf<LnReceivePhase>(LnReceivePhase.AmountEntry) }
    var amountBuffer by rememberSaveable { mutableStateOf("0") }
    var entryCurrency by rememberSaveable { mutableStateOf(LnEntryCurrency.BTC) }
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    val parsed = parseAmountMinorUnits(amountBuffer, entryCurrency.allowsDecimal)
    val amountValid = parsed != null && parsed > 0u

    // Poll loop while on the invoice phase. Tied to the handle; cancels
    // automatically when the phase leaves Invoice (LaunchedEffect key).
    val invoicePhase = phase as? LnReceivePhase.Invoice
    LaunchedEffect(invoicePhase?.handle?.quoteId) {
        val h = invoicePhase?.handle ?: return@LaunchedEffect
        while (isActive) {
            delay(2000)
            when (val o = viewModel.pollLightningQuote(h.quoteId)) {
                is WalletViewModel.LightningPollOutcome.State -> when (o.state) {
                    MintQuoteFfiState.UNPAID -> Unit // keep polling
                    MintQuoteFfiState.PAID, MintQuoteFfiState.COMPLETED -> {
                        // Flip the "completing" flag on the SAME Invoice
                        // phase — preserves `handle.quoteId` as the
                        // LaunchedEffect key, so the suspend below
                        // doesn't get cancelled out from under us.
                        phase = LnReceivePhase.Invoice(h, completing = true)
                        phase = when (val c = viewModel.completeLightningQuote(h.quoteId)) {
                            is WalletViewModel.ReceiveOutcome.Success ->
                                LnReceivePhase.Success(c.result)
                            is WalletViewModel.ReceiveOutcome.Failure ->
                                LnReceivePhase.Failure(c.message)
                        }
                        return@LaunchedEffect
                    }
                    MintQuoteFfiState.EXPIRED -> {
                        phase = LnReceivePhase.Failure("Invoice expired before payment. Try again.")
                        return@LaunchedEffect
                    }
                    MintQuoteFfiState.FAILED -> {
                        phase = LnReceivePhase.Failure(o.failureReason ?: "Quote failed.")
                        return@LaunchedEffect
                    }
                }
                // Transient network blip — keep polling so a single
                // failure doesn't kick the user off the invoice screen.
                is WalletViewModel.LightningPollOutcome.Failure -> Unit
            }
        }
    }

    // Auto-dismiss the whole carousel 4s after success.
    LaunchedEffect(phase is LnReceivePhase.Success) {
        if (phase is LnReceivePhase.Success) {
            delay(4000)
            onDismissCarousel()
        }
    }

    fun resetToEntry() {
        amountBuffer = "0"
        phase = LnReceivePhase.AmountEntry
    }

    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        when (val p = phase) {
            is LnReceivePhase.AmountEntry -> Column(
                modifier = Modifier
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(vertical = Spacing.xxl),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.xxl),
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Row(verticalAlignment = Alignment.Bottom) {
                        Text(
                            formatAmountDisplay(amountBuffer),
                            style = BrandTypography.numericHero,
                            color = BrandColors.foreground,
                        )
                        Spacer(Modifier.size(4.dp))
                        Text(
                            entryCurrency.unit,
                            style = BrandTypography.titleSmall,
                            color = BrandColors.mutedForeground,
                            modifier = Modifier.padding(bottom = 10.dp),
                        )
                    }
                    Text(
                        "Receive over Lightning",
                        style = BrandTypography.label,
                        color = BrandColors.mutedForeground,
                    )
                    Spacer(Modifier.size(Spacing.xs))
                    CurrencyTogglePill(
                        otherLabel = if (entryCurrency == LnEntryCurrency.BTC) "USD" else "sats",
                        onClick = {
                            entryCurrency = if (entryCurrency == LnEntryCurrency.BTC) {
                                LnEntryCurrency.USD
                            } else {
                                LnEntryCurrency.BTC
                            }
                            amountBuffer = "0"
                        },
                    )
                }
                AmountNumpad(
                    value = amountBuffer,
                    onValueChange = { amountBuffer = it },
                    allowsDecimal = entryCurrency.allowsDecimal,
                    modifier = Modifier.widthIn(max = 360.dp).padding(horizontal = Spacing.l),
                )
                BrandButton(
                    label = "Create invoice",
                    onClick = {
                        val amt = parsed ?: return@BrandButton
                        phase = LnReceivePhase.Generating
                        scope.launch {
                            phase = when (
                                val o = viewModel.startLightningQuote(amt, null, entryCurrency.ffi)
                            ) {
                                is WalletViewModel.LightningQuoteOutcome.Success ->
                                    LnReceivePhase.Invoice(o.handle)
                                is WalletViewModel.LightningQuoteOutcome.Failure ->
                                    LnReceivePhase.Failure(o.message)
                            }
                        }
                    },
                    variant = BrandButtonVariant.Primary,
                    size = BrandButtonSize.Large,
                    enabled = amountValid,
                    modifier = Modifier.widthIn(max = 360.dp).padding(horizontal = Spacing.l),
                )
            }
            is LnReceivePhase.Generating -> CenteredProgress("Requesting invoice from mint…")
            is LnReceivePhase.Invoice -> if (p.completing) {
                CenteredProgress("Minting proofs…")
            } else {
                InvoiceCard(
                    handle = p.handle,
                    onCopy = { clipboard.setText(androidx.compose.ui.text.AnnotatedString(p.handle.invoice)) },
                    onCancel = { resetToEntry() },
                )
            }
            is LnReceivePhase.Success -> Column(
                modifier = Modifier.padding(Spacing.l),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.l),
            ) {
                SuccessCheckCard(
                    title = "Received",
                    amount = p.result.amount,
                    unit = p.result.unit,
                )
                Column(
                    modifier = Modifier.widthIn(max = 360.dp),
                    verticalArrangement = Arrangement.spacedBy(Spacing.s),
                ) {
                    BrandButton("Receive more", { resetToEntry() }, variant = BrandButtonVariant.Secondary)
                    BrandButton("Done", onDismissCarousel, variant = BrandButtonVariant.Primary)
                }
            }
            is LnReceivePhase.Failure -> FailureCard(
                title = "Couldn't receive",
                message = p.message,
                onRetry = { resetToEntry() },
                onDismiss = onDismissCarousel,
            )
        }
    }
}

@Composable
private fun CurrencyTogglePill(otherLabel: String, onClick: () -> Unit) {
    Row(
        modifier = Modifier
            .clip(CircleShape)
            .background(BrandColors.muted)
            .clickableNoIndication(onClick = onClick)
            .padding(horizontal = Spacing.m, vertical = Spacing.s),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
    ) {
        Icon(
            Icons.Outlined.SwapVert,
            contentDescription = null,
            tint = BrandColors.mutedForeground,
            modifier = Modifier.size(14.dp),
        )
        Text(otherLabel, style = BrandTypography.label, color = BrandColors.mutedForeground)
    }
}

@Composable
private fun InvoiceCard(
    handle: MintQuoteHandle,
    onCopy: () -> Unit,
    onCancel: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(Spacing.l),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Spacing.l),
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Row(verticalAlignment = Alignment.Bottom) {
                Text(handle.amount, style = BrandTypography.numericInline, color = BrandColors.foreground)
                Spacer(Modifier.size(4.dp))
                Text(handle.unit, style = BrandTypography.label, color = BrandColors.mutedForeground)
            }
            Text("Waiting for payment…", style = BrandTypography.label, color = BrandColors.mutedForeground)
        }
        val qr = remember(handle.invoice) { QrCode.encode(handle.invoice, 720) }
        if (qr != null) {
            androidx.compose.foundation.Image(
                bitmap = qr,
                contentDescription = "Lightning invoice QR code",
                modifier = Modifier
                    .size(240.dp)
                    .clip(Radius.control)
                    .graphicsLayer { },
            )
        } else {
            Box(
                modifier = Modifier
                    .size(240.dp)
                    .clip(Radius.control)
                    .background(BrandColors.muted),
                contentAlignment = Alignment.Center,
            ) {
                Text("QR unavailable", style = BrandTypography.caption, color = BrandColors.mutedForeground)
            }
        }
        Row(
            modifier = Modifier
                .clip(Radius.control)
                .background(BrandColors.muted)
                .clickableNoIndication(onClick = onCopy)
                .padding(horizontal = Spacing.m, vertical = Spacing.s)
                .widthIn(max = 280.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
        ) {
            Text(
                truncateMiddle(handle.invoice),
                style = BrandTypography.caption,
                color = BrandColors.mutedForeground,
                maxLines = 1,
                modifier = Modifier.weight(1f, fill = false),
            )
            Icon(
                Icons.Outlined.ContentCopy,
                contentDescription = "Copy invoice",
                tint = BrandColors.mutedForeground,
                modifier = Modifier.size(14.dp),
            )
        }
        if (handle.fee.isNotEmpty() && handle.fee != "0") {
            Row(
                modifier = Modifier.widthIn(max = 280.dp).fillMaxWidth().padding(horizontal = Spacing.m),
            ) {
                Text("Mint fee", style = BrandTypography.label, color = BrandColors.mutedForeground, modifier = Modifier.weight(1f))
                Text("${handle.fee} ${handle.unit}", style = BrandTypography.label, color = BrandColors.foreground)
            }
        }
        BrandButton(
            "Cancel",
            onCancel,
            variant = BrandButtonVariant.Ghost,
            modifier = Modifier.widthIn(max = 280.dp),
        )
    }
}

// ---- Buy placeholder page (visual scaffolding only — mirrors iOS BuyView) ----

@Composable
private fun BuyPlaceholderPage() {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = Spacing.l, vertical = Spacing.xxl),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        BrandCard {
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
                Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                    Text("Buy sats", style = BrandTypography.title, color = BrandColors.cardForeground)
                    Text(
                        "Top up with fiat via Cash App",
                        style = BrandTypography.label,
                        color = BrandColors.mutedForeground,
                    )
                }
                Text(
                    "Fiat onramp is coming soon. For now, receive over Lightning or paste a Cashu token.",
                    style = BrandTypography.body,
                    color = BrandColors.mutedForeground,
                )
                BrandButton("Coming soon", {}, variant = BrandButtonVariant.Primary, enabled = false)
            }
        }
    }
}

// ---- Shared pieces ----

@Composable
private fun ReceiveSuccessCard(
    amount: String,
    unit: String,
    subline: String,
    headline: String,
    onDone: () -> Unit,
) {
    BrandCard {
        Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
            Text(headline, style = BrandTypography.title, color = BrandColors.cardForeground)
            Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.s),
            ) {
                // Only render the amount line when we have one. The
                // `AlreadyClaimed` state synthesizes an empty `amount`
                // because the FFI deliberately omits it (re-rendering
                // "0 sats" would be misleading). Mirrors iOS
                // `SuccessCard`.
                if (amount.isNotEmpty()) {
                    Row(verticalAlignment = Alignment.Bottom) {
                        Text(amount, style = BrandTypography.numericInline, color = BrandColors.cardForeground)
                        Spacer(Modifier.size(6.dp))
                        Text(unit, style = BrandTypography.label, color = BrandColors.mutedForeground)
                    }
                }
                Text(
                    subline,
                    style = BrandTypography.caption,
                    color = BrandColors.mutedForeground,
                    maxLines = 1,
                )
            }
            BrandButton("Done", onDone, variant = BrandButtonVariant.Primary)
        }
    }
}

@Composable
internal fun SuccessCheckCard(title: String, amount: String, unit: String) {
    BrandCard {
        Column(
            modifier = Modifier.fillMaxWidth(),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(Spacing.m),
        ) {
            Icon(
                Icons.Filled.CheckCircle,
                contentDescription = null,
                tint = Color(0xFF22C55E),
                modifier = Modifier.size(56.dp),
            )
            Text(title, style = BrandTypography.title, color = BrandColors.cardForeground)
            Row(verticalAlignment = Alignment.Bottom) {
                Text(amount, style = BrandTypography.numericInline, color = BrandColors.cardForeground)
                Spacer(Modifier.size(4.dp))
                Text(unit, style = BrandTypography.label, color = BrandColors.mutedForeground)
            }
        }
    }
}

@Composable
internal fun FailureCard(
    title: String,
    message: String,
    onRetry: (() -> Unit)?,
    onDismiss: () -> Unit,
) {
    Column(
        modifier = Modifier.padding(Spacing.l),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Spacing.l),
    ) {
        BrandCard {
            Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.m),
            ) {
                Icon(
                    Icons.Outlined.ErrorOutline,
                    contentDescription = null,
                    tint = BrandColors.destructive,
                    modifier = Modifier.size(48.dp),
                )
                Text(title, style = BrandTypography.title, color = BrandColors.cardForeground)
                Text(
                    message,
                    style = BrandTypography.label,
                    color = BrandColors.mutedForeground,
                    textAlign = TextAlign.Center,
                )
            }
        }
        Column(
            modifier = Modifier.widthIn(max = 360.dp),
            verticalArrangement = Arrangement.spacedBy(Spacing.s),
        ) {
            if (onRetry != null) {
                BrandButton("Try again", onRetry, variant = BrandButtonVariant.Primary)
                BrandButton("Dismiss", onDismiss, variant = BrandButtonVariant.Ghost)
            } else {
                BrandButton("Dismiss", onDismiss, variant = BrandButtonVariant.Primary)
            }
        }
    }
}

@Composable
internal fun CenteredProgress(label: String) {
    Column(
        modifier = Modifier.fillMaxSize(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        androidx.compose.material3.CircularProgressIndicator()
        Spacer(Modifier.size(Spacing.m))
        Text(label, style = BrandTypography.label, color = BrandColors.mutedForeground)
    }
}

@Composable
internal fun BrandCard(content: @Composable () -> Unit) {
    Box(
        modifier = Modifier
            .widthIn(max = 384.dp)
            .fillMaxWidth()
            .clip(Radius.card)
            .background(BrandColors.card)
            .border(0.5.dp, BrandColors.border, Radius.card)
            .padding(Spacing.xxl),
    ) { content() }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun brandFieldColors() = OutlinedTextFieldDefaults.colors(
    focusedContainerColor = BrandColors.background,
    unfocusedContainerColor = BrandColors.background,
    focusedBorderColor = BrandColors.foreground,
    unfocusedBorderColor = BrandColors.border,
    focusedTextColor = BrandColors.foreground,
    unfocusedTextColor = BrandColors.foreground,
)

internal fun truncateMiddle(s: String): String {
    if (s.length <= 24) return s
    return "${s.take(12)}…${s.takeLast(8)}"
}

/**
 * `clickable` with no ripple/indication. Shared by the carousel
 * screens (the `AddMintScreen` one is `private`; this is the
 * package-internal equivalent so the receive/send pages reuse it).
 */
@Composable
internal fun Modifier.clickableNoIndication(
    enabled: Boolean = true,
    onClick: () -> Unit,
): Modifier {
    val interactionSource = remember { MutableInteractionSource() }
    return this.clickable(
        interactionSource = interactionSource,
        indication = null,
        enabled = enabled,
        onClick = onClick,
    )
}
