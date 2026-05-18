//! Browser bindings for the View Transitions API.
//!
//! All real work is wasm-only. The native `rlib` build (for unit
//! tests in non-wasm pieces of the crate) gets no-op stubs so the
//! workspace `cargo test` still compiles.
//!
//! We deliberately do NOT use `web_sys::Document::start_view_transition`
//! even though it exists in `web-sys 0.3.98` — that binding is gated
//! behind `web_sys_unstable_apis`, which would require a workspace
//! cfg flag and complicates the spike's blast radius. Instead we go
//! through `js_sys::Reflect` to:
//!
//!   1. Feature-detect (`startViewTransition` may be `undefined` on
//!      Firefox <134 / Safari <18).
//!   2. Construct the JS callback that wraps our Rust closure.
//!   3. Invoke the method dynamically.
//!
//! If the API is missing, [`start_view_transition`] just runs the
//! callback synchronously — the route still changes, the user just
//! doesn't see an animation. Matches the React side's graceful
//! degradation behavior.

use super::Transition;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

/// Write the CSS variables that `style/transitions.css` reads via
/// `var(--vt-direction-in)` etc. Must be called BEFORE
/// [`start_view_transition`] so the variables are set when the
/// browser takes its snapshot of the old DOM.
///
/// The React side calls the equivalent setter synchronously in the
/// link's `onClick` handler — we do the same.
#[cfg(target_arch = "wasm32")]
pub fn apply_transition_styles(t: Transition) {
    if let Some(style) = root_style() {
        let _ = style.set_property("--vt-direction-out", t.out_animation());
        let _ = style.set_property("--vt-direction-in", t.in_animation());
        let _ = style.set_property("--vt-z-index-out", t.out_z_index());
        let _ = style.set_property("--vt-z-index-in", t.in_z_index());
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn apply_transition_styles(_t: Transition) {}

/// Remove the CSS variables set by [`apply_transition_styles`]. Run
/// after the animation finishes so the next navigation gets a clean
/// slate (otherwise a follow-up route change would inherit the
/// previous direction and animate wrong).
#[cfg(target_arch = "wasm32")]
pub fn clear_transition_styles() {
    if let Some(style) = root_style() {
        let _ = style.remove_property("--vt-direction-out");
        let _ = style.remove_property("--vt-direction-in");
        let _ = style.remove_property("--vt-z-index-out");
        let _ = style.remove_property("--vt-z-index-in");
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn clear_transition_styles() {}

/// Call `document.startViewTransition(callback)` if the API exists,
/// otherwise run `callback` directly.
///
/// The callback is what mutates the DOM (in our case: invoke
/// `use_navigate()` which triggers `leptos_router` to swap pages).
/// The browser takes a snapshot of the page BEFORE running the
/// callback, runs it, takes a second snapshot of the new state, and
/// then crossfades between them using whatever CSS animations are
/// targeted at `::view-transition-old/new(root)`.
/// Type alias for the nested ownership chain that holds the `FnOnce`
/// across the JS callback boundary. Used by [`start_view_transition`].
/// Hoisted to module scope so clippy's `items_after_statements` is
/// happy and the type is named once instead of inline-expanded.
#[cfg(target_arch = "wasm32")]
type FnOnceSlot = std::rc::Rc<std::cell::RefCell<Option<Box<dyn FnOnce()>>>>;

#[cfg(target_arch = "wasm32")]
pub fn start_view_transition<F: FnOnce() + 'static>(callback: F) {
    use js_sys::{Function, Reflect};
    use std::cell::RefCell;
    use std::rc::Rc;

    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        callback();
        return;
    };

    let doc_val: &JsValue = document.as_ref();
    let method_val = Reflect::get(doc_val, &JsValue::from_str("startViewTransition"));

    let method: Function = match method_val {
        Ok(v) if v.is_function() => v.into(),
        _ => {
            // Firefox <134, Safari <18, etc. Just navigate, no animation.
            callback();
            return;
        }
    };

    // Wrap the FnOnce in an Rc<RefCell<Option<F>>> so the Closure can
    // own a stable `Fn` (called once by the browser) that drops F on
    // its first invocation.
    //
    // IMPORTANT: The browser invokes `updateCallback` ASYNCHRONOUSLY —
    // at the start of the next frame after `startViewTransition`
    // returns. We MUST keep the Closure alive past the call1 below
    // until the browser actually calls it. `Closure::into_js_value`
    // hands ownership to the JS garbage collector; the browser holds
    // the function reference long enough to invoke it once, then
    // releases. Tiny one-time leak per navigation — acceptable.
    let slot: FnOnceSlot = Rc::new(RefCell::new(Some(Box::new(callback))));
    let slot_clone = slot.clone();

    let js_callback = Closure::wrap(Box::new(move || {
        if let Some(cb) = slot_clone.borrow_mut().take() {
            cb();
        }
    }) as Box<dyn FnMut()>);

    let js_value = js_callback.into_js_value();
    let _ = method.call1(doc_val, &js_value);
}

#[cfg(not(target_arch = "wasm32"))]
pub fn start_view_transition<F: FnOnce() + 'static>(callback: F) {
    callback();
}

#[cfg(target_arch = "wasm32")]
fn root_style() -> Option<web_sys::CssStyleDeclaration> {
    let document = web_sys::window()?.document()?;
    let root = document.document_element()?;
    let html: web_sys::HtmlElement = root.dyn_into().ok()?;
    Some(html.style())
}
