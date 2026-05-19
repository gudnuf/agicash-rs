package com.makeprisms.agicash.ui.screens

import android.content.Intent
import androidx.compose.foundation.background
import androidx.compose.foundation.border
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
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Bolt
import androidx.compose.material.icons.outlined.AccountBalanceWallet
import androidx.compose.material.icons.outlined.AlternateEmail
import androidx.compose.material.icons.outlined.Close
import androidx.compose.material.icons.outlined.ContentCopy
import androidx.compose.material.icons.outlined.Share
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import com.makeprisms.agicash.ui.components.AmountNumpad
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
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import uniffi.agicash_ffi.MeltQuoteFfiState
import uniffi.agicash_ffi.MeltQuoteHandle
import uniffi.agicash_ffi.MeltQuotePreview
import uniffi.agicash_ffi.MeltQuoteSnapshot
import uniffi.agicash_ffi.SendQuotePreview
import uniffi.agicash_ffi.SendSwapClaimState
import uniffi.agicash_ffi.SendSwapHandle

/**
 * Top-level Send surface. Compose analogue of
 * `ios/Agicash/Agicash/SendCarouselView.swift`.
 *
 * Three-tab swipeable carousel: Cashu (token) / Lightning (BOLT-11
 * melt) / Lightning Address (LUD-16 → melt). Sibling of
 * [ReceiveCarouselScreen] — same `HorizontalPager` + custom bottom
 * indicator-bar pattern. Opens on the Cashu tab, matching iOS's
 * default `initialTab`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SendCarouselScreen(
    viewModel: WalletViewModel,
    onClose: () -> Unit,
) {
    val tabs = SendTab.entries
    val pager = rememberPagerState(initialPage = SendTab.CASHU.ordinal) { tabs.size }
    val scope = rememberCoroutineScope()
    val selected = tabs[pager.currentPage]

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(selected.title, style = BrandTypography.titleSmall) },
                actions = {
                    IconButton(onClick = onClose) {
                        Icon(Icons.Outlined.Close, contentDescription = "Close", tint = BrandColors.foreground)
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
                    SendTab.CASHU -> SendCashuPage(viewModel, onClose)
                    SendTab.LIGHTNING -> LightningSendPage(viewModel, onClose, presetInvoice = null)
                    SendTab.LN_ADDRESS -> LightningAddressSendPage(viewModel, onClose)
                }
            }
            SendTabIndicatorBar(
                tabs = tabs,
                selected = selected,
                onSelect = { tab -> scope.launch { pager.animateScrollToPage(tab.ordinal) } },
            )
        }
    }
}

private enum class SendTab(val title: String, val icon: ImageVector) {
    CASHU("Send Cashu", Icons.Outlined.AccountBalanceWallet),
    LIGHTNING("Send Lightning", Icons.Filled.Bolt),
    LN_ADDRESS("Send to address", Icons.Outlined.AlternateEmail),
}

@Composable
private fun SendTabIndicatorBar(
    tabs: List<SendTab>,
    selected: SendTab,
    onSelect: (SendTab) -> Unit,
) {
    Row(
        modifier = Modifier
            .fillMaxWidth()
            .background(BrandColors.background)
            .border(0.5.dp, BrandColors.border)
            .padding(horizontal = Spacing.l, vertical = Spacing.s),
    ) {
        tabs.forEach { tab ->
            Box(modifier = Modifier.weight(1f).height(40.dp), contentAlignment = Alignment.Center) {
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

// ---- Cashu send page ----

private sealed interface CashuSendPhase {
    data object AmountEntry : CashuSendPhase
    data object Quoting : CashuSendPhase
    data class Confirming(val quote: SendQuotePreview) : CashuSendPhase
    data object Swapping : CashuSendPhase
    data class Share(val handle: SendSwapHandle) : CashuSendPhase
    data class Claimed(val handle: SendSwapHandle) : CashuSendPhase
    data class Failure(val message: String) : CashuSendPhase
}

/**
 * Pick an amount, produce a Cashu token, share it, watch for the
 * receiver to claim. Mirrors iOS `SendCashuTokenView`'s state machine:
 * amountEntry → quoting → confirming → swapping → share (poll claim
 * every 3s) → claimed (auto-dismiss 3s) / failure. BTC/sats-only for
 * v0, same constraint the iOS view ships under.
 */
