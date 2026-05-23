//! `CashuTokenPasteView` — paste-a-Cashu-token receive flow.
//!
//! Ports the iOS `CashuTokenPasteView` (see
//! `ios/Agicash/Agicash/CashuTokenPasteView.swift`) to Leptos 0.7. Same
//! state machine, same visual chrome (card on a centered column, "Paste"
//! affordance on the field label, inline destructive error line under the
//! textarea, primary "Receive" button at the bottom).
//!
//! Behaviour (cross-account receive — `agicash-rs/master` 27afefae):
//!   - The textarea + `Preview` button parse the token client-side using
//!     `cdk::nuts::Token::from_str` (wasm-clean — no network).
//!   - The `Receive` button drives the wasm `ReceiveFlow` state machine
//!     (`AgicashWasmWallet::makeReceiveFlow()`):
//!     `Idle → Parsing → NeedsMintConfirmation → AddingMint → Swapping
//!     → Done | AlreadyClaimed | Failed`.
//!     Mirrors iOS (`8b630a54`) + Android (`77e7ce13`). When the pasted
//!     token is from a mint the user hasn't added, the flow surfaces a
//!     `NeedsMintConfirmation` card with copy "Add this mint?" + primary
//!     CTA "Add Mint and Claim" + ghost "Cancel" — same copy iOS +
//!     Android use, sourced from React's `<ReceiveToken/>` page
//!     (`app/features/receive/receive-cashu-token.tsx` lines 333-339,
//!     the `!isReceiveAccountKnown && purpose === 'transactional'`
//!     branch where the CTA copy switches to "Add Mint and Claim").
//!   - "Mint already added?" remains a UX hint on the Preview card,
//!     driven by `AgicashWasmWallet::list_accounts()`. It is not load-
//!     bearing: clicking Receive on an unknown mint now lands at the
//!     confirmation card instead of a raw error, regardless of the hint.

// The view body is long but linear; splitting into private sub-components
// would just add indirection without reuse benefit.
#![allow(clippy::too_many_lines)]

use std::str::FromStr;

use cdk::nuts::{CurrencyUnit, Token};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;

use crate::config::AppConfig;
use crate::tokens;

/// Parsed token preview — what we render between "user pasted + clicked
/// Preview" and "user clicked Receive". All fields are cheap derivations
/// of `cdk::nuts::Token`; we don't keep the `Token` itself in state
/// because the cdk type isn't `Clone` on every variant we care about.
#[derive(Clone, Debug)]
struct TokenPreview {
    /// Original encoded token string. Passed straight into
    /// `AgicashWasmWallet::receive_token(preview.raw)` (12d) without
    /// re-asking the user to paste; the preview/success cards render the
    /// derived fields below. `#[allow(dead_code)]` retained: the real
    /// read is inside the `cfg(target_arch = "wasm32")` receive block,
    /// so the native `rlib` (unit-test) build only writes this field.
    #[allow(dead_code)]
    raw: String,
    amount: u64,
    unit: String,
    mint_url: String,
    memo: Option<String>,
    /// True iff `mint_url` matches one of the user's existing Cashu
    /// accounts (real `AgicashWasmWallet::list_accounts()` lookup).
    /// Drives only the "Add mint first?" CTA — does NOT gate receive.
    /// Defaults to `true` from the synchronous parse (CTA hidden) and
    /// is corrected by the async account-list lookup once it resolves,
    /// so the unknown-mint CTA never flashes before the answer is known.
    mint_known: bool,
}

/// Result rendered on the success card after the redeem completes.
/// `amount` may be empty when the flow lands in `AlreadyClaimed` —
/// re-rendering "0 sats" would be misleading (the FFI/WASM deliberately
/// omits an amount for that variant), so `SuccessCard` skips the
/// amount block when it's blank. Mirrors iOS `SuccessCard` /
/// `AlreadyClaimed` handling (`8b630a54`).
#[derive(Clone, Debug)]
struct ReceiveResult {
    /// Decimal amount string (e.g. "10"), or empty for `AlreadyClaimed`.
    amount: String,
    unit: String,
    mint_url: String,
}

/// Mint-confirmation card data — the payload that surfaces when the
/// `ReceiveFlow` state machine pauses on `NeedsMintConfirmation`. Local
/// deserialize target for the JSON envelope `AgicashReceiveFlow` emits
/// (`{ "kind": "needsMintConfirmation", "confirmation": { … } }`). Field
/// shape mirrors `agicash_wasm::MintConfirmationWasm` 1:1 (which itself
/// mirrors `agicash-cashu::MintConfirmation`). Decoded via
/// `serde_wasm_bindgen::from_value` — same idiom this file already uses
/// for `AccountWasm` in `fetch_mint_known`.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct MintConfirmationData {
    mint_url: String,
    mint_name: String,
    unit: String,
    #[allow(dead_code)]
    currency: String,
    amount: String,
    fee: String,
}

/// View state machine. Mirrors iOS `Phase` (`8b630a54`):
///   `entry → working → (confirmingMint? → addingMint?) → swapping →
///    success | error`
/// with a Leptos-only `Preview` sub-state inserted between `Entry` and
/// `Working` (iOS jumps straight from paste → Working because it has
/// carousel-level chrome that gives a continuous "receive" affordance;
/// the web flow needs an explicit Preview so the user sees what they're
/// about to claim before committing).
///
/// `Success` is constructed only on the real wasm receive path
/// (`cfg(target_arch = "wasm32")` — the browser is the only shipping
/// target). The native `rlib` (workspace unit-test build) has no
/// browser wallet so it constructs only entry/preview/error;
/// the variant is still pattern-matched by the `view!` render arms.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
#[derive(Clone, Debug)]
enum Phase {
    /// User is editing the textarea. No preview yet.
    Entry,
    /// Token parsed successfully — preview card visible, Receive button
    /// armed.
    Preview(TokenPreview),
    /// Receive in flight — spinner on the button, fields locked. Covers
    /// the `Parsing` state from the `ReceiveFlow` state machine.
    Working(TokenPreview),
    /// Token's source mint isn't in the user's accounts. Renders the
    /// `MintConfirmationCard` with "Add Mint and Claim" / Cancel CTAs.
    /// Mirrors iOS `confirmingMint(MintConfirmationFfi)`.
    ConfirmingMint(MintConfirmationData),
    /// Between confirm tap and the mint being added (typically 1-3 s).
    /// Indeterminate-progress card. Mirrors iOS `addingMint`.
    AddingMint,
    /// Between mint-added and swap completion (typically 1-3 s).
    /// Indeterminate-progress card. Mirrors iOS `swapping`.
    Swapping,
    /// Redeem complete — success card with amount + mint url + Done.
    Success(ReceiveResult),
    /// Parse error or receive error. Shown inline under the textarea
    /// (Entry) or as a destructive header line in the form re-entered
    /// after a Cancel; user can edit and retry without dismissing.
    Error(String),
}

