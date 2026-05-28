//! Runtime theme: body-class swap + cookie persistence + the user-facing
//! `ColorModeToggle` switcher.
//!
//! ## Mirror of the React `app/features/theme/*` feature
//!
//! The canonical React app (`MakePrisms/agicash`) models the theme as two
//! orthogonal axes:
//!
//!   - **track** — `theme: 'usd' | 'btc'` (currency colour variant). This is
//!     NOT a user-facing control; it follows the wallet's default currency
//!     (`app/features/wallet/wallet.tsx` calls `setTheme(currency)`). Default
//!     `btc`.
//!   - **color mode** — `colorMode: 'light' | 'dark' | 'system'`. THIS is the
//!     user-facing switcher (`color-mode-toggle.tsx`, rendered in the settings
//!     footer). `system` resolves against `prefers-color-scheme`. Default
//!     `system`.
//!
//! React applies `${theme} ${effectiveColorMode}` as classes on `<html>`
//! (`theme-provider.tsx::updateDocumentClasses`), e.g. `btc light`, `btc dark`,
//! `usd dark`. The Leptos PWA boots `<body class="btc">` (`index.html`) so we
//! swap the class list on `<body>` instead — the CSS cascade in
//! `style/tailwind.in.css` (`.btc` / `.usd` / `.dark` selectors, ported verbatim
//! from React) resolves identically.
//!
//! ## Cookies (exact React scheme — `theme.constants.ts`)
//!
//! Three cookies, one-year max-age, `path=/; samesite=lax`:
//!   - `theme` → `btc` | `usd` (default `btc`)
//!   - `color-mode` → `light` | `dark` | `system` (default `system`)
//!   - `system-color-mode` → `light` | `dark` (the last-observed OS preference)
//!
//! We mirror React's `saveCookies` / `getClientThemeCookies` byte-for-byte so a
//! cookie written by either client is read by the other.
//!
//! ## What this lane wires
//!
//! 1. [`ThemeState`] context: reactive `(track, color_mode, system_color_mode)`,
//!    derived `effective_color_mode`, and a body-class `Effect`.
//! 2. Cookie read on boot (defaults if absent) + write on every change.
//! 3. [`ColorModeToggle`] — the settings-footer switcher (light/dark/system),
//!    mirroring React's `ColorModeToggle` dropdown + lucide Sun/Moon/SunMoon
//!    icons.
//!
//! The track axis is persisted + applied here (so the body class is complete +
//! a future "follow default currency" wire-up has a `set_track` to call), but
//! no user control writes it yet — matching React, where only the color-mode
//! toggle is user-facing.

use leptos::prelude::*;

#[cfg(target_arch = "wasm32")]
use leptos::ev;

/// Currency colour track. Mirrors React `Theme = 'usd' | 'btc'`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Track {
    /// Bitcoin track — deep BTC-blue palette. React `defaultTheme = 'btc'`.
    #[default]
    Btc,
    /// USD track.
    Usd,
}

impl Track {
    /// Cookie value (matches React `Theme` strings).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Track::Btc => "btc",
            Track::Usd => "usd",
        }
    }

    // Used by the wasm cookie reader + the unit tests; dead on a native
    // non-test build.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn from_cookie(s: &str) -> Option<Self> {
        match s {
            "btc" => Some(Track::Btc),
            "usd" => Some(Track::Usd),
            _ => None,
        }
    }
}

/// User-selectable color mode. Mirrors React
/// `ColorMode = 'light' | 'dark' | 'system'` (`theme.constants.ts::colorModes`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ColorMode {
    /// Force light.
    Light,
    /// Force dark.
    Dark,
    /// Follow the OS `prefers-color-scheme`. React `defaultColorMode = 'system'`.
    #[default]
    System,
}

impl ColorMode {
    /// The three options, in React's `colorModes` order.
    pub const ALL: [ColorMode; 3] = [ColorMode::Light, ColorMode::Dark, ColorMode::System];

    /// Cookie value (matches React `ColorMode` strings).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            ColorMode::Light => "light",
            ColorMode::Dark => "dark",
            ColorMode::System => "system",
        }
    }

    /// Capitalised label rendered in the menu (React renders `capitalize` on
    /// the raw value, giving "Light" / "Dark" / "System").
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ColorMode::Light => "Light",
            ColorMode::Dark => "Dark",
            ColorMode::System => "System",
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn from_cookie(s: &str) -> Option<Self> {
        match s {
            "light" => Some(ColorMode::Light),
            "dark" => Some(ColorMode::Dark),
            "system" => Some(ColorMode::System),
            _ => None,
        }
    }
}