@Composable
private fun SendCashuPage(
    viewModel: WalletViewModel,
    onDismissCarousel: () -> Unit,
) {
    var phase by remember { mutableStateOf<CashuSendPhase>(CashuSendPhase.AmountEntry) }
    var amountBuffer by rememberSaveable { mutableStateOf("0") }
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current
    val context = LocalContext.current
    var copied by remember { mutableStateOf(false) }

    val parsed = parseAmountMinorUnits(amountBuffer, allowsDecimal = false)
    val amountValid = parsed != null && parsed > 0u

    // Poll the receiver-claim every 3s while on the Share phase.
    val sharePhase = phase as? CashuSendPhase.Share
    LaunchedEffect(sharePhase?.handle?.swapId) {
        val h = sharePhase?.handle ?: return@LaunchedEffect
        while (isActive) {
            delay(3000)
            when (val o = viewModel.pollSendClaim(h.swapId)) {
                is WalletViewModel.SendClaimOutcome.State -> when (o.state) {
                    SendSwapClaimState.PENDING -> Unit
                    SendSwapClaimState.COMPLETED -> {
                        phase = CashuSendPhase.Claimed(h)
                        return@LaunchedEffect
                    }
                    SendSwapClaimState.FAILED -> {
                        phase = CashuSendPhase.Failure(o.failureReason ?: "Send failed.")
                        return@LaunchedEffect
                    }
                }
                is WalletViewModel.SendClaimOutcome.Failure -> Unit // transient, keep polling
            }
        }
    }

    LaunchedEffect(phase is CashuSendPhase.Claimed) {
        if (phase is CashuSendPhase.Claimed) {
            delay(3000)
            onDismissCarousel()
        }
    }

    fun resetToEntry() {
        amountBuffer = "0"
        phase = CashuSendPhase.AmountEntry
    }

    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        when (val p = phase) {
            is CashuSendPhase.AmountEntry -> AmountEntryColumn(
                amountBuffer = amountBuffer,
                onBufferChange = { amountBuffer = it },
                unit = "sats",
                caption = "Send Cashu token",
                ctaLabel = "Continue",
                ctaEnabled = amountValid,
                onCta = {
                    val amt = parsed ?: return@AmountEntryColumn
                    phase = CashuSendPhase.Quoting
                    scope.launch {
                        phase = when (val o = viewModel.prepareSend(amt, null, "BTC")) {
                            is WalletViewModel.SendQuoteOutcome.Success ->
                                CashuSendPhase.Confirming(o.quote)
                            is WalletViewModel.SendQuoteOutcome.Failure ->
                                CashuSendPhase.Failure(o.message)
                        }
                    }
                },
            )
            is CashuSendPhase.Quoting -> CenteredProgress("Preparing send…")
            is CashuSendPhase.Confirming -> CashuConfirmCard(
                quote = p.quote,
                onSend = {
                    val amt = parsed ?: return@CashuConfirmCard
                    phase = CashuSendPhase.Swapping
                    scope.launch {
                        phase = when (val o = viewModel.createSend(amt, null, "BTC")) {
                            is WalletViewModel.SendOutcome.Success ->
                                CashuSendPhase.Share(o.handle)
                            is WalletViewModel.SendOutcome.Failure ->
                                CashuSendPhase.Failure(o.message)
                        }
                    }
                },
                onCancel = { resetToEntry() },
            )
            is CashuSendPhase.Swapping -> CenteredProgress("Producing token…")
            is CashuSendPhase.Share -> ShareCard(
                handle = p.handle,
                copied = copied,
                onCopy = {
                    clipboard.setText(AnnotatedString(p.handle.token))
                    copied = true
                    scope.launch { delay(1500); copied = false }
                },
                onShare = {
                    val intent = Intent(Intent.ACTION_SEND).apply {
                        type = "text/plain"
                        putExtra(Intent.EXTRA_TEXT, p.handle.token)
                    }
                    context.startActivity(Intent.createChooser(intent, "Share Cashu token"))
                },
                onCancel = onDismissCarousel,
            )
            is CashuSendPhase.Claimed -> Column(
                modifier = Modifier.padding(Spacing.l),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.l),
            ) {
                SuccessCheckCard(title = "Sent", amount = p.handle.amount, unit = p.handle.unit)
                BrandButton(
                    "Done",
                    onDismissCarousel,
                    variant = BrandButtonVariant.Primary,
                    modifier = Modifier.widthIn(max = 360.dp),
                )
            }
            is CashuSendPhase.Failure -> FailureCard(
                title = "Couldn't send",
                message = p.message,
                onRetry = { resetToEntry() },
                onDismiss = onDismissCarousel,
            )
        }
    }
}

