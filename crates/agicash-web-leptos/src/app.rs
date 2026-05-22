//! Root `<App/>` component.
//!
//! Pure CSR (no SSR shell): the surrounding `<html>...<body>` envelope
//! lives in `crates/agicash-web-leptos/index.html`, which loads the
//! wasm bundle and invokes `hydrate()` (see `lib.rs`). The previous
//! `shell()` function and `LeptosOptions` plumbing were removed when
//! the axum SSR pipeline was ripped on 2026-05-17.

use leptos::prelude::*;
use leptos_meta::{provide_meta_context, Stylesheet, Title};
use leptos_router::{
    components::{ParentRoute, Route, Router, Routes},
    path, StaticSegment,
};

use crate::components::{ProtectedLayout, ToastProvider, WalletData};
use crate::config::AppConfig;
use crate::pages::{
    AccountsAddPage, AccountsIndexPage, HomePage, LoginPage, ReceiveCashuPage, ReceivePage,
    SendPage, SettingsAppearancePage, SettingsContactsPage, SettingsIndexPage, SettingsProfilePage,
};

/// Auth signal stored in the Leptos context. `Some(access_token)` means
/// "logged in"; `None` redirects to `/login`. The access token stays in
/// memory only — the refresh token persists to `window.localStorage`
/// via `BrowserSessionStorage` so a page reload can rehydrate the
/// session (matches the legacy React app's convention).
#[derive(Clone, Debug)]
pub struct AccessToken(pub RwSignal<Option<String>>);

/// "Has the on-startup session-rehydration attempt finished?" signal.
///
/// On app start a persisted refresh token (if any) is exchanged for a
/// fresh access token *before* the protected-route guard is allowed to
/// decide anything. This signal starts `true` (an attempt is in flight)
/// and flips to `false` exactly once, when the attempt resolves —
/// success (token set) or genuine no/expired token (stays `None`).
///
/// `ProtectedLayout` gates its redirect on this: it must not bounce to
/// `/login` while rehydration is still running, otherwise it races the
/// async refresh and logs the user out on every reload despite a valid
/// refresh token sitting in `localStorage` (the bug this fixes).
#[derive(Clone, Copy, Debug)]
pub struct SessionRehydrating(pub RwSignal<bool>);

/// Root reactive component. Provides the `AccessToken` context, the
/// `<Title/>` + `<Stylesheet/>` from `leptos_meta`, and the router with
/// the route tree:
///
/// ```text
/// /login                        (public)
/// / (ProtectedLayout)           (auth-gated, renders <Outlet/> + BottomNav)
///   ├── ""                      Home
///   ├── receive                 Receive
///   ├── receive/cashu           Paste-Cashu-token receive flow (lane L4)
///   ├── send                    Send
///   ├── accounts                Accounts list
///   │     └── add               Add mint
///   └── settings                Settings index
///         ├── profile           Profile
///         ├── appearance        Appearance
///         └── contacts          Contacts
/// ```
///
/// The protected group uses `ParentRoute` so the `BottomNav` stays
/// mounted across navigations (no flash, no scroll-position loss).
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();

    // Endpoint config (opensecret + supabase URLs / keys) loaded from
    // `<meta>` tags on hydrate. See `config.rs` for the full rationale.
    // Provided as a single context so LoginView + WalletData::refresh
    // (and future consumers) can read the same values.
    provide_context(AppConfig::load());

    // Empty on first paint — the LoginView reads + writes this; protected
    // routes redirect to /login when it's None.
    let access_token = AccessToken(RwSignal::new(None));
    provide_context(access_token.clone());

    // Session rehydration is in flight until the startup attempt below
    // resolves. `ProtectedLayout` won't redirect while this is `true`,
    // so a valid persisted refresh token gets a chance to restore the
    // session before the auth guard runs (otherwise every reload bounces
    // to /login despite the token in localStorage).
    let rehydrating = SessionRehydrating(RwSignal::new(true));
    provide_context(rehydrating);

    // Kick off the on-startup session restore. We are inside the `App`
    // component body — a valid Leptos reactive owner — so capturing the
    // `AppConfig` from context here is sound; the detached future below
    // only ever touches the captured value + the (Copy) signals, never
    // `use_context` off-owner (the documented spawn_local/use_context
    // gotcha). `wasm_bindgen_futures::spawn_local` is used (not
    // `leptos::task::spawn_local`) because this can run before the
    // leptos Executor handshake completes, same as `WalletData::refresh`.
    #[cfg(target_arch = "wasm32")]
    {
        let app_config = use_context::<AppConfig>();
        let token_signal = access_token.0;
        let rehydrating_signal = rehydrating.0;
        wasm_bindgen_futures::spawn_local(async move {
            rehydrate_session(app_config, token_signal).await;
            // Whatever happened (restored, no token, expired token, or
            // network failure) the attempt is over — release the guard
            // so ProtectedLayout can make its decision.
            rehydrating_signal.set(false);
        });
    }
    // Native rlib build (unit tests): no browser, nothing to rehydrate.
    #[cfg(not(target_arch = "wasm32"))]
    rehydrating.0.set(false);

    // Shared wallet view-model. Idle on first paint; the home page (and
    // any other consumer) calls `.start(config)` from an Effect once it
    // mounts — that single entry point builds the session-seeded
    // `Arc<WalletClient>`, runs the foreground cache populate, and
    // spawns the realtime apply + cache dispatch pumps. See
    // `components/wallet_context.rs`.
    provide_context(WalletData::new());

    view! {
        <Stylesheet id="leptos" href="/style/main.css"/>
        <Title text="Agicash"/>

        // Toast queue wraps the whole router so any descendant can call
        // `use_toast()` without each route having to provide its own
        // context. Used today by `SendCashuView`'s copy-to-clipboard
        // feedback (lane L5); future receive / settings flows can lean
        // on the same handle.
        <ToastProvider>
            <Router>
                <main>
                    <Routes fallback=|| "Not found.">
                        <Route path=StaticSegment("/login") view=LoginPage/>

                        // Protected group. The empty-path ParentRoute matches
                        // every URL that didn't match `/login` above; the inner
                        // index child (also empty path) renders Home, siblings
                        // handle the named tabs + their nested sub-routes.
                        <ParentRoute path=StaticSegment("") view=ProtectedLayout>
                            <Route path=StaticSegment("") view=HomePage/>
                            <Route path=StaticSegment("receive") view=ReceivePage/>
                            // Paste-Cashu-token receive flow (lane L4).
                            // `path!` expands to a tuple of `StaticSegment`s
                            // for multi-segment static paths.
                            <Route path=path!("/receive/cashu") view=ReceiveCashuPage/>
                            <Route path=StaticSegment("send") view=SendPage/>
                            <Route path=StaticSegment("accounts") view=AccountsIndexPage/>
                            <Route path=(StaticSegment("accounts"), StaticSegment("add"))
                                   view=AccountsAddPage/>
                            <Route path=StaticSegment("settings") view=SettingsIndexPage/>
                            <Route path=(StaticSegment("settings"), StaticSegment("profile"))
                                   view=SettingsProfilePage/>
                            <Route path=(StaticSegment("settings"), StaticSegment("appearance"))
                                   view=SettingsAppearancePage/>
                            <Route path=(StaticSegment("settings"), StaticSegment("contacts"))
                                   view=SettingsContactsPage/>
                        </ParentRoute>
                    </Routes>
                </main>
            </Router>
        </ToastProvider>
    }
}

