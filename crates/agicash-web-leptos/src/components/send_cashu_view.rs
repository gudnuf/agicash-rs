//! `SendCashuView` — Cashu token send flow (the only real surface in the
//! Send carousel for v0).
//!
//! Mirrors iOS `SendCashuTokenView` (`ios/Agicash/Agicash/SendCashuTokenView.swift`)
//! 1:1 in shape:
//!
//! ```text
//!   amountEntry   → numpad + Continue
//!     ↓ startQuote() (mocked)
//!   quoting       → spinner ("Preparing send…")
//!     ↓ on success
//!   confirming    → fee + total card, Confirm button
//!     ↓ commitSend() (mocked)
//!   swapping      → spinner ("Producing token…")
//!     ↓ on success
//!   share         → token + copy + share sheet, polling claim every 3s
//!     ↓ poll returns .completed (real NUT-07 check_send_swap_claimed)
//!   claimed       → "Sent" check + Done
//!     ↓ (or .failed / catch-all)
//!   failure       → error + Retry
//! ```
//!
//! ## SDK boundary (12d: real)
//!
//! As of slice 12d the send path is **un-mocked** against the real
//! `agicash-wasm` shell over `WalletClient::from_config` (the
//! storage-supabase wasm port + the `AuthClient`/dep target-gates that
//! made `agicash-wallet` wasm-buildable all landed):
//!   - `on_continue` → `AgicashWasmWallet::prepare_send_quote` (real fee
//!     breakdown from `CashuSendSwapService.get_quote`).
//!   - `on_confirm`  → `AgicashWasmWallet::create_send_swap` (real V4
//!     `cashuB…` token + real swap id; claimable by a real receiver).
//!   - `poll_claim_once` → `AgicashWasmWallet::check_send_swap_claimed`
//!     (real single-shot NUT-07 claim poll over
//!     `WalletClient::check_send_token_claimed`). 12b-3 un-mock of the
//!     former `mock_poll_claim(tick)` timer — the share-screen 3 s loop
//!     now asks the mint whether the receiver's proofs are SPENT.
//!
//! The view geometry, state machine, and UX timings are unchanged — only
//! the data source moved from synthetic to real.
//!
//! ## Constraints
//!
//! - **No view-transitions spike inheritance.** Plain `Show`/match-arm
//!   conditional rendering per the lane brief.
//! - **Plain `<A>` links for navigation** (no `view-transitions`
//!   wrapping). Inside the carousel page there's no inter-route
//!   navigation; the back/close link lives in the page header.
//! - **L3 components throughout** — `Button`, `Numpad`, `ShareSheet`,
//!   `Toast`. No custom button or input styling.

// The view body is long but linear; splitting into private sub-components
// would just add indirection without reuse benefit. Matches the existing
// allow on `cashu_token_paste_view.rs`.
#![allow(clippy::too_many_lines)]

use leptos::ev::MouseEvent;
use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::components::{
    use_toast, Button, ButtonSize, ButtonVariant, Numpad, SharePayload, ShareSheet, ToastVariant,
};
use crate::config::AppConfig;
use crate::tokens;

// ---- View-model types -----------------------------------------------------
// Shape mirrors iOS `SendQuotePreview` / `SendSwapHandle` / `SendClaimState`
// so when the real wallet call lands the swap is mechanical.

/// Quote previewed on the Confirm card. Sender-pays-fee mode for v0:
/// `total = amount + send_fee` (receive fee is zero from sender's POV,
/// shown only for parity with iOS so the receiver-side number is visible).
#[derive(Clone, Debug, PartialEq, Eq)]
struct SendQuotePreview {
    /// What the receiver claims.
    amount_to_send: u64,
    /// Fee burned at the input swap (sender pays).
    send_fee: u64,
    /// Fee the receiver burns claiming (informational; sender doesn't pay).
    receive_fee: u64,
    /// Sender's total debit.
    total: u64,
    /// Display unit label, e.g. `"sats"`.
    unit: String,
}

/// Result of a successful `createSend`. Holds the encoded token + the
/// swap id so the polling loop can ask "has the receiver claimed?".
#[derive(Clone, Debug, PartialEq, Eq)]
struct SendSwapHandle {
    /// Encoded V4 token. In the mock this is a deterministic-but-fake
    /// `cashuB...` string; the real wallet will emit a valid token.
    token: String,
    /// Stable swap id for polling. Mock uses a UUIDv4-shaped string.
    swap_id: String,
    /// What the receiver claims. Held so the share / claimed cards can
    /// render the amount without re-deriving from the token.
    amount: u64,
    unit: String,
}

/// Outcome of a single poll-claim shot. Mapped from
/// `agicash_wasm::SendClaimStatusWasm` (12b-3 un-mock); see
/// `poll_claim_once`. Constructed only on the real wasm path — the
/// native `rlib` (unit-test) build's `poll_claim_once` returns only
/// `Pending` (no browser wallet), so `Completed`/`Failed` are
/// native-dead. Same native-only honest `allow(dead_code)` idiom the
/// `Phase` enum uses.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClaimPoll {
    /// Receiver hasn't claimed yet (or a transient lookup error); keep
    /// polling on the next tick.
    Pending,
    /// Receiver claimed the token. The view transitions to `Claimed`.
    Completed,
    /// Mint declared the swap terminally FAILED — the real
    /// `check_send_swap_claimed` returned `SendClaimStateWasm::Failed`.
    /// Drops straight into `Phase::Failure` with the reason.
    Failed(String),
}