@Composable
private fun CashuConfirmCard(
    quote: SendQuotePreview,
    onSend: () -> Unit,
    onCancel: () -> Unit,
) {
    Column(
        modifier = Modifier.padding(Spacing.l),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        BrandCard {
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
                Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                    Text("Confirm send", style = BrandTypography.title, color = BrandColors.cardForeground)
                    Text(
                        "Producing a token the receiver can claim",
                        style = BrandTypography.label,
                        color = BrandColors.mutedForeground,
                    )
                }
                Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
                    AmountRow("They receive", quote.amountToSend, quote.unit, prominent = true)
                    AmountRow("Send fee", quote.cashuSendFee, quote.unit)
                    AmountRow("Receive fee", quote.cashuReceiveFee, quote.unit)
                    HorizontalDivider(color = BrandColors.border)
                    AmountRow("You pay", quote.totalAmount, quote.unit, prominent = true)
                }
                Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
                    BrandButton("Send", onSend, variant = BrandButtonVariant.Primary)
                    BrandButton("Cancel", onCancel, variant = BrandButtonVariant.Ghost)
                }
            }
        }
    }
}

@Composable
private fun ShareCard(
    handle: SendSwapHandle,
    copied: Boolean,
    onCopy: () -> Unit,
    onShare: () -> Unit,
    onCancel: () -> Unit,
) {
    Column(
        modifier = Modifier.padding(Spacing.l),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        BrandCard {
            Column(
                modifier = Modifier.fillMaxWidth(),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.l),
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Row(verticalAlignment = Alignment.Bottom) {
                        Text(handle.amount, style = BrandTypography.numericInline, color = BrandColors.cardForeground)
                        Spacer(Modifier.size(4.dp))
                        Text(handle.unit, style = BrandTypography.label, color = BrandColors.mutedForeground)
                    }
                    Row(
                        verticalAlignment = Alignment.CenterVertically,
                        horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
                    ) {
                        CircularProgressIndicator(modifier = Modifier.size(14.dp), strokeWidth = 2.dp)
                        Text("Waiting for receiver…", style = BrandTypography.label, color = BrandColors.mutedForeground)
                    }
                }
                Row(
                    modifier = Modifier
                        .clip(Radius.control)
                        .background(BrandColors.muted)
                        .clickableNoIndication(onClick = onCopy)
                        .padding(horizontal = Spacing.m, vertical = Spacing.s)
                        .widthIn(max = 320.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(Spacing.xs),
                ) {
                    Text(
                        truncateMiddle(handle.token),
                        style = BrandTypography.caption,
                        color = BrandColors.mutedForeground,
                        maxLines = 1,
                        modifier = Modifier.weight(1f, fill = false),
                    )
                    Icon(
                        Icons.Outlined.ContentCopy,
                        contentDescription = if (copied) "Copied" else "Copy token",
                        tint = BrandColors.mutedForeground,
                        modifier = Modifier.size(14.dp),
                    )
                }
                Column(
                    modifier = Modifier.widthIn(max = 320.dp),
                    verticalArrangement = Arrangement.spacedBy(Spacing.s),
                ) {
                    BrandButton("Share", onShare, variant = BrandButtonVariant.Primary)
                    BrandButton("Cancel", onCancel, variant = BrandButtonVariant.Ghost)
                }
            }
        }
    }
}

// ---- Lightning send page (BOLT-11 melt) ----

private sealed interface LnSendPhase {
    data object InvoiceEntry : LnSendPhase
    data object Quoting : LnSendPhase
    data class Confirming(val preview: MeltQuotePreview) : LnSendPhase
    data object Creating : LnSendPhase
    data object Paying : LnSendPhase
    data object Verifying : LnSendPhase
    data class Paid(val snapshot: MeltQuoteSnapshot) : LnSendPhase
    data class Failure(val message: String, val retryable: Boolean) : LnSendPhase
}

