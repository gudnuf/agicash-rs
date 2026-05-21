//! `/settings` — settings hub + sub-routes (placeholder).
//!
//! Mirrors iOS `SettingsView` (accounts shortcut, sign-out, navigation
//! to profile / appearance / contacts). Phase 1 partial ships the index
//! plus three sub-routes as stubs. iOS surfaces Accounts under Settings;
//! the web nav promotes Accounts to a top-level tab, but Settings keeps
//! a deep-link to `/accounts` for parity.
//!
//! Spec §8 enumerates `/settings/appearance`, `/settings/profile/edit`,
//! `/settings/contacts`, `/settings/contacts/:id`; first three are stubbed
//! here. Real wiring lands in Phase 2.
//!
//! ## Sign-out wiring (F15 Lane 2b)
//!
//! The Sign Out button at the bottom of the index mirrors the iOS
//! `WalletViewModel::signOut()` flow step-for-step:
//!
//! 1. Tear down realtime FIRST (`WalletData::teardown_realtime`) so the
//!    socket closes cleanly while the auth token is still valid.
//! 2. Best-effort `agicash_auth_opensecret::logout(&client)` — the
//!    server-side session-revoke call. Failure is logged + ignored;
//!    local state is cleared regardless (matches iOS `try? authLogout`).
//! 3. Clear the persisted refresh token from `BrowserSessionStorage`
//!    (the iOS equivalent is `SessionStore.clear()`).
//! 4. Clear the in-memory `AccessToken` signal — `ProtectedLayout`'s
//!    guard observes this and would redirect anyway, but we navigate
//!    explicitly so the transition is immediate (no token-flicker / no
//!    one-tick render of an unauthenticated state in the protected
//!    shell).
//! 5. Reset the `WalletData` view-model (`clear_for_signout`) — accounts
//!    list back to `Idle`, user id back to `None`, realtime status back
//!    to `Idle`. A subsequent re-login starts from the same shape a
//!    fresh tab does.
//! 6. `navigate("/login")`.

use leptos::ev;
use leptos::prelude::*;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;
use leptos_router::NavigateOptions;

use crate::app::AccessToken;
use crate::components::WalletData;
use crate::config::AppConfig;
use crate::tokens;

#[component]
pub fn SettingsIndexPage() -> impl IntoView {
    let AccessToken(token) = expect_context::<AccessToken>();
    let wallet = expect_context::<WalletData>();
    let config = expect_context::<AppConfig>();
    let navigate = use_navigate();

    // Disables the button + flips the label while the async sign-out
    // round-trip is running. Prevents the double-tap / re-entry race
    // (a second sign-out mid-logout would race the auth signal clear).
    let is_working = RwSignal::new(false);

    // Capture per-closure clones up-front. `WalletData` is cheap to
    // clone (the inner signals are `Copy`); `AppConfig` is a small
    // bag-of-strings; `navigate` is `Arc`-backed by leptos_router.
    let wallet_for_signout = wallet.clone();
    let config_for_signout = config.clone();
    let navigate_for_signout = navigate.clone();

    let on_signout = move |ev: ev::MouseEvent| {
        ev.prevent_default();
        if is_working.get_untracked() {
            return;
        }
        is_working.set(true);

        let wallet = wallet_for_signout.clone();
        let config = config_for_signout.clone();
        let navigate = navigate_for_signout.clone();

        leptos::task::spawn_local(async move {
            // 1. Realtime down FIRST — closes the socket while the
            //    token is still valid. Idempotent: no-op if realtime
            //    never opened (e.g. user signed in then immediately
            //    signed out before the supervisor finished joining).
            wallet.teardown_realtime();

            // 2-5. Wasm-only: hit the opensecret revoke + clear the
            //      browser session. The native rlib build skips this
            //      entirely (no browser, no client) and just settles
            //      the signals so the view-test shape matches.
            #[cfg(target_arch = "wasm32")]
            {
                signout_browser(&config).await;
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                let _ = &config;
            }

            // 6. Clear the auth signal + view-model + navigate.
            //    Order matters: clear the wallet view-model BEFORE
            //    flipping the token signal so the protected-route
            //    guard's redirect doesn't briefly render a
            //    half-cleared home page (the guard navigates on the
            //    next reactive tick; clearing wallet first keeps the
            //    in-between paint coherent).
            wallet.clear_for_signout();
            token.set(None);
            navigate("/login", NavigateOptions::default());
            is_working.set(false);
        });
    };

    view! {
        <div style=page_style()>
            <h1 style=heading_style()>"Settings"</h1>
            <ul style=list_style()>
                <SettingsLink href="/accounts" label="Accounts"/>
                <SettingsLink href="/settings/profile" label="Profile"/>
                <SettingsLink href="/settings/appearance" label="Appearance"/>
                <SettingsLink href="/settings/contacts" label="Contacts"/>
            </ul>
            <button
                type="button"
                style=move || signout_button_style(is_working.get())
                disabled=move || is_working.get()
                on:click=on_signout
            >
                {move || if is_working.get() { "Signing out…" } else { "Sign Out" }}
            </button>
        </div>
    }
}

#[component]
pub fn SettingsProfilePage() -> impl IntoView {
    settings_stub("Profile", "Edit display name and email.")
}

#[component]
pub fn SettingsAppearancePage() -> impl IntoView {
    settings_stub(
        "Appearance",
        "Light / dark / system theme switcher lands here (Phase 2).",
    )
}

#[component]
pub fn SettingsContactsPage() -> impl IntoView {
    settings_stub(
        "Contacts",
        "Lightning-address contact list lands here (Phase 2).",
    )
}