/// On-startup session restore. Reads the persisted refresh token from
/// `BrowserSessionStorage` (`window.localStorage`) and exchanges it for
/// a fresh access token, then seeds the in-memory `AccessToken` signal —
/// the half of the persisted-session contract the doc comments on
/// `AccessToken` / `ProtectedLayout` assert but which was never wired.
///
/// Mirrors how `LoginView` builds its `OpenSecretClient` (from
/// `AppConfig`) and what it does with the result (`token.set(Some(at))`),
/// but starts from a stored refresh token instead of a fresh login:
///
/// 1. `BrowserSessionStorage::load()` → no session ⇒ nothing to do
///    (genuine logged-out: `ProtectedLayout` will correctly send the
///    user to `/login` once the rehydration gate is released).
/// 2. Build an `OpenSecretClient` from the same `AppConfig` the login
///    flow uses, seed it with the persisted refresh token via
///    `inner().set_tokens` (empty access token — `refresh()` only reads
///    the refresh token).
/// 3. `agicash_auth_opensecret::refresh(&client)` performs the
///    attestation handshake then the `/refresh` exchange, storing the
///    new access + refresh tokens in the client's session manager.
/// 4. Read the fresh access token back out and set the signal. On any
///    failure (expired/revoked refresh token, network) we leave the
///    signal `None`; the gate still releases and `ProtectedLayout`
///    redirects to `/login`, which is the correct behaviour for a
///    genuinely-expired session.
///
/// `config` is `Option` so the signature is uniform; a missing config on
/// wasm just means we can't build a client → treated as "no session".
#[cfg(target_arch = "wasm32")]
async fn rehydrate_session(config: Option<AppConfig>, token: RwSignal<Option<String>>) {
    use agicash_auth_opensecret::{
        refresh, BrowserSessionStorage, OpenSecretClient, OpenSecretConfig,
    };
    use agicash_traits::SessionStorage;

    let Some(config) = config else {
        leptos::logging::log!("session rehydrate skipped: AppConfig context missing");
        return;
    };

    // 1. Persisted session?
    let session = match BrowserSessionStorage::new().load().await {
        Ok(Some(s)) => s,
        Ok(None) => return, // No stored session — genuine logged-out.
        Err(e) => {
            leptos::logging::log!("session rehydrate: storage load failed: {e}");
            return;
        }
    };

    // 2. Build the client and seed the refresh token.
    let client = match OpenSecretClient::new(OpenSecretConfig {
        base_url: config.opensecret_base_url.clone(),
        client_id: config.opensecret_client_id,
    }) {
        Ok(c) => c,
        Err(e) => {
            leptos::logging::log!("session rehydrate: build client failed: {e}");
            return;
        }
    };
    if let Err(e) = client
        .inner()
        .set_tokens(String::new(), Some(session.refresh_token))
    {
        leptos::logging::log!("session rehydrate: seed refresh token failed: {e}");
        return;
    }

    // 3. Exchange the refresh token for a fresh access token.
    if let Err(e) = refresh(&client).await {
        // Expired / revoked refresh token, or network — leave the
        // signal None so ProtectedLayout sends the user to /login.
        leptos::logging::log!("session rehydrate: refresh failed (re-login required): {e}");
        return;
    }

    // 4. Restore the in-memory access token, exactly as LoginView does
    //    on a fresh login (`token.set(Some(...))`).
    match client.inner().get_access_token() {
        Ok(Some(at)) => token.set(Some(at)),
        Ok(None) => {
            leptos::logging::log!("session rehydrate: refresh ok but no access token present");
        }
        Err(e) => {
            leptos::logging::log!("session rehydrate: read access token failed: {e}");
        }
    }
}
