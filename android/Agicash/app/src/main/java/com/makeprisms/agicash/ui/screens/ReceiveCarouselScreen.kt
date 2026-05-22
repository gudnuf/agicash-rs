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
import uniffi.agicash_ffi.extractCashuToken
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import uniffi.agicash_ffi.MintQuoteFfiState
import uniffi.agicash_ffi.MintQuoteHandle
import uniffi.agicash_ffi.ReceiveResult

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

private sealed interface CashuPastePhase {
    data object Entry : CashuPastePhase
    data object Working : CashuPastePhase
    data class Success(val result: ReceiveResult) : CashuPastePhase
    data class Error(val message: String) : CashuPastePhase
}

/**
 * Paste a Cashu token and claim it. Mirrors iOS
 * `CashuTokenPasteView`'s state machine: entry → working → success
 * (auto-dismisses the whole carousel after 2s, or Done) / error
 * (inline, retry-in-place).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun CashuTokenPastePage(
    viewModel: WalletViewModel,
    onDismissCarousel: () -> Unit,
) {
    var token by rememberSaveable { mutableStateOf("") }
    var phase by remember { mutableStateOf<CashuPastePhase>(CashuPastePhase.Entry) }
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    if (phase is CashuPastePhase.Success) {
        LaunchedEffect(phase) {
            delay(2000)
            onDismissCarousel()
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
                headline = when (p.result.status.name) {
                    "RECEIVED" -> "Token received"
                    "ALREADY_CLAIMED" -> "Already claimed"
                    "PENDING" -> "Pending"
                    else -> "Token unavailable"
                },
                onDone = onDismissCarousel,
            )
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
                    // Step 0: extract the encoded cashu token from whatever
                    // the user pasted (URL with ?token=…/#…, cashu: URI,
                    // embedded prose, or raw cashuA…/cashuB…). The
                    // downstream FFI receive is strict — passing the raw
                    // URL through re-creates the wrap-paste bug.
                    val encoded = extractCashuToken(trimmed)
                    if (encoded == null) {
                        phase = CashuPastePhase.Error("No Cashu token found in that text.")
                        return@CashuPasteFormCard
                    }
                    phase = CashuPastePhase.Working
                    scope.launch {
                        phase = when (val o = viewModel.receive(encoded)) {
                            is WalletViewModel.ReceiveOutcome.Success ->
                                CashuPastePhase.Success(o.result)
                            is WalletViewModel.ReceiveOutcome.Failure ->
                                CashuPastePhase.Error(o.message)
                        }
                    }
                },
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
                Row(verticalAlignment = Alignment.Bottom) {
                    Text(amount, style = BrandTypography.numericInline, color = BrandColors.cardForeground)
                    Spacer(Modifier.size(6.dp))
                    Text(unit, style = BrandTypography.label, color = BrandColors.mutedForeground)
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