/// View phase. Direct port of iOS `SendCashuTokenView.Phase` with the
/// inner payload types reshaped into the Rust view-model above.
///
/// `Confirming`/`Swapping`/`Share`/`Claimed` are constructed only on the
/// real wasm path (12d: `cfg(target_arch = "wasm32")` — the browser is
/// the only shipping target). The native `rlib` (workspace unit-test
/// build) has no browser wallet so it constructs only the entry/error
/// phases; the variants are still pattern-matched by the `view!` render
/// arms. The `allow(dead_code)` is therefore native-only + honest.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Clone, Debug)]
enum Phase {
    /// Numpad + Continue.
    AmountEntry,
    /// Spinner while the (mocked) quote round-trip runs.
    Quoting,
    /// Fee breakdown card; user taps Send to commit.
    Confirming(SendQuotePreview),
    /// Spinner while the (mocked) swap round-trip runs.
    Swapping,
    /// Share card: token, copy, share sheet. Polling claim every 3 s.
    Share(SendSwapHandle),
    /// "Sent" confirmation card.
    Claimed(SendSwapHandle),
    /// Inline error + Retry.
    Failure(String),
}

// ---- Component ------------------------------------------------------------

/// Send-Cashu surface. Sits inside the Send carousel (the carousel page
/// owns the tab header + tab bar; this component owns the body).
#[component]
pub fn SendCashuView() -> impl IntoView {
    let amount_buffer = RwSignal::new("0".to_string());
    let phase: RwSignal<Phase> = RwSignal::new(Phase::AmountEntry);
    // Share-screen copy-confirmation flag. Drives the icon swap on the
    // inline truncated-token chip (doc.on.doc → checkmark for 1.5 s).
    let show_copied = RwSignal::new(false);
    let toast = use_toast();
    // 12d: same config source LoginView / wallet_context use. Captured
    // at component-body level (NOT inside spawn_local) per
    // feedback_leptos_spawn_local_gotchas — `expect_context` returns
    // None inside spawn_local. Held in a `StoredValue` (Copy) so the
    // handler closures stay `Fn + Copy` (the reactive `match phase`
    // render closure requires `FnMut`; a moved-in non-Copy `AppConfig`
    // would make a handler `FnOnce` and break it).
    let config = StoredValue::new(expect_context::<AppConfig>());

    // ---- Handlers (mirror iOS method names) ------------------------------

    let on_continue = move |_ev: MouseEvent| {
        let Some(amount) = parsed_amount(&amount_buffer.get()) else {
            return;
        };
        if amount == 0 {
            return;
        }
        phase.set(Phase::Quoting);
        let config = config.get_value();

        spawn_local(async move {
            // 12d: real SDK boundary (was mock_prepare_send).
            // `Ok(quote)` → `Phase::Confirming`; `Err(msg)` →
            // `Phase::Failure`. Account/currency: v0 web has no
            // chooser (None/None → wallet picks the BTC Cashu
            // account), exactly the FFI `prepare_send_quote(amount,
            // None, None)` shape.
            #[cfg(target_arch = "wasm32")]
            {
                match crate::components::wallet_context::seed_wasm_wallet(&config).await {
                    Ok(wallet) => match wallet.prepare_send_quote(amount, None, None).await {
                        Ok(q) => phase.set(Phase::Confirming(quote_from_wasm(&q))),
                        Err(e) => phase.set(Phase::Failure(js_err_string(&e))),
                    },
                    Err(e) => phase.set(Phase::Failure(js_err_string(&e))),
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                // Native rlib (unit tests): no browser wallet. Touch
                // captured values so they aren't flagged unused.
                let _ = (&config, amount);
                phase.set(Phase::Failure(
                    "wallet unavailable (native build)".to_string(),
                ));
            }
        });
    };

    let on_confirm = move |_ev: MouseEvent| {
        let Phase::Confirming(quote) = phase.get() else {
            return;
        };
        phase.set(Phase::Swapping);
        let config = config.get_value();

        spawn_local(async move {
            // 12d: real SDK boundary (was mock_commit_send). The
            // facade re-derives the amount from its own quote path;
            // the view passes the same amount it quoted (V4 always,
            // verbatim the FFI `create_send_swap`).
            #[cfg(target_arch = "wasm32")]
            {
                match crate::components::wallet_context::seed_wasm_wallet(&config).await {
                    Ok(wallet) => {
                        match wallet
                            .create_send_swap(quote.amount_to_send, None, None)
                            .await
                        {
                            Ok(h) => phase.set(Phase::Share(handle_from_wasm(&h))),
                            Err(e) => phase.set(Phase::Failure(js_err_string(&e))),
                        }
                    }
                    Err(e) => phase.set(Phase::Failure(js_err_string(&e))),
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = (&config, &quote);
                phase.set(Phase::Failure(
                    "wallet unavailable (native build)".to_string(),
                ));
            }
        });
    };

    let on_retry = move |_ev: MouseEvent| {
        amount_buffer.set("0".to_string());
        phase.set(Phase::AmountEntry);
    };

    let on_cancel_share = move |_ev: MouseEvent| {
        amount_buffer.set("0".to_string());
        phase.set(Phase::AmountEntry);
    };

    let on_done = move |_ev: MouseEvent| {
        amount_buffer.set("0".to_string());
        phase.set(Phase::AmountEntry);
    };

    let on_copy_token = move |token: String| {
        copy_to_clipboard(&token, show_copied, toast);
    };

    // ---- Polling effect --------------------------------------------------
    //
    // Watches the phase. When it transitions into Share(handle), starts a
    // 3 s-cadence poll loop. The loop is owned by an Effect closure that
    // captures the current `swap_id` — when phase changes (user leaves the
    // share screen, or claim flips to Completed) the next tick is a no-op
    // because `phase.get_untracked()` no longer matches.
    //
    // Cancellation is implicit: the Effect re-runs on the next phase
    // change (cancelling the in-flight timer via the early-return guard);
    // we don't need an explicit AbortController because the next scheduled
    // tick reads `phase.get_untracked()` and bails if it's stale.

    Effect::new(move |_| {
        // Only react to entering Share — the spawn below polls until the
        // phase moves elsewhere.
        let Phase::Share(handle) = phase.get() else {
            return;
        };
        let swap_id = handle.swap_id.clone();
        // Captured at the Effect's reactive owner (NOT inside
        // spawn_local) per feedback_leptos_spawn_local_gotchas — same
        // contract the on_continue/on_confirm handlers use.
        let config = config.get_value();

        spawn_local(async move {
            // 3 s-cadence single-shot poll matching the iOS
            // SendCashuTokenView "Waiting for receiver…" loop. The
            // facade's `check_send_token_claimed` is single-shot by
            // design (runtime-agnostic — no sleep/spawn); the consumer
            // (this loop) owns the cadence.
            let _ = &config;
            loop {
                #[cfg(feature = "hydrate")]
                {
                    gloo_timers::future::TimeoutFuture::new(3_000).await;
                }
                #[cfg(not(feature = "hydrate"))]
                {
                    // SSR path: bail immediately. Polling has no meaning
                    // server-side.
                    return;
                }
                // Cancellation guard. If the user moved off the share
                // screen (Cancel / Done / nav-away), stop polling.
                let still_sharing = matches!(
                    phase.get_untracked(),
                    Phase::Share(ref h) if h.swap_id == swap_id,
                );
                if !still_sharing {
                    return;
                }
                // 12b-3 un-mock: real single-shot NUT-07 claim poll over
                // the wasm shell (was `mock_poll_claim(tick)`). A fresh
                // handle per tick mirrors on_continue/on_confirm — the
                // shell is a thin `from_config` delegate, construction is
                // cheap + has no network I/O.
                let outcome = poll_claim_once(&config, &swap_id).await;
                match outcome {
                    // Pending (or a transient lookup error — see
                    // poll_claim_once): keep polling. Loop body falls
                    // through to the next iteration — no explicit
                    // `continue` (clippy flags it).
                    ClaimPoll::Pending => {}
                    ClaimPoll::Completed => {
                        phase.set(Phase::Claimed(handle));
                        return;
                    }
                    ClaimPoll::Failed(reason) => {
                        phase.set(Phase::Failure(reason));
                        return;
                    }
                }
            }
        });
    });

    // ---- Render ----------------------------------------------------------

    view! {
        <div style=pane_style()>
            {move || match phase.get() {
                Phase::AmountEntry => view! {
                    <AmountEntry
                        buffer=amount_buffer
                        on_continue=on_continue
                    />
                }.into_any(),
                Phase::Quoting => view! {
                    <SpinnerPane label="Preparing send...".to_string()/>
                }.into_any(),
                Phase::Confirming(quote) => view! {
                    <ConfirmCard
                        quote=quote
                        on_send=on_confirm
                        on_cancel=on_retry
                    />
                }.into_any(),
                Phase::Swapping => view! {
                    <SpinnerPane label="Producing token...".to_string()/>
                }.into_any(),
                Phase::Share(handle) => view! {
                    <ShareCard
                        handle=handle
                        show_copied=show_copied
                        on_copy=on_copy_token
                        on_cancel=on_cancel_share
                    />
                }.into_any(),
                Phase::Claimed(handle) => view! {
                    <ClaimedCard handle=handle on_done=on_done/>
                }.into_any(),
                Phase::Failure(message) => view! {
                    <FailureCard
                        message=message
                        on_retry=on_retry
                        on_dismiss=on_done
                    />
                }.into_any(),
            }}
        </div>
    }
}

// ---- Phase: AmountEntry ---------------------------------------------------

#[component]
fn AmountEntry<C>(buffer: RwSignal<String>, on_continue: C) -> impl IntoView
where
    C: Fn(MouseEvent) + Send + Sync + 'static,
{
    let column_style = format!(
        "display:flex; flex-direction:column; align-items:center; \
         gap:{gap}; padding:{pad_v} {pad_h}; flex:1;",
        gap = tokens::SPACE_XXL,
        pad_v = tokens::SPACE_L,
        pad_h = tokens::SPACE_L,
    );
    let hero_wrap_style = format!(
        "display:flex; flex-direction:column; align-items:center; \
         gap:{gap}; margin-top:{top};",
        gap = tokens::SPACE_XS,
        top = tokens::SPACE_L,
    );
    let amount_row_style = "display:flex; align-items:baseline; \
         justify-content:center; gap:6px;"
        .to_string();
    let amount_style = format!(
        "font-family:{font}; font-size:64px; font-weight:600; color:{fg}; \
         line-height:1; font-variant-numeric:tabular-nums;",
        font = tokens::FONT_NUMERIC,
        fg = tokens::COLOR_FOREGROUND,
    );
    let unit_style = format!(
        "font-size:{size}; color:{fg};",
        size = tokens::TEXT_LG,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );
    let caption_style = format!(
        "font-size:{size}; color:{fg}; margin:0;",
        size = tokens::TEXT_SM,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );
    let cta_wrap_style = format!("width:100%; max-width:{max};", max = tokens::CARD_MAX_WIDTH,);

    let is_valid = Signal::derive(move || matches!(parsed_amount(&buffer.get()), Some(n) if n > 0));
    let disabled = Signal::derive(move || !is_valid.get());

    view! {
        <div style=column_style>
            <div style=hero_wrap_style>
                <div style=amount_row_style>
                    <span style=amount_style aria-label="Amount to send">
                        {move || display_amount(&buffer.get())}
                    </span>
                    <span style=unit_style>"sats"</span>
                </div>
                <p style=caption_style>"Send Cashu token"</p>
            </div>

            <Numpad value=buffer allows_decimal=Signal::derive(|| false)/>

            <div style=cta_wrap_style>
                <Button
                    variant=ButtonVariant::Primary
                    size=ButtonSize::Large
                    disabled=disabled
                    on_click=Callback::new(on_continue)
                >
                    "Continue"
                </Button>
            </div>
        </div>
    }
}

// ---- Phase: Quoting / Swapping (shared spinner) ---------------------------

#[component]
fn SpinnerPane(label: String) -> impl IntoView {
    let column_style = format!(
        "display:flex; flex-direction:column; align-items:center; \
         justify-content:center; gap:{gap}; flex:1;",
        gap = tokens::SPACE_M,
    );
    let spinner_style = format!(
        "width:32px; height:32px; border:2px solid {color}; \
         border-top-color:transparent; border-radius:50%; \
         animation:agicash-spin 0.7s linear infinite;",
        color = tokens::COLOR_MUTED_FOREGROUND,
    );
    let caption_style = format!(
        "font-size:{size}; color:{fg}; margin:0;",
        size = tokens::TEXT_SM,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );

    view! {
        <div style=column_style role="status" aria-live="polite">
            <span aria-hidden="true" style=spinner_style/>
            <p style=caption_style>{label}</p>
        </div>
    }
}

// ---- Phase: Confirming ----------------------------------------------------

#[component]
fn ConfirmCard<S, C>(quote: SendQuotePreview, on_send: S, on_cancel: C) -> impl IntoView
where
    S: Fn(MouseEvent) + Send + Sync + 'static,
    C: Fn(MouseEvent) + Send + Sync + 'static,
{
    let wrap_style = format!(
        "display:flex; flex-direction:column; align-items:center; \
         justify-content:center; flex:1; padding:{pad};",
        pad = tokens::SPACE_L,
    );
    let card_style = card_style();
    let title_style = format!(
        "font-size:{size}; font-weight:600; margin:0; color:{fg};",
        size = tokens::TEXT_2XL,
        fg = tokens::COLOR_CARD_FOREGROUND,
    );
    let subhead_style = format!(
        "font-size:{size}; color:{fg}; margin:0;",
        size = tokens::TEXT_SM,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );
    let rows_style = format!(
        "display:flex; flex-direction:column; gap:{gap};",
        gap = tokens::SPACE_S,
    );
    let divider_style = format!(
        "height:1px; background:{color}; margin:{m} 0;",
        color = tokens::COLOR_BORDER,
        m = tokens::SPACE_XS,
    );
    let buttons_style = format!(
        "display:flex; flex-direction:column; gap:{gap}; \
         width:100%;",
        gap = tokens::SPACE_S,
    );

    let unit = quote.unit.clone();

    view! {
        <div style=wrap_style>
            <div style=card_style>
                <div>
                    <h2 style=title_style>"Confirm send"</h2>
                    <p style=subhead_style>
                        "Producing a token the receiver can claim"
                    </p>
                </div>

                <div style=rows_style>
                    <AmountRow
                        label="They receive".to_string()
                        value=quote.amount_to_send
                        unit=unit.clone()
                        prominent=true
                    />
                    <AmountRow
                        label="Send fee".to_string()
                        value=quote.send_fee
                        unit=unit.clone()
                        prominent=false
                    />
                    <AmountRow
                        label="Receive fee".to_string()
                        value=quote.receive_fee
                        unit=unit.clone()
                        prominent=false
                    />
                    <div style=divider_style/>
                    <AmountRow
                        label="You pay".to_string()
                        value=quote.total
                        unit=unit
                        prominent=true
                    />
                </div>

                <div style=buttons_style>
                    <Button
                        variant=ButtonVariant::Primary
                        on_click=Callback::new(on_send)
                    >
                        "Send"
                    </Button>
                    <Button
                        variant=ButtonVariant::Ghost
                        on_click=Callback::new(on_cancel)
                    >
                        "Cancel"
                    </Button>
                </div>
            </div>
        </div>
    }
}

#[component]
fn AmountRow(label: String, value: u64, unit: String, prominent: bool) -> impl IntoView {
    let row_style = "display:flex; align-items:center; \
         justify-content:space-between;"
        .to_string();
    let label_style = format!(
        "font-size:{size}; font-weight:{weight}; color:{fg};",
        size = tokens::TEXT_SM,
        weight = if prominent { "600" } else { "400" },
        fg = if prominent {
            tokens::COLOR_CARD_FOREGROUND
        } else {
            tokens::COLOR_MUTED_FOREGROUND
        },
    );
    let value_row_style = "display:flex; align-items:baseline; gap:4px;".to_string();
    let value_style = format!(
        "font-size:{size}; font-weight:{weight}; color:{fg}; \
         font-variant-numeric:tabular-nums;",
        size = tokens::TEXT_SM,
        weight = if prominent { "600" } else { "400" },
        fg = tokens::COLOR_CARD_FOREGROUND,
    );
    let unit_style = format!(
        "font-size:{size}; color:{fg};",
        size = tokens::TEXT_SM,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );

    view! {
        <div style=row_style>
            <span style=label_style>{label}</span>
            <span style=value_row_style>
                <span style=value_style>{format_amount(value)}</span>
                <span style=unit_style>{unit}</span>
            </span>
        </div>
    }
}

// ---- Phase: Share ---------------------------------------------------------

#[component]
fn ShareCard<C>(
    handle: SendSwapHandle,
    show_copied: RwSignal<bool>,
    on_copy: C,
    on_cancel: impl Fn(MouseEvent) + Send + Sync + 'static,
) -> impl IntoView
where
    C: Fn(String) + Send + Sync + 'static + Clone,
{
    let wrap_style = format!(
        "display:flex; flex-direction:column; align-items:center; \
         justify-content:center; flex:1; padding:{pad};",
        pad = tokens::SPACE_L,
    );
    let card_style = card_style();
    let header_style = format!(
        "display:flex; flex-direction:column; align-items:center; gap:{gap};",
        gap = tokens::SPACE_XS,
    );
    let amount_row_style = "display:flex; align-items:baseline; \
         justify-content:center; gap:6px;"
        .to_string();
    let amount_style = format!(
        "font-family:{font}; font-size:40px; font-weight:600; color:{fg}; \
         line-height:1; font-variant-numeric:tabular-nums;",
        font = tokens::FONT_NUMERIC,
        fg = tokens::COLOR_CARD_FOREGROUND,
    );
    let unit_style = format!(
        "font-size:{size}; color:{fg};",
        size = tokens::TEXT_SM,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );
    let waiting_style = format!(
        "display:flex; align-items:center; gap:{gap}; \
         font-size:{size}; color:{fg};",
        gap = tokens::SPACE_XS,
        size = tokens::TEXT_SM,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );
    let inline_spinner_style = format!(
        "width:14px; height:14px; border:2px solid {color}; \
         border-top-color:transparent; border-radius:50%; \
         animation:agicash-spin 0.7s linear infinite;",
        color = tokens::COLOR_MUTED_FOREGROUND,
    );
    let chip_style = format!(
        "display:inline-flex; align-items:center; gap:{gap}; \
         padding:{pad_v} {pad_h}; border:1px solid {border}; \
         border-radius:{radius}; background:{bg}; \
         font-size:{size}; color:{fg}; \
         cursor:pointer; font-family:inherit; \
         max-width:100%; \
         -webkit-tap-highlight-color:transparent;",
        gap = tokens::SPACE_XS,
        pad_v = tokens::SPACE_S,
        pad_h = tokens::SPACE_M,
        border = tokens::COLOR_BORDER,
        radius = tokens::RADIUS_MD,
        bg = tokens::COLOR_MUTED,
        size = tokens::TEXT_SM,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );
    let chip_text_style = "overflow:hidden; text-overflow:ellipsis; \
         white-space:nowrap; font-variant-numeric:tabular-nums; \
         font-family:ui-monospace, SFMono-Regular, Menlo, monospace;"
        .to_string();
    let buttons_style = format!(
        "display:flex; flex-direction:column; gap:{gap}; width:100%;",
        gap = tokens::SPACE_S,
    );

    let token = handle.token.clone();
    let token_for_copy = token.clone();
    let token_for_share = token.clone();
    let truncated = truncate_token(&token);

    let on_copy_clone = on_copy.clone();
    let copy_handler = move |_ev: MouseEvent| {
        on_copy_clone(token_for_copy.clone());
    };

    let share_payload = Signal::derive(move || SharePayload {
        title: Some("Cashu token".to_string()),
        text: Some(token_for_share.clone()),
        url: None,
    });
    let on_share_copied = Callback::new(move |()| {
        // Same on_copy path the chip uses — surface the toast via the
        // sibling handler so success feedback matches.
        on_copy(token.clone());
    });
    let on_share_error = Callback::new(move |msg: String| {
        // Best-effort log. Toast would be noisy on the "user dismissed
        // share sheet" rejection path.
        leptos::logging::warn!("share_sheet error: {msg}");
    });

    view! {
        <div style=wrap_style>
            <div style=card_style>
                <div style=header_style>
                    <div style=amount_row_style>
                        <span style=amount_style>{format_amount(handle.amount)}</span>
                        <span style=unit_style.clone()>{handle.unit}</span>
                    </div>
                    <div style=waiting_style>
                        <span aria-hidden="true" style=inline_spinner_style/>
                        <span>"Waiting for receiver..."</span>
                    </div>
                </div>

                <button
                    style=chip_style
                    on:click=copy_handler
                    aria-label="Copy token"
                >
                    <span style=chip_text_style>{truncated}</span>
                    <CopyOrCheckIcon copied=show_copied/>
                </button>

                <div style=buttons_style>
                    <ShareSheet
                        payload=share_payload
                        variant=ButtonVariant::Primary
                        on_copied=on_share_copied
                        on_error=on_share_error
                    >
                        "Share"
                    </ShareSheet>
                    <Button
                        variant=ButtonVariant::Ghost
                        on_click=Callback::new(on_cancel)
                    >
                        "Cancel"
                    </Button>
                </div>
            </div>
        </div>
    }
}

/// Inline icon for the truncated-token chip. Flips between the
/// document-on-document "copy" glyph and a checkmark for 1.5 s after
/// a successful copy.
#[component]
fn CopyOrCheckIcon(copied: RwSignal<bool>) -> impl IntoView {
    view! {
        <span aria-hidden="true" style="display:inline-flex; width:14px; height:14px;">
            {move || if copied.get() {
                view! {
                    <svg width="14" height="14" viewBox="0 0 24 24"
                        fill="none" stroke="currentColor" stroke-width="2"
                        stroke-linecap="round" stroke-linejoin="round">
                        <polyline points="20 6 9 17 4 12"/>
                    </svg>
                }.into_any()
            } else {
                view! {
                    <svg width="14" height="14" viewBox="0 0 24 24"
                        fill="none" stroke="currentColor" stroke-width="2"
                        stroke-linecap="round" stroke-linejoin="round">
                        <rect x="9" y="9" width="13" height="13" rx="2" ry="2"/>
                        <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/>
                    </svg>
                }.into_any()
            }}
        </span>
    }
}

// ---- Phase: Claimed -------------------------------------------------------

#[component]
fn ClaimedCard<D>(handle: SendSwapHandle, on_done: D) -> impl IntoView
where
    D: Fn(MouseEvent) + Send + Sync + 'static,
{
    let wrap_style = format!(
        "display:flex; flex-direction:column; align-items:center; \
         justify-content:center; flex:1; padding:{pad}; gap:{gap};",
        pad = tokens::SPACE_L,
        gap = tokens::SPACE_XXL,
    );
    let card_style = card_style();
    let header_style = format!(
        "display:flex; flex-direction:column; align-items:center; gap:{gap};",
        gap = tokens::SPACE_M,
    );
    let title_style = format!(
        "font-size:{size}; font-weight:600; margin:0; color:{fg};",
        size = tokens::TEXT_2XL,
        fg = tokens::COLOR_CARD_FOREGROUND,
    );
    let amount_row_style = "display:flex; align-items:baseline; \
         justify-content:center; gap:6px;"
        .to_string();
    let amount_style = format!(
        "font-family:{font}; font-size:36px; font-weight:600; color:{fg}; \
         line-height:1; font-variant-numeric:tabular-nums;",
        font = tokens::FONT_NUMERIC,
        fg = tokens::COLOR_CARD_FOREGROUND,
    );
    let unit_style = format!(
        "font-size:{size}; color:{fg};",
        size = tokens::TEXT_SM,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );
    // Tailwind emerald-500 to match the toast success colour.
    let check_color = "hsl(160 84% 39%)";
    let check_style = format!("color:{check_color}; width:56px; height:56px;",);

    view! {
        <div style=wrap_style>
            <div style=card_style>
                <div style=header_style>
                    <svg
                        style=check_style
                        viewBox="0 0 24 24"
                        fill="currentColor"
                        aria-hidden="true"
                    >
                        <path d="M12 2a10 10 0 1 0 10 10A10 10 0 0 0 12 2zm-1 14.4L6.6 12l1.4-1.4L11 13.6l5-5L17.4 10z"/>
                    </svg>
                    <h2 style=title_style>"Sent"</h2>
                    <div style=amount_row_style>
                        <span style=amount_style>{format_amount(handle.amount)}</span>
                        <span style=unit_style>{handle.unit}</span>
                    </div>
                </div>
            </div>
            <div style=format!("width:100%; max-width:{max};", max = tokens::CARD_MAX_WIDTH)>
                <Button
                    variant=ButtonVariant::Primary
                    on_click=Callback::new(on_done)
                >
                    "Done"
                </Button>
            </div>
        </div>
    }
}

// ---- Phase: Failure -------------------------------------------------------

#[component]
fn FailureCard<R, D>(message: String, on_retry: R, on_dismiss: D) -> impl IntoView
where
    R: Fn(MouseEvent) + Send + Sync + 'static,
    D: Fn(MouseEvent) + Send + Sync + 'static,
{
    let wrap_style = format!(
        "display:flex; flex-direction:column; align-items:center; \
         justify-content:center; flex:1; padding:{pad}; gap:{gap};",
        pad = tokens::SPACE_L,
        gap = tokens::SPACE_XXL,
    );
    let card_style = card_style();
    let header_style = format!(
        "display:flex; flex-direction:column; align-items:center; gap:{gap};",
        gap = tokens::SPACE_M,
    );
    let title_style = format!(
        "font-size:{size}; font-weight:600; margin:0; color:{fg};",
        size = tokens::TEXT_2XL,
        fg = tokens::COLOR_CARD_FOREGROUND,
    );
    let message_style = format!(
        "font-size:{size}; color:{fg}; margin:0; text-align:center;",
        size = tokens::TEXT_SM,
        fg = tokens::COLOR_MUTED_FOREGROUND,
    );
    let icon_style = format!(
        "color:{color}; width:48px; height:48px;",
        color = tokens::COLOR_DESTRUCTIVE,
    );
    let buttons_style = format!(
        "display:flex; flex-direction:column; gap:{gap}; width:100%; max-width:{max};",
        gap = tokens::SPACE_S,
        max = tokens::CARD_MAX_WIDTH,
    );

    view! {
        <div style=wrap_style>
            <div style=card_style>
                <div style=header_style>
                    <svg
                        style=icon_style
                        viewBox="0 0 24 24"
                        fill="currentColor"
                        aria-hidden="true"
                    >
                        <path d="M12 2 1 21h22zm0 4 8.5 14.7H3.5zM11 10v5h2v-5zm0 7v2h2v-2z"/>
                    </svg>
                    <h2 style=title_style>"Couldn't send"</h2>
                    <p style=message_style>{message}</p>
                </div>
            </div>
            <div style=buttons_style>
                <Button
                    variant=ButtonVariant::Primary
                    on_click=Callback::new(on_retry)
                >
                    "Try again"
                </Button>
                <Button
                    variant=ButtonVariant::Ghost
                    on_click=Callback::new(on_dismiss)
                >
                    "Dismiss"
                </Button>
            </div>
        </div>
    }
}

// ---- Shared styles --------------------------------------------------------

fn pane_style() -> String {
    format!(
        "display:flex; flex-direction:column; flex:1; \
         background:{bg}; min-height:0;",
        bg = tokens::COLOR_BACKGROUND,
    )
}

fn card_style() -> String {
    format!(
        "background:{bg}; color:{fg}; border:1px solid {border}; \
         border-radius:{radius}; padding:{pad}; box-shadow:{shadow}; \
         width:100%; max-width:{max}; display:flex; \
         flex-direction:column; gap:{gap};",
        bg = tokens::COLOR_CARD,
        fg = tokens::COLOR_CARD_FOREGROUND,
        border = tokens::COLOR_BORDER,
        radius = tokens::RADIUS_LG,
        pad = tokens::SPACE_XXL,
        shadow = tokens::SHADOW_XS,
        max = tokens::CARD_MAX_WIDTH,
        gap = tokens::SPACE_L,
    )
}

// ---- Pure helpers (testable without a DOM) --------------------------------

/// Parse the numpad buffer into a `u64`. Returns `None` if the buffer
/// holds a non-integer (e.g. user typed a `.`) or an unparseable string.
/// Sat-only for v0 — mirrors iOS `parsedAmount`.
fn parsed_amount(buffer: &str) -> Option<u64> {
    let cleaned = buffer.trim_matches('.');
    cleaned.parse::<u64>().ok()
}

/// Render the amount buffer with thousands separators for the hero
/// display. Empty buffer renders `"0"` to keep the hero from collapsing.
fn display_amount(buffer: &str) -> String {
    let n = parsed_amount(buffer).unwrap_or(0);
    format_amount(n)
}

/// Comma-separated thousands grouping. Same shape `home.rs::format_amount`
/// + `cashu_token_paste_view::format_amount` use.
fn format_amount(amount: u64) -> String {
    let digits: Vec<char> = amount.to_string().chars().collect();
    let mut out = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

/// Render the token as `head...tail` for the inline copy chip. Mirrors
/// iOS `truncated` (head 12, tail 8, ellipsis between).
fn truncate_token(s: &str) -> String {
    let len = s.chars().count();
    if len <= 24 {
        return s.to_string();
    }
    let head: String = s.chars().take(12).collect();
    let tail: String = s.chars().skip(len - 8).collect();
    format!("{head}...{tail}")
}

// ---- Real SDK boundary ----------------------------------------------------
//
// `on_continue`     → `AgicashWasmWallet::prepare_send_quote`,
// `on_confirm`      → `AgicashWasmWallet::create_send_swap`,
// `poll_claim_once` → `AgicashWasmWallet::check_send_swap_claimed`
//                     (12b-3 un-mock — the former `mock_poll_claim`
//                     timer is gone; the share-screen 3 s loop now
//                     drives the real single-shot NUT-07 claim poll).

/// Parse a wasm-shell decimal amount string (e.g. `"100"`) into the
/// view's `u64` minor-unit. The shell already normalized to the
/// account's minor unit (sat/cent); a non-numeric value means the
/// facade returned something unexpected — fall back to 0 so the UI
/// renders a zero rather than panicking.
#[cfg(target_arch = "wasm32")]
fn wasm_amount_to_u64(s: &str) -> u64 {
    s.trim().parse::<u64>().unwrap_or(0)
}

/// `agicash_wasm::SendQuotePreviewWasm` → the view's `SendQuotePreview`.
/// `mint_url` is NOT on the wasm quote (note ◇: facade `SendTokenQuote`
/// has no `mint_url`); the view's `SendQuotePreview` never carried it
/// either, so this is a clean 1:1 of the fields the confirm card
/// renders. `total` ← wasm `total_amount`; `send_fee`/`receive_fee` ←
/// wasm `cashu_send_fee`/`cashu_receive_fee`.
#[cfg(target_arch = "wasm32")]
fn quote_from_wasm(q: &agicash_wasm::SendQuotePreviewWasm) -> SendQuotePreview {
    SendQuotePreview {
        amount_to_send: wasm_amount_to_u64(&q.amount_to_send),
        send_fee: wasm_amount_to_u64(&q.cashu_send_fee),
        receive_fee: wasm_amount_to_u64(&q.cashu_receive_fee),
        total: wasm_amount_to_u64(&q.total_amount),
        unit: q.unit.clone(),
    }
}

/// `agicash_wasm::SendSwapHandleWasm` → the view's `SendSwapHandle`.
/// The receipt is field-complete (real V4 token + real swap id).
#[cfg(target_arch = "wasm32")]
fn handle_from_wasm(h: &agicash_wasm::SendSwapHandleWasm) -> SendSwapHandle {
    SendSwapHandle {
        token: h.token.clone(),
        swap_id: h.swap_id.clone(),
        amount: wasm_amount_to_u64(&h.amount),
        unit: h.unit.clone(),
    }
}

/// `JsValue` error → display string. The wasm shell's
/// `wallet_error_to_js` makes the message the `WalletError` Display
/// string (the discriminator-bearing text the UI branches on, exactly
/// as iOS parses the FFI string today).
#[cfg(target_arch = "wasm32")]
fn js_err_string(e: &wasm_bindgen::JsValue) -> String {
    e.as_string()
        .unwrap_or_else(|| "send failed (unknown error)".to_string())
}

/// One single-shot NUT-07 claim poll over the wasm shell
/// (`AgicashWasmWallet::check_send_swap_claimed` →
/// `WalletClient::check_send_token_claimed`). 12b-3 un-mock of the
/// former `mock_poll_claim(tick)`.
///
/// A transient error (handle construction, no session yet, mint
/// round-trip blip) maps to [`ClaimPoll::Pending`] — NOT a hard
/// failure: the facade poll is single-shot and the cadence-owning loop
/// just retries on the next 3 s tick. A genuine terminal failure is
/// carried by the snapshot's `state == Failed`, exactly as iOS reads
/// `SendSwapClaimState`. The native `rlib` (unit-test) build has no
/// browser wallet, so it returns `Pending` (the loop never runs there —
/// the SSR `cfg(not(hydrate))` arm returns before the first call).
// Native (`rlib`) has no `.await` in the body (no browser wallet) — the
// signature stays `async` so the single wasm call site is uniform. The
// allow is honest + native-only, same idiom as the dead_code gates.
#[cfg_attr(
    not(target_arch = "wasm32"),
    allow(unused_variables, clippy::unused_async)
)]
async fn poll_claim_once(config: &AppConfig, swap_id: &str) -> ClaimPoll {
    #[cfg(target_arch = "wasm32")]
    {
        let Ok(wallet) = crate::components::wallet_context::seed_wasm_wallet(config).await else {
            return ClaimPoll::Pending;
        };
        match wallet.check_send_swap_claimed(swap_id.to_string()).await {
            Ok(status) => match status.state {
                agicash_wasm::SendClaimStateWasm::Pending => ClaimPoll::Pending,
                agicash_wasm::SendClaimStateWasm::Completed => ClaimPoll::Completed,
                agicash_wasm::SendClaimStateWasm::Failed => ClaimPoll::Failed(
                    status
                        .failure_reason
                        .unwrap_or_else(|| "send swap failed".to_string()),
                ),
            },
            // Transient — retry on the next tick (single-shot poll
            // philosophy; terminal failure rides the Failed state).
            Err(_) => ClaimPoll::Pending,
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        ClaimPoll::Pending
    }
}

// ---- Clipboard helper -----------------------------------------------------
//
// Single-call helper that writes `text` to the clipboard and flips the
// `show_copied` flag for 1.5 s. SSR-only build compiles a no-op so the
// type-checker is happy on the `cfg(not(feature = "hydrate"))` branch.

#[cfg(feature = "hydrate")]
fn copy_to_clipboard(
    text: &str,
    show_copied: RwSignal<bool>,
    toast: crate::components::ToastHandle,
) {
    use wasm_bindgen_futures::JsFuture;

    let Some(window) = web_sys::window() else {
        return;
    };
    let clipboard = window.navigator().clipboard();
    let promise = clipboard.write_text(text);
    spawn_local(async move {
        match JsFuture::from(promise).await {
            Ok(_) => {
                show_copied.set(true);
                toast.push("Copied to clipboard", ToastVariant::Success);
                // Reset the chip icon after 1.5 s. Best-effort — if the
                // user has already navigated away the signal is gone and
                // the update is a no-op.
                gloo_timers::future::TimeoutFuture::new(1_500).await;
                show_copied.set(false);
            }
            Err(err) => {
                toast.push(format!("Couldn't copy: {err:?}"), ToastVariant::Error);
            }
        }
    });
}

#[cfg(not(feature = "hydrate"))]
fn copy_to_clipboard(
    _text: &str,
    _show_copied: RwSignal<bool>,
    _toast: crate::components::ToastHandle,
) {
    // SSR build never runs in a browser; no-op.
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{display_amount, format_amount, parsed_amount, truncate_token};

    #[test]
    fn parses_integer_buffer() {
        assert_eq!(parsed_amount("0"), Some(0));
        assert_eq!(parsed_amount("12345"), Some(12_345));
    }

    #[test]
    fn rejects_decimal_buffer() {
        // Sat-only — anything with a real decimal is None.
        assert_eq!(parsed_amount("1.5"), None);
    }

    #[test]
    fn tolerates_trailing_dot() {
        // Mid-edit state `"12."` should still parse as 12 so the hero
        // doesn't flicker to 0 between digit and decimal.
        assert_eq!(parsed_amount("12."), Some(12));
    }

    #[test]
    fn display_amount_zero_for_empty() {
        assert_eq!(display_amount(""), "0");
        assert_eq!(display_amount("0"), "0");
    }

    #[test]
    fn display_amount_formats_thousands() {
        assert_eq!(display_amount("1234"), "1,234");
        assert_eq!(display_amount("1234567"), "1,234,567");
    }

    #[test]
    fn format_amount_groups_long_numbers() {
        assert_eq!(format_amount(0), "0");
        assert_eq!(format_amount(999), "999");
        assert_eq!(format_amount(100_000), "100,000");
    }

    #[test]
    fn truncate_short_token_unchanged() {
        assert_eq!(truncate_token("cashuBshort"), "cashuBshort");
    }

    #[test]
    fn truncate_long_token() {
        let raw = format!("cashuB{}", "A".repeat(100));
        let truncated = truncate_token(&raw);
        // head 12 + ... + tail 8 = 23 chars
        assert_eq!(truncated.chars().count(), 23);
        assert!(truncated.starts_with("cashuBAAAAAA"));
        assert!(truncated.ends_with("AAAAAAAA"));
        assert!(truncated.contains("..."));
    }
}