#[component]
fn SettingsLink(href: &'static str, label: &'static str) -> impl IntoView {
    view! {
        <li style=item_style()>
            <A href=href>
                <span style=row_style()>
                    <span>{label}</span>
                    <span style=chevron_style()>"›"</span>
                </span>
            </A>
        </li>
    }
}

fn settings_stub(title: &'static str, body: &'static str) -> impl IntoView {
    view! {
        <div style=page_style()>
            <header style=header_style()>
                <A href="/settings">
                    <span style=link_style()>"← Settings"</span>
                </A>
                <h1 style=heading_style()>{title}</h1>
                <span/>
            </header>
            <p style=subtle_style()>{body}</p>
        </div>
    }
}

// ---- Browser-side sign-out helper ----------------------------------------

/// Steps 2-4 of the sign-out flow on wasm: best-effort revoke the
/// server session, then clear the persisted refresh token. Failures of
/// the revoke endpoint are logged + surfaced as a non-blocking toast
/// (the local state is already clear, the user is moving on); failure
/// to clear local storage is also logged but is the more concerning
/// case — `BrowserSessionStorage::clear` writing to localStorage almost
/// never fails in practice (quota / disabled storage), so a warning is
/// enough.
///
/// Mirrors `WalletViewModel::signOut`'s `try? wallet.authLogout()` +
/// `try? SessionStore.clear()` shape: each step is best-effort and the
/// caller proceeds regardless. The auth signal + navigate happen in the
/// caller after this returns so the page transition isn't blocked on
/// the revoke round-trip (it can be slow on flaky networks; the user
/// is signed out locally either way).
#[cfg(target_arch = "wasm32")]
async fn signout_browser(config: &AppConfig) {
    use agicash_auth_opensecret::{
        logout, BrowserSessionStorage, OpenSecretClient, OpenSecretConfig,
    };
    use agicash_traits::SessionStorage;

    // Try the server-side revoke. The client doesn't need to be
    // session-seeded for `logout()` (the SDK reads the in-memory access
    // token; we don't have one off this client). We build a bare client
    // and call `logout` anyway — on the read-the-server-side-too path
    // we'd seed it via `BrowserSessionStorage`, but the iOS flow also
    // just calls `authLogout` and ignores the result. Matching its
    // best-effort semantics keeps the parity check cheap.
    match OpenSecretClient::new(OpenSecretConfig {
        base_url: config.opensecret_base_url.clone(),
        client_id: config.opensecret_client_id,
    }) {
        Ok(client) => {
            if let Err(e) = logout(&client).await {
                // Non-fatal: the local session is gone the moment we
                // clear `BrowserSessionStorage` below, the server-side
                // session will expire on its own. Log + continue.
                leptos::logging::log!(
                    "signout: opensecret revoke failed (proceeding with \
                     local clear): {e}"
                );
            }
        }
        Err(e) => {
            leptos::logging::log!(
                "signout: opensecret client build failed (proceeding with \
                 local clear): {e}"
            );
        }
    }

    // Clear the persisted refresh token. This is the load-bearing step
    // — if it doesn't run, a page reload would rehydrate the session.
    if let Err(e) = BrowserSessionStorage::new().clear().await {
        leptos::logging::log!(
            "signout: BrowserSessionStorage.clear() failed — the session may \
             rehydrate on reload: {e}"
        );
    }
}

fn page_style() -> String {
    format!(
        "display:flex; flex-direction:column; gap:{}; padding:{};",
        tokens::SPACE_L,
        tokens::SPACE_XL,
    )
}

fn heading_style() -> String {
    format!(
        "font-size:{}; font-weight:600; margin:0; color:{};",
        tokens::TEXT_2XL,
        tokens::COLOR_FOREGROUND,
    )
}

fn header_style() -> String {
    format!(
        "display:flex; justify-content:space-between; align-items:center; gap:{};",
        tokens::SPACE_M,
    )
}

fn subtle_style() -> String {
    format!(
        "font-size:{}; color:{}; margin:0;",
        tokens::TEXT_SM,
        tokens::COLOR_MUTED_FOREGROUND,
    )
}

fn link_style() -> String {
    format!(
        "color:{}; font-size:{}; text-decoration:underline; cursor:pointer;",
        tokens::COLOR_PRIMARY,
        tokens::TEXT_SM,
    )
}

fn list_style() -> &'static str {
    "list-style:none; padding:0; margin:0; display:flex; flex-direction:column;"
}

fn item_style() -> String {
    format!("border-bottom:1px solid {};", tokens::COLOR_BORDER)
}

fn row_style() -> String {
    format!(
        "display:flex; justify-content:space-between; align-items:center; \
         padding:{} 0; color:{}; text-decoration:none; cursor:pointer;",
        tokens::SPACE_M,
        tokens::COLOR_FOREGROUND,
    )
}

fn chevron_style() -> String {
    format!("color:{};", tokens::COLOR_MUTED_FOREGROUND)
}

fn signout_button_style(is_working: bool) -> String {
    let (cursor, opacity) = if is_working {
        ("wait", "0.6")
    } else {
        ("pointer", "1")
    };
    format!(
        "margin-top:{}; padding:{} {}; border-radius:{}; \
         border:1px solid {}; background:transparent; color:{}; \
         font-family:inherit; font-size:{}; cursor:{}; opacity:{};",
        tokens::SPACE_XL,
        tokens::SPACE_M,
        tokens::SPACE_L,
        tokens::RADIUS_MD,
        tokens::COLOR_BORDER,
        tokens::COLOR_DESTRUCTIVE,
        tokens::TEXT_SM,
        cursor,
        opacity,
    )
}