#[component]
pub fn CashuTokenPasteView() -> impl IntoView {
    let navigate = use_navigate();
    // 12d: same config source LoginView / wallet_context use. Captured
    // at component-body level (NOT inside spawn_local) per
    // feedback_leptos_spawn_local_gotchas. Held in a `StoredValue`
    // (Copy) so the `on_receive` handler stays `Fn + Copy` — the
    // reactive `match phase` render closure requires `FnMut`.
    let config = StoredValue::new(expect_context::<AppConfig>());

    let token_text = RwSignal::new(String::new());
    let phase: RwSignal<Phase> = RwSignal::new(Phase::Entry);
    // Long-lived `AgicashReceiveFlow` handle scoped to the current
    // interaction. Each Receive-button click constructs a fresh flow
    // and stores it here; the confirm/cancel handlers re-read it to
    // dispatch the next event into the same Rust-side state machine.
    // `LocalStorage` (not the default `SyncStorage`) because the wasm
    // `AgicashReceiveFlow` wraps an `Rc<Mutex<...>>` (non-`Send`); the
    // value is pinned to the JS main thread where it was constructed,
    // same idiom `wallet_context::WalletData` uses for the non-`Send`
    // realtime/driver handles. Native (`rlib`) builds don't see the
    // wasm wallet so this field is wasm-only.
    #[cfg(target_arch = "wasm32")]
    let active_flow: StoredValue<
        Option<std::rc::Rc<agicash_wasm::AgicashReceiveFlow>>,
        leptos::prelude::LocalStorage,
    > = StoredValue::new_local(None);

    // ---- Handlers ---------------------------------------------------------

    let on_preview = move |_ev| {
        let raw = token_text.get().trim().to_string();
        if raw.is_empty() {
            phase.set(Phase::Error("Paste a Cashu token first.".into()));
            return;
        }
        match parse_token(&raw) {
            Ok(preview) => {
                phase.set(Phase::Preview(preview.clone()));
                resolve_mint_known(config, phase, preview);
            }
            Err(msg) => phase.set(Phase::Error(msg)),
        }
    };

    // Reset preview/error when the user edits the textarea so they don't
    // see a stale preview matching a previous paste.
    let on_token_input = move |ev: leptos::ev::Event| {
        let value = event_target_value(&ev);
        token_text.set(value);
        // If we were showing a Preview / Success / Error, drop back to
        // Entry so the user can re-trigger Preview deliberately.
        match phase.get() {
            Phase::Preview(_) | Phase::Error(_) | Phase::Success(_) => {
                phase.set(Phase::Entry);
            }
            // Don't yank the user out of an in-flight state — Working,
            // ConfirmingMint, AddingMint, Swapping each represent a
            // committed receive interaction the user is mid-stream on.
            Phase::Entry
            | Phase::Working(_)
            | Phase::ConfirmingMint(_)
            | Phase::AddingMint
            | Phase::Swapping => {}
        }
    };

    let on_blur = move |_ev| {
        // Mirror iOS UX: parse on blur as well so the user sees the
        // preview even if they don't click the explicit Preview button.
        // Only run if we're still in Entry and the field has content.
        if !matches!(phase.get(), Phase::Entry) {
            return;
        }
        let raw = token_text.get().trim().to_string();
        if raw.is_empty() {
            return;
        }
        match parse_token(&raw) {
            Ok(preview) => {
                phase.set(Phase::Preview(preview.clone()));
                resolve_mint_known(config, phase, preview);
            }
            Err(msg) => phase.set(Phase::Error(msg)),
        }
    };

    let on_receive = move |_ev| {
        // Snapshot the preview so we can route to Success regardless
        // of what the user types during the receive round-trip.
        let Phase::Preview(preview) = phase.get() else {
            return;
        };
        phase.set(Phase::Working(preview.clone()));
        let config = config.get_value();

        spawn_local(async move {
            // Drives the `ReceiveFlow` state machine — mirrors iOS
            // `CashuTokenPasteView.submit()` (8b630a54) and Android
            // `ReceiveCarouselScreen.submit()` (77e7ce13). The one-shot
            // `receive_token` path it replaced dead-ended with the raw
            // "no matching account for mint <url>" error on unknown
            // mints; the flow lets the user confirm + add the mint
            // inline.
            #[cfg(target_arch = "wasm32")]
            {
                use std::rc::Rc;

                // `seed_wasm_wallet` is the canonical session-threaded
                // composition root the other 4 button-click sites use
                // (see `wallet_context::seed_wasm_wallet`). It calls
                // `setSession` immediately after `new()` so the
                // facade's `require_session()` guard inside
                // `WalletClient::receive_flow()` passes.
                let wallet =
                    match crate::components::wallet_context::seed_wasm_wallet(&config).await {
                        Ok(w) => w,
                        Err(e) => {
                            phase.set(Phase::Error(
                                e.as_string()
                                    .unwrap_or_else(|| "wallet init failed".to_string()),
                            ));
                            return;
                        }
                    };

                // Each click of Receive gets a fresh flow handle —
                // flows are not persisted across constructions
                // (verbatim FFI/WASM semantics).
                let flow = match wallet.make_receive_flow().await {
                    Ok(f) => Rc::new(f),
                    Err(e) => {
                        phase.set(Phase::Error(
                            e.as_string()
                                .unwrap_or_else(|| "receive flow init failed".to_string()),
                        ));
                        return;
                    }
                };
                active_flow.set_value(Some(flow.clone()));

                match flow.start(preview.raw.clone()).await {
                    Ok(js) => render_flow_state(js, phase, active_flow),
                    Err(e) => {
                        active_flow.set_value(None);
                        phase.set(Phase::Error(
                            e.as_string()
                                .unwrap_or_else(|| "receive failed".to_string()),
                        ));
                    }
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                // Native rlib (unit tests): no browser wallet. Touch
                // captured values so they aren't flagged unused.
                let _ = &config;
                phase.set(Phase::Error(
                    "wallet unavailable (native build)".to_string(),
                ));
            }
        });
    };

    // User said yes to "Add Mint and Claim". Dispatches `ConfirmAddMint`
    // into the live flow — the Rust side runs `add_mint` + the receive
    // swap in sequence and reports back through state transitions.
    // Mirrors iOS `confirmAddMint()` (8b630a54).
    let on_confirm_add_mint = move |_ev| {
        spawn_local(async move {
            #[cfg(target_arch = "wasm32")]
            {
                let Some(flow) = active_flow.with_value(Clone::clone) else {
                    phase.set(Phase::Error(
                        "Receive flow was lost. Please paste the token again.".to_string(),
                    ));
                    return;
                };
                phase.set(Phase::AddingMint);
                match flow.confirm_add_mint().await {
                    Ok(js) => render_flow_state(js, phase, active_flow),
                    Err(e) => {
                        active_flow.set_value(None);
                        phase.set(Phase::Error(
                            e.as_string()
                                .unwrap_or_else(|| "add-mint failed".to_string()),
                        ));
                    }
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                phase.set(Phase::Error(
                    "wallet unavailable (native build)".to_string(),
                ));
            }
        });
    };

    // User declined the mint-add. Dispatch `CancelAddMint` so the Rust
    // side closes the flow cleanly, then return to Entry so the user
    // can paste a different token (or close the page). Mirrors iOS
    // `cancelAddMint()` (8b630a54).
    let on_cancel_add_mint = move |_ev| {
        spawn_local(async move {
            #[cfg(target_arch = "wasm32")]
            {
                if let Some(flow) = active_flow.with_value(Clone::clone) {
                    // Best-effort: the cancel transition is internal
                    // bookkeeping; we don't need to surface its
                    // resulting state, just drop it.
                    let _ = flow.cancel_add_mint().await;
                }
                active_flow.set_value(None);
            }
            phase.set(Phase::Entry);
            // Clear the textarea so the user paints a fresh paste,
            // matching iOS's `tokenFocused = true` re-entry rhythm.
            token_text.set(String::new());
        });
    };

    let on_done = {
        let navigate = navigate.clone();
        move |_ev| {
            navigate("/", leptos_router::NavigateOptions::default());
        }
    };

    let on_add_mint = {
        let navigate = navigate.clone();
        move |_ev| {
            // Whether to show this CTA is now a REAL decision (the
            // account-list lookup in `resolve_mint_known`). The
            // destination is the real registered add-mint route
            // (`/accounts/add`, app.rs) — was `/accounts/add-mint`,
            // which matched no route and 404'd to "Not found.". The
            // add-mint *form page itself* is still a placeholder
            // (tracked in `pages/accounts.rs`); inline pre-fill of the
            // pasted mint URL is a follow-up there, not in this view.
            navigate("/accounts/add", leptos_router::NavigateOptions::default());
        }
    };

    // ---- Styles -----------------------------------------------------------
    // Mirror the page chrome from LoginView (centered column, single card
    // capped at CARD_MAX_WIDTH) and the FormCard chrome from the iOS
    // `CashuTokenPasteView` (Spacing.xxl padding, brandCard border).

    let page_style = format!(
        "display:flex; flex-direction:column; align-items:center; \
         min-height:100dvh; padding:{} {}; background:{}; \
         color:{}; font-family:{};",
        tokens::SPACE_HERO,
        tokens::SPACE_L,
        tokens::COLOR_BACKGROUND,
        tokens::COLOR_FOREGROUND,
        tokens::FONT_PRIMARY,
    );

    let header_style = format!(
        "width:100%; max-width:{}; display:flex; \
         align-items:center; justify-content:space-between; \
         margin-bottom:{};",
        tokens::CARD_MAX_WIDTH,
        tokens::SPACE_L,
    );

    let back_link_style = format!(
        "font-size:{}; color:{}; text-decoration:none; cursor:pointer;",
        tokens::TEXT_SM,
        tokens::COLOR_MUTED_FOREGROUND,
    );

    let header_title_style = format!(
        "font-size:{}; font-weight:600; margin:0; color:{};",
        tokens::TEXT_LG,
        tokens::COLOR_FOREGROUND,
    );

    view! {
        <div style=page_style>
            <div style=header_style>
                <a href="/" style=back_link_style>"← Back"</a>
                <h1 style=header_title_style>"Receive"</h1>
                // Spacer for symmetry with the back link.
                <span style="width:48px;"/>
            </div>

            {move || match phase.get() {
                Phase::Entry | Phase::Error(_) => view! {
                    <FormCard
                        token_text=token_text
                        is_working=false
                        error_message=if let Phase::Error(m) = phase.get() { Some(m) } else { None }
                        on_token_input=on_token_input
                        on_blur=on_blur
                        on_preview=on_preview
                    />
                }.into_any(),
                Phase::Preview(preview) => view! {
                    <PreviewCard
                        preview=preview
                        is_working=false
                        on_receive=on_receive
                        on_add_mint=on_add_mint.clone()
                    />
                }.into_any(),
                Phase::Working(preview) => view! {
                    <PreviewCard
                        preview=preview
                        is_working=true
                        on_receive=on_receive
                        on_add_mint=on_add_mint.clone()
                    />
                }.into_any(),
                Phase::ConfirmingMint(confirmation) => view! {
                    <MintConfirmationCard
                        confirmation=confirmation
                        is_working=false
                        on_confirm=on_confirm_add_mint
                        on_cancel=on_cancel_add_mint
                    />
                }.into_any(),
                Phase::AddingMint => view! {
                    <ProgressCard
                        title="Adding mint".to_string()
                        subtitle="Setting up your new Cashu account…".to_string()
                    />
                }.into_any(),
                Phase::Swapping => view! {
                    <ProgressCard
                        title="Claiming token".to_string()
                        subtitle="Finalizing the receive swap…".to_string()
                    />
                }.into_any(),
                Phase::Success(result) => view! {
                    <SuccessCard
                        result=result
                        on_done=on_done.clone()
                    />
                }.into_any(),
            }}
        </div>
    }
}

// ---- Sub-components -------------------------------------------------------

/// Paste-token form card. Mirrors iOS `FormCard` (private struct inside
/// `CashuTokenPasteView.swift`).
#[component]
fn FormCard<O, B, P>(
    token_text: RwSignal<String>,
    is_working: bool,
    error_message: Option<String>,
    on_token_input: O,
    on_blur: B,
    on_preview: P,
) -> impl IntoView
where
    O: Fn(leptos::ev::Event) + 'static,
    B: Fn(leptos::ev::FocusEvent) + 'static,
    P: Fn(leptos::ev::MouseEvent) + 'static,
{
    let card_style = card_style();
    let label_row_style =
        "display:flex; align-items:center; justify-content:space-between;".to_string();
    let label_style = format!(
        "font-size:{}; font-weight:600; color:{};",
        tokens::TEXT_SM,
        tokens::COLOR_CARD_FOREGROUND,
    );
    let textarea_style = format!(
        "width:100%; min-height:96px; max-height:200px; resize:vertical; \
         padding:{}; border:1px solid {}; border-radius:{}; \
         font-family:{}; font-size:{}; color:{}; background:{}; \
         box-sizing:border-box;",
        tokens::SPACE_S,
        tokens::COLOR_BORDER,
        tokens::RADIUS_MD,
        tokens::FONT_PRIMARY,
        tokens::TEXT_SM,
        tokens::COLOR_FOREGROUND,
        tokens::COLOR_BACKGROUND,
    );
    let title_style = format!(
        "font-size:{}; font-weight:600; margin:0; color:{};",
        tokens::TEXT_2XL,
        tokens::COLOR_CARD_FOREGROUND,
    );
    let description_style = format!(
        "font-size:{}; margin:0 0 {} 0; color:{};",
        tokens::TEXT_SM,
        tokens::SPACE_S,
        tokens::COLOR_MUTED_FOREGROUND,
    );
    let error_style = format!(
        "color:{}; font-size:{}; margin:0;",
        tokens::COLOR_DESTRUCTIVE,
        tokens::TEXT_SM,
    );

    view! {
        <div style=card_style>
            <div>
                <h2 style=title_style>"Receive Cashu"</h2>
                <p style=description_style>
                    "Paste a Cashu token to claim it into your wallet"
                </p>
            </div>

            <div style="display:flex; flex-direction:column; gap:8px;">
                <div style=label_row_style>
                    <span style=label_style>"Token"</span>
                </div>
                <textarea
                    style=textarea_style
                    prop:value=move || token_text.get()
                    placeholder="cashuA... or cashuB..."
                    autocapitalize="none"
                    spellcheck="false"
                    disabled=is_working
                    on:input=on_token_input
                    on:blur=on_blur
                />
            </div>

            {error_message.map(|msg| view! {
                <p style=error_style>{msg}</p>
            })}

            <button
                style=button_style(ButtonVariant::Primary)
                disabled=move || is_working || token_text.get().trim().is_empty()
                on:click=on_preview
            >
                "Preview"
            </button>
        </div>
    }
}

/// Preview card shown after the token is parsed. Renders amount + mint
/// URL + memo and an "Add mint first?" CTA when the mint isn't in the
/// (mocked) known-mints list. Reuses the same card chrome as `FormCard`.
#[component]
fn PreviewCard<R, A>(
    preview: TokenPreview,
    is_working: bool,
    on_receive: R,
    on_add_mint: A,
) -> impl IntoView
where
    R: Fn(leptos::ev::MouseEvent) + 'static,
    A: Fn(leptos::ev::MouseEvent) + 'static,
{
    let card_style = card_style();
    let title_style = format!(
        "font-size:{}; font-weight:600; margin:0; color:{};",
        tokens::TEXT_2XL,
        tokens::COLOR_CARD_FOREGROUND,
    );
    let amount_row_style =
        "display:flex; align-items:baseline; justify-content:center; gap:6px;".to_string();
    let amount_style = format!(
        "font-family:{}; font-size:48px; font-weight:600; color:{}; \
         line-height:1; font-variant-numeric:tabular-nums;",
        tokens::FONT_NUMERIC,
        tokens::COLOR_CARD_FOREGROUND,
    );
    let unit_style = format!(
        "font-size:{}; color:{};",
        tokens::TEXT_BASE,
        tokens::COLOR_MUTED_FOREGROUND,
    );
    let meta_label_style = format!(
        "font-size:{}; font-weight:600; color:{}; margin:0;",
        tokens::TEXT_SM,
        tokens::COLOR_CARD_FOREGROUND,
    );
    let meta_value_style = format!(
        "font-size:{}; color:{}; margin:0; overflow-wrap:anywhere;",
        tokens::TEXT_SM,
        tokens::COLOR_MUTED_FOREGROUND,
    );
    let unknown_mint_style = format!(
        "color:{}; font-size:{}; margin:0;",
        tokens::COLOR_DESTRUCTIVE,
        tokens::TEXT_SM,
    );

    let amount = preview.amount;
    let unit = preview.unit.clone();
    let mint_url = preview.mint_url.clone();
    let memo = preview.memo.clone();
    let mint_known = preview.mint_known;

    view! {
        <div style=card_style>
            <h2 style=title_style>"Preview"</h2>

            <div style=amount_row_style>
                <span style=amount_style>{format_amount(amount)}</span>
                <span style=unit_style>{unit}</span>
            </div>

            <div style="display:flex; flex-direction:column; gap:4px;">
                <p style=meta_label_style.clone()>"Mint"</p>
                <p style=meta_value_style.clone()>{mint_url}</p>
            </div>

            {memo.map(|m| view! {
                <div style="display:flex; flex-direction:column; gap:4px;">
                    <p style=meta_label_style.clone()>"Memo"</p>
                    <p style=meta_value_style.clone()>{m}</p>
                </div>
            })}

            {(!mint_known).then(|| view! {
                <div style="display:flex; flex-direction:column; gap:8px;">
                    <p style=unknown_mint_style>
                        "This mint isn't in your wallet yet."
                    </p>
                    <button
                        style=button_style(ButtonVariant::Ghost)
                        on:click=on_add_mint
                    >
                        "Add mint first?"
                    </button>
                </div>
            })}

            <button
                style=button_style(ButtonVariant::Primary)
                disabled=is_working
                on:click=on_receive
            >
                {move || if is_working { "Receiving..." } else { "Receive" }}
            </button>
        </div>
    }
}

/// Confirmation card shown when the pasted token is from a mint the
/// user hasn't added yet — the `NeedsMintConfirmation` state from the
/// `ReceiveFlow` machine. Same copy + CTA pair iOS (8b630a54) and
/// Android (77e7ce13) use, sourced from React's `<ReceiveToken/>` page
/// (`app/features/receive/receive-cashu-token.tsx` lines 333-339, the
/// `!isReceiveAccountKnown && purpose === 'transactional'` branch where
/// the CTA copy switches to `"Add Mint and Claim"`).
///
/// Tailwind classes mirror the React app's design language (the
/// canonical source per `feedback_visual_parity_all_clients`). Card
/// chrome: `rounded-lg bg-card text-card-foreground border` + shadow,
/// matching React's `<Card/>` primitive
/// (`app/components/ui/card.tsx`). Buttons mirror React's
/// `<Button variant="default"/>` (primary) and
/// `<Button variant="ghost"/>` (cancel) — `bg-primary
/// text-primary-foreground hover:bg-primary/90 h-10 px-4 rounded-md
/// text-sm font-medium` for primary,
/// `hover:bg-accent hover:text-accent-foreground` for ghost.
/// Numeric amount uses `font-[var(--font-numeric)] tabular-nums` —
/// React renders the equivalent via the Teko-family `font-numeric`
/// utility bound in `style/tailwind.in.css`.
#[component]
fn MintConfirmationCard<C, X>(
    confirmation: MintConfirmationData,
    is_working: bool,
    on_confirm: C,
    on_cancel: X,
) -> impl IntoView
where
    C: Fn(leptos::ev::MouseEvent) + 'static,
    X: Fn(leptos::ev::MouseEvent) + 'static,
{
    let mint_name = confirmation.mint_name.clone();
    let mint_url = confirmation.mint_url.clone();
    let amount = confirmation.amount.clone();
    let unit = confirmation.unit.clone();
    let fee = confirmation.fee.clone();
    let show_fee = !fee.is_empty() && fee != "0";
    let fee_label = format!("Mint fee: {fee} {unit_label}", unit_label = &unit);

    view! {
        <div class="w-full max-w-sm rounded-lg border bg-card text-card-foreground \
                    shadow-xs p-8 flex flex-col gap-4">
            // Card header — same rhythm as the iOS/Android sibling
            // (title + supporting caption) so the language reads
            // consistently across the receive surface.
            <div class="flex flex-col gap-1">
                <h2 class="text-2xl font-semibold m-0 text-card-foreground">
                    "Add this mint?"
                </h2>
                <p class="text-sm m-0 text-muted-foreground">
                    "This token is from a mint you haven't added yet. \
                     Add it to claim the funds."
                </p>
            </div>

            // Mint identity block — name above, URL below. Same shape
            // as iOS's `AddMintSuccessCard`-derived block.
            <div class="flex flex-col items-center gap-2">
                <p class="text-base font-semibold m-0 text-card-foreground \
                          text-center truncate w-full">
                    {mint_name}
                </p>
                <p class="text-xs m-0 text-muted-foreground text-center \
                          break-all w-full">
                    {mint_url}
                </p>
            </div>

            // Amount block — mirrors the SuccessCard's amount rendering
            // so the pre-claim and post-claim cards read as one visual
            // family. `font-numeric` (Teko) + tabular-nums matches the
            // React `<Money/>` component's display rhythm.
            <div class="flex flex-col items-center gap-1">
                <p class="text-xs m-0 text-muted-foreground">"Claiming"</p>
                <div class="flex items-baseline justify-center gap-1.5">
                    <span class="text-5xl font-semibold leading-none \
                                 text-card-foreground font-numeric tabular-nums">
                        {amount}
                    </span>
                    <span class="text-base text-muted-foreground">{unit.clone()}</span>
                </div>
                {show_fee.then(|| view! {
                    <p class="text-xs m-0 text-muted-foreground">{fee_label}</p>
                })}
            </div>

            // CTA stack — primary "Add Mint and Claim" (exact React +
            // iOS + Android copy) + ghost "Cancel". Tailwind classes
            // mirror React's `<Button/>` shadcn primitive shape:
            // primary = bg-primary/text-primary-foreground, ghost =
            // transparent with hover background. Disabled-during-work
            // styling matches the `disabled:opacity-50` shadcn pattern.
            <button
                class="inline-flex items-center justify-center h-10 px-4 \
                       rounded-md text-sm font-medium bg-primary \
                       text-primary-foreground hover:bg-primary/90 \
                       disabled:opacity-50 disabled:cursor-not-allowed \
                       transition-opacity cursor-pointer"
                disabled=is_working
                on:click=on_confirm
            >
                "Add Mint and Claim"
            </button>
            <button
                class="inline-flex items-center justify-center h-10 px-4 \
                       rounded-md text-sm font-medium bg-transparent \
                       text-card-foreground border border-border \
                       hover:bg-accent hover:text-accent-foreground \
                       disabled:opacity-50 disabled:cursor-not-allowed \
                       transition-colors cursor-pointer"
                disabled=is_working
                on:click=on_cancel
            >
                "Cancel"
            </button>
        </div>
    }
}

/// Indeterminate-progress card shown during the `AddingMint` and
/// `Swapping` phases (typically 1-3 s each). Mirrors iOS
/// `ProgressCard` (8b630a54). Uses the same Tailwind card chrome as
/// `MintConfirmationCard` so the visual rhythm carries across the
/// state machine. Spinner is a CSS-only `animate-spin` ring — pure
/// utility-class, no `<ProgressView/>`-style platform dependency.
#[component]
fn ProgressCard(title: String, subtitle: String) -> impl IntoView {
    view! {
        <div class="w-full max-w-sm rounded-lg border bg-card text-card-foreground \
                    shadow-xs p-8 flex flex-col items-center gap-4">
            <div class="flex flex-col items-center gap-1">
                <h2 class="text-2xl font-semibold m-0 text-card-foreground \
                           text-center">
                    {title}
                </h2>
                <p class="text-sm m-0 text-muted-foreground text-center">
                    {subtitle}
                </p>
            </div>
            // Pure-Tailwind indeterminate spinner — animate-spin (built
            // into Tailwind core) + a ring made from a transparent
            // top border on a foreground-colored circle. Mirrors the
            // React app's <Spinner/> shape (border-2 + animate-spin).
            <div class="h-10 w-10 rounded-full border-2 border-muted-foreground \
                        border-t-transparent animate-spin my-4"
                 aria-label="loading"
                 role="status"/>
        </div>
    }
}

/// Success card shown after the redeem completes. Mirrors iOS
/// `SuccessCard`. Handles the `AlreadyClaimed` case by suppressing the
/// amount block when `result.amount` is empty — re-rendering "0 sats"
/// would be misleading (the WASM `AlreadyClaimedInfoWasm` deliberately
/// omits an amount; see `crates/agicash-wasm/src/types.rs`).
#[component]
fn SuccessCard<D>(result: ReceiveResult, on_done: D) -> impl IntoView
where
    D: Fn(leptos::ev::MouseEvent) + 'static,
{
    let card_style = card_style();
    let title_style = format!(
        "font-size:{}; font-weight:600; margin:0; color:{};",
        tokens::TEXT_2XL,
        tokens::COLOR_CARD_FOREGROUND,
    );
    let subhead_style = format!(
        "font-size:{}; margin:0 0 {} 0; color:{};",
        tokens::TEXT_SM,
        tokens::SPACE_S,
        tokens::COLOR_MUTED_FOREGROUND,
    );
    let amount_row_style = "display:flex; align-items:baseline; \
         justify-content:center; gap:6px;"
        .to_string();
    let amount_style = format!(
        "font-family:{}; font-size:48px; font-weight:600; color:{}; \
         line-height:1; font-variant-numeric:tabular-nums;",
        tokens::FONT_NUMERIC,
        tokens::COLOR_CARD_FOREGROUND,
    );
    let unit_style = format!(
        "font-size:{}; color:{};",
        tokens::TEXT_BASE,
        tokens::COLOR_MUTED_FOREGROUND,
    );
    let mint_style = format!(
        "font-size:{}; color:{}; margin:0; text-align:center; \
         overflow-wrap:anywhere;",
        tokens::TEXT_SM,
        tokens::COLOR_MUTED_FOREGROUND,
    );

    let subhead = if result.amount.is_empty() {
        "This token was already claimed."
    } else {
        "Proofs added to your wallet."
    };
    let show_amount = !result.amount.is_empty();
    let amount = result.amount.clone();
    let unit = result.unit.clone();

    view! {
        <div style=card_style>
            <div>
                <h2 style=title_style>"Token received"</h2>
                <p style=subhead_style>{subhead}</p>
            </div>

            <div style="display:flex; flex-direction:column; align-items:center; gap:8px;">
                {show_amount.then(|| view! {
                    <div style=amount_row_style>
                        <span style=amount_style>{amount}</span>
                        <span style=unit_style>{unit}</span>
                    </div>
                })}
                <p style=mint_style>{result.mint_url}</p>
            </div>

            <button
                style=button_style(ButtonVariant::Primary)
                on:click=on_done
            >
                "Done"
            </button>
        </div>
    }
}

// ---- Helpers --------------------------------------------------------------

/// Parse a Cashu token from arbitrary pasted text into the preview
/// view-model. Pure (no network).
///
/// Step 1: `agicash_cashu::extract_cashu_token` finds the encoded
/// token inside the input (URL with hash/query, `cashu:` URI,
/// embedded prose, or raw paste all work). Returns the verbatim
/// matched substring — what we then re-decode with
/// `cdk::nuts::Token::from_str` so we can read `.value() /
/// .mint_url() / .unit() / .memo()` for the preview card.
///
/// The `raw` field on `TokenPreview` carries the **extracted** token
/// (not the original input), so the receive call downstream gets a
/// clean encoded string — the strict `WalletClient::receive_token`
/// inside the wasm wallet would reject the original URL paste.
fn parse_token(raw: &str) -> Result<TokenPreview, String> {
    let encoded = agicash_cashu::extract_cashu_token(raw)
        .ok_or_else(|| "No Cashu token found in that text.".to_string())?;
    let token = Token::from_str(&encoded).map_err(|e| format!("Invalid token: {e}"))?;
    let mint_url = token
        .mint_url()
        .map_err(|e| format!("Token is missing a mint URL: {e}"))?
        .to_string();
    let unit_label = unit_label(token.unit());
    let amount = token
        .value()
        .map_err(|e| format!("Could not compute token amount: {e}"))?;
    let memo = token.memo().clone();
    Ok(TokenPreview {
        raw: encoded,
        amount: amount.into(),
        unit: unit_label,
        mint_url,
        memo,
        // Optimistically hide the "Add mint first?" CTA; the async
        // account-list lookup (`fetch_mint_known`) corrects this once
        // the real wallet answers, so the CTA never flashes.
        mint_known: true,
    })
}

fn unit_label(unit: Option<CurrencyUnit>) -> String {
    match unit {
        Some(CurrencyUnit::Sat) => "sats".to_string(),
        Some(CurrencyUnit::Msat) => "msats".to_string(),
        Some(CurrencyUnit::Usd) => "USD".to_string(),
        Some(CurrencyUnit::Eur) => "EUR".to_string(),
        Some(other) => other.to_string(),
        // Missing unit isn't fatal — cdk treats it as "unknown unit".
        // We render an empty label rather than a placeholder so the
        // amount stays the focal point.
        None => String::new(),
    }
}

/// True iff `mint_url` matches any URL in `account_mint_urls`. Trailing
/// slash is normalized on both sides so `https://m.example` and
/// `https://m.example/` compare equal (the user-pasted token URL and
/// the stored account URL may differ only by a trailing slash).
//
// Called only from `fetch_mint_known` (wasm-only) + the unit tests; the
// native `rlib` build sees no non-test caller. Same native-only honest
// `allow(dead_code)` idiom the `Phase` enum uses above.
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
fn mint_in_accounts<'a>(
    mint_url: &str,
    account_mint_urls: impl IntoIterator<Item = &'a str>,
) -> bool {
    let needle = mint_url.trim_end_matches('/');
    account_mint_urls
        .into_iter()
        .any(|known| known.trim_end_matches('/') == needle)
}

/// Cashu account mint URL fetched from the real wallet. Local
/// deserialize target for the JSON array `AgicashWasmWallet::
/// list_accounts()` returns (`AccountWasm` serializes with verbatim
/// field names; only the two fields the CTA hint needs are read).
#[cfg(target_arch = "wasm32")]
#[derive(serde::Deserialize)]
struct AccountMintUrl {
    account_type: String,
    mint_url: Option<String>,
}

/// Real "is this mint already a wallet account?" lookup. Asks
/// `AgicashWasmWallet::list_accounts()` (12c receive-flow path) and
/// returns whether `mint_url` matches any of the user's existing Cashu
/// accounts. On any error (no session, network) returns `true` — the
/// CTA is a non-blocking hint, so a failed lookup must NOT pop a
/// spurious "Add mint first?" prompt; `receive_token` handles unknown
/// mints itself regardless.
#[cfg(target_arch = "wasm32")]
async fn fetch_mint_known(config: &AppConfig, mint_url: &str) -> bool {
    let Ok(wallet) = crate::components::wallet_context::seed_wasm_wallet(config).await else {
        return true;
    };
    let Ok(js) = wallet.list_accounts().await else {
        return true;
    };
    let accounts: Vec<AccountMintUrl> = match serde_wasm_bindgen::from_value(js) {
        Ok(a) => a,
        Err(_) => return true,
    };
    let cashu_mint_urls = accounts
        .iter()
        .filter(|a| a.account_type == "cashu")
        .filter_map(|a| a.mint_url.as_deref());
    mint_in_accounts(mint_url, cashu_mint_urls)
}

/// Tagged-union deserialize target for the `AgicashReceiveFlow` state
/// envelope (`{ "kind": "<variant>", … }`). Mirrors the Serialize-only
/// `agicash_wasm::types::ReceiveFlowStateWasm` 1:1 — local
/// `Deserialize`-only twin so the Rust/JS round-trip stays a JSON shape
/// across the `serde_wasm_bindgen` boundary (same idiom the existing
/// `AccountMintUrl` struct uses against `AccountWasm`). Tag is `kind`,
/// variants are `camelCase` per the wasm crate's `#[serde(...)]`
/// attribute on `ReceiveFlowStateWasm`.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum ReceiveFlowStateView {
    Idle,
    Parsing,
    NeedsMintConfirmation {
        confirmation: MintConfirmationData,
    },
    AddingMint {
        #[allow(dead_code)]
        mint_url: String,
    },
    Swapping {
        #[allow(dead_code)]
        account_id: String,
        #[allow(dead_code)]
        mint_url: String,
    },
    Done {
        result: ReceiveFlowResultView,
    },
    AlreadyClaimed {
        info: AlreadyClaimedInfoView,
    },
    Failed {
        reason: String,
        #[allow(dead_code)]
        code: String,
    },
}

