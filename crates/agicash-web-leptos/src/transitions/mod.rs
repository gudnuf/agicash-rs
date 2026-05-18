//! View Transitions API integration for `leptos_router`.
//!
//! ## What this is
//!
//! A thin wrapper around the browser's native View Transitions API
//! (`document.startViewTransition`) that hooks into `leptos_router`'s
//! navigation lifecycle so route changes can animate with platform
//! primitives — fade, slide, and shared-element morphs.
//!
//! ## Mechanism
//!
//! `leptos_router` 0.7 does NOT call `startViewTransition` itself
//! (unlike React Router 7's `viewTransition: true` flag). We
//! intercept link clicks at the `<a>` level via the [`AnimatedA`]
//! component:
//!
//! 1. `prevent_default()` so the browser doesn't navigate the page
//! 2. Write CSS direction variables on `<html>` so the
//!    `::view-transition-old(root)` / `::view-transition-new(root)`
//!    rules in `style/transitions.css` know which keyframes to play
//! 3. Call `document.startViewTransition(callback)` where the
//!    callback invokes `use_navigate()(href, _)`
//! 4. Schedule a cleanup that wipes the CSS vars after the animation
//!    finishes (~220ms), so subsequent navigations don't inherit
//!    stale direction state
//!
//! ## Browser support
//!
//! View Transitions API: Chrome 111+, Edge 111+, Safari 18+,
//! Firefox 134+. Below those versions [`view_transition::start_view_transition`]
//! detects `startViewTransition === undefined` on `document` and just
//! invokes the callback directly — the route still changes, just
//! without animation.
//!
//! ## Reference
//!
//! Mirrors the React app's `app/lib/transitions/view-transition.tsx`
//! pattern (see archived branch `agicash-rs/archive/react-web-app`,
//! tag `react-web-app-final`). The CSS shape is identical so the
//! visual feel matches the legacy app.
//!
//! ## Spike caveats
//!
//! - Back-button reverse animation is NOT implemented. The React side
//!   tracks `window.history.state.idx` and reverses the stored
//!   transition; we'd need the same here for production parity.
//! - The cleanup timer is a fixed 220ms rather than `.finished.then(...)`
//!   on the returned `ViewTransition` promise. Production should
//!   await the real promise.

mod animated_link;
mod view_transition;

pub use animated_link::AnimatedA;
pub use view_transition::{
    apply_transition_styles, clear_transition_styles, start_view_transition,
};

/// The animation vocabulary supported by the spike. Mirrors the
/// React app's [`Transition`] union in
/// `app/lib/transitions/view-transition.tsx`.
///
/// `SlideLeft` / `SlideRight` are for forward / back stack navigation
/// (the iOS push-stack feel). `SlideUp` / `SlideDown` are for modal
/// sheets — `SlideUp` brings a sheet in from the bottom (e.g.
/// `/receive`), `SlideDown` dismisses it back down. `Fade` is the
/// default cross-route transition.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Transition {
    Fade,
    SlideUp,
    SlideDown,
    SlideLeft,
    SlideRight,
}

impl Transition {
    /// Keyframe name played on `::view-transition-old(root)` — the
    /// snapshot of the page we're navigating AWAY from. Matches the
    /// `@keyframes` defined in `style/transitions.css`.
    pub fn out_animation(self) -> &'static str {
        match self {
            Self::Fade => "agicash-vt-fade-out",
            Self::SlideUp => "agicash-vt-slide-out-to-top",
            Self::SlideDown => "agicash-vt-slide-out-to-bottom",
            Self::SlideLeft => "agicash-vt-slide-out-to-left",
            Self::SlideRight => "agicash-vt-slide-out-to-right",
        }
    }

    /// Keyframe name played on `::view-transition-new(root)` — the
    /// snapshot of the page we're navigating TO.
    pub fn in_animation(self) -> &'static str {
        match self {
            Self::Fade => "agicash-vt-fade-in",
            Self::SlideUp => "agicash-vt-slide-in-from-bottom",
            Self::SlideDown => "agicash-vt-slide-in-from-top",
            Self::SlideLeft => "agicash-vt-slide-in-from-right",
            Self::SlideRight => "agicash-vt-slide-in-from-left",
        }
    }

    /// Z-index for the outgoing snapshot. The slide transitions need
    /// the moving snapshot to sit above the stationary one so the
    /// effect reads as "this page slides off" rather than "both pages
    /// shuffle in place". Fade does not need stacking.
    pub fn out_z_index(self) -> &'static str {
        match self {
            Self::Fade => "auto",
            Self::SlideUp | Self::SlideLeft => "1",
            Self::SlideDown | Self::SlideRight => "0",
        }
    }

    /// Z-index for the incoming snapshot. Mirror of [`Self::out_z_index`].
    pub fn in_z_index(self) -> &'static str {
        match self {
            Self::Fade => "auto",
            Self::SlideUp | Self::SlideLeft => "0",
            Self::SlideDown | Self::SlideRight => "1",
        }
    }
}