/**
 * Pay a BOLT-11 invoice over Lightning (NUT-05 melt). Mirrors iOS
 * `LightningSendPlaceholderView`'s state machine and — critically —
 * its double-pay safety discipline: once a melt is INITIATED for a
 * quote (`executeMeltQuote` called), every non-success outcome
 * reconciles by polling the held handle (a NUT-05 status check) and
 * is NEVER retryable (a fresh quote for the same invoice would
 * double-pay). `presetInvoice`, when non-null, skips the entry step
 * and quotes immediately — the LN-address page feeds the resolved
 * invoice down this same melt machine.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LightningSendPage(
    viewModel: WalletViewModel,
    onDismissCarousel: () -> Unit,
    presetInvoice: String?,
) {
    var phase by remember { mutableStateOf<LnSendPhase>(LnSendPhase.InvoiceEntry) }
    var invoice by rememberSaveable { mutableStateOf("") }
    var quotedInvoice by remember { mutableStateOf("") }
    // The handle the melt was initiated for — held so reconcile polls
    // the right quote and never re-quotes.
    var activeHandle by remember { mutableStateOf<MeltQuoteHandle?>(null) }
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    fun stripScheme(s: String): String {
        val t = s.trim()
        return if (t.lowercase().startsWith("lightning:")) t.drop("lightning:".length) else t
    }

    fun startPolling(handle: MeltQuoteHandle, reconcile: Boolean) {
        phase = if (reconcile) LnSendPhase.Verifying else LnSendPhase.Paying
        scope.launch {
            while (isActive) {
                delay(2000)
                when (val o = viewModel.pollMeltQuote(handle.quoteId)) {
                    is WalletViewModel.MeltStatusOutcome.State -> when (o.state) {
                        MeltQuoteFfiState.PENDING -> Unit // keep polling, no retry affordance
                        MeltQuoteFfiState.PAID -> {
                            phase = LnSendPhase.Paid(o.snapshot)
                            return@launch
                        }
                        MeltQuoteFfiState.FAILED -> {
                            phase = LnSendPhase.Failure(
                                o.snapshot.failureReason ?: "Payment failed.",
                                retryable = false,
                            )
                            return@launch
                        }
                        MeltQuoteFfiState.EXPIRED -> {
                            phase = LnSendPhase.Failure(
                                "The invoice expired before payment.",
                                retryable = false,
                            )
                            return@launch
                        }
                        MeltQuoteFfiState.UNPAID -> {
                            phase = LnSendPhase.Failure(
                                "The mint could not pay this invoice.",
                                retryable = false,
                            )
                            return@launch
                        }
                    }
                    // Transient blip — keep polling; never fall through
                    // to a re-quote. The payment is in flight regardless.
                    is WalletViewModel.MeltStatusOutcome.Failure -> Unit
                }
            }
        }
    }

    suspend fun startQuote(bolt11Raw: String) {
        val bolt11 = stripScheme(bolt11Raw)
        if (bolt11.isEmpty()) return
        quotedInvoice = bolt11
        phase = LnSendPhase.Quoting
        phase = when (val o = viewModel.prepareMeltQuote(bolt11, null, "BTC")) {
            is WalletViewModel.MeltQuoteOutcome.Success -> LnSendPhase.Confirming(o.preview)
            // Pre-initiation: nothing persisted; re-quoting is safe.
            is WalletViewModel.MeltQuoteOutcome.Failure ->
                LnSendPhase.Failure(o.message, retryable = true)
        }
    }

    // LN-address page handed us a resolved invoice — quote immediately.
    LaunchedEffect(presetInvoice) {
        if (presetInvoice != null && phase == LnSendPhase.InvoiceEntry && invoice.isEmpty()) {
            invoice = presetInvoice
            startQuote(presetInvoice)
        }
    }

    fun resetToEntry() {
        if (presetInvoice != null) {
            onDismissCarousel()
            return
        }
        phase = LnSendPhase.InvoiceEntry
    }

    fun commitAndPay() {
        phase = LnSendPhase.Creating
        scope.launch {
            val handle = when (val c = viewModel.createMeltQuote(quotedInvoice, null, "BTC")) {
                is WalletViewModel.MeltCreateOutcome.Success -> c.handle
                is WalletViewModel.MeltCreateOutcome.Failure -> {
                    // Pre-initiation: quote row didn't persist / proofs
                    // not reserved / post_melt never fired. Safe to
                    // re-quote the same invoice.
                    phase = LnSendPhase.Failure(c.message, retryable = true)
                    return@launch
                }
            }
            // From here the melt is INITIATED for `handle`. Every
            // non-success path reconciles via the poll — never re-quote.
            activeHandle = handle
            phase = LnSendPhase.Paying
            when (val e = viewModel.executeMeltQuote(handle.quoteId)) {
                is WalletViewModel.MeltStatusOutcome.State -> when (e.state) {
                    MeltQuoteFfiState.PAID -> phase = LnSendPhase.Paid(e.snapshot)
                    MeltQuoteFfiState.PENDING -> startPolling(handle, reconcile = false)
                    // Mint replied non-paid on the execute round-trip;
                    // don't trust it as "no funds moved" — reconcile.
                    MeltQuoteFfiState.FAILED,
                    MeltQuoteFfiState.EXPIRED,
                    MeltQuoteFfiState.UNPAID -> startPolling(handle, reconcile = true)
                }
                // executeMeltQuote threw AFTER initiation — the classic
                // double-pay trigger. Reconcile by polling the held
                // handle, never by re-quoting.
                is WalletViewModel.MeltStatusOutcome.Failure ->
                    startPolling(handle, reconcile = true)
            }
        }
    }

    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        when (val p = phase) {
            is LnSendPhase.InvoiceEntry -> Column(
                modifier = Modifier
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = Spacing.l, vertical = Spacing.xxl),
                horizontalAlignment = Alignment.CenterHorizontally,
            ) {
                BrandCard {
                    Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
                        Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                            Text("Pay Lightning invoice", style = BrandTypography.title, color = BrandColors.cardForeground)
                            Text(
                                "Paste a BOLT-11 invoice to pay it from your Cashu balance",
                                style = BrandTypography.label,
                                color = BrandColors.mutedForeground,
                            )
                        }
                        Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Text(
                                    "Invoice",
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
                                        .clickableNoIndication {
                                            clipboard.getText()?.text?.let { invoice = stripScheme(it) }
                                        },
                                )
                            }
                            OutlinedTextField(
                                value = invoice,
                                onValueChange = { invoice = it },
                                placeholder = { Text("lnbc...", style = BrandTypography.body, color = BrandColors.mutedForeground) },
                                textStyle = BrandTypography.body,
                                modifier = Modifier.fillMaxWidth().heightIn(min = 96.dp),
                                shape = Radius.control,
                                colors = brandFieldColors(),
                            )
                        }
                        BrandButton(
                            "Continue",
                            { scope.launch { startQuote(invoice) } },
                            variant = BrandButtonVariant.Primary,
                            enabled = invoice.trim().isNotEmpty(),
                        )
                    }
                }
            }
            is LnSendPhase.Quoting -> CenteredProgress("Fetching quote from mint…")
            is LnSendPhase.Confirming -> LnConfirmCard(
                preview = p.preview,
                onPay = { commitAndPay() },
                onCancel = { resetToEntry() },
            )
            is LnSendPhase.Creating -> CenteredProgress("Reserving proofs…")
            is LnSendPhase.Paying -> CenteredProgress("Paying invoice…")
            // No cancel affordance — bailing now loses the only UI
            // tracking of a possibly-settled payment, and there's
            // nothing safe to "retry".
            is LnSendPhase.Verifying -> CenteredProgress("Confirming payment status…")
            is LnSendPhase.Paid -> Column(
                modifier = Modifier.padding(Spacing.l),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.l),
            ) {
                PaidCard(snapshot = p.snapshot)
                BrandButton(
                    "Done",
                    onDismissCarousel,
                    variant = BrandButtonVariant.Primary,
                    modifier = Modifier.widthIn(max = 360.dp),
                )
            }
            is LnSendPhase.Failure -> FailureCard(
                title = "Couldn't pay",
                message = p.message,
                onRetry = if (p.retryable) ({ resetToEntry() }) else null,
                onDismiss = onDismissCarousel,
            )
        }
    }
}

@Composable
private fun LnConfirmCard(
    preview: MeltQuotePreview,
    onPay: () -> Unit,
    onCancel: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(Spacing.l),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        BrandCard {
            Column(verticalArrangement = Arrangement.spacedBy(Spacing.l)) {
                Column(verticalArrangement = Arrangement.spacedBy(Spacing.xs)) {
                    Text("Confirm payment", style = BrandTypography.title, color = BrandColors.cardForeground)
                    Text(
                        "Paying a Lightning invoice from your Cashu balance",
                        style = BrandTypography.label,
                        color = BrandColors.mutedForeground,
                    )
                }
                Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
                    AmountRow("They receive", preview.amount, preview.unit, prominent = true)
                    AmountRow("Lightning fee reserve", preview.lightningFeeReserve, preview.unit)
                    if (preview.cashuFee != "0") {
                        AmountRow("Cashu fee", preview.cashuFee, preview.unit)
                    }
                    HorizontalDivider(color = BrandColors.border)
                    AmountRow("Estimated total", preview.totalAmount, preview.unit, prominent = true)
                }
                Text(
                    "Unused fee reserve is refunded after the payment settles.",
                    style = BrandTypography.caption,
                    color = BrandColors.mutedForeground,
                )
                Column(verticalArrangement = Arrangement.spacedBy(Spacing.s)) {
                    BrandButton("Pay", onPay, variant = BrandButtonVariant.Primary)
                    BrandButton("Cancel", onCancel, variant = BrandButtonVariant.Ghost)
                }
            }
        }
    }
}

@Composable
private fun PaidCard(snapshot: MeltQuoteSnapshot) {
    BrandCard {
        Column(
            modifier = Modifier.fillMaxWidth(),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.spacedBy(Spacing.m),
        ) {
            Icon(
                Icons.Outlined.AccountBalanceWallet,
                contentDescription = null,
                tint = androidx.compose.ui.graphics.Color(0xFF22C55E),
                modifier = Modifier.size(48.dp),
            )
            Text("Paid", style = BrandTypography.title, color = BrandColors.cardForeground)
            Row(verticalAlignment = Alignment.Bottom) {
                Text(
                    snapshot.amountSpent ?: "",
                    style = BrandTypography.numericInline,
                    color = BrandColors.cardForeground,
                )
                Spacer(Modifier.size(4.dp))
                Text("sats", style = BrandTypography.label, color = BrandColors.mutedForeground)
            }
            snapshot.lightningFee?.let {
                Text(
                    "Lightning fee: $it sats",
                    style = BrandTypography.caption,
                    color = BrandColors.mutedForeground,
                )
            }
        }
    }
}

// ---- Lightning Address send page (LUD-16 → melt) ----

private sealed interface LnAddrPhase {
    data object Entry : LnAddrPhase
    data object Resolving : LnAddrPhase
    data class Melt(val invoice: String) : LnAddrPhase
    data class Failure(val message: String) : LnAddrPhase
}

/**
 * Enter `alice@example.com` + an amount, resolve to a BOLT-11 invoice
 * via the wallet-agnostic LUD-16 FFI, then hand the invoice to the
 * shared melt flow ([LightningSendPage] with `presetInvoice`). Mirrors
 * iOS `LightningAddressSendPlaceholderView`.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun LightningAddressSendPage(
    viewModel: WalletViewModel,
    onDismissCarousel: () -> Unit,
) {
    var phase by remember { mutableStateOf<LnAddrPhase>(LnAddrPhase.Entry) }
    var address by rememberSaveable { mutableStateOf("") }
    var amountBuffer by rememberSaveable { mutableStateOf("0") }
    val scope = rememberCoroutineScope()
    val clipboard = LocalClipboardManager.current

    val parsed = parseAmountMinorUnits(amountBuffer, allowsDecimal = false)
    val addr = address.trim()
    val valid = parsed != null && parsed > 0u &&
        addr.contains("@") && !addr.startsWith("@") && !addr.endsWith("@")

    Box(Modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
        when (val p = phase) {
            is LnAddrPhase.Entry -> Column(
                modifier = Modifier
                    .fillMaxSize()
                    .verticalScroll(rememberScrollState())
                    .padding(vertical = Spacing.xxl),
                horizontalAlignment = Alignment.CenterHorizontally,
                verticalArrangement = Arrangement.spacedBy(Spacing.xxl),
            ) {
                Column(horizontalAlignment = Alignment.CenterHorizontally) {
                    Row(verticalAlignment = Alignment.Bottom) {
                        Text(formatAmountDisplay(amountBuffer), style = BrandTypography.numericHero, color = BrandColors.foreground)
                        Spacer(Modifier.size(4.dp))
                        Text("sats", style = BrandTypography.titleSmall, color = BrandColors.mutedForeground, modifier = Modifier.padding(bottom = 10.dp))
                    }
                    Text("Send to a Lightning Address", style = BrandTypography.label, color = BrandColors.mutedForeground)
                }
                Column(
                    modifier = Modifier.widthIn(max = 384.dp).padding(horizontal = Spacing.l),
                    verticalArrangement = Arrangement.spacedBy(Spacing.s),
                ) {
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        Text(
                            "Lightning Address",
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
                                .clickableNoIndication {
                                    clipboard.getText()?.text?.let { address = it.trim() }
                                },
                        )
                    }
                    OutlinedTextField(
                        value = address,
                        onValueChange = { address = it },
                        placeholder = { Text("alice@example.com", style = BrandTypography.body, color = BrandColors.mutedForeground) },
                        singleLine = true,
                        textStyle = BrandTypography.body,
                        keyboardOptions = KeyboardOptions(
                            keyboardType = KeyboardType.Email,
                            imeAction = ImeAction.Done,
                        ),
                        modifier = Modifier.fillMaxWidth(),
                        shape = Radius.control,
                        colors = brandFieldColors(),
                    )
                }
                AmountNumpad(
                    value = amountBuffer,
                    onValueChange = { amountBuffer = it },
                    allowsDecimal = false,
                    modifier = Modifier.widthIn(max = 360.dp).padding(horizontal = Spacing.l),
                )
                BrandButton(
                    "Continue",
                    {
                        val amt = parsed ?: return@BrandButton
                        phase = LnAddrPhase.Resolving
                        scope.launch {
                            phase = when (
                                val o = viewModel.resolveLnAddressInvoice(address, amt, null)
                            ) {
                                is WalletViewModel.LnAddressInvoiceOutcome.Success ->
                                    LnAddrPhase.Melt(o.invoice)
                                is WalletViewModel.LnAddressInvoiceOutcome.Failure ->
                                    LnAddrPhase.Failure(o.message)
                            }
                        }
                    },
                    variant = BrandButtonVariant.Primary,
                    size = BrandButtonSize.Large,
                    enabled = valid,
                    modifier = Modifier.widthIn(max = 360.dp).padding(horizontal = Spacing.l),
                )
            }
            is LnAddrPhase.Resolving -> CenteredProgress("Resolving address…")
            is LnAddrPhase.Melt -> LightningSendPage(
                viewModel = viewModel,
                onDismissCarousel = onDismissCarousel,
                presetInvoice = p.invoice,
            )
            is LnAddrPhase.Failure -> FailureCard(
                title = "Couldn't resolve",
                message = p.message,
                onRetry = { phase = LnAddrPhase.Entry },
                onDismiss = onDismissCarousel,
            )
        }
    }
}

// ---- Shared amount-entry column + row ----

@Composable
private fun AmountEntryColumn(
    amountBuffer: String,
    onBufferChange: (String) -> Unit,
    unit: String,
    caption: String,
    ctaLabel: String,
    ctaEnabled: Boolean,
    onCta: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(vertical = Spacing.xxl),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(Spacing.xxl),
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Row(verticalAlignment = Alignment.Bottom) {
                Text(formatAmountDisplay(amountBuffer), style = BrandTypography.numericHero, color = BrandColors.foreground)
                Spacer(Modifier.size(4.dp))
                Text(unit, style = BrandTypography.titleSmall, color = BrandColors.mutedForeground, modifier = Modifier.padding(bottom = 10.dp))
            }
            Text(caption, style = BrandTypography.label, color = BrandColors.mutedForeground)
        }
        AmountNumpad(
            value = amountBuffer,
            onValueChange = onBufferChange,
            allowsDecimal = false,
            modifier = Modifier.widthIn(max = 360.dp).padding(horizontal = Spacing.l),
        )
        BrandButton(
            ctaLabel,
            onCta,
            variant = BrandButtonVariant.Primary,
            size = BrandButtonSize.Large,
            enabled = ctaEnabled,
            modifier = Modifier.widthIn(max = 360.dp).padding(horizontal = Spacing.l),
        )
    }
}

@Composable
private fun AmountRow(label: String, value: String, unit: String, prominent: Boolean = false) {
    Row(modifier = Modifier.fillMaxWidth(), verticalAlignment = Alignment.Bottom) {
        Text(
            label,
            style = if (prominent) BrandTypography.labelEmphasis else BrandTypography.label,
            color = if (prominent) BrandColors.cardForeground else BrandColors.mutedForeground,
            modifier = Modifier.weight(1f),
        )
        Row(verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
            Text(
                value,
                style = if (prominent) BrandTypography.labelEmphasis else BrandTypography.label,
                color = BrandColors.cardForeground,
            )
            Text(unit, style = BrandTypography.caption, color = BrandColors.mutedForeground)
        }
    }
}
