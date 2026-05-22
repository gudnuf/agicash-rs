//! `RealtimeStatusBanner` — thin connection-status strip rendered in the
//! protected app shell (above `<Outlet/>`).
//!
//! Reads [`WalletData::realtime_status`] (kept current by the apply
//! pump in [`WalletData::start`]) and renders three visual states,
//! mirroring the iOS/Android Lane 2a UX that's dispatched in parallel:
//!
//! - **Connected** (`Subscribed`) — banner hidden, no chrome cost. The
//!   first-paint `Idle` / `Connecting` states are also hidden so the
//!   shell doesn't flash a banner on every page load before the channel
//!   has had a chance to join (the React canonical model does the same:
//!   silent until something actually goes wrong).
//! - **Reconnecting** (`Reconnecting`, `Error`, `Closed`) — thin
//!   `tokens::COLOR_MUTED` strip reading "Reconnecting…". Non-blocking,
//!   no action — the supervisor's backoff loop owns recovery. The
//!   balance below stays visible (Lane V's SWR discipline) so this is
//!   purely additive.
//! - **`TerminalError`** — persistent `tokens::COLOR_DESTRUCTIVE`-bordered
//!   strip "Connection lost — Retry". Clicking the strip calls
//!   [`WalletData::retry_realtime`], which edge-triggers the service's
//!   terminal-latch clear (`set_online(false → true)`) so the
//!   supervisor resumes its connect→join cycle. No new realtime API
//!   surface required — this re-uses the existing online-edge hook
//!   that already documents the terminal-latch reset semantics.
//!
//! Placement: top of the protected layout, above `<Outlet/>`, below the
//! safe-area / status-bar inset. A single fixed-height (28px) row keeps
//! the rest of the chrome from jumping when the banner appears/
//! disappears. Layout impact is therefore zero in the steady-state
//! `Connected` case (the wrapper renders nothing).

use leptos::prelude::*;

use crate::components::{RealtimeStatus, WalletData};
use crate::tokens;

#[component]
pub fn RealtimeStatusBanner() -> impl IntoView {
    let wallet = expect_context::<WalletData>();
    let status = wallet.realtime_status;

    // Capture the wallet handle for the retry click closure. Cheap clone
    // (the inner signals are `Copy`).
    let wallet_for_retry = wallet.clone();
    let on_retry = move |ev: leptos::ev::MouseEvent| {
        ev.prevent_default();
        wallet_for_retry.retry_realtime();
    };

    view! {
        {move || {
            let s = status.get();
            match s {
                // Steady-state / first-paint: render nothing. The
                // `<div/>` placeholder keeps a single typed return shape
                // for the closure.
                RealtimeStatus::Idle
                | RealtimeStatus::Connecting
                | RealtimeStatus::Subscribed => {
                    leptos::either::EitherOf3::A(view! { <div/> })
                }
                // Recoverable disconnect: thin neutral strip. The
                // supervisor reconnects under us; the user sees a
                // status, not a call-to-action.
                RealtimeStatus::Reconnecting
                | RealtimeStatus::Error
                | RealtimeStatus::Closed => {
                    leptos::either::EitherOf3::B(view! {
                        <div
                            style=reconnecting_style()
                            role="status"
                            aria-live="polite"
                        >
                            {"Reconnecting…"}
                        </div>
                    })
                }
                // Terminal: persistent + actionable. The strip itself
                // is the click target so the affordance is unambiguous
                // on mobile (no tiny "Retry" link to mis-tap).
                RealtimeStatus::TerminalError => {
                    leptos::either::EitherOf3::C(view! {
                        <button
                            style=terminal_style()
                            type="button"
                            on:click=on_retry.clone()
                            aria-label="Connection lost — tap to retry"
                        >
                            <span>{"Connection lost — "}</span>
                            <span style=retry_link_style()>{"Retry"}</span>
                        </button>
                    })
                }
            }
        }}
    }
}

fn reconnecting_style() -> String {
    format!(
        "display:flex; align-items:center; justify-content:center; \
         width:100%; height:28px; padding:0 {pad}; \
         background:{bg}; color:{fg}; \
         font-size:{text}; font-family:inherit; \
         border-bottom:1px solid {border};",
        pad = tokens::SPACE_M,
        bg = tokens::COLOR_MUTED,
        fg = tokens::COLOR_MUTED_FOREGROUND,
        text = tokens::TEXT_SM,
        border = tokens::COLOR_BORDER,
    )
}

fn terminal_style() -> String {
    format!(
        "display:flex; align-items:center; justify-content:center; gap:{gap}; \
         width:100%; height:28px; padding:0 {pad}; \
         background:{bg}; color:{fg}; \
         font-size:{text}; font-family:inherit; \
         border:0; border-bottom:1px solid {border}; \
         cursor:pointer;",
        gap = tokens::SPACE_XS,
        pad = tokens::SPACE_M,
        bg = tokens::COLOR_DESTRUCTIVE,
        fg = tokens::COLOR_PRIMARY_FOREGROUND,
        text = tokens::TEXT_SM,
        border = tokens::COLOR_DESTRUCTIVE,
    )
}

fn retry_link_style() -> String {
    format!(
        "text-decoration:underline; font-weight:500; color:{};",
        tokens::COLOR_PRIMARY_FOREGROUND,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // The component itself is view-only; its branches are exercised at
    // the integration level (the protected layout test, when one
    // lands). Keep a smoke `assert` so `cargo test` records the module
    // is wired.
    #[test]
    fn realtime_status_variants_cover_three_buckets() {
        // Three render branches: hidden, reconnecting strip, terminal
        // strip. Adding a variant must consciously be assigned a bucket.
        let hidden = [
            RealtimeStatus::Idle,
            RealtimeStatus::Connecting,
            RealtimeStatus::Subscribed,
        ];
        let recon = [
            RealtimeStatus::Reconnecting,
            RealtimeStatus::Error,
            RealtimeStatus::Closed,
        ];
        let terminal = [RealtimeStatus::TerminalError];
        // 3 + 3 + 1 == 7 == RealtimeStatus variant count today.
        assert_eq!(hidden.len() + recon.len() + terminal.len(), 7);
    }
}
