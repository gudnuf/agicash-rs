//! `AnimatedA` — drop-in `<A/>` replacement that triggers a view
//! transition on click.
//!
//! Why not wrap `leptos_router::components::A`? `<A/>` calls
//! `use_navigate()` internally on its own click handler, and we have
//! no hook to intercept BEFORE that runs. The simplest answer is to
//! render a plain `<a href=...>` and own the click pipeline end to
//! end — which is exactly what React Router 7's
//! `<Link viewTransition>` does internally.
//!
//! The plain `<a>` is also degrade-friendly: with JS disabled, the
//! browser still navigates via the href. With the View Transitions
//! API unavailable, the click handler still routes (just without
//! animation). See [`super::view_transition::start_view_transition`]
//! for the feature detect.

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use leptos_router::NavigateOptions;

#[cfg(target_arch = "wasm32")]
use super::view_transition::clear_transition_styles;
use super::view_transition::{apply_transition_styles, start_view_transition};
use super::Transition;

/// Drop-in replacement for `<A href=... />` that animates the route
/// change using the View Transitions API.
///
/// Example:
///
/// ```ignore
/// view! {
///     <AnimatedA href="/receive" transition=Transition::SlideUp>
///         "Receive"
///     </AnimatedA>
/// }
/// ```
///
/// The render is a plain `<a>` so:
///   - middle-click / cmd-click still opens in a new tab (preventDefault
///     only fires on left-click without modifiers)
///   - JS-disabled browsers still navigate
///   - the link is a real link for accessibility / screenreaders
#[component]
pub fn AnimatedA(
    /// Destination href. Treated as a same-origin SPA path; goes
    /// straight to `leptos_router::hooks::use_navigate`.
    #[prop(into)]
    href: String,
    /// Which animation to play. See [`Transition`].
    transition: Transition,
    children: Children,
) -> impl IntoView {
    let navigate = use_navigate();
    let href_for_click = href.clone();

    // The click handler. Boxed and shared because Leptos's `on:click`
    // attribute takes an `FnMut` and we need to capture `navigate` +
    // `href_for_click` by move.
    let on_click = move |ev: leptos::ev::MouseEvent| {
        // Bail to default browser navigation for modifier-clicks
        // (cmd-click, middle-click etc.) so "open in new tab" works.
        if ev.default_prevented()
            || ev.button() != 0
            || ev.meta_key()
            || ev.ctrl_key()
            || ev.shift_key()
            || ev.alt_key()
        {
            return;
        }

        ev.prevent_default();

        // 1. Set the CSS direction variables on <html> so the
        //    ::view-transition-{old,new}(root) rules know which
        //    keyframes to play. Must happen BEFORE the browser
        //    snapshots the old DOM (i.e. before startViewTransition's
        //    callback fires).
        apply_transition_styles(transition);

        // 2. Kick off the view transition. The callback is what
        //    actually mutates the DOM (leptos_router swaps the
        //    matched <Outlet/> child). The browser took a snapshot
        //    of the old DOM in step 1's tick, runs this callback
        //    synchronously, snapshots the new DOM, and then plays
        //    the crossfade using the CSS rules.
        let nav = navigate.clone();
        let to = href_for_click.clone();
        start_view_transition(move || {
            nav(&to, NavigateOptions::default());
        });

        // 3. Schedule cleanup. The CSS variables must come back off
        //    so the next navigation gets a fresh slate. ~220ms gives
        //    the animation a comfortable margin past the 180ms
        //    duration in transitions.css.
        #[cfg(target_arch = "wasm32")]
        schedule_cleanup_ms(220);
    };

    view! {
        <a href=href on:click=on_click>
            {children()}
        </a>
    }
}

/// Spawn a setTimeout that runs `clear_transition_styles` after `ms`.
/// `gloo-timers::callback::Timeout` would be more idiomatic but is
/// not in the workspace deps; we already have `web-sys` Window in the
/// dep set so a direct `set_timeout_with_callback_and_timeout_and_arguments_0`
/// call is one line.
#[cfg(target_arch = "wasm32")]
fn schedule_cleanup_ms(ms: i32) {
    use wasm_bindgen::prelude::*;
    use wasm_bindgen::JsCast;

    // Closure::once_into_js returns a JsValue that wraps a function;
    // setTimeout wants a `&js_sys::Function`. unchecked_ref does the
    // cast without runtime overhead — safe here because we know the
    // closure produced a function.
    let cb: JsValue = Closure::once_into_js(move || {
        clear_transition_styles();
    });
    let func: &js_sys::Function = cb.unchecked_ref();

    if let Some(window) = web_sys::window() {
        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(func, ms);
    }
}