/// The resolved light/dark used for the `.dark` body class. `system` collapses
/// to the last-observed OS preference. Mirrors React `effectiveColorMode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SystemMode {
    /// Light — React `defaultSystemColorMode = 'light'`.
    #[default]
    Light,
    /// Dark.
    Dark,
}

impl SystemMode {
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn as_str(self) -> &'static str {
        match self {
            SystemMode::Light => "light",
            SystemMode::Dark => "dark",
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    fn from_cookie(s: &str) -> Option<Self> {
        match s {
            "light" => Some(SystemMode::Light),
            "dark" => Some(SystemMode::Dark),
            _ => None,
        }
    }
}

/// Pure resolver for the effective light/dark, factored out so it's testable
/// without a reactive runtime. Mirrors React
/// `colorMode === 'system' ? systemColorMode : colorMode`.
#[must_use]
fn resolve_effective(color_mode: ColorMode, system_mode: SystemMode) -> SystemMode {
    match color_mode {
        ColorMode::Light => SystemMode::Light,
        ColorMode::Dark => SystemMode::Dark,
        ColorMode::System => system_mode,
    }
}

// Cookie names — verbatim from React `theme.constants.ts`. Only the wasm
// build reads/writes cookies, so they'd be dead code on the native rlib
// (test) build; gate them to wasm to keep that build warning-clean.
#[cfg(target_arch = "wasm32")]
const THEME_COOKIE: &str = "theme";
#[cfg(target_arch = "wasm32")]
const COLOR_MODE_COOKIE: &str = "color-mode";
#[cfg(target_arch = "wasm32")]
const SYSTEM_COLOR_MODE_COOKIE: &str = "system-color-mode";

/// Reactive theme context, provided once at the app root and read by the
/// switcher. Cheap to clone (the inner signals are `Copy`).
#[derive(Clone, Copy, Debug)]
pub struct ThemeState {
    /// Currency colour track (`btc` / `usd`).
    pub track: RwSignal<Track>,
    /// User-selected color mode (`light` / `dark` / `system`).
    pub color_mode: RwSignal<ColorMode>,
    /// Last-observed OS preference; only consulted when `color_mode == System`.
    pub system_mode: RwSignal<SystemMode>,
}

impl Default for ThemeState {
    fn default() -> Self {
        Self::new()
    }
}

impl ThemeState {
    /// Build from cookies (or React's defaults when a cookie is absent).
    #[must_use]
    pub fn new() -> Self {
        let (track, color_mode, system_mode) = read_cookies();
        Self {
            track: RwSignal::new(track),
            color_mode: RwSignal::new(color_mode),
            system_mode: RwSignal::new(system_mode),
        }
    }

    /// Resolved light/dark for the `.dark` class. Mirrors React
    /// `effectiveColorMode = colorMode === 'system' ? systemColorMode : colorMode`.
    #[must_use]
    pub fn effective(&self) -> SystemMode {
        resolve_effective(self.color_mode.get(), self.system_mode.get())
    }

    /// User picks a color mode. Updates the signal; the body-class + cookie
    /// `Effect`s react. Mirrors React `setColorMode`.
    pub fn set_color_mode(&self, mode: ColorMode) {
        self.color_mode.set(mode);
    }