/// `Done` payload mirror — `agicash_wasm::types::ReceiveFlowResultWasm`.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct ReceiveFlowResultView {
    #[allow(dead_code)]
    status: String,
    amount: String,
    #[allow(dead_code)]
    fee: String,
    unit: String,
    #[allow(dead_code)]
    currency: String,
    #[allow(dead_code)]
    account_id: String,
    mint_url: String,
    #[allow(dead_code)]
    token_hash: String,
}

/// `AlreadyClaimed` payload mirror —
/// `agicash_wasm::types::AlreadyClaimedInfoWasm`. The info deliberately
/// omits amount/fee (re-rendering "0 sats" would be misleading); the
/// success card renders an empty amount block in that case.
#[cfg(target_arch = "wasm32")]
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
struct AlreadyClaimedInfoView {
    unit: String,
    #[allow(dead_code)]
    currency: String,
    #[allow(dead_code)]
    account_id: String,
    mint_url: String,
    #[allow(dead_code)]
    token_hash: String,
}

/// Decode a `ReceiveFlow` state envelope `JsValue` into our local
/// `Phase` and patch the `phase` signal. Mirrors iOS `render(state:)`
/// (`8b630a54`). Drops the active-flow handle on terminal states so
/// the Rust side can free the inner service.
#[cfg(target_arch = "wasm32")]
fn render_flow_state(
    js: wasm_bindgen::JsValue,
    phase: RwSignal<Phase>,
    active_flow: StoredValue<
        Option<std::rc::Rc<agicash_wasm::AgicashReceiveFlow>>,
        leptos::prelude::LocalStorage,
    >,
) {
    let state: ReceiveFlowStateView = match serde_wasm_bindgen::from_value(js) {
        Ok(s) => s,
        Err(e) => {
            active_flow.set_value(None);
            phase.set(Phase::Error(format!("decode receive flow state: {e}")));
            return;
        }
    };
    match state {
        ReceiveFlowStateView::Idle => {
            // Shouldn't normally surface here (flow starts in Idle but
            // the very next event drives it forward). Treat as a
            // soft-reset.
            active_flow.set_value(None);
            phase.set(Phase::Entry);
        }
        ReceiveFlowStateView::Parsing => {
            // Transient — leave the Working card up.
        }
        ReceiveFlowStateView::NeedsMintConfirmation { confirmation } => {
            phase.set(Phase::ConfirmingMint(confirmation));
        }
        ReceiveFlowStateView::AddingMint { .. } => phase.set(Phase::AddingMint),
        ReceiveFlowStateView::Swapping { .. } => phase.set(Phase::Swapping),
        ReceiveFlowStateView::Done { result } => {
            active_flow.set_value(None);
            phase.set(Phase::Success(ReceiveResult {
                amount: result.amount,
                unit: result.unit,
                mint_url: result.mint_url,
            }));
        }
        ReceiveFlowStateView::AlreadyClaimed { info } => {
            active_flow.set_value(None);
            phase.set(Phase::Success(ReceiveResult {
                amount: String::new(),
                unit: info.unit,
                mint_url: info.mint_url,
            }));
        }
        ReceiveFlowStateView::Failed { reason, .. } => {
            active_flow.set_value(None);
            phase.set(Phase::Error(reason));
        }
    }
}

