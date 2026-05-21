//! `ProtectedLayout` — the app shell rendered inside every
//! `_protected/*` route (mirror of the React `_protected.tsx` parent).
//!
//! Two jobs:
//!   1. Auth guard. Mirrors `HomePage`'s `AccessToken` check — when the
//!      in-memory access-token signal is `None` we navigate to `/login`.
//!      The redirect runs in a client-only `Effect`, and is **gated on
//!      the `SessionRehydrating` signal**: while the on-startup
//!      refresh-token exchange (`app.rs::rehydrate_session`) is still in
//!      flight we do not redirect, so a valid persisted refresh token
//!      gets a chance to restore the session before the guard fires.
//!      Without this gate the Effect runs on the first tick (token still
//!      `None`) and bounces every reload to `/login` despite a valid
//!      token in `localStorage`.
//!   2. App chrome. Renders `<Outlet/>` for the child page and a fixed
//!      bottom nav (Home / Receive / Send / Accounts / Settings) — the
//!      iOS 2-tab pattern lifted into a 5-tab mobile-PWA nav, matching
//!      spec §8's route surface.
//!
//! This is a placeholder shell — real data wires in once Slice 12's
//! `WalletClient` lands.

use leptos::prelude::*;
use leptos_router::components::Outlet;
use leptos_router::hooks::use_navigate;
use leptos_router::NavigateOptions;

use crate::app::{AccessToken, SessionRehydrating};
use crate::components::{BottomNav, RealtimeStatusBanner};
use crate::tokens;

#[component]
pub fn ProtectedLayout() -> impl IntoView {
    let AccessToken(token) = expect_context::<AccessToken>();
    let SessionRehydrating(rehydrating) = expect_context::<SessionRehydrating>();
    let navigate = use_navigate();

    // Client-only redirect. `Effect::new` runs post-hydration; SSR's
    // first paint of the protected shell is harmless.
    //
    // The Effect tracks BOTH signals, so it re-runs when rehydration
    // finishes and when the token changes:
    //   - while `rehydrating` is true → do nothing (the startup
    //     refresh-token exchange may still set the token);
    //   - once rehydration is done AND there is still no token →
    //     redirect (genuine logged-out / expired-token case);
    //   - if rehydration set the token → `is_none()` is false, no
    //     redirect, the protected page renders.
    Effect::new(move |_| {
        if rehydrating.get() {
            return;
        }
        if token.get().is_none() {
            navigate("/login", NavigateOptions::default());
        }
    });

    let shell_style = format!(
        "display:flex; flex-direction:column; min-height:100dvh; \
         background:{}; color:{}; font-family:{};",
        tokens::COLOR_BACKGROUND,
        tokens::COLOR_FOREGROUND,
        tokens::FONT_PRIMARY,
    );

    // Content area pads the bottom by the nav height so fixed nav doesn't
    // occlude scrolled content. Nav height = 64px (8 + 40 button + 16).
    let content_style = "flex:1 1 auto; padding-bottom:64px; \
        display:flex; flex-direction:column;"
        .to_string();

    view! {
        <div style=shell_style>
            // Connection-status strip: hidden in the steady-state
            // `Subscribed` (and first-paint Idle/Connecting) case, so
            // it adds zero chrome cost when realtime is healthy. See
            // `RealtimeStatusBanner` for the visual states + retry
            // affordance.
            <RealtimeStatusBanner/>
            <div style=content_style>
                <Outlet/>
            </div>
            <BottomNav/>
        </div>
    }
}