    /// Set the currency track. Not user-facing today; the future
    /// "follow default currency" wire-up calls this (React's `wallet.tsx`).
    pub fn set_track(&self, track: Track) {
        self.track.set(track);
    }
}

/// Provide [`ThemeState`] at the app root + install the reactive body-class
/// swap and cookie-persistence effects. Call once, inside the `App` component
/// owner (a valid reactive owner — see the `spawn_local`/`use_context` gotcha
/// note in `feedback_leptos_spawn_local_gotchas`).
pub fn provide_theme() {
    let state = ThemeState::new();
    provide_context(state);

    // On wasm: seed `system_mode` from the live OS preference (so the boot
    // class matches the inline default) and listen for OS changes while in
    // `system` mode — mirrors React's `matchMedia` effect.
    #[cfg(target_arch = "wasm32")]
    {
        if let Some(os) = detect_system_mode() {
            state.system_mode.set(os);
        }
        install_media_listener(state);
    }

    // Reactive body-class swap. Re-runs whenever track or effective mode
    // changes. On native (rlib unit tests) this is a harmless no-op.
    Effect::new(move |_| {
        let track = state.track.get();
        let effective = state.effective();
        apply_body_class(track, effective);
    });

    // Reactive cookie persistence. Re-runs on any axis change and rewrites all
    // three cookies (matching React's `saveCookies`, which always writes the
    // full triple).
    Effect::new(move |_| {
        let track = state.track.get();
        let color_mode = state.color_mode.get();
        let system_mode = state.system_mode.get();
        write_cookies(track, color_mode, system_mode);
    });
}

/// The user-facing color-mode switcher. Mirrors React's `ColorModeToggle`
/// (`app/features/theme/color-mode-toggle.tsx`): an icon button showing the
/// current mode's icon (Sun / Moon / SunMoon), opening a dropdown of the three
/// options as labelled icon rows. Placed in the settings footer, same slot as
/// React + iOS.
///
/// React uses a Radix `DropdownMenu`; we render a lightweight click-to-open
/// menu with the same shape (icon trigger, three labelled rows). Styling uses
/// Tailwind classes that resolve through the active theme bundle (`bg-popover`,
/// `text-popover-foreground`, etc.).
#[component]
pub fn ColorModeToggle() -> impl IntoView {
    let theme = expect_context::<ThemeState>();
    let open = RwSignal::new(false);

    let current_icon = move || mode_icon(theme.color_mode.get());

    view! {
        <div class="relative inline-flex">
            <button
                type="button"
                aria-label=move || {
                    format!(
                        "Current color mode: {}. Click to switch.",
                        theme.color_mode.get().label(),
                    )
                }
                aria-haspopup="menu"
                aria-expanded=move || open.get().to_string()
                class="flex h-9 w-9 items-center justify-center rounded-md \
                       text-foreground focus-visible:outline-hidden"
                on:click=move |_| open.update(|o| *o = !*o)
            >
                {current_icon}
            </button>
            <Show when=move || open.get()>
                <div
                    role="menu"
                    class="absolute right-0 z-50 mt-2 min-w-32 rounded-md border \
                           border-border bg-popover p-1 text-popover-foreground \
                           shadow-md"
                >
                    {ColorMode::ALL
                        .into_iter()
                        .map(|mode| {
                            view! {
                                <button
                                    type="button"
                                    role="menuitem"
                                    class="flex w-full items-center gap-2 rounded-sm \
                                           px-2 py-1.5 text-sm hover:bg-accent \
                                           hover:text-accent-foreground"
                                    on:click=move |_| {
                                        theme.set_color_mode(mode);
                                        open.set(false);
                                    }
                                >
                                    {mode_icon(mode)}
                                    <span class="font-mono capitalize">{mode.label()}</span>
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </Show>
        </div>
    }
}

/// lucide-react icon mirror for the given mode. Paths copied verbatim from
/// lucide-react v0.468.0 (`Sun` / `Moon` / `SunMoon`) — the exact icons React's
/// `ColorModeToggle` renders. `h-5 w-5` matches React's `className="h-5 w-5"`.
fn mode_icon(mode: ColorMode) -> impl IntoView {
    match mode {
        ColorMode::Light => sun_icon().into_any(),
        ColorMode::Dark => moon_icon().into_any(),
        ColorMode::System => sun_moon_icon().into_any(),
    }
}

/// Shared lucide SVG attributes (`stroke="currentColor"`, 24-box, round caps).
macro_rules! lucide_svg {
    ($($child:tt)*) => {
        view! {
            <svg
                class="h-5 w-5"
                xmlns="http://www.w3.org/2000/svg"
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                stroke-width="2"
                stroke-linecap="round"
                stroke-linejoin="round"
            >
                $($child)*
            </svg>
        }
    };
}

fn sun_icon() -> impl IntoView {
    lucide_svg! {
        <circle cx="12" cy="12" r="4"/>
        <path d="M12 2v2"/>
        <path d="M12 20v2"/>
        <path d="m4.93 4.93 1.41 1.41"/>
        <path d="m17.66 17.66 1.41 1.41"/>
        <path d="M2 12h2"/>
        <path d="M20 12h2"/>
        <path d="m6.34 17.66-1.41 1.41"/>
        <path d="m19.07 4.93-1.41 1.41"/>
    }
}

fn moon_icon() -> impl IntoView {
    lucide_svg! {
        <path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>
    }
}

fn sun_moon_icon() -> impl IntoView {
    lucide_svg! {
        <path d="M12 8a2.83 2.83 0 0 0 4 4 4 4 0 1 1-4-4"/>
        <path d="M12 2v2"/>
        <path d="M12 20v2"/>
        <path d="m4.9 4.9 1.4 1.4"/>
        <path d="m17.7 17.7 1.4 1.4"/>
        <path d="M2 12h2"/>
        <path d="M20 12h2"/>
        <path d="m6.3 17.7-1.4 1.4"/>
        <path d="m19.1 4.9-1.4 1.4"/>
    }
}

// ---- Cookie + DOM plumbing -----------------------------------------------
//
// Split by `target_arch`: the wasm build touches `document` / `window`; the
// native rlib build (workspace `cargo test`) gets pure no-ops + a default
// cookie read so the reactive harness still constructs `ThemeState`.

/// Read `(track, color_mode, system_mode)` from cookies, falling back to
/// React's defaults per-axis. Mirrors `getClientThemeCookies` BUT applies the
/// constant defaults when a cookie is missing (React's `useState` initialisers
/// do the same: `cookieSettings?.theme || defaultTheme`).
#[cfg(target_arch = "wasm32")]
fn read_cookies() -> (Track, ColorMode, SystemMode) {
    let track = cookie_value(THEME_COOKIE)
        .as_deref()
        .and_then(Track::from_cookie)
        .unwrap_or_default();
    let color_mode = cookie_value(COLOR_MODE_COOKIE)
        .as_deref()
        .and_then(ColorMode::from_cookie)
        .unwrap_or_default();
    let system_mode = cookie_value(SYSTEM_COLOR_MODE_COOKIE)
        .as_deref()
        .and_then(SystemMode::from_cookie)
        .unwrap_or_default();
    (track, color_mode, system_mode)
}

/// Native build: no document — return defaults so the unit-test harness can
/// build a `ThemeState`.
#[cfg(not(target_arch = "wasm32"))]
fn read_cookies() -> (Track, ColorMode, SystemMode) {
    (
        Track::default(),
        ColorMode::default(),
        SystemMode::default(),
    )
}

/// Read a single cookie by name. Mirrors React's
/// `getCookieValue` regex (`(?:^|; )name=([^;]*)`).
#[cfg(target_arch = "wasm32")]
fn cookie_value(name: &str) -> Option<String> {
    let doc = html_document()?;
    let cookie = doc.cookie().ok()?;
    for part in cookie.split("; ") {
        if let Some(rest) = part.strip_prefix(&format!("{name}=")) {
            return Some(rest.to_string());
        }
        // Tolerate a leading entry without the "; " separator.
        if let Some(rest) = part.strip_prefix(&format!("{name} =")) {
            return Some(rest.to_string());
        }
    }
    None
}

/// Write all three cookies. Mirrors React `saveCookies` exactly:
/// `name=value; path=/; samesite=lax; max-age=<one year>`.
#[cfg(target_arch = "wasm32")]
fn write_cookies(track: Track, color_mode: ColorMode, system_mode: SystemMode) {
    const ONE_YEAR: u32 = 60 * 60 * 24 * 365;
    let Some(doc) = html_document() else {
        return;
    };
    let set = |name: &str, value: &str| {
        let _ = doc.set_cookie(&format!(
            "{name}={value}; path=/; samesite=lax; max-age={ONE_YEAR}"
        ));
    };
    set(THEME_COOKIE, track.as_str());
    set(COLOR_MODE_COOKIE, color_mode.as_str());
    set(SYSTEM_COLOR_MODE_COOKIE, system_mode.as_str());
}

/// Native build: nothing to persist.
#[cfg(not(target_arch = "wasm32"))]
fn write_cookies(_track: Track, _color_mode: ColorMode, _system_mode: SystemMode) {}

/// Set the `<body>` class list to `"{track}"` or `"{track} dark"`. Mirrors
/// React `updateDocumentClasses` (which clears the track + light/dark classes
/// then re-adds), but the Leptos PWA only ever puts theme classes on `<body>`,
/// so a full overwrite is correct + simpler. `.dark` only when effective is
/// dark — React adds `light`/`dark` explicitly, but the ported stylesheet keys
/// dark styles off `.dark` (light is the base), so emitting `dark` only is
/// equivalent for the cascade.
#[cfg(target_arch = "wasm32")]
fn apply_body_class(track: Track, effective: SystemMode) {
    let Some(body) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.body())
    else {
        return;
    };
    let class = match effective {
        SystemMode::Dark => format!("{} dark", track.as_str()),
        SystemMode::Light => track.as_str().to_string(),
    };
    body.set_class_name(&class);
}

/// Native build: no DOM body.
#[cfg(not(target_arch = "wasm32"))]
fn apply_body_class(_track: Track, _effective: SystemMode) {}

/// Read the live OS `prefers-color-scheme`. Mirrors React's
/// `window.matchMedia('(prefers-color-scheme: dark)').matches`.
#[cfg(target_arch = "wasm32")]
fn detect_system_mode() -> Option<SystemMode> {
    let mql = web_sys::window()?
        .match_media("(prefers-color-scheme: dark)")
        .ok()??;
    Some(if mql.matches() {
        SystemMode::Dark
    } else {
        SystemMode::Light
    })
}

/// Listen for OS color-scheme changes and update `system_mode`. Active for the
/// app's lifetime (the closure is `forget`-leaked, same lifetime as the app);
/// the body-class/cookie effects only consult `system_mode` while `color_mode`
/// is `System`, so updates are harmless in forced modes. Mirrors React's
/// `mediaQuery.addEventListener('change', …)`.
#[cfg(target_arch = "wasm32")]
fn install_media_listener(state: ThemeState) {
    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;

    let Some(window) = web_sys::window() else {
        return;
    };
    let Ok(Some(mql)) = window.match_media("(prefers-color-scheme: dark)") else {
        return;
    };

    let closure = Closure::<dyn FnMut(ev::Event)>::new(move |_ev: ev::Event| {
        if let Some(os) = detect_system_mode() {
            state.system_mode.set(os);
        }
    });
    let _ = mql.add_event_listener_with_callback("change", closure.as_ref().unchecked_ref());
    closure.forget();
}

/// `document` cast to `HtmlDocument` for the cookie getter/setter (cookies live
/// on `HtmlDocument`, not the base `Document`, in web-sys).
#[cfg(target_arch = "wasm32")]
fn html_document() -> Option<web_sys::HtmlDocument> {
    use wasm_bindgen::JsCast;
    web_sys::window()?.document()?.dyn_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_cookie_roundtrip() {
        assert_eq!(Track::from_cookie("btc"), Some(Track::Btc));
        assert_eq!(Track::from_cookie("usd"), Some(Track::Usd));
        assert_eq!(Track::from_cookie("nope"), None);
        assert_eq!(Track::Btc.as_str(), "btc");
        assert_eq!(Track::Usd.as_str(), "usd");
    }

    #[test]
    fn color_mode_cookie_roundtrip() {
        for mode in ColorMode::ALL {
            assert_eq!(ColorMode::from_cookie(mode.as_str()), Some(mode));
        }
        assert_eq!(ColorMode::from_cookie("system"), Some(ColorMode::System));
        assert_eq!(ColorMode::from_cookie("garbage"), None);
    }

    #[test]
    fn defaults_match_react() {
        // React: defaultTheme = 'btc', defaultColorMode = 'system',
        // defaultSystemColorMode = 'light'.
        assert_eq!(Track::default(), Track::Btc);
        assert_eq!(ColorMode::default(), ColorMode::System);
        assert_eq!(SystemMode::default(), SystemMode::Light);
    }

    #[test]
    fn effective_resolves_system_to_observed() {
        // `system` collapses to the observed OS preference; forced modes win.
        assert_eq!(
            resolve_effective(ColorMode::System, SystemMode::Dark),
            SystemMode::Dark
        );
        assert_eq!(
            resolve_effective(ColorMode::System, SystemMode::Light),
            SystemMode::Light
        );
        assert_eq!(
            resolve_effective(ColorMode::Light, SystemMode::Dark),
            SystemMode::Light
        );
        assert_eq!(
            resolve_effective(ColorMode::Dark, SystemMode::Light),
            SystemMode::Dark
        );
    }

    #[test]
    fn system_mode_cookie_roundtrip() {
        assert_eq!(SystemMode::Light.as_str(), "light");
        assert_eq!(SystemMode::Dark.as_str(), "dark");
        assert_eq!(SystemMode::from_cookie("light"), Some(SystemMode::Light));
        assert_eq!(SystemMode::from_cookie("dark"), Some(SystemMode::Dark));
        assert_eq!(SystemMode::from_cookie("x"), None);
    }

    #[test]
    fn labels_are_capitalized() {
        assert_eq!(ColorMode::Light.label(), "Light");
        assert_eq!(ColorMode::Dark.label(), "Dark");
        assert_eq!(ColorMode::System.label(), "System");
    }
}