/// Spawn the real account-list lookup for `preview`'s mint and patch
/// the live phase's `mint_known` once it resolves. The write is guarded
/// on the phase still showing this exact token (Preview/Working with
/// the same `raw`) so a late answer can't clobber a phase the user
/// already moved away from. Native (`rlib` unit-test) builds have no
/// browser wallet, so this is a no-op there (`config` is consumed to
/// keep the handler's capture honest).
fn resolve_mint_known(
    config: StoredValue<AppConfig>,
    phase: RwSignal<Phase>,
    preview: TokenPreview,
) {
    #[cfg(target_arch = "wasm32")]
    {
        let config = config.get_value();
        spawn_local(async move {
            let known = fetch_mint_known(&config, &preview.mint_url).await;
            // Default is already `true`; only a confirmed-unknown mint
            // needs a phase patch (and only if we're still on it).
            if known {
                return;
            }
            phase.update(|p| match p {
                Phase::Preview(cur) | Phase::Working(cur) if cur.raw == preview.raw => {
                    cur.mint_known = false;
                }
                _ => {}
            });
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = (config, phase, preview);
    }
}

/// Format an integer amount with `_` thousand separators so a 100,000
/// sat token doesn't read as a wall of digits.
fn format_amount(amount: u64) -> String {
    let digits: Vec<char> = amount.to_string().chars().collect();
    let mut out = String::new();
    for (i, c) in digits.iter().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

// ---- Shared card / button styles ------------------------------------------
// Lifted from `components/login_view.rs`. When L3 lands its reusable
// Card/Button primitives (`feat/leptos-components` branch) replace these
// helpers with the L3 components.
//
// TODO: replace with L3 <Card> / <Button> when the L3 branch lands.

fn card_style() -> String {
    format!(
        "background:{}; color:{}; border:1px solid {}; \
         border-radius:{}; padding:{}; box-shadow:{}; \
         width:100%; max-width:{}; display:flex; \
         flex-direction:column; gap:{};",
        tokens::COLOR_CARD,
        tokens::COLOR_CARD_FOREGROUND,
        tokens::COLOR_BORDER,
        tokens::RADIUS_LG,
        tokens::SPACE_XXL,
        tokens::SHADOW_XS,
        tokens::CARD_MAX_WIDTH,
        tokens::SPACE_L,
    )
}

#[derive(Clone, Copy)]
enum ButtonVariant {
    Primary,
    Ghost,
}

fn button_style(variant: ButtonVariant) -> String {
    let (bg, fg, border) = match variant {
        ButtonVariant::Primary => (
            tokens::COLOR_PRIMARY,
            tokens::COLOR_PRIMARY_FOREGROUND,
            tokens::COLOR_PRIMARY,
        ),
        ButtonVariant::Ghost => (
            "transparent",
            tokens::COLOR_CARD_FOREGROUND,
            tokens::COLOR_BORDER,
        ),
    };
    format!(
        "display:inline-flex; align-items:center; justify-content:center; \
         height:40px; padding:0 {pad}; border-radius:{radius}; \
         font-size:{text}; font-weight:500; font-family:inherit; \
         background:{bg}; color:{fg}; border:1px solid {border}; \
         cursor:pointer; transition:opacity 150ms ease;",
        pad = tokens::SPACE_L,
        radius = tokens::RADIUS_MD,
        text = tokens::TEXT_SM,
    )
}

// ---- Tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{mint_in_accounts, parse_token};

    /// V3 token (cashuA) fixture copied from `cashu-0.15.1` upstream
    /// tests. 2+8 = 10 sat at <https://8333.space:3338>, memo "Thank you
    /// very much.". Verifies the pure-parse path doesn't need any
    /// network calls — exactly what makes the wasm preview viable.
    const V3_TOKEN_FIXTURE: &str = "cashuAeyJ0b2tlbiI6W3sibWludCI6Imh0dHBzOi8vODMzMy5zcGFjZTozMzM4IiwicHJvb2ZzIjpbeyJhbW91bnQiOjIsImlkIjoiMDA5YTFmMjkzMjUzZTQxZSIsInNlY3JldCI6IjQwNzkxNWJjMjEyYmU2MWE3N2UzZTZkMmFlYjRjNzI3OTgwYmRhNTFjZDA2YTZhZmMyOWUyODYxNzY4YTc4MzciLCJDIjoiMDJiYzkwOTc5OTdkODFhZmIyY2M3MzQ2YjVlNDM0NWE5MzQ2YmQyYTUwNmViNzk1ODU5OGE3MmYwY2Y4NTE2M2VhIn0seyJhbW91bnQiOjgsImlkIjoiMDA5YTFmMjkzMjUzZTQxZSIsInNlY3JldCI6ImZlMTUxMDkzMTRlNjFkNzc1NmIwZjhlZTBmMjNhNjI0YWNhYTNmNGUwNDJmNjE0MzNjNzI4YzcwNTdiOTMxYmUiLCJDIjoiMDI5ZThlNTA1MGI4OTBhN2Q2YzA5NjhkYjE2YmMxZDVkNWZhMDQwZWExZGUyODRmNmVjNjlkNjEyOTlmNjcxMDU5In1dfV0sInVuaXQiOiJzYXQiLCJtZW1vIjoiVGhhbmsgeW91IHZlcnkgbXVjaC4ifQ==";

    #[test]
    fn parses_v3_token_fixture() {
        let preview = parse_token(V3_TOKEN_FIXTURE).expect("token parses");
        assert_eq!(preview.amount, 10, "2+8 sat fixture");
        assert_eq!(preview.unit, "sats");
        assert_eq!(preview.mint_url, "https://8333.space:3338");
        assert_eq!(preview.memo.as_deref(), Some("Thank you very much."));
        // The synchronous parse no longer gates on a static allowlist;
        // it optimistically defaults `mint_known = true` (CTA hidden)
        // and the real async `list_accounts()` lookup corrects it.
        assert!(preview.mint_known);
    }

    #[test]
    fn rejects_garbage_string() {
        let err = parse_token("not-a-token").expect_err("garbage rejected");
        // Post-extractor: garbage has no `cashu[AB]…` substring at
        // all, so the new "No Cashu token found" message surfaces
        // before we ever reach cdk's "Invalid token" wording.
        assert!(
            err.contains("No Cashu token")
                || err.contains("Invalid token")
                || err.contains("decode"),
            "unexpected message: {err}",
        );
    }

    #[test]
    fn extracts_token_from_url() {
        // The whole point of routing through extract_cashu_token: the
        // user pastes a redeem URL and we still get the preview.
        let url = format!("https://wallet.example/redeem?token={V3_TOKEN_FIXTURE}");
        let preview = parse_token(&url).expect("URL-wrapped token parses");
        assert_eq!(preview.amount, 10);
        assert_eq!(preview.unit, "sats");
        // `raw` should carry the extracted token, NOT the full URL —
        // downstream `WalletClient::receive_token` is strict.
        assert_eq!(preview.raw, V3_TOKEN_FIXTURE);
    }

    #[test]
    fn rejects_empty_string() {
        let err = parse_token("").expect_err("empty rejected");
        assert!(!err.is_empty());
    }

    #[test]
    fn mint_in_accounts_matches_with_trailing_slash() {
        // The pasted token URL and the stored account URL may differ
        // only by a trailing slash; the comparison must normalize both
        // sides so they still match.
        assert!(mint_in_accounts(
            "https://m.example/",
            ["https://m.example"],
        ));
        assert!(mint_in_accounts(
            "https://m.example",
            ["https://m.example/"],
        ));
    }

    #[test]
    fn mint_in_accounts_false_when_absent() {
        assert!(!mint_in_accounts(
            "https://example.com",
            ["https://other.example", "https://third.example"],
        ));
        // Empty account list (no Cashu accounts yet) ⇒ not known.
        assert!(!mint_in_accounts("https://example.com", []));
    }
}
